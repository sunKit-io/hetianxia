//! 合天下桌面壳。界面只通过命令层调用核心。
//! macOS 正式包用 universal-apple-darwin 打出一份双架构二进制。
//!
//! 第二个进程由单实例插件把已有窗口拉到前面，然后自己退出，不改 `run.lock`。
//! `begin_run` 在任何恢复提示之前完成：先核对崩溃那次的 `needs_verify`，再写新的 `run_id`。
//! 窗口关闭、退出请求和进程退出都会 `release_clean`。

pub mod commands;

use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{Manager, RunEvent, WindowEvent};

pub const MACOS_TARGET: &str = "universal-apple-darwin";

pub struct Session {
    db_path: PathBuf,
    /// 固定为 app_data/bak。界面不能改。
    backup_root: PathBuf,
    project_root: Mutex<PathBuf>,
    lock: Mutex<Option<hetianxia_core::runlock::RunLock>>,
    offers: Vec<hetianxia_core::exec::RecoveryOffer>,
    startup_error: Option<String>,
}

pub fn app_db_path(data: &std::path::Path) -> PathBuf {
    data.join("hetianxia.sqlite")
}

pub fn app_backup_root(data: &std::path::Path) -> PathBuf {
    data.join("bak")
}

fn boot_error(err: &hetianxia_core::error::Error) -> String {
    format!("{} · {}", err.code_str(), err.message)
}

fn failed_session(message: String) -> Session {
    Session {
        db_path: PathBuf::new(),
        backup_root: PathBuf::new(),
        project_root: Mutex::new(PathBuf::new()),
        lock: Mutex::new(None),
        offers: Vec::new(),
        startup_error: Some(message),
    }
}

fn open_session(data: PathBuf) -> Result<Session, String> {
    std::fs::create_dir_all(&data).map_err(|e| format!("E_IO · {e}"))?;
    let db_path = app_db_path(&data);
    let backup_root = app_backup_root(&data);
    std::fs::create_dir_all(&backup_root).map_err(|e| format!("E_IO · {e}"))?;
    let saved = std::fs::read_to_string(data.join("project-root")).unwrap_or_default();
    let root = if saved.trim().is_empty() { data.clone() } else { PathBuf::from(saved.trim()) };
    let db = hetianxia_core::exec::open_db(&db_path).map_err(|e| boot_error(&e))?;
    let fsys = hetianxia_core::vfs::LocalFs::new(&root);
    match hetianxia_core::exec::begin_run(&db, &fsys, &data).map_err(|e| boot_error(&e))? {
        hetianxia_core::exec::RunStart::AnotherInstance => std::process::exit(0),
        hetianxia_core::exec::RunStart::Started { lock, offers, .. } => Ok(Session {
            db_path,
            backup_root,
            project_root: Mutex::new(root),
            lock: Mutex::new(Some(lock)),
            offers,
            startup_error: None,
        }),
    }
}

fn end_session(session: &Session) {
    let Ok(mut guard) = session.lock.lock() else { return };
    let Some(lock) = guard.take() else { return };
    if let Ok(db) = hetianxia_core::exec::open_db(&session.db_path) {
        let _ = lock.release_clean(&db);
    }
}

