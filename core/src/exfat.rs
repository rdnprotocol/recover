//! exFAT scanner (USB flash drives, SD cards > 32 GB). Deleting a file only
//! clears the InUse bit (0x80) of its directory entry set and its allocation
//! bitmap bits; the stream extension still holds start cluster and length.

use crate::device::Device;
use crate::fat::truncate_extents;
use crate::model::*;
use crate::scan::Sink;
use crate::util::*;
use std::collections::HashSet;

struct ExFat<'a> {
    dev: &'a Device,
    cluster_size: u64,
    heap: u64,
    cluster_count: u32,
    fat: Vec<u32>,
    bitmap: Vec<u8>,
}

impl<'a> ExFat<'a> {
    fn off(&self, c: u32) -> u64 {
        self.heap + (c as u64 - 2) * self.cluster_size
    }
    fn valid(&self, c: u32) -> bool {
        c >= 2 && c < self.cluster_count + 2
    }
    fn allocated(&self, c: u32) -> bool {
        let i = (c - 2) as usize;
        self.bitmap.get(i / 8).map(|b| b & (1 << (i % 8)) != 0).unwrap_or(false)
    }
    fn chain(&self, start: u32, max: usize) -> Vec<u32> {
        let mut out = Vec::new();
        let mut c = start;
        while self.valid(c) && out.len() < max {
            out.push(c);
            let n = *self.fat.get(c as usize).unwrap_or(&0);
            if n < 2 || n >= 0xFFFF_FFF7 {
                break;
            }
            c = n;
        }
        out
    }
    fn clusters_for(&self, start: u32, len: u64, no_fat_chain: bool, deleted: bool) -> Option<Vec<u32>> {
        let need = ((len + self.cluster_size - 1) / self.cluster_size) as usize;
        if need == 0 {
            return Some(Vec::new());
        }
        if !self.valid(start) {
            return None;
        }
        let contiguous = || -> Option<Vec<u32>> {
            let last = start as u64 + need as u64 - 1;
            if last >= self.cluster_count as u64 + 2 {
                return None;
            }
            Some((start..start + need as u32).collect())
        };
        if no_fat_chain {
            return contiguous();
        }
        let ch = self.chain(start, need);
        if ch.len() >= need || !deleted {
            Some(ch)
        } else {
            contiguous()
        }
    }
    /// Deleted file whose chain is gone and whose contiguous range now
    /// crosses allocated clusters: assume it was written into free gaps,
    /// so take only clusters that are still free.
    fn free_walk(&self, start: u32, need: usize) -> Option<Vec<u32>> {
        let mut out = Vec::with_capacity(need);
        let mut c = start;
        while out.len() < need && self.valid(c) {
            if !self.allocated(c) {
                out.push(c);
            }
            c += 1;
        }
        if out.len() == need { Some(out) } else { None }
    }
    fn extents(&self, clusters: &[u32]) -> Vec<Extent> {
        let mut out: Vec<Extent> = Vec::new();
        for &c in clusters {
            let o = self.off(c);
            if let Some(l) = out.last_mut() {
                if l.offset.unwrap() + l.len == o {
                    l.len += self.cluster_size;
                    continue;
                }
            }
            out.push(Extent { offset: Some(o), len: self.cluster_size });
        }
        out
    }
    fn read(&self, ex: &[Extent]) -> Vec<u8> {
        let mut v = Vec::new();
        for e in ex {
            if let Ok(d) = self.dev.read_vec(e.offset.unwrap(), e.len as usize) {
                v.extend_from_slice(&d);
            }
        }
        v
    }
}

