#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use base64::Engine;
use metarecover_core::{self as core, Device, DeviceInfo, FoundFile, Progress, Quality, RecoverReport, ScanOptions};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, State};

struct Session {
    device: String,
    files: Arc<Vec<FoundFile>>,
}

#[derive(Default)]
struct AppState {
    session: Mutex<Option<Session>>,
    cancel: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
}

#[derive(Serialize, Clone)]
struct ScanDone {
    total: u64,
    deleted: u64,
    volumes: Vec<core::VolumeInfo>,
    errors: Vec<String>,
    cancelled: bool,
    seconds: f64,
}

#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct Filter {
    category: Option<String>,
    deleted_only: bool,
    search: String,
    quality: Option<String>,
    sort: String,
    desc: bool,
}

#[derive(Serialize)]
struct Page {
    total: usize,
    total_bytes: u64,
    items: Vec<FoundFile>,
}

#[derive(Serialize)]
struct Summary {
    categories: BTreeMap<String, u64>,
    total: u64,
}

#[derive(Serialize)]
struct Preview {
    kind: String,
    mime: String,
    data: String,
}

fn matches(f: &FoundFile, flt: &Filter, q: &str) -> bool {
    if flt.deleted_only && !f.deleted {
        return false;
    }
    if let Some(c) = &flt.category {
        if !c.is_empty() && c != "all" && f.category != c {
            return false;
        }
    }
    if let Some(ql) = &flt.quality {
        let want = match ql.as_str() {
            "good" => Some(Quality::Good),
            "poor" => Some(Quality::Poor),
            "unknown" => Some(Quality::Unknown),
            _ => None,
        };
        if want.is_some_and(|w| f.quality != w) {
            return false;
        }
    }
    if !q.is_empty() && !f.name.to_lowercase().contains(q) && !f.path.to_lowercase().contains(q) {
        return false;
    }
    true
}

fn filtered<'a>(files: &'a [FoundFile], flt: &Filter) -> Vec<&'a FoundFile> {
    let q = flt.search.trim().to_lowercase();
    let mut v: Vec<&FoundFile> = files.iter().filter(|f| matches(f, flt, &q)).collect();
    match flt.sort.as_str() {
        "name" => v.sort_by_cached_key(|f| f.name.to_lowercase()),
        "size" => v.sort_by_key(|f| f.size),
        "date" => v.sort_by_key(|f| f.modified.unwrap_or(0)),
        "path" => v.sort_by(|a, b| (&a.path, &a.name).cmp(&(&b.path, &b.name))),
        _ => {}
    }
    if flt.desc {
        v.reverse();
    }
    v
}

fn session_files(state: &State<AppState>) -> Result<(String, Arc<Vec<FoundFile>>), String> {
    let s = state.session.lock().unwrap();
    s.as_ref().map(|s| (s.device.clone(), s.files.clone())).ok_or_else(|| "Скан хийгдээгүй байна".to_string())
}

#[tauri::command]
async fn list_devices() -> Vec<DeviceInfo> {
    tauri::async_runtime::spawn_blocking(core::list_devices).await.unwrap_or_default()
}

#[tauri::command]
async fn open_image(path: String) -> Result<DeviceInfo, String> {
    tauri::async_runtime::spawn_blocking(move || core::image_info(&path).map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
fn is_elevated() -> bool {
    #[cfg(windows)]
    {
        // Opening the raw C: volume only succeeds with admin rights.
        std::fs::File::open("\\\\.\\C:").is_ok()
    }
    #[cfg(unix)]
    {
        euid_is_root()
    }
}

#[cfg(unix)]
fn euid_is_root() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("Uid:")).map(|l| l.split_whitespace().nth(2) == Some("0")))
        .unwrap_or(false)
}

#[tauri::command]
fn start_scan(app: AppHandle, state: State<AppState>, path: String, deep: bool) -> Result<(), String> {
    if state.busy.swap(true, Ordering::SeqCst) {
        return Err("Өөр ажил явагдаж байна".into());
    }
    state.cancel.store(false, Ordering::SeqCst);
    let dev = match Device::open(&path) {
        Ok(d) => d,
        Err(e) => {
            state.busy.store(false, Ordering::SeqCst);
            return Err(format!("Төхөөрөмжийг нээж чадсангүй: {}. Програмыг Administrator эрхээр ажиллуулна уу.", e));
        }
    };
    let cancel = state.cancel.clone();
    let busy = state.busy.clone();
    let app2 = app.clone();
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let progress = move |p: &Progress| {
            let _ = app2.emit("scan-progress", p.clone());
        };
        let res = core::scan(&dev, &ScanOptions { deep }, &progress, &cancel);
        let done = ScanDone {
            total: res.files.len() as u64,
            deleted: res.files.iter().filter(|f| f.deleted).count() as u64,
            volumes: res.volumes,
            errors: res.errors,
            cancelled: res.cancelled,
            seconds: started.elapsed().as_secs_f64(),
        };
        let state = tauri::Manager::state::<AppState>(&app);
        *state.session.lock().unwrap() = Some(Session { device: path, files: Arc::new(res.files) });
        busy.store(false, Ordering::SeqCst);
        let _ = app.emit("scan-done", done);
    });
    Ok(())
}

#[tauri::command]
fn cancel(state: State<AppState>) {
    state.cancel.store(true, Ordering::SeqCst);
}

