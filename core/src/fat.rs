//! FAT12/16/32 scanner. Deleted entries are marked with 0xE5 in the first
//! byte of the 8.3 name; their FAT chain is zeroed, so data is assumed to be
//! contiguous from the (still recorded) start cluster.

use crate::device::Device;
use crate::model::*;
use crate::scan::Sink;
use crate::util::*;
use crate::volume::FsType;
use std::collections::HashSet;

struct Fat<'a> {
    dev: &'a Device,
    kind: FsType,
    cluster_size: u64,
    data_start: u64, // absolute byte offset of cluster 2
    fat: Vec<u32>,
    max_cluster: u32,
    eoc: u32,
}

impl<'a> Fat<'a> {
    fn cluster_off(&self, c: u32) -> u64 {
        self.data_start + (c as u64 - 2) * self.cluster_size
    }
    fn valid(&self, c: u32) -> bool {
        c >= 2 && c <= self.max_cluster
    }
    fn chain(&self, start: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut c = start;
        while self.valid(c) && out.len() <= self.max_cluster as usize {
            out.push(c);
            let next = self.fat[c as usize];
            if next >= self.eoc || next < 2 {
                break;
            }
            c = next;
        }
        out
    }
    fn read_clusters(&self, clusters: &[u32]) -> Vec<u8> {
        let mut out = Vec::with_capacity(clusters.len() * self.cluster_size as usize);
        for e in to_extents(self, clusters) {
            if let Ok(v) = self.dev.read_vec(e.offset.unwrap(), e.len as usize) {
                out.extend_from_slice(&v);
            }
        }
        out
    }
    /// Read a directory whose FAT chain is gone: take contiguous clusters
    /// while they still look like directory entries.
    fn read_deleted_dir(&self, start: u32) -> Option<Vec<u8>> {
        let first = self.dev.read_vec(self.cluster_off(start), self.cluster_size as usize).ok()?;
        // A directory's first cluster begins with the "." and ".." entries.
        if first.len() < 64 || &first[0..11] != b".          " || first[11] & 0x10 == 0 {
            return None;
        }
        let mut out = first;
        let mut c = start + 1;
        while out.len() < 64 * self.cluster_size as usize && self.valid(c) {
            if out.chunks_exact(32).any(|e| e[0] == 0) {
                break;
            }
            let Ok(next) = self.dev.read_vec(self.cluster_off(c), self.cluster_size as usize) else { break };
            if !next.chunks_exact(32).take(8).all(plausible_entry) {
                break;
            }
            out.extend_from_slice(&next);
            c += 1;
        }
        Some(out)
    }
}

fn plausible_entry(e: &[u8]) -> bool {
    let a = e[11];
    e[0] == 0 || e[0] == 0xE5 || (e[0] >= 0x20 && (a == 0x0F || a & 0xC0 == 0))
}

fn to_extents(fat: &Fat, clusters: &[u32]) -> Vec<Extent> {
    let mut out: Vec<Extent> = Vec::new();
    for &c in clusters {
        let off = fat.cluster_off(c);
        if let Some(last) = out.last_mut() {
            if last.offset.unwrap() + last.len == off {
                last.len += fat.cluster_size;
                continue;
            }
        }
        out.push(Extent { offset: Some(off), len: fat.cluster_size });
    }
    out
}

