//! Signature-based file carving ("deep scan"). Works without any filesystem
//! metadata — after formatting, or on corrupted/RAW partitions. Each sector
//! start is checked for a known header; a format-specific parser then walks
//! the file's internal structure to find its exact end.

use crate::device::Device;
use crate::model::*;
use crate::scan::Sink;
use crate::util::*;

const WIN: usize = 1 << 20;

/// Windowed random-access reader relative to a carve start position.
struct Rd<'a> {
    dev: &'a Device,
    start: u64,
    max: u64,
    buf: Vec<u8>,
    buf_rel: u64,
}

impl<'a> Rd<'a> {
    fn new(dev: &'a Device, start: u64, max: u64) -> Self {
        let max = max.min(dev.size().saturating_sub(start));
        Rd { dev, start, max, buf: Vec::new(), buf_rel: 0 }
    }
    fn get(&mut self, rel: u64, n: usize) -> Option<&[u8]> {
        if rel + n as u64 > self.max {
            return None;
        }
        if rel < self.buf_rel || rel + n as u64 > self.buf_rel + self.buf.len() as u64 {
            let len = (n.max(WIN) as u64).min(self.max - rel) as usize;
            self.buf = self.dev.read_vec(self.start + rel, len).ok()?;
            self.buf_rel = rel;
            if self.buf.len() < n {
                return None;
            }
        }
        let o = (rel - self.buf_rel) as usize;
        Some(&self.buf[o..o + n])
    }
    fn u8(&mut self, rel: u64) -> Option<u8> {
        self.get(rel, 1).map(|b| b[0])
    }
    fn be16(&mut self, rel: u64) -> Option<u16> {
        self.get(rel, 2).map(|b| be16(b, 0))
    }
    fn be32(&mut self, rel: u64) -> Option<u32> {
        self.get(rel, 4).map(|b| be32(b, 0))
    }
    fn le32(&mut self, rel: u64) -> Option<u32> {
        self.get(rel, 4).map(|b| le32(b, 0))
    }
    /// Find `pat` in [from, to). Returns relative offset.
    fn find(&mut self, from: u64, to: u64, pat: &[u8]) -> Option<u64> {
        let to = to.min(self.max);
        let mut pos = from;
        while pos + pat.len() as u64 <= to {
            let n = ((to - pos) as usize).min(WIN);
            if n < pat.len() {
                return None;
            }
            let chunk = self.get(pos, n)?.to_vec();
            if let Some(i) = chunk.windows(pat.len()).position(|w| w == pat) {
                return Some(pos + i as u64);
            }
            pos += (n - pat.len() + 1) as u64;
        }
        None
    }
    fn find_byte(&mut self, from: u64, b: u8) -> Option<u64> {
        let mut pos = from;
        while pos < self.max {
            let n = ((self.max - pos) as usize).min(WIN);
            let chunk = self.get(pos, n)?;
            if let Some(i) = chunk.iter().position(|&x| x == b) {
                return Some(pos + i as u64);
            }
            pos += n as u64;
        }
        None
    }
}