pub fn run() {
    hetianxia_core::boot();
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .setup(|app| {
            // begin_run 失败也要把窗口留住，错误交给界面上的对话框。
            let session = match app.path().app_data_dir() {
                Ok(data) => match open_session(data) {
                    Ok(session) => session,
                    Err(message) => failed_session(message),
                },
                Err(err) => failed_session(format!("E_IO · {err}")),
            };
            app.manage(session);
            Ok(())
        })
        .on_window_event(|window, event| {
            if matches!(event, WindowEvent::CloseRequested { .. }) {
                if let Some(session) = window.try_state::<Session>() {
                    end_session(session.inner());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::scan,
            commands::compare,
            commands::diff,
            commands::merge,
            commands::execute,
            commands::rollback,
            commands::rollback_run,
            commands::resume_run,
            commands::history,
            commands::journal_runs,
            commands::connections,
            commands::startup_offers,
            commands::startup_failure,
            commands::ssh_secret_service,
            commands::save_ssh_secret,
            commands::ssh_secret_status,
            commands::delete_ssh_secret,
            commands::host_key_status,
            commands::clear_host_fingerprint,
            commands::trust_host_fingerprint,
            commands::remote_backup_path,
            commands::remote_rename_plan,
            commands::remote_write_prompt,
            commands::commit_remote_overwrite,
            commands::app_bak_dir,
            commands::probe_remote,
        ])
        .build(tauri::generate_context!())
        .expect("合天下启动失败")
        .run(|app, event| {
            if matches!(event, RunEvent::ExitRequested { .. } | RunEvent::Exit) {
                if let Some(session) = app.try_state::<Session>() {
                    end_session(session.inner());
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use hetianxia_core::model::{ExecPlan, OpKind, PlanOp, Summary};

    fn command_header(source: &str, name: &str) -> String {
        let marker = format!("pub fn {name}(");
        let start = source.find(&marker).unwrap_or_else(|| panic!("missing {name}"));
        let rest = &source[start..];
        let end = rest.find("->").unwrap_or_else(|| panic!("missing arrow on {name}"));
        rest[..end].to_string()
    }

    #[test]
    fn shell_does_not_enable_devtools_and_keeps_the_bundle_id() {
        let manifest = include_str!("../Cargo.toml");
        assert!(manifest.contains("fault-inject = [\"hetianxia-core/fault-inject\"]"));
        assert!(!manifest.contains("devtools"));
        let conf = include_str!("../tauri.conf.json");
        assert!(conf.contains("\"productName\": \"HeTianXia\""));
        assert!(conf.contains("\"identifier\": \"com.hetianxia.app\""));
        assert!(conf.contains("\"minimumSystemVersion\": \"12.0\""));
        assert!(conf.contains("\"frontendDist\": \"../dist\""));
        assert!(conf.contains("pnpm build"));
        assert!(!conf.contains("\"frontendDist\": \"ui\""));
        let value: serde_json::Value = serde_json::from_str(conf).unwrap();
        assert_eq!(value["build"]["frontendDist"], "../dist");
        let hook = &value["build"]["beforeBuildCommand"];
        let script = hook.as_str().unwrap_or_else(|| hook["script"].as_str().expect("beforeBuildCommand"));
        assert!(script.contains("pnpm build"), "{script}");
        let placeholder = include_str!("../ui/index.html");
        assert!(placeholder.contains("<p>合天下</p>"));
        assert!(!placeholder.contains("id=\"root\""));
        assert_eq!(MACOS_TARGET, "universal-apple-darwin");
        let source = include_str!("lib.rs");
        assert!(source.contains("tauri_plugin_single_instance"));
        assert!(source.contains("begin_run"));
        assert!(source.contains("release_clean"));
        assert!(source.contains("startup_offers"));
        assert!(source.contains("startup_failure"));
        assert!(source.contains("failed_session"));
        assert_eq!(app_db_path(std::path::Path::new("/data")), std::path::PathBuf::from("/data/hetianxia.sqlite"));
        assert_eq!(app_backup_root(std::path::Path::new("/data")), std::path::PathBuf::from("/data/bak"));
        let commands = include_str!("commands.rs");
        for name in ["execute", "rollback", "rollback_run", "resume_run", "history", "journal_runs"] {
            let header = command_header(commands, name);
            assert!(!header.contains("db_path"), "{name}: {header}");
            assert!(!header.contains("backup_root"), "{name}: {header}");
        }
        assert!(commands.contains("session.backup_root"));
        assert!(!commands.contains("join(\"backups\")"));
        assert!(!commands.contains("load_ssh_secret"));
        assert!(commands.contains("ssh_secret_status"));
        assert!(commands.contains("req.secret.clear()"));
    }

    #[test]
    fn csp_is_set_and_capabilities_do_not_grant_fs_or_shell() {
        let conf = include_str!("../tauri.conf.json");
        assert!(!conf.contains("\"csp\": null"));
        assert!(conf.contains("default-src 'self'"));
        assert!(conf.contains("object-src 'none'"));
        assert!(!conf.contains("unsafe-eval"));
        let caps = include_str!("../capabilities/default.json");
        assert!(!caps.contains("fs:"));
        assert!(!caps.contains("shell:"));
        assert!(caps.contains("allow-execute"));
        assert!(caps.contains("allow-journal-runs"));
        assert!(caps.contains("core:default"));
    }

    #[test]
    fn commands_scan_diff_merge_history_and_connections() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "old\n").unwrap();
        let right = dir.path().join("right");
        std::fs::create_dir(&right).unwrap();
        std::fs::write(right.join("a.txt"), "new\n").unwrap();
        let scanned = commands::scan(dir.path().display().to_string(), right.display().to_string()).unwrap();
        assert!(scanned.files.iter().any(|f| f.rel == "a.txt"));
        let compared = commands::compare(dir.path().display().to_string(), right.display().to_string()).unwrap();
        assert_eq!(compared.files.len(), scanned.files.len());

        let diff = commands::diff("old\n".into(), "new\n".into()).unwrap();
        assert!(!diff.hunks.is_empty());
        let merged = commands::merge("old\n".into(), "new\n".into(), "keepLeft".into()).unwrap();
        assert!(merged.text.contains("old"));

        let db = dir.path().join("t.sqlite");
        let plan = ExecPlan {
            id: "exec-1".into(),
            project_key: "demo".into(),
            ops: vec![PlanOp {
                index: 1,
                kind: OpKind::Overwrite,
                path: "a.txt".into(),
                from: None,
                bytes: Some(b"new\n".to_vec()),
                expect: None,
                mode: None,
                source_mtime_ms: None,
                merged: false,
                keep_source: false,
                readonly: false,
                label: "覆盖".into(),
            }],
            summary: Summary { overwrite: 1, ..Summary::default() },
        };
        let backup = dir.path().join("bak");
        let report = commands::execute_in(&db, dir.path(), &backup, plan, false).unwrap();
        assert_eq!(report.history_result, "成功");
        let rows = commands::history_in(&db).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].result, "成功");
        let rolled = commands::rollback_in(
            &db,
            dir.path(),
            "exec-1",
            hetianxia_core::exec::RollbackChoice::ProceedBackupCurrent,
        )
        .unwrap();
        assert_eq!(rolled.state, "ROLLED_BACK");
        let runs = commands::journal_runs_in(&db).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].exec_id, "exec-1");
        assert!(runs[0].project_root.is_none());
        assert!(!runs[0].remote);
        assert_eq!(runs[0].file_count, 1);
        assert_eq!(runs[0].status, "ROLLED_BACK");

        let hosts = commands::connections("Host build-01\n  HostName 10.0.8.15\n  User deploy\n".into()).unwrap();
        assert_eq!(hosts[0].alias, "build-01");
        assert!(hosts[0].available());

        assert_eq!(commands::ssh_secret_service(), "com.hetianxia.app.ssh");
        assert_eq!(
            commands::remote_backup_path("/home/tester".into(), "exec-9".into()).unwrap(),
            "/home/tester/.hetianxia-backups/exec-9"
        );
        assert_eq!(
            commands::remote_rename_plan(false),
            vec!["rename-backup".to_string(), "rename-replace".into(), "delete-backup".into()]
        );
        let known = dir.path().join("known_hosts");
        std::fs::write(
            &known,
            "build-01 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILagOJFgwaMNhBWQINinKOXmqS4Gh5NgxgriXwdOoINJ\n",
        )
        .unwrap();
        let before = std::fs::read(&known).unwrap();
        let status = commands::host_key_status(
            db.display().to_string(),
            known.display().to_string(),
            "build-01".into(),
            22,
            "SHA256:ldyiXa1JQakitNU5tErauu8DvWQ1dZ7aXu+rm7KQuog".into(),
        )
        .unwrap();
        assert_eq!(status, "match");
        commands::clear_host_fingerprint(db.display().to_string(), "build-01".into(), 22).unwrap();
        let again = commands::host_key_status(
            db.display().to_string(),
            known.display().to_string(),
            "build-01".into(),
            22,
            "SHA256:other".into(),
        )
        .unwrap();
        assert_eq!(again, "new");
        assert_eq!(std::fs::read(&known).unwrap(), before);
    }
}