pub fn scan(dev: &Device, base: u64, kind: FsType, sink: &mut Sink) -> Result<(), String> {
    let b = dev.read_vec(base, 512).map_err(|e| e.to_string())?;
    let bps = le16(&b, 11) as u64;
    let spc = b[13] as u64;
    let reserved = le16(&b, 14) as u64;
    let nfats = b[16] as u64;
    let root_entries = le16(&b, 17) as u64;
    let total = if le16(&b, 19) != 0 { le16(&b, 19) as u64 } else { le32(&b, 32) as u64 };
    let fatsz = if le16(&b, 22) != 0 { le16(&b, 22) as u64 } else { le32(&b, 36) as u64 };
    let root_secs = (root_entries * 32 + bps - 1) / bps;
    let first_data_sec = reserved + nfats * fatsz + root_secs;
    let clusters = (total - first_data_sec) / spc;
    let max_cluster = (clusters + 1) as u32;

    sink.report("fs", format!("{} хүснэгт уншиж байна…", kind.name()), 0, 1);
    let fat_bytes = dev.read_vec(base + reserved * bps, (fatsz * bps) as usize).map_err(|e| e.to_string())?;
    let n = (max_cluster as usize + 1).min(match kind {
        FsType::Fat32 => fat_bytes.len() / 4,
        FsType::Fat16 => fat_bytes.len() / 2,
        _ => fat_bytes.len() * 2 / 3,
    });
    let mut table = vec![0u32; max_cluster as usize + 1];
    for (i, slot) in table.iter_mut().enumerate().take(n) {
        *slot = match kind {
            FsType::Fat32 => le32(&fat_bytes, i * 4) & 0x0FFF_FFFF,
            FsType::Fat16 => le16(&fat_bytes, i * 2) as u32,
            _ => {
                let o = i + i / 2;
                if o + 1 >= fat_bytes.len() {
                    0
                } else {
                    let v = le16(&fat_bytes, o) as u32;
                    if i % 2 == 0 { v & 0xFFF } else { v >> 4 }
                }
            }
        };
    }
    let fat = Fat {
        dev,
        kind,
        cluster_size: bps * spc,
        data_start: base + first_data_sec * bps,
        fat: table,
        max_cluster,
        eoc: match kind {
            FsType::Fat32 => 0x0FFF_FFF7,
            FsType::Fat16 => 0xFFF7,
            _ => 0xFF7,
        },
    };

    let root = if kind == FsType::Fat32 {
        let rc = le32(&b, 44);
        fat.read_clusters(&fat.chain(rc))
    } else {
        let off = base + (reserved + nfats * fatsz) * bps;
        dev.read_vec(off, (root_secs * bps) as usize).map_err(|e| e.to_string())?
    };
    let mut visited = HashSet::new();
    walk(&fat, &root, "", false, 0, &mut visited, sink);
    Ok(())
}

fn sfn(e: &[u8], deleted: bool) -> String {
    let lower_base = e[12] & 0x08 != 0;
    let lower_ext = e[12] & 0x10 != 0;
    let conv = |bytes: &[u8], lower: bool| -> String {
        bytes
            .iter()
            .map(|&c| {
                let ch = if c < 0x80 { c as char } else { '_' };
                if lower { ch.to_ascii_lowercase() } else { ch }
            })
            .collect::<String>()
            .trim_end()
            .to_string()
    };
    let mut base = conv(&e[0..8], lower_base);
    if deleted || e[0] == 0x05 {
        base.replace_range(0..1, if deleted { "_" } else { "\u{e5}" });
    }
    let ext = conv(&e[8..11], lower_ext);
    if ext.is_empty() { base } else { format!("{}.{}", base, ext) }
}

fn lfn_checksum(e: &[u8]) -> u8 {
    let mut sum: u8 = 0;
    for &c in &e[0..11] {
        sum = (if sum & 1 != 0 { 0x80u8 } else { 0 }).wrapping_add(sum >> 1).wrapping_add(c);
    }
    sum
}