pub fn scan(dev: &Device, base: u64, sink: &mut Sink) -> Result<(), String> {
    let b = dev.read_vec(base, 512).map_err(|e| e.to_string())?;
    let bps = 1u64 << b[108];
    let spc = 1u64 << b[109];
    let fat_off = le32(&b, 80) as u64 * bps;
    let fat_len = le32(&b, 84) as u64 * bps;
    let heap = base + le32(&b, 88) as u64 * bps;
    let cluster_count = le32(&b, 92);
    let root = le32(&b, 96);
    sink.report("fs", "exFAT хүснэгт уншиж байна…".into(), 0, 1);
    let fat_bytes = dev.read_vec(base + fat_off, fat_len.min((cluster_count as u64 + 2) * 4) as usize).map_err(|e| e.to_string())?;
    let fat: Vec<u32> = fat_bytes.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    let mut ex = ExFat { dev, cluster_size: bps * spc, heap, cluster_count, fat, bitmap: Vec::new() };

    let root_cl = ex.chain(root, 1 << 16);
    let root_data = ex.read(&ex.extents(&root_cl));
    // allocation bitmap entry (0x81)
    for e in root_data.chunks_exact(32) {
        if e[0] == 0x81 {
            let c = le32(e, 20);
            let len = le64(e, 24);
            if let Some(cl) = ex.clusters_for(c, len, false, false) {
                let mut bm = ex.read(&ex.extents(&cl));
                bm.truncate(len as usize);
                ex.bitmap = bm;
            }
            break;
        }
    }
    let mut visited = HashSet::new();
    visited.insert(root);
    walk(&ex, &root_data, "", false, 0, &mut visited, sink);
    Ok(())
}

fn walk(ex: &ExFat, dir: &[u8], path: &str, parent_deleted: bool, depth: u32, visited: &mut HashSet<u32>, sink: &mut Sink) {
    if depth > 48 || sink.cancelled() {
        return;
    }
    let entries: Vec<&[u8]> = dir.chunks_exact(32).collect();
    let mut i = 0;
    while i < entries.len() {
        let e = entries[i];
        let t = e[0];
        if t == 0 {
            break;
        }
        if t & 0x7F != 0x05 {
            i += 1;
            continue;
        }
        let deleted = t & 0x80 == 0;
        let sec = e[1] as usize;
        if sec < 2 || i + sec >= entries.len() || entries[i + 1][0] & 0x7F != 0x40 {
            i += 1;
            continue;
        }
        let attrs = le16(e, 4);
        let ts = le32(e, 12);
        let st = entries[i + 1];
        let flags = st[1];
        let name_len = st[3] as usize;
        let first = le32(st, 20);
        let len = le64(st, 24);
        let mut units: Vec<u16> = Vec::new();
        for k in 2..=sec {
            let n = entries[i + k];
            if n[0] & 0x7F != 0x41 {
                break;
            }
            for j in 0..15 {
                units.push(le16(n, 2 + j * 2));
            }
        }
        units.truncate(name_len);
        let name = String::from_utf16_lossy(&units);
        i += 1 + sec;
        if name.is_empty() {
            continue;
        }
        let is_deleted = deleted || parent_deleted;
        let no_chain = flags & 0x02 != 0;
        let Some(mut cl) = ex.clusters_for(first, len, no_chain, is_deleted) else { continue };

        if attrs & 0x10 != 0 {
            if len == 0 || len > 256 << 20 || !visited.insert(first) {
                continue;
            }
            let mut data = ex.read(&ex.extents(&cl));
            data.truncate(len as usize);
            let p = if path.is_empty() { name } else { format!("{}/{}", path, name) };
            walk(ex, &data, &p, is_deleted, depth + 1, visited, sink);
            continue;
        }
        let mut quality = if !is_deleted || cl.iter().all(|&c| !ex.allocated(c)) { Quality::Good } else { Quality::Poor };
        if quality == Quality::Poor && !ex.allocated(first) {
            if let Some(fw) = ex.free_walk(first, cl.len()) {
                cl = fw;
                quality = Quality::Unknown;
            }
        }
        let mut extents = ex.extents(&cl);
        truncate_extents(&mut extents, len);
        let ext = ext_of(&name);
        sink.push(FoundFile {
            id: 0,
            category: category_of(&ext),
            ext,
            name,
            path: path.to_string(),
            size: len,
            source: Source::Exfat,
            deleted: is_deleted,
            modified: dos_time((ts >> 16) as u16, ts as u16),
            naive_time: true,
            quality,
            offset: extents.first().and_then(|x| x.offset),
            fragments: 0,
            extents,
            resident: None,
        });
    }
}
