fn main() {
    let manifest = tauri_build::AppManifest::new().commands(&[
        "scan",
        "compare",
        "diff",
        "merge",
        "execute",
        "rollback",
        "history",
        "connections",
    ]);
    if let Err(error) = tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest)) {
        println!("{error:#}");
        std::process::exit(1);
    }
}