/// Result of a format parser: length, extension, whether the end was found.
type Hit = (u64, &'static str, bool);

fn jpeg(r: &mut Rd) -> Option<Hit> {
    let mut p = 2u64;
    let mut sof = false;
    let mut sos = false;
    loop {
        if r.u8(p)? != 0xFF {
            return if sos && sof { Some((p, "jpg", false)) } else { None };
        }
        while r.u8(p + 1)? == 0xFF {
            p += 1;
        }
        let m = r.u8(p + 1)?;
        p += 2;
        match m {
            0xD9 => return if sof && sos { Some((p, "jpg", true)) } else { None },
            0x01 | 0xD0..=0xD7 => continue,
            0xD8 | 0x00 => return None,
            _ => {}
        }
        let len = r.be16(p)? as u64;
        if len < 2 {
            return None;
        }
        if matches!(m, 0xC0..=0xCF) && !matches!(m, 0xC4 | 0xC8 | 0xCC) {
            sof = true;
        }
        p += len;
        if m == 0xDA {
            sos = true;
            // entropy-coded data until a real marker
            loop {
                let q = match r.find_byte(p, 0xFF) {
                    Some(q) => q,
                    None => return Some((r.max, "jpg", false)),
                };
                let n = r.u8(q + 1)?;
                if n == 0x00 || (0xD0..=0xD7).contains(&n) {
                    p = q + 2;
                } else if n == 0xFF {
                    p = q + 1;
                } else {
                    p = q;
                    break;
                }
            }
        }
    }
}

fn png(r: &mut Rd) -> Option<Hit> {
    let mut p = 8u64;
    let mut first = true;
    loop {
        let len = r.be32(p)? as u64;
        let t = r.get(p + 4, 4)?.to_vec();
        if len > 0x7FFF_FFFF || !t.iter().all(|c| c.is_ascii_alphabetic()) {
            return if first { None } else { Some((p, "png", false)) };
        }
        if first && &t != b"IHDR" {
            return None;
        }
        first = false;
        p += 12 + len;
        if &t == b"IEND" {
            return Some((p, "png", true));
        }
        if p > r.max {
            return Some((r.max, "png", false));
        }
    }
}

fn gif(r: &mut Rd) -> Option<Hit> {
    let flags = r.u8(10)?;
    let mut p = 13u64;
    if flags & 0x80 != 0 {
        p += 3 * (1u64 << ((flags & 7) + 1));
    }
    let skip_blocks = |r: &mut Rd, mut p: u64| -> Option<u64> {
        loop {
            let n = r.u8(p)? as u64;
            p += 1;
            if n == 0 {
                return Some(p);
            }
            p += n;
        }
    };
    let mut frames = 0;
    loop {
        match r.u8(p)? {
            0x21 => p = skip_blocks(r, p + 2)?,
            0x2C => {
                let f = r.u8(p + 9)?;
                p += 10;
                if f & 0x80 != 0 {
                    p += 3 * (1u64 << ((f & 7) + 1));
                }
                p = skip_blocks(r, p + 1)?;
                frames += 1;
            }
            0x3B => return if frames > 0 { Some((p + 1, "gif", true)) } else { None },
            _ => return if frames > 0 { Some((p, "gif", false)) } else { None },
        }
    }
}

fn bmp(r: &mut Rd) -> Option<Hit> {
    let h = r.get(0, 30)?.to_vec();
    let size = le32(&h, 2) as u64;
    let off = le32(&h, 10) as u64;
    let dib = le32(&h, 14);
    let planes = le16(&h, 26);
    let bpp = le16(&h, 28);
    if le32(&h, 6) != 0
        || !matches!(dib, 12 | 40 | 52 | 56 | 108 | 124)
        || planes != 1
        || !matches!(bpp, 1 | 4 | 8 | 16 | 24 | 32)
        || off < 26
        || off >= size
        || size > 512 << 20
    {
        return None;
    }
    Some((size, "bmp", size <= r.max))
}

fn pdf(r: &mut Rd) -> Option<Hit> {
    let mut end = None;
    let mut from = 5u64;
    loop {
        let limit = match end {
            None => r.max,
            Some(e) => e + (1 << 20),
        };
        let Some(q) = r.find(from, limit, b"%%EOF") else { break };
        if let Some(e) = end {
            if r.find(e, q, b"%PDF-").is_some() {
                break;
            }
        }
        let mut e = q + 5;
        if r.u8(e) == Some(b'\r') {
            e += 1;
        }
        if r.u8(e) == Some(b'\n') {
            e += 1;
        }
        end = Some(e);
        from = e;
    }
    end.map(|e| (e, "pdf", true))
}

fn zip(r: &mut Rd) -> Option<Hit> {
    let mut from = 4u64;
    loop {
        let q = r.find(from, r.max, b"PK\x05\x06")?;
        let e = r.get(q, 22)?.to_vec();
        let cd_size = le32(&e, 12) as u64;
        let cd_off = le32(&e, 16) as u64;
        let cmt = le16(&e, 20) as u64;
        if cd_off + cd_size == q && cd_size > 0 {
            let cd = r.get(cd_off, cd_size.min(4 << 20) as usize)?.to_vec();
            let mut ext = "zip";
            let mut p = 0usize;
            while p + 46 <= cd.len() && &cd[p..p + 4] == b"PK\x01\x02" {
                let nl = le16(&cd, p + 28) as usize;
                let xl = le16(&cd, p + 30) as usize;
                let cl = le16(&cd, p + 32) as usize;
                let name = &cd[(p + 46).min(cd.len())..(p + 46 + nl).min(cd.len())];
                if name.starts_with(b"word/") {
                    ext = "docx";
                } else if name.starts_with(b"xl/") {
                    ext = "xlsx";
                } else if name.starts_with(b"ppt/") {
                    ext = "pptx";
                } else if name == b"AndroidManifest.xml" {
                    ext = "apk";
                } else if name == b"mimetype" {
                    let mt = r.get(38, 50).map(|m| m.to_vec()).unwrap_or_default();
                    let s = String::from_utf8_lossy(&mt);
                    if s.contains("opendocument.text") {
                        ext = "odt";
                    } else if s.contains("opendocument.spreadsheet") {
                        ext = "ods";
                    } else if s.contains("opendocument.presentation") {
                        ext = "odp";
                    } else if s.contains("epub") {
                        ext = "epub";
                    }
                }
                if ext != "zip" {
                    break;
                }
                p += 46 + nl + xl + cl;
            }
            return Some((q + 22 + cmt, ext, true));
        }
        from = q + 4;
    }
}

fn riff(r: &mut Rd) -> Option<Hit> {
    let size = r.le32(4)? as u64 + 8;
    let form = r.get(8, 4)?.to_vec();
    let ext = match &form[..] {
        b"WAVE" => "wav",
        b"AVI " => "avi",
        b"WEBP" => "webp",
        _ => return None,
    };
    if size < 16 {
        return None;
    }
    Some((size, ext, size <= r.max))
}

fn mp4(r: &mut Rd) -> Option<Hit> {
    const BOXES: &[&[u8; 4]] = &[
        b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide", b"uuid", b"pdin", b"meta", b"moof", b"mfra", b"styp",
        b"sidx", b"udta", b"pnot", b"PICT", b"junk", b"prfl", b"iinf", b"iloc", b"idat",
    ];
    let brand = r.get(8, 4)?.to_vec();
    let ext = match &brand[..] {
        b"qt  " => "mov",
        b"M4A " | b"M4B " => "m4a",
        b"heic" | b"heix" | b"mif1" | b"msf1" | b"hevc" => "heic",
        b"avif" => "avif",
        b if b.starts_with(b"3g") => "3gp",
        _ => "mp4",
    };
    let mut p = 0u64;
    let (mut moov, mut mdat, mut meta) = (false, false, false);
    while p + 8 <= r.max {
        let mut sz = r.be32(p)? as u64;
        let t = r.get(p + 4, 4)?.to_vec();
        if !BOXES.iter().any(|b| &b[..] == &t[..]) {
            break;
        }
        if sz == 1 {
            sz = r.get(p + 8, 8).map(|b| be64(b, 0))?;
        }
        if sz < 8 {
            break;
        }
        match &t[..] {
            b"moov" => moov = true,
            b"mdat" => mdat = true,
            b"meta" => meta = true,
            _ => {}
        }
        p += sz;
    }
    if p > r.max {
        return Some((r.max, ext, false));
    }
    if (moov && mdat) || (ext == "heic" && meta) {
        Some((p, ext, true))
    } else if moov || mdat {
        Some((p, ext, false))
    } else {
        None
    }
}

fn ole2(r: &mut Rd) -> Option<Hit> {
    let h = r.get(0, 512)?.to_vec();
    let shift = le16(&h, 30);
    if shift != 9 && shift != 12 {
        return None;
    }
    let ssz = 1u64 << shift;
    let nfat = le32(&h, 44) as usize;
    let dir_start = le32(&h, 48);
    let mut difat_sec = le32(&h, 68);
    if nfat == 0 || nfat > 1 << 16 {
        return None;
    }
    let mut fat_secs: Vec<u32> = (0..109).map(|i| le32(&h, 76 + i * 4)).filter(|&s| s < 0xFFFF_FFFA).collect();
    let mut guard = 0;
    while fat_secs.len() < nfat && difat_sec < 0xFFFF_FFFA && guard < 4096 {
        let d = r.get((difat_sec as u64 + 1) * ssz, ssz as usize)?.to_vec();
        let n = ssz as usize / 4 - 1;
        fat_secs.extend((0..n).map(|i| le32(&d, i * 4)).filter(|&s| s < 0xFFFF_FFFA));
        difat_sec = le32(&d, n * 4);
        guard += 1;
    }
    fat_secs.truncate(nfat);
    let mut max_used: i64 = -1;
    for (k, &s) in fat_secs.iter().enumerate() {
        let f = r.get((s as u64 + 1) * ssz, ssz as usize)?.to_vec();
        for i in 0..(ssz as usize / 4) {
            if le32(&f, i * 4) != 0xFFFF_FFFF {
                max_used = max_used.max((k * (ssz as usize / 4) + i) as i64);
            }
        }
    }
    if max_used < 0 {
        return None;
    }
    let size = (max_used as u64 + 2) * ssz;
    let mut ext = "doc";
    if let Some(d) = r.get((dir_start as u64 + 1) * ssz, ssz as usize).map(|d| d.to_vec()) {
        let mut names = Vec::new();
        for e in d.chunks_exact(128) {
            let nl = (le16(e, 64) as usize).min(64);
            names.push(utf16le(&e[..nl.saturating_sub(2)]));
        }
        if names.iter().any(|n| n == "WordDocument") {
            ext = "doc";
        } else if names.iter().any(|n| n == "Workbook" || n == "Book") {
            ext = "xls";
        } else if names.iter().any(|n| n == "PowerPoint Document") {
            ext = "ppt";
        } else if names.iter().any(|n| n.starts_with("__substg1.0_")) {
            ext = "msg";
        } else {
            ext = "ole";
        }
    }
    Some((size, ext, size <= r.max))
}

fn mp3(r: &mut Rd) -> Option<Hit> {
    let h = r.get(0, 10)?.to_vec();
    let tag = ((h[6] as u64 & 0x7F) << 21) | ((h[7] as u64 & 0x7F) << 14) | ((h[8] as u64 & 0x7F) << 7) | (h[9] as u64 & 0x7F);
    let mut p = 10 + tag + if h[5] & 0x10 != 0 { 10 } else { 0 };
    // some encoders pad after the tag
    let mut pad = 0;
    while r.u8(p)? == 0 && pad < 65536 {
        p += 1;
        pad += 1;
    }
    const BR1: [u32; 15] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
    const BR2: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
    let mut frames = 0u32;
    loop {
        let Some(f) = r.get(p, 4).map(|b| b.to_vec()) else { break };
        if f[0] != 0xFF || f[1] & 0xE0 != 0xE0 {
            break;
        }
        let ver = (f[1] >> 3) & 3;
        let layer = (f[1] >> 1) & 3;
        let bi = (f[2] >> 4) as usize;
        let si = ((f[2] >> 2) & 3) as usize;
        if ver == 1 || layer != 1 || bi == 0 || bi == 15 || si == 3 {
            break;
        }
        let pad = ((f[2] >> 1) & 1) as u64;
        let (br, sr, k) = match ver {
            3 => (BR1[bi], [44100, 48000, 32000][si], 144u64),
            2 => (BR2[bi], [22050, 24000, 16000][si], 72),
            _ => (BR2[bi], [11025, 12000, 8000][si], 72),
        };
        let len = k * br as u64 * 1000 / sr as u64 + pad;
        if len < 24 {
            break;
        }
        p += len;
        frames += 1;
    }
    if frames < 8 {
        return None;
    }
    if r.get(p, 3) == Some(b"TAG") {
        p += 128;
    }
    Some((p, "mp3", true))
}

fn sevenz(r: &mut Rd) -> Option<Hit> {
    let h = r.get(0, 32)?.to_vec();
    let off = le64(&h, 12);
    let size = le64(&h, 20);
    let total = 32u64.checked_add(off)?.checked_add(size)?;
    if size == 0 || size > 1 << 30 || total > 16 << 30 {
        return None;
    }
    Some((total, "7z", total <= r.max))
}

fn ebml_vint(r: &mut Rd, p: u64) -> Option<(u64, u64, bool)> {
    let b0 = r.u8(p)?;
    if b0 == 0 {
        return None;
    }
    let n = b0.leading_zeros() as u64 + 1;
    let mut v = (b0 as u64) & ((1u64 << (8 - n)) - 1);
    let mut all_ones = v == (1u64 << (8 - n)) - 1;
    for i in 1..n {
        let b = r.u8(p + i)?;
        all_ones &= b == 0xFF;
        v = (v << 8) | b as u64;
    }
    Some((v, n, all_ones))
}

fn mkv(r: &mut Rd) -> Option<Hit> {
    let (hsize, hl, _) = ebml_vint(r, 4)?;
    let doc = r.get(4 + hl, hsize.min(64) as usize)?.to_vec();
    let ext = if doc.windows(4).any(|w| w == b"webm") { "webm" } else { "mkv" };
    let p = 4 + hl + hsize;
    if r.get(p, 4)? != [0x18, 0x53, 0x80, 0x67] {
        return None;
    }
    let (ssize, sl, unknown) = ebml_vint(r, p + 4)?;
    if unknown {
        return Some((r.max, ext, false));
    }
    let end = p + 4 + sl + ssize;
    Some((end.min(r.max), ext, end <= r.max))
}

/// Header signature check at a sector start. Returns (parser, max file size).
fn detect(b: &[u8]) -> Option<(fn(&mut Rd) -> Option<Hit>, u64)> {
    const MB: u64 = 1 << 20;
    if b.len() < 16 {
        return None;
    }
    match b {
        [0xFF, 0xD8, 0xFF, x, ..] if *x >= 0xC0 => Some((jpeg, 64 * MB)),
        [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, ..] => Some((png, 256 * MB)),
        [b'G', b'I', b'F', b'8', b'7' | b'9', b'a', ..] => Some((gif, 128 * MB)),
        [b'B', b'M', ..] => Some((bmp, 512 * MB)),
        [b'%', b'P', b'D', b'F', b'-', b'1' | b'2', b'.', ..] => Some((pdf, 512 * MB)),
        [b'P', b'K', 3, 4, ..] => Some((zip, 2048 * MB)),
        [b'R', b'I', b'F', b'F', ..] => Some((riff, 4096 * MB)),
        [_, _, _, _, b'f', b't', b'y', b'p', ..] if be32(b, 0) >= 16 && be32(b, 0) <= 256 => Some((mp4, 16384 * MB)),
        [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, ..] => Some((ole2, 1024 * MB)),
        [b'I', b'D', b'3', 2..=4, _, ..] => Some((mp3, 512 * MB)),
        [b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C, ..] => Some((sevenz, 16384 * MB)),
        [0x1A, 0x45, 0xDF, 0xA3, ..] => Some((mkv, 16384 * MB)),
        _ => None,
    }
}

/// Carve [start, end) of the device. `skip` is a sorted, merged list of
/// byte ranges already accounted for by the filesystem scan.
pub fn carve(dev: &Device, start: u64, end: u64, skip: &[(u64, u64)], sink: &mut Sink) {
    const CHUNK: usize = 8 << 20;
    let end = end.min(dev.size());
    let mut pos = start - start % 512;
    let mut si = 0usize;
    let mut buf: Vec<u8> = Vec::new();
    let mut buf_off = 0u64;
    let mut last_report = 0u64;
    while pos < end {
        if sink.cancelled() {
            return;
        }
        while si < skip.len() && skip[si].1 <= pos {
            si += 1;
        }
        if si < skip.len() && skip[si].0 <= pos {
            pos = (skip[si].1 + 511) / 512 * 512;
            continue;
        }
        if pos < buf_off || pos + 16 > buf_off + buf.len() as u64 {
            let len = ((end - pos) as usize).min(CHUNK).max(512);
            buf = dev.read_vec(pos, len).unwrap_or_default();
            buf_off = pos;
            if buf.is_empty() {
                pos += CHUNK as u64;
                continue;
            }
        }
        let o = (pos - buf_off) as usize;
        let head = &buf[o..(o + 16).min(buf.len())];
        let mut next = pos + 512;
        if let Some((parser, max)) = detect(head) {
            let mut r = Rd::new(dev, pos, max);
            if let Some((len, ext, complete)) = parser(&mut r) {
                if len >= 32 {
                    let sector = pos / 512;
                    sink.push(FoundFile {
                        id: 0,
                        name: format!("f{:09}.{}", sector, ext),
                        path: format!("Гүн скан/{}", ext.to_uppercase()),
                        size: len,
                        ext: ext.to_string(),
                        category: category_of(ext),
                        source: Source::Carved,
                        deleted: true,
                        modified: None,
                        naive_time: false,
                        quality: if complete { Quality::Good } else { Quality::Poor },
                        offset: Some(pos),
                        fragments: 0,
                        extents: vec![Extent { offset: Some(pos), len }],
                        resident: None,
                    });
                    // Skip past the file so embedded thumbnails/parts are not
                    // reported as separate files.
                    if complete {
                        next = (pos + len + 511) / 512 * 512;
                    }
                }
            }
        }
        pos = next;
        if pos - last_report >= 64 << 20 || pos >= end {
            last_report = pos;
            sink.report("carve", "Гүн скан: дискийг салбар бүрээр шалгаж байна…".into(), pos.min(end) - start, end - start);
        }
    }
}
