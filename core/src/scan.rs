use crate::device::Device;
use crate::model::*;
use crate::volume::{self, FsType};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Collects results and forwards throttled progress to the caller.
pub struct Sink<'a> {
    pub files: Vec<FoundFile>,
    cancel: &'a AtomicBool,
    progress: &'a (dyn Fn(&Progress) + Sync),
    last: Instant,
}

impl<'a> Sink<'a> {
    pub fn new(cancel: &'a AtomicBool, progress: &'a (dyn Fn(&Progress) + Sync)) -> Self {
        Sink { files: Vec::new(), cancel, progress, last: Instant::now() - Duration::from_secs(1) }
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    pub fn push(&mut self, mut f: FoundFile) {
        f.id = self.files.len() as u64 + 1;
        f.fragments = f.extents.len().max(1);
        self.files.push(f);
    }
    pub fn report(&mut self, phase: &str, message: String, done: u64, total: u64) {
        if self.last.elapsed() < Duration::from_millis(150) && done < total {
            return;
        }
        self.last = Instant::now();
        (self.progress)(&Progress { phase: phase.into(), message, done, total, found: self.files.len() as u64 });
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScanOptions {
    /// Signature carving over the whole device (slow, finds files after
    /// format / on RAW partitions).
    pub deep: bool,
}

pub struct ScanResult {
    pub files: Vec<FoundFile>,
    pub volumes: Vec<VolumeInfo>,
    pub errors: Vec<String>,
    pub cancelled: bool,
}

pub fn scan(dev: &Device, opts: &ScanOptions, progress: &(dyn Fn(&Progress) + Sync), cancel: &AtomicBool) -> ScanResult {
    let mut sink = Sink::new(cancel, progress);
    let mut errors = Vec::new();
    let mut vols: Vec<(u64, u64, FsType)> = Vec::new();
    if let Some(fs) = volume::detect_fs(dev, 0) {
        vols.push((0, dev.size(), fs));
    } else {
        for p in volume::partitions(dev) {
            if let Some(fs) = volume::detect_fs(dev, p.offset) {
                vols.push((p.offset, p.size, fs));
            }
        }
    }
    let mut volumes = Vec::new();
    for &(off, size, fs) in &vols {
        if sink.cancelled() {
            break;
        }
        volumes.push(VolumeInfo { offset: off, size, filesystem: fs.name().into(), label: volume::volume_label(dev, off) });
        let first = sink.files.len();
        let r = match fs {
            FsType::Ntfs => crate::ntfs::scan(dev, off, &mut sink),
            FsType::Exfat => crate::exfat::scan(dev, off, &mut sink),
            _ => crate::fat::scan(dev, off, fs, &mut sink),
        };
        if let Err(e) = r {
            errors.push(format!("{} @ {}: {}", fs.name(), off, e));
        }
        // On a whole disk with several partitions, keep files apart by volume.
        if vols.len() > 1 {
            let n = volumes.len();
            let label = volumes[n - 1].label.clone().map(|l| format!(" {}", l)).unwrap_or_default();
            let prefix = format!("Хуваалт {} ({}{})", n, fs.name(), label);
            for f in &mut sink.files[first..] {
                f.path = if f.path.is_empty() { prefix.clone() } else { format!("{}/{}", prefix, f.path) };
            }
        }
    }
    if vols.is_empty() {
        errors.push("Танигдах файлын систем олдсонгүй — гүн скан хэрэглэнэ үү.".into());
    }
    if (opts.deep || vols.is_empty()) && !sink.cancelled() {
        let mut skip: Vec<(u64, u64)> = sink
            .files
            .iter()
            .flat_map(|f| f.extents.iter().filter_map(|e| e.offset.map(|o| (o, o + e.len))))
            .collect();
        skip.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(skip.len());
        for (s, e) in skip {
            match merged.last_mut() {
                Some(l) if s <= l.1 => l.1 = l.1.max(e),
                _ => merged.push((s, e)),
            }
        }
        crate::carve::carve(dev, 0, dev.size(), &merged, &mut sink);
    }
    let cancelled = sink.cancelled();
    let total = sink.files.len() as u64;
    (progress)(&Progress { phase: "done".into(), message: "Скан дууслаа".into(), done: 1, total: 1, found: total });
    ScanResult { files: sink.files, volumes, errors, cancelled }
}
