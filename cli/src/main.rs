//! MetaRecover CLI
//!
//!   metarecover devices
//!   metarecover scan <device|image> [--deep] [--all] [--json FILE]
//!   metarecover recover <device|image> --out DIR [--deep] [--all] [--ext jpg,pdf] [--ids 1,2] [--flat]

use metarecover_core::*;
use std::path::Path;
use std::sync::atomic::AtomicBool;

fn usage() -> ! {
    eprintln!(
        "MetaRecover — устгагдсан файл сэргээх\n\n\
         Хэрэглээ:\n  \
         metarecover devices\n  \
         metarecover scan <төхөөрөмж|image> [--deep] [--all] [--json FILE]\n  \
         metarecover recover <төхөөрөмж|image> --out DIR [--deep] [--all] [--ext jpg,pdf] [--ids 1,2,3] [--flat]\n\n\
         --deep   гүн скан (signature carving)\n  \
         --all    устгагдаагүй файлуудыг мөн харуулах/сэргээх\n\n\
         Windows жишээ:  metarecover scan \\\\.\\E: --deep"
    );
    std::process::exit(2)
}

fn human(n: u64) -> String {
    let u = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < 4 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{} B", n) } else { format!("{:.1} {}", v, u[i]) }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
    }
    let flag = |f: &str| args.iter().any(|a| a == f);
    let opt = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();

    match args[0].as_str() {
        "devices" => {
            for d in list_devices() {
                println!(
                    "{:<24} {:>10}  {:<6} {:<6} {}",
                    d.path,
                    human(d.size),
                    d.kind,
                    d.filesystem.unwrap_or_default(),
                    d.label.unwrap_or_default()
                );
            }
        }
        cmd @ ("scan" | "recover") => {
            let Some(path) = args.get(1) else { usage() };
            let dev = Device::open(path).unwrap_or_else(|e| {
                eprintln!("Нээж чадсангүй {}: {} (Administrator эрхээр ажиллуулна уу)", path, e);
                std::process::exit(1)
            });
            let cancel = AtomicBool::new(false);
            let opts = ScanOptions { deep: flag("--deep") };
            let progress = |p: &Progress| {
                if p.phase != "done" {
                    let pct = if p.total > 0 { p.done as f64 * 100.0 / p.total as f64 } else { 0.0 };
                    eprint!("\r{:<60} {:5.1}%  олдсон: {}   ", p.message, pct, p.found);
                }
            };
            let res = scan(&dev, &opts, &progress, &cancel);
            eprintln!();
            for v in &res.volumes {
                eprintln!("Боть: {} @ {} ({}) {}", v.filesystem, v.offset, human(v.size), v.label.clone().unwrap_or_default());
            }
            for e in &res.errors {
                eprintln!("Анхааруулга: {}", e);
            }
            let all = flag("--all");
            let exts: Option<Vec<String>> = opt("--ext").map(|s| s.split(',').map(|x| x.trim().to_lowercase()).collect());
            let ids: Option<Vec<u64>> = opt("--ids").map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect());
            let sel: Vec<&FoundFile> = res
                .files
                .iter()
                .filter(|f| all || f.deleted)
                .filter(|f| exts.as_ref().map(|e| e.contains(&f.ext)).unwrap_or(true))
                .filter(|f| ids.as_ref().map(|i| i.contains(&f.id)).unwrap_or(true))
                .collect();

            if cmd == "scan" {
                println!("{:>6}  {:<7} {:<5} {:>10}  {:<40} {}", "ID", "Эх", "Чанар", "Хэмжээ", "Нэр", "Зам");
                for f in &sel {
                    println!(
                        "{:>6}  {:<7} {:<5} {:>10}  {:<40} {}",
                        f.id,
                        format!("{:?}", f.source).to_lowercase(),
                        match f.quality {
                            Quality::Good => "сайн",
                            Quality::Poor => "муу",
                            Quality::Unknown => "?",
                        },
                        human(f.size),
                        f.name,
                        f.path
                    );
                }
                println!("\nНийт: {} файл ({} устгагдсан)", sel.len(), sel.iter().filter(|f| f.deleted).count());
                if let Some(j) = opt("--json") {
                    let s = serde_json::to_string_pretty(&sel).unwrap();
                    std::fs::write(&j, s).expect("json бичиж чадсангүй");
                    println!("JSON: {}", j);
                }
            } else {
                let Some(out) = opt("--out") else { usage() };
                if same_volume(path, &out) {
                    eprintln!("АНХААР: гаралтын хавтас сэргээж буй дисктэй ижил байна — өгөгдөл дарагдаж болзошгүй. Өөр диск сонгоно уу.");
                    std::process::exit(1);
                }
                let prog = |d: u64, t: u64, name: &str| eprint!("\rСэргээж байна {}/{} {:<50}", d, t, name);
                let rep = recover(&dev, &sel, Path::new(&out), !flag("--flat"), &prog, &cancel);
                eprintln!();
                println!("Амжилттай: {}  Алдаа: {}  Нийт: {}", rep.ok, rep.failed, human(rep.bytes));
                for e in &rep.errors {
                    println!("  {}", e);
                }
            }
        }
        _ => usage(),
    }
}
