//! Filesystem detection and partition table (MBR / GPT) parsing.

use crate::device::Device;
use crate::util::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsType {
    Ntfs,
    Fat12,
    Fat16,
    Fat32,
    Exfat,
}

impl FsType {
    pub fn name(&self) -> &'static str {
        match self {
            FsType::Ntfs => "NTFS",
            FsType::Fat12 => "FAT12",
            FsType::Fat16 => "FAT16",
            FsType::Fat32 => "FAT32",
            FsType::Exfat => "exFAT",
        }
    }
}

pub fn detect_fs(dev: &Device, base: u64) -> Option<FsType> {
    let b = dev.read_vec(base, 512).ok()?;
    if b.len() < 512 {
        return None;
    }
    if &b[3..11] == b"NTFS    " {
        return Some(FsType::Ntfs);
    }
    if &b[3..11] == b"EXFAT   " {
        return Some(FsType::Exfat);
    }
    if b[510] != 0x55 || b[511] != 0xAA {
        return None;
    }
    let bps = le16(&b, 11) as u64;
    let spc = b[13] as u64;
    let reserved = le16(&b, 14) as u64;
    let nfats = b[16] as u64;
    if !matches!(bps, 512 | 1024 | 2048 | 4096) || spc == 0 || !spc.is_power_of_two() || reserved == 0 || nfats == 0 || nfats > 4 {
        return None;
    }
    let looks_fat = &b[54..57] == b"FAT" || &b[82..87] == b"FAT32" || b[0] == 0xEB || b[0] == 0xE9;
    if !looks_fat {
        return None;
    }
    let root_entries = le16(&b, 17) as u64;
    let total = if le16(&b, 19) != 0 { le16(&b, 19) as u64 } else { le32(&b, 32) as u64 };
    let fatsz = if le16(&b, 22) != 0 { le16(&b, 22) as u64 } else { le32(&b, 36) as u64 };
    if total == 0 || fatsz == 0 {
        return None;
    }
    let root_secs = (root_entries * 32 + bps - 1) / bps;
    let data_secs = total.checked_sub(reserved + nfats * fatsz + root_secs)?;
    let clusters = data_secs / spc;
    Some(if clusters < 4085 {
        FsType::Fat12
    } else if clusters < 65525 {
        FsType::Fat16
    } else {
        FsType::Fat32
    })
}

/// Best-effort volume label from the boot sector.
pub fn volume_label(dev: &Device, base: u64) -> Option<String> {
    let fs = detect_fs(dev, base)?;
    let b = dev.read_vec(base, 512).ok()?;
    let raw = match fs {
        FsType::Fat32 => &b[71..82],
        FsType::Fat12 | FsType::Fat16 => &b[43..54],
        _ => return None,
    };
    let s = String::from_utf8_lossy(raw).trim().to_string();
    if s.is_empty() || s == "NO NAME" {
        None
    } else {
        Some(s)
    }
}

#[derive(Debug, Clone)]
pub struct Partition {
    pub offset: u64,
    pub size: u64,
}

/// Partitions of a whole disk. Returns an empty list when the device itself
/// is a volume (has a filesystem boot sector at offset 0) or has no table.
pub fn partitions(dev: &Device) -> Vec<Partition> {
    let mut out = Vec::new();
    let Ok(mbr) = dev.read_vec(0, 512) else { return out };
    if mbr.len() < 512 || mbr[510] != 0x55 || mbr[511] != 0xAA {
        return out;
    }
    if detect_fs(dev, 0).is_some() {
        return out;
    }
    let mut gpt = false;
    for i in 0..4 {
        let e = 446 + i * 16;
        let ptype = mbr[e + 4];
        let lba = le32(&mbr, e + 8) as u64;
        let count = le32(&mbr, e + 12) as u64;
        if ptype == 0xEE {
            gpt = true;
            break;
        }
        if ptype == 0 || count == 0 {
            continue;
        }
        if ptype == 0x05 || ptype == 0x0F || ptype == 0x85 {
            extended(dev, lba * 512, &mut out);
            continue;
        }
        out.push(Partition { offset: lba * 512, size: count * 512 });
    }
    if gpt {
        out.clear();
        for sector in [512u64, 4096] {
            let Ok(h) = dev.read_vec(sector, 512) else { continue };
            if h.len() < 92 || &h[0..8] != b"EFI PART" {
                continue;
            }
            let entries_lba = le64(&h, 72);
            let n = le32(&h, 80).min(256) as u64;
            let esz = le32(&h, 84) as u64;
            if esz < 128 || esz > 1024 {
                continue;
            }
            let Ok(tbl) = dev.read_vec(entries_lba * sector, (n * esz) as usize) else { continue };
            for i in 0..n as usize {
                let e = i * esz as usize;
                if e + 128 > tbl.len() {
                    break;
                }
                if tbl[e..e + 16].iter().all(|&x| x == 0) {
                    continue;
                }
                let first = le64(&tbl, e + 32);
                let last = le64(&tbl, e + 40);
                if last >= first {
                    out.push(Partition { offset: first * sector, size: (last - first + 1) * sector });
                }
            }
            break;
        }
    }
    out
}

fn extended(dev: &Device, ext_base: u64, out: &mut Vec<Partition>) {
    let mut ebr_off = ext_base;
    for _ in 0..64 {
        let Ok(ebr) = dev.read_vec(ebr_off, 512) else { return };
        if ebr.len() < 512 || ebr[510] != 0x55 || ebr[511] != 0xAA {
            return;
        }
        let lba = le32(&ebr, 446 + 8) as u64;
        let cnt = le32(&ebr, 446 + 12) as u64;
        if ebr[446 + 4] != 0 && cnt > 0 {
            out.push(Partition { offset: ebr_off + lba * 512, size: cnt * 512 });
        }
        let next = le32(&ebr, 462 + 8) as u64;
        if ebr[462 + 4] == 0 || next == 0 {
            return;
        }
        ebr_off = ext_base + next * 512;
    }
}
