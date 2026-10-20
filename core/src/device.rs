//! Raw device / disk-image access with sector-aligned reads.
//!
//! On Windows, raw volumes (`\\.\C:`) and physical drives (`\\.\PhysicalDrive0`)
//! only accept reads whose offset and length are multiples of the sector size,
//! so every read here is widened to 4 KiB boundaries and copied out.

use serde::Serialize;
use std::fs::File;
use std::io;
use std::path::Path;

const ALIGN: u64 = 4096;

pub struct Device {
    file: File,
    size: u64,
    path: String,
}

impl Device {
    pub fn open(path: &str) -> io::Result<Device> {
        let file = File::open(path)?;
        let mut dev = Device { file, size: 0, path: path.to_string() };
        dev.size = dev.detect_size();
        if dev.size == 0 {
            return Err(io::Error::new(io::ErrorKind::Other, "Төхөөрөмжийн хэмжээг тодорхойлж чадсангүй"));
        }
        Ok(dev)
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    fn detect_size(&self) -> u64 {
        if let Ok(m) = self.file.metadata() {
            if m.is_file() && m.len() > 0 {
                return m.len();
            }
        }
        #[cfg(unix)]
        {
            use std::io::{Seek, SeekFrom};
            if let Ok(mut f) = self.file.try_clone() {
                if let Ok(end) = f.seek(SeekFrom::End(0)) {
                    if end > 0 {
                        return end;
                    }
                }
            }
        }
        self.probe_size()
    }

    /// Find the readable size by probing 512-byte reads (works on raw Windows
    /// handles where seeking to the end is not supported).
    fn probe_size(&self) -> u64 {
        // 4096-byte probes work on both 512e and 4K-native disks.
        const U: u64 = 4096;
        let mut buf = AlignedBuf::new(U as usize);
        let mut readable = |off: u64| -> bool { matches!(self.raw_read(off, buf.as_mut()), Ok(n) if n as u64 == U) };
        if !readable(0) {
            let mut size = 0;
            let mut small = AlignedBuf::new(512);
            while size < U && matches!(self.raw_read(size, small.as_mut()), Ok(512)) {
                size += 512;
            }
            return size;
        }
        let mut lo: u64 = 0; // readable unit index
        let mut hi: u64 = 1;
        while readable(hi * U) {
            lo = hi;
            hi *= 2;
            if hi > (1u64 << 36) {
                return hi * U;
            }
        }
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if readable(mid * U) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let mut size = (lo + 1) * U;
        // Volumes need not end on a 4 KiB boundary: add trailing 512-byte sectors.
        let mut small = AlignedBuf::new(512);
        for _ in 0..7 {
            if matches!(self.raw_read(size, small.as_mut()), Ok(512)) {
                size += 512;
            } else {
                break;
            }
        }
        size
    }

    #[cfg(unix)]
    fn raw_read(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        use std::os::unix::fs::FileExt;
        let mut done = 0;
        while done < buf.len() {
            match self.file.read_at(&mut buf[done..], off + done as u64) {
                Ok(0) => break,
                Ok(n) => done += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    if done > 0 {
                        break;
                    }
                    return Err(e);
                }
            }
        }
        Ok(done)
    }

    #[cfg(windows)]
    fn raw_read(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        use std::os::windows::fs::FileExt;
        let mut done = 0;
        while done < buf.len() {
            match self.file.seek_read(&mut buf[done..], off + done as u64) {
                Ok(0) => break,
                Ok(n) => done += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    if done > 0 {
                        break;
                    }
                    return Err(e);
                }
            }
        }
        Ok(done)
    }

    /// Read `buf.len()` bytes at `off`. Returns number of bytes read (short at
    /// the end of the device). Unreadable (bad) sectors are filled with zeros.
    pub fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        if off >= self.size || buf.is_empty() {
            return Ok(0);
        }
        let want = (buf.len() as u64).min(self.size - off) as usize;
        let start = off - off % ALIGN;
        let mut end = off + want as u64;
        end = (end + ALIGN - 1) / ALIGN * ALIGN;
        // never read past the device end rounded to 512
        let dev_end = (self.size + 511) / 512 * 512;
        if end > dev_end {
            end = dev_end;
        }
        let span = (end - start) as usize;
        if start == off && span == want && (buf.as_ptr() as usize) % 16 == 0 {
            return self.read_resilient(off, &mut buf[..want]);
        }
        let mut tmp = AlignedBuf::new(span);
        let n = self.read_resilient(start, tmp.as_mut())?;
        let skip = (off - start) as usize;
        if n <= skip {
            return Ok(0);
        }
        let avail = (n - skip).min(want);
        buf[..avail].copy_from_slice(&tmp.as_mut()[skip..skip + avail]);
        Ok(avail)
    }

    /// Like raw_read, but on an I/O error falls back to sector-by-sector reads,
    /// zero-filling bad sectors so a damaged disk can still be scanned.
    fn read_resilient(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        match self.raw_read(off, buf) {
            Ok(n) => Ok(n),
            Err(e) => {
                if buf.len() <= 512 {
                    return Err(e);
                }
                let mut sector = AlignedBuf::new(512);
                let mut done = 0usize;
                while done < buf.len() {
                    let take = 512.min(buf.len() - done);
                    match self.raw_read(off + done as u64, sector.as_mut()) {
                        Ok(n) if n > 0 => {
                            let c = n.min(take);
                            buf[done..done + c].copy_from_slice(&sector.as_mut()[..c]);
                            if n < 512 {
                                return Ok(done + c);
                            }
                        }
                        Ok(_) => return Ok(done),
                        Err(_) => buf[done..done + take].fill(0),
                    }
                    done += take;
                }
                Ok(done)
            }
        }
    }

    pub fn read_vec(&self, off: u64, len: usize) -> io::Result<Vec<u8>> {
        let mut v = vec![0u8; len];
        let n = self.read_at(off, &mut v)?;
        v.truncate(n);
        Ok(v)
    }
}

