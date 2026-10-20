//! NTFS scanner. Walks every $MFT record; records whose "in use" flag is
//! cleared are deleted files. Their $FILE_NAME and $DATA run lists usually
//! survive deletion, so names, folders and fragmented data are recoverable.

use crate::device::Device;
use crate::model::*;
use crate::scan::Sink;
use crate::util::*;
use std::collections::HashMap;

const ROOT: u64 = 5;

#[derive(Default, Clone)]
struct DataAttr {
    resident: Option<Vec<u8>>,
    /// (start_vcn, runs) fragments; runs are (lcn or None if sparse, clusters)
    frags: Vec<(u64, Vec<(Option<u64>, u64)>)>,
    real_size: Option<u64>,
    compressed: bool,
    encrypted: bool,
}

struct Rec {
    name: String,
    parent: u64,
    parent_seq: u16,
    seq: u16,
    in_use: bool,
    is_dir: bool,
    modified: Option<i64>,
    data: Option<DataAttr>,
}

struct Vol<'a> {
    dev: &'a Device,
    base: u64,
    cs: u64,
}

fn fixup(rec: &mut [u8]) -> bool {
    if rec.len() < 48 || &rec[0..4] != b"FILE" {
        return false;
    }
    let uo = le16(rec, 4) as usize;
    let uc = le16(rec, 6) as usize;
    if uc < 2 || uo + uc * 2 > rec.len() || (uc - 1) * 512 > rec.len() {
        return false;
    }
    let usn = [rec[uo], rec[uo + 1]];
    for i in 1..uc {
        let end = i * 512 - 2;
        if rec[end] != usn[0] || rec[end + 1] != usn[1] {
            return false;
        }
        rec[end] = rec[uo + i * 2];
        rec[end + 1] = rec[uo + i * 2 + 1];
    }
    true
}

fn decode_runs(b: &[u8]) -> Vec<(Option<u64>, u64)> {
    let mut out = Vec::new();
    let mut p = 0;
    let mut lcn: i64 = 0;
    while p < b.len() {
        let h = b[p];
        if h == 0 {
            break;
        }
        let ls = (h & 0x0F) as usize;
        let os = (h >> 4) as usize;
        p += 1;
        if ls == 0 || ls > 8 || os > 8 || p + ls + os > b.len() {
            break;
        }
        let mut len: u64 = 0;
        for i in 0..ls {
            len |= (b[p + i] as u64) << (8 * i);
        }
        p += ls;
        if os == 0 {
            out.push((None, len));
        } else {
            let mut off: i64 = 0;
            for i in 0..os {
                off |= (b[p + i] as i64) << (8 * i);
            }
            if b[p + os - 1] & 0x80 != 0 && os < 8 {
                off -= 1i64 << (8 * os);
            }
            p += os;
            lcn += off;
            if lcn < 0 {
                break;
            }
            out.push((Some(lcn as u64), len));
        }
    }
    out
}

/// Parse the attributes of one (fixed-up) MFT record.
fn parse(rec: &[u8]) -> (Option<(String, u8, u64, u16)>, Option<i64>, Option<DataAttr>) {
    let mut fname: Option<(String, u8, u64, u16)> = None;
    let mut modified = None;
    let mut data: Option<DataAttr> = None;
    let mut p = le16(rec, 20) as usize;
    let used = (le32(rec, 24) as usize).min(rec.len());
    while p + 16 <= used {
        let t = le32(rec, p);
        if t == 0xFFFF_FFFF {
            break;
        }
        let len = le32(rec, p + 4) as usize;
        if len < 16 || p + len > used {
            break;
        }
        let a = &rec[p..p + len];
        let nonres = a[8] != 0;
        let name_len = a[9];
        let flags = le16(a, 12);
        match t {
            0x10 if !nonres && len >= 24 => {
                let vo = le16(a, 20) as usize;
                if vo + 16 <= a.len() {
                    modified = filetime(le64(a, vo + 8));
                }
            }
            0x30 if !nonres && len >= 24 => {
                let vo = le16(a, 20) as usize;
                let vl = le32(a, 16) as usize;
                if vo + vl <= a.len() && vl >= 66 {
                    let v = &a[vo..vo + vl];
                    let nl = v[64] as usize;
                    let ns = v[65];
                    if 66 + nl * 2 <= v.len() {
                        let name = utf16le(&v[66..66 + nl * 2]);
                        let parent = le64(v, 0);
                        let better = match &fname {
                            None => true,
                            Some((_, old_ns, _, _)) => *old_ns == 2 && ns != 2,
                        };
                        if better {
                            fname = Some((name, ns, parent & 0xFFFF_FFFF_FFFF, (parent >> 48) as u16));
                        }
                    }
                }
            }
            0x80 if name_len == 0 => {
                let d = data.get_or_insert_with(DataAttr::default);
                d.compressed |= flags & 0x00FF != 0;
                d.encrypted |= flags & 0x4000 != 0;
                if !nonres {
                    let vo = le16(a, 20) as usize;
                    let vl = le32(a, 16) as usize;
                    if vo + vl <= a.len() {
                        d.resident = Some(a[vo..vo + vl].to_vec());
                        d.real_size = Some(vl as u64);
                    }
                } else if len >= 64 {
                    let start_vcn = le64(a, 16);
                    let ro = le16(a, 32) as usize;
                    if start_vcn == 0 {
                        d.real_size = Some(le64(a, 48));
                    }
                    if ro < a.len() {
                        d.frags.push((start_vcn, decode_runs(&a[ro..])));
                    }
                }
            }
            _ => {}
        }
        p += len;
    }
    (fname, modified, data)
}

