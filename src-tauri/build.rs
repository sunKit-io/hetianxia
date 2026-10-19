fn main() {
    let manifest = tauri_build::AppManifest::new().commands(&[
        "scan",
        "compare",
        "diff",
        "merge",
        "execute",
        "rollback",
        "rollback_run",
        "resume_run",
        "history",
        "journal_runs",
        "connections",
        "startup_offers",
        "startup_failure",
        "ssh_secret_service",
        "save_ssh_secret",
        "ssh_secret_status",
        "delete_ssh_secret",
        "host_key_status",
        "clear_host_fingerprint",
        "trust_host_fingerprint",
        "remote_backup_path",
        "remote_rename_plan",
        "probe_remote",
    ]);
    if let Err(error) = tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest)) {
        println!("{error:#}");
        std::process::exit(1);
    }
}
