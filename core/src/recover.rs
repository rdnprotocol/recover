use crate::device::Device;
use crate::model::*;
use crate::util::sanitize_name;
use serde::Serialize;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Stream a found file's content into `w`. Returns bytes written.
pub fn write_content<W: Write>(dev: &Device, f: &FoundFile, w: &mut W, limit: u64) -> io::Result<u64> {
    let want = f.size.min(limit);
    if let Some(res) = &f.resident {
        let n = (res.len() as u64).min(want) as usize;
        w.write_all(&res[..n])?;
        return Ok(n as u64);
    }
    let mut written = 0u64;
    let mut buf = vec![0u8; 4 << 20];
    for e in &f.extents {
        let mut done = 0u64;
        while done < e.len && written < want {
            let n = ((e.len - done).min(want - written) as usize).min(buf.len());
            match e.offset {
                Some(o) => {
                    let got = dev.read_at(o + done, &mut buf[..n])?;
                    if got < n {
                        buf[got..n].fill(0);
                    }
                }
                None => buf[..n].fill(0),
            }
            w.write_all(&buf[..n])?;
            done += n as u64;
            written += n as u64;
        }
    }
    Ok(written)
}

pub fn read_content(dev: &Device, f: &FoundFile, limit: u64) -> io::Result<Vec<u8>> {
    let mut v = Vec::with_capacity(f.size.min(limit) as usize);
    write_content(dev, f, &mut v, limit)?;
    Ok(v)
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct RecoverReport {
    pub ok: u64,
    pub failed: u64,
    pub bytes: u64,
    pub errors: Vec<String>,
    pub out_dir: String,
}

fn unique(p: PathBuf) -> PathBuf {
    if !p.exists() {
        return p;
    }
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = p.extension().map(|s| format!(".{}", s.to_string_lossy())).unwrap_or_default();
    for i in 1..10000 {
        let c = p.with_file_name(format!("{} ({}){}", stem, i, ext));
        if !c.exists() {
            return c;
        }
    }
    p
}

pub fn recover(
    dev: &Device,
    files: &[&FoundFile],
    out_dir: &Path,
    keep_paths: bool,
    progress: &dyn Fn(u64, u64, &str),
    cancel: &AtomicBool,
) -> RecoverReport {
    let mut rep = RecoverReport { out_dir: out_dir.to_string_lossy().to_string(), ..Default::default() };
    let total = files.len() as u64;
    for (i, f) in files.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        progress(i as u64, total, &f.name);
        let mut dir = out_dir.to_path_buf();
        if keep_paths {
            for part in f.path.split('/').filter(|s| !s.is_empty()) {
                dir.push(sanitize_name(part));
            }
        }
        let res = (|| -> io::Result<u64> {
            fs::create_dir_all(&dir)?;
            let target = unique(dir.join(sanitize_name(&f.name)));
            let file = File::create(&target)?;
            let mut w = BufWriter::with_capacity(1 << 20, file);
            let n = write_content(dev, f, &mut w, u64::MAX)?;
            let file = w.into_inner().map_err(|e| e.into_error())?;
            if let Some(t) = f.modified {
                if t > 0 {
                    let _ = file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(t as u64));
                }
            }
            Ok(n)
        })();
        match res {
            Ok(n) => {
                rep.ok += 1;
                rep.bytes += n;
            }
            Err(e) => {
                rep.failed += 1;
                if rep.errors.len() < 100 {
                    rep.errors.push(format!("{}: {}", f.name, e));
                }
            }
        }
    }
    progress(total, total, "");
    rep
}