impl<'a> Vol<'a> {
    fn runs_to_extents(&self, d: &DataAttr) -> Vec<Extent> {
        let mut frags = d.frags.clone();
        frags.sort_by_key(|f| f.0);
        let mut out: Vec<Extent> = Vec::new();
        for (_, runs) in frags {
            for (lcn, n) in runs {
                let len = n * self.cs;
                let off = lcn.map(|l| self.base + l * self.cs);
                if let (Some(last), Some(o)) = (out.last_mut(), off) {
                    if let Some(lo) = last.offset {
                        if lo + last.len == o {
                            last.len += len;
                            continue;
                        }
                    }
                }
                out.push(Extent { offset: off, len });
            }
        }
        out
    }

    fn read_extents(&self, ex: &[Extent], limit: u64) -> Vec<u8> {
        let mut v = Vec::new();
        for e in ex {
            if v.len() as u64 >= limit {
                break;
            }
            let take = e.len.min(limit - v.len() as u64) as usize;
            match e.offset {
                Some(o) => match self.dev.read_vec(o, take) {
                    Ok(d) => v.extend_from_slice(&d),
                    Err(_) => v.resize(v.len() + take, 0),
                },
                None => v.resize(v.len() + take, 0),
            }
        }
        v
    }
}

pub fn scan(dev: &Device, base: u64, sink: &mut Sink) -> Result<(), String> {
    let b = dev.read_vec(base, 512).map_err(|e| e.to_string())?;
    let bps = le16(&b, 11) as u64;
    let spc_raw = b[13];
    let spc = if spc_raw > 0x80 { 1u64 << (256 - spc_raw as u32) } else { spc_raw as u64 };
    let cs = bps * spc;
    let mft_lcn = le64(&b, 48);
    let cpr = b[64] as i8;
    let rec_size = if cpr > 0 { cpr as u64 * cs } else { 1u64 << (-(cpr as i32)) } as usize;
    if cs == 0 || !(256..=65536).contains(&rec_size) {
        return Err("NTFS boot sector буруу".into());
    }
    let vol = Vol { dev, base, cs };

    // $MFT record 0 describes where the MFT itself lives.
    let mut r0 = dev.read_vec(base + mft_lcn * cs, rec_size).map_err(|e| e.to_string())?;
    if !fixup(&mut r0) {
        return Err("$MFT бичлэг уншигдсангүй (гэмтсэн байж магадгүй)".into());
    }
    let (_, _, mft_data) = parse(&r0);
    let mft_data = mft_data.ok_or("$MFT-ийн $DATA олдсонгүй")?;
    let mft_ex = vol.runs_to_extents(&mft_data);
    let mft_size = mft_data.real_size.unwrap_or_else(|| mft_ex.iter().map(|e| e.len).sum());
    let total = mft_size / rec_size as u64;

    let mut recs: Vec<Option<Rec>> = Vec::with_capacity(total as usize);
    let mut ext_data: HashMap<u64, Vec<DataAttr>> = HashMap::new();
    let chunk_recs = (4 << 20) / rec_size;
    let mut idx: u64 = 0;
    'outer: for e in &mft_ex {
        let Some(eo) = e.offset else {
            for _ in 0..e.len / rec_size as u64 {
                recs.push(None);
                idx += 1;
            }
            continue;
        };
        let mut pos = 0u64;
        while pos < e.len && idx < total {
            if sink.cancelled() {
                break 'outer;
            }
            let n = (((e.len - pos) / rec_size as u64) as usize).min(chunk_recs);
            if n == 0 {
                break;
            }
            let buf = dev.read_vec(eo + pos, n * rec_size).unwrap_or_default();
            for k in 0..n {
                if idx >= total {
                    break;
                }
                let s = k * rec_size;
                if s + rec_size > buf.len() {
                    recs.push(None);
                    idx += 1;
                    continue;
                }
                let mut r = buf[s..s + rec_size].to_vec();
                if !fixup(&mut r) {
                    recs.push(None);
                    idx += 1;
                    continue;
                }
                let flags = le16(&r, 22);
                let seq = le16(&r, 16);
                let base_ref = le64(&r, 32) & 0xFFFF_FFFF_FFFF;
                let (fname, modified, data) = parse(&r);
                if base_ref != 0 {
                    if let Some(d) = data {
                        ext_data.entry(base_ref).or_default().push(d);
                    }
                    recs.push(None);
                } else if let Some((name, _ns, parent, pseq)) = fname {
                    recs.push(Some(Rec {
                        name,
                        parent,
                        parent_seq: pseq,
                        seq,
                        in_use: flags & 1 != 0,
                        is_dir: flags & 2 != 0,
                        modified,
                        data,
                    }));
                } else {
                    recs.push(None);
                }
                idx += 1;
            }
            pos += (n * rec_size) as u64;
            sink.report("fs", "NTFS MFT уншиж байна…".into(), idx, total);
        }
    }

    // Cluster allocation bitmap ($Bitmap, record 6) to judge overwrite risk.
    let bitmap: Vec<u8> = recs
        .get(6)
        .and_then(|r| r.as_ref())
        .and_then(|r| r.data.as_ref())
        .map(|d| {
            let ex = vol.runs_to_extents(d);
            vol.read_extents(&ex, d.real_size.unwrap_or(0).min(512 << 20))
        })
        .unwrap_or_default();
    let allocated = |lcn: u64| -> bool {
        bitmap.get((lcn / 8) as usize).map(|b| b & (1 << (lcn % 8)) != 0).unwrap_or(true)
    };

    // Folder path resolution with memoisation.
    let mut dir_paths: HashMap<u64, String> = HashMap::new();
    fn resolve(i: u64, recs: &[Option<Rec>], memo: &mut HashMap<u64, String>, depth: u32) -> String {
        if i == ROOT {
            return String::new();
        }
        if let Some(p) = memo.get(&i) {
            return p.clone();
        }
        let p = match recs.get(i as usize).and_then(|r| r.as_ref()) {
            Some(r) if depth < 64 && r.is_dir => {
                let parent_ok = recs
                    .get(r.parent as usize)
                    .and_then(|x| x.as_ref())
                    .map(|p| p.is_dir && (p.seq == r.parent_seq || !p.in_use || r.parent == ROOT))
                    .unwrap_or(r.parent == ROOT);
                let pp = if parent_ok { resolve(r.parent, recs, memo, depth + 1) } else { "$Орхигдсон".to_string() };
                if pp.is_empty() { r.name.clone() } else { format!("{}/{}", pp, r.name) }
            }
            _ => "$Орхигдсон".to_string(),
        };
        memo.insert(i, p.clone());
        p
    }

    let n = recs.len();
    for i in 24..n {
        if i % 65536 == 0 && sink.cancelled() {
            break;
        }
        let Some(r) = recs[i].as_ref() else { continue };
        if r.is_dir {
            continue;
        }
        let mut data = match r.data.clone() {
            Some(d) => d,
            None => DataAttr::default(),
        };
        if let Some(extra) = ext_data.get(&(i as u64)) {
            for d in extra {
                data.frags.extend(d.frags.iter().cloned());
                if data.real_size.is_none() {
                    data.real_size = d.real_size;
                }
                data.compressed |= d.compressed;
                data.encrypted |= d.encrypted;
            }
        }
        if data.encrypted || (data.resident.is_none() && data.frags.is_empty() && data.real_size.is_none()) {
            continue;
        }
        let path = {
            let parent_ok = recs
                .get(r.parent as usize)
                .and_then(|x| x.as_ref())
                .map(|p| p.is_dir && (p.seq == r.parent_seq || !r.in_use))
                .unwrap_or(r.parent == ROOT);
            if parent_ok { resolve(r.parent, &recs, &mut dir_paths, 0) } else { "$Орхигдсон".to_string() }
        };
        if path.starts_with("$Extend") || (path.is_empty() && r.name.starts_with('$')) {
            continue;
        }
        let size = data.real_size.unwrap_or(0);
        let (extents, resident) = if let Some(res) = data.resident.take() {
            (Vec::new(), Some(res))
        } else {
            let mut ex = vol.runs_to_extents(&data);
            crate::fat::truncate_extents(&mut ex, size);
            (ex, None)
        };
        let quality = if data.compressed {
            Quality::Poor
        } else if r.in_use {
            Quality::Good
        } else if resident.is_some() {
            Quality::Good
        } else {
            let reused = extents.iter().any(|e| match e.offset {
                Some(o) => {
                    let first = (o - base) / cs;
                    let last = (o - base + e.len.max(1) - 1) / cs;
                    (first..=last).any(&allocated)
                }
                None => false,
            });
            let covered: u64 = extents.iter().map(|e| e.len).sum();
            if reused || covered < size { Quality::Poor } else { Quality::Good }
        };
        let ext = ext_of(&r.name);
        sink.push(FoundFile {
            id: 0,
            name: r.name.clone(),
            category: category_of(&ext),
            ext,
            path,
            size,
            source: Source::Ntfs,
            deleted: !r.in_use,
            modified: r.modified,
            naive_time: false,
            quality,
            offset: extents.first().and_then(|e| e.offset),
            fragments: 0,
            extents,
            resident,
        });
    }
    Ok(())
}