#[tauri::command]
fn summary(state: State<AppState>, deleted_only: bool) -> Result<Summary, String> {
    let (_, files) = session_files(&state)?;
    let mut categories = BTreeMap::new();
    let mut total = 0;
    for f in files.iter().filter(|f| !deleted_only || f.deleted) {
        *categories.entry(f.category.to_string()).or_insert(0) += 1;
        total += 1;
    }
    Ok(Summary { categories, total })
}

#[tauri::command]
fn query(state: State<AppState>, filter: Filter, offset: usize, limit: usize) -> Result<Page, String> {
    let (_, files) = session_files(&state)?;
    let v = filtered(&files, &filter);
    let total_bytes = v.iter().map(|f| f.size).sum();
    let items = v.iter().skip(offset).take(limit.min(2000)).map(|f| (*f).clone()).collect();
    Ok(Page { total: v.len(), total_bytes, items })
}

#[tauri::command]
async fn preview(state: State<'_, AppState>, id: u64) -> Result<Preview, String> {
    let (dev_path, files) = session_files(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let f = files.iter().find(|f| f.id == id).ok_or("Файл олдсонгүй")?;
        let dev = Device::open(&dev_path).map_err(|e| e.to_string())?;
        let image_mime = match f.ext.as_str() {
            "jpg" | "jpeg" => Some("image/jpeg"),
            "png" => Some("image/png"),
            "gif" => Some("image/gif"),
            "bmp" => Some("image/bmp"),
            "webp" => Some("image/webp"),
            _ => None,
        };
        let text = matches!(f.ext.as_str(), "txt" | "csv" | "md" | "json" | "xml" | "html" | "htm" | "log" | "ini" | "sql");
        if let (Some(mime), true) = (image_mime, f.size <= 30 << 20) {
            let data = core::read_content(&dev, f, 30 << 20).map_err(|e| e.to_string())?;
            return Ok(Preview {
                kind: "image".into(),
                mime: mime.into(),
                data: base64::engine::general_purpose::STANDARD.encode(data),
            });
        }
        if text {
            let data = core::read_content(&dev, f, 64 << 10).map_err(|e| e.to_string())?;
            return Ok(Preview { kind: "text".into(), mime: "text/plain".into(), data: String::from_utf8_lossy(&data).into() });
        }
        // Hex dump of the first bytes — lets the user see whether data is
        // still there (all zeros usually means overwritten).
        let data = core::read_content(&dev, f, 512).map_err(|e| e.to_string())?;
        let mut s = String::new();
        for (i, row) in data.chunks(8).enumerate() {
            let hex: Vec<String> = row.iter().map(|b| format!("{:02X}", b)).collect();
            let asc: String = row.iter().map(|&b| if (32..127).contains(&b) { b as char } else { '.' }).collect();
            s.push_str(&format!("{:04X}  {:<24} {}\n", i * 8, hex.join(" "), asc));
        }
        Ok(Preview { kind: "hex".into(), mime: String::new(), data: s })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn check_destination(state: State<AppState>, out_dir: String) -> Result<bool, String> {
    let (dev, _) = session_files(&state)?;
    Ok(core::same_volume(&dev, &out_dir))
}

#[tauri::command]
fn start_recover(
    app: AppHandle,
    state: State<AppState>,
    ids: Vec<u64>,
    filter: Option<Filter>,
    out_dir: String,
    keep_paths: bool,
) -> Result<(), String> {
    let (dev_path, files) = session_files(&state)?;
    if core::same_volume(&dev_path, &out_dir) {
        return Err("Сэргээж буй дискэн дээрээ хадгалах боломжгүй — өгөгдөл дарагдана. Өөр диск сонгоно уу.".into());
    }
    if state.busy.swap(true, Ordering::SeqCst) {
        return Err("Өөр ажил явагдаж байна".into());
    }
    state.cancel.store(false, Ordering::SeqCst);
    let cancel = state.cancel.clone();
    let busy = state.busy.clone();
    std::thread::spawn(move || {
        let rep = (|| -> Result<RecoverReport, String> {
            let dev = Device::open(&dev_path).map_err(|e| e.to_string())?;
            let sel: Vec<&FoundFile> = match &filter {
                Some(flt) => filtered(&files, flt),
                None => {
                    let set: std::collections::HashSet<u64> = ids.into_iter().collect();
                    files.iter().filter(|f| set.contains(&f.id)).collect()
                }
            };
            let app2 = app.clone();
            let last = std::cell::Cell::new(std::time::Instant::now());
            let prog = move |d: u64, t: u64, name: &str| {
                if d == t || last.get().elapsed().as_millis() > 100 {
                    last.set(std::time::Instant::now());
                    let _ = app2.emit("recover-progress", (d, t, name.to_string()));
                }
            };
            Ok(core::recover(&dev, &sel, Path::new(&out_dir), keep_paths, &prog, &cancel))
        })();
        busy.store(false, Ordering::SeqCst);
        match rep {
            Ok(r) => {
                let _ = app.emit("recover-done", r);
            }
            Err(e) => {
                let _ = app.emit("recover-done", RecoverReport { failed: 1, errors: vec![e], ..Default::default() });
            }
        }
    });
    Ok(())
}

#[tauri::command]
fn open_folder(path: String) -> Result<(), String> {
    #[cfg(windows)]
    let cmd = "explorer";
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = "xdg-open";
    std::process::Command::new(cmd).arg(&path).spawn().map(|_| ()).map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            list_devices,
            open_image,
            is_elevated,
            start_scan,
            cancel,
            summary,
            query,
            preview,
            check_destination,
            start_recover,
            open_folder
        ])
        .run(tauri::generate_context!())
        .expect("MetaRecover эхлүүлэхэд алдаа гарлаа");
}