fn walk(fat: &Fat, dir: &[u8], path: &str, parent_deleted: bool, depth: u32, visited: &mut HashSet<u32>, sink: &mut Sink) {
    if depth > 48 || sink.cancelled() {
        return;
    }
    let mut lfn: Vec<&[u8]> = Vec::new();
    for e in dir.chunks_exact(32) {
        if e[0] == 0x00 {
            break;
        }
        let attr = e[11];
        if attr == 0x0F {
            lfn.push(e);
            continue;
        }
        let parts = std::mem::take(&mut lfn);
        if attr & 0x08 != 0 || e[0] == b'.' {
            continue;
        }
        let deleted = e[0] == 0xE5;
        let is_dir = attr & 0x10 != 0;

        // Long file name: entries appear in reverse order before the 8.3 entry.
        let mut name = None;
        if !parts.is_empty() {
            let chk = parts[0][13];
            let consistent = parts.iter().all(|p| p[13] == chk);
            let matches_sfn = deleted || lfn_checksum(e) == chk;
            if consistent && matches_sfn {
                let mut units: Vec<u16> = Vec::new();
                for p in parts.iter().rev() {
                    for &(s, n) in &[(1usize, 5usize), (14, 6), (28, 2)] {
                        for k in 0..n {
                            units.push(le16(p, s + k * 2));
                        }
                    }
                }
                if let Some(end) = units.iter().position(|&u| u == 0) {
                    units.truncate(end);
                }
                let s = String::from_utf16_lossy(&units);
                if !s.is_empty() {
                    name = Some(s);
                }
            }
        }
        let name = name.unwrap_or_else(|| sfn(e, deleted));
        let hi = if fat.kind == FsType::Fat32 { le16(e, 20) as u32 } else { 0 };
        let cluster = (hi << 16) | le16(e, 26) as u32;
        let size = le32(e, 28) as u64;
        let modified = dos_time(le16(e, 24), le16(e, 22));
        let is_deleted = deleted || parent_deleted;

        if is_dir {
            if !fat.valid(cluster) || !visited.insert(cluster) {
                continue;
            }
            let sub = if is_deleted { fat.read_deleted_dir(cluster) } else { Some(fat.read_clusters(&fat.chain(cluster))) };
            if let Some(sub) = sub {
                let p = if path.is_empty() { name.clone() } else { format!("{}/{}", path, name) };
                walk(fat, &sub, &p, is_deleted, depth + 1, visited, sink);
            }
            continue;
        }

        let (extents, quality) = if size == 0 {
            (Vec::new(), Quality::Good)
        } else if !fat.valid(cluster) {
            continue;
        } else if is_deleted {
            let count = ((size + fat.cluster_size - 1) / fat.cluster_size) as u32;
            if cluster as u64 + count as u64 - 1 > fat.max_cluster as u64 {
                continue;
            }
            let free = (cluster..cluster + count).all(|c| fat.fat[c as usize] == 0);
            let start = fat.cluster_off(cluster);
            if free {
                (vec![Extent { offset: Some(start), len: size }], Quality::Good)
            } else {
                // Chain is erased and the contiguous range crosses live data:
                // assume the file filled free gaps and skip allocated clusters.
                let mut cl = Vec::with_capacity(count as usize);
                let mut c = cluster;
                while cl.len() < count as usize && fat.valid(c) {
                    if fat.fat[c as usize] == 0 {
                        cl.push(c);
                    }
                    c += 1;
                }
                if fat.fat[cluster as usize] == 0 && cl.len() == count as usize {
                    let mut ex = to_extents(fat, &cl);
                    truncate_extents(&mut ex, size);
                    (ex, Quality::Unknown)
                } else {
                    (vec![Extent { offset: Some(start), len: size }], Quality::Poor)
                }
            }
        } else {
            let chain = fat.chain(cluster);
            let mut ex = to_extents(fat, &chain);
            truncate_extents(&mut ex, size);
            let have: u64 = ex.iter().map(|x| x.len).sum();
            (ex, if have >= size { Quality::Good } else { Quality::Poor })
        };
        let ext = ext_of(&name);
        sink.push(FoundFile {
            id: 0,
            category: category_of(&ext),
            ext,
            name,
            path: path.to_string(),
            size,
            source: Source::Fat,
            deleted: is_deleted,
            modified,
            naive_time: true,
            quality,
            offset: extents.first().and_then(|x| x.offset),
            fragments: 0,
            extents,
            resident: None,
        });
    }
}

pub fn truncate_extents(ex: &mut Vec<Extent>, size: u64) {
    let mut left = size;
    let mut keep = 0;
    for e in ex.iter_mut() {
        if left == 0 {
            break;
        }
        if e.len > left {
            e.len = left;
        }
        left -= e.len;
        keep += 1;
    }
    ex.truncate(keep);
}
