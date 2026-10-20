fn main() {
    // Raw disk access needs Administrator rights: the manifest makes Windows
    // show the UAC prompt on launch.
    let windows = tauri_build::WindowsAttributes::new().app_manifest(include_str!("app.manifest"));
    tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(windows)).expect("tauri build failed");
}