/// Heap buffer whose data pointer is 4 KiB aligned (needed for some raw
/// device handles on Windows).
struct AlignedBuf {
    raw: Vec<u8>,
    start: usize,
    len: usize,
}

impl AlignedBuf {
    fn new(len: usize) -> Self {
        let raw = vec![0u8; len + ALIGN as usize];
        let addr = raw.as_ptr() as usize;
        let start = (ALIGN as usize - addr % ALIGN as usize) % ALIGN as usize;
        AlignedBuf { raw, start, len }
    }
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.raw[self.start..self.start + self.len]
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    pub path: String,
    pub name: String,
    pub size: u64,
    /// "volume" | "disk" | "image"
    pub kind: String,
    pub filesystem: Option<String>,
    pub label: Option<String>,
}

/// Enumerate drives/volumes the current user can open.
pub fn list_devices() -> Vec<DeviceInfo> {
    let mut out = Vec::new();
    #[cfg(windows)]
    {
        for letter in b'A'..=b'Z' {
            let root = format!("{}:\\", letter as char);
            if !Path::new(&root).exists() {
                continue;
            }
            let path = format!("\\\\.\\{}:", letter as char);
            if let Ok(dev) = Device::open(&path) {
                let fs = crate::volume::detect_fs(&dev, 0);
                out.push(DeviceInfo {
                    name: format!("{}: диск", letter as char),
                    path,
                    size: dev.size(),
                    kind: "volume".into(),
                    filesystem: fs.map(|f| f.name().to_string()),
                    label: crate::volume::volume_label(&dev, 0),
                });
            }
        }
        for n in 0..16 {
            let path = format!("\\\\.\\PhysicalDrive{}", n);
            if let Ok(dev) = Device::open(&path) {
                out.push(DeviceInfo {
                    name: format!("Физик диск {}", n),
                    path,
                    size: dev.size(),
                    kind: "disk".into(),
                    filesystem: None,
                    label: None,
                });
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(parts) = std::fs::read_to_string("/proc/partitions") {
            for line in parts.lines().skip(2) {
                let cols: Vec<&str> = line.split_whitespace().collect();
                if cols.len() < 4 {
                    continue;
                }
                let name = cols[3];
                if name.starts_with("ram") || name.starts_with("zram") {
                    continue;
                }
                let path = format!("/dev/{}", name);
                let is_disk = Path::new(&format!("/sys/block/{}", name)).exists();
                if let Ok(dev) = Device::open(&path) {
                    let fs = crate::volume::detect_fs(&dev, 0);
                    out.push(DeviceInfo {
                        name: name.to_string(),
                        path,
                        size: dev.size(),
                        kind: if is_disk { "disk".into() } else { "volume".into() },
                        filesystem: fs.map(|f| f.name().to_string()),
                        label: crate::volume::volume_label(&dev, 0),
                    });
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        for n in 0..16 {
            let path = format!("/dev/rdisk{}", n);
            if let Ok(dev) = Device::open(&path) {
                out.push(DeviceInfo {
                    name: format!("disk{}", n),
                    path,
                    size: dev.size(),
                    kind: "disk".into(),
                    filesystem: None,
                    label: None,
                });
            }
        }
    }
    let _ = Path::new(""); // keep import used on all targets
    out
}

/// Describe a disk image file chosen by the user.
pub fn image_info(path: &str) -> io::Result<DeviceInfo> {
    let dev = Device::open(path)?;
    let fs = crate::volume::detect_fs(&dev, 0);
    let name = Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string());
    Ok(DeviceInfo {
        name,
        path: path.to_string(),
        size: dev.size(),
        kind: "image".into(),
        filesystem: fs.map(|f| f.name().to_string()),
        label: crate::volume::volume_label(&dev, 0),
    })
}

/// True when the output directory lives on the volume being recovered —
/// writing there could overwrite the very data we're trying to recover.
pub fn same_volume(device_path: &str, out_dir: &str) -> bool {
    let dp = device_path.to_ascii_uppercase();
    let od = out_dir.to_ascii_uppercase();
    if let Some(rest) = dp.strip_prefix("\\\\.\\") {
        if rest.len() == 2 && rest.ends_with(':') {
            return od.starts_with(&rest[..2]);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(dm), Ok(om)) = (std::fs::metadata(device_path), std::fs::metadata(out_dir)) {
            use std::os::unix::fs::FileTypeExt;
            if dm.file_type().is_block_device() {
                return dm.rdev() == om.dev();
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn probe_matches_file_size() {
        let p = std::env::temp_dir().join("metarecover_probe.img");
        for len in [4096u64 * 10 + 1536, 4096 * 33, 512 * 3] {
            std::fs::write(&p, vec![7u8; len as usize]).unwrap();
            let d = Device::open(p.to_str().unwrap()).unwrap();
            assert_eq!(d.probe_size(), len, "len {}", len);
            let mut b = [0u8; 100];
            assert_eq!(d.read_at(len - 50, &mut b).unwrap(), 50);
        }
        let _ = std::fs::remove_file(p);
    }
}
