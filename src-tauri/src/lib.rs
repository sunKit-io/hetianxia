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
    lock: Mutex<Option<hetianxia_core::runlock::RunLock>>,
    offers: Vec<hetianxia_core::exec::RecoveryOffer>,
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
            let data = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data)?;
            let db_path = data.join("hetianxia.sqlite");
            let saved = std::fs::read_to_string(data.join("project-root")).unwrap_or_default();
            let root = if saved.trim().is_empty() { data.clone() } else { PathBuf::from(saved.trim()) };
            let db = hetianxia_core::exec::open_db(&db_path)?;
            let fsys = hetianxia_core::vfs::LocalFs::new(&root);
            let session = match hetianxia_core::exec::begin_run(&db, &fsys, &data)? {
                hetianxia_core::exec::RunStart::AnotherInstance => std::process::exit(0),
                hetianxia_core::exec::RunStart::Started { lock, offers, .. } => Session {
                    db_path,
                    lock: Mutex::new(Some(lock)),
                    offers,
                },
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
            commands::history,
            commands::connections,
            commands::startup_offers,
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
        assert_eq!(MACOS_TARGET, "universal-apple-darwin");
        let source = include_str!("lib.rs");
        assert!(source.contains("tauri_plugin_single_instance"));
        assert!(source.contains("begin_run"));
        assert!(source.contains("release_clean"));
        assert!(source.contains("startup_offers"));
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
        let report = commands::execute(
            db.display().to_string(),
            dir.path().display().to_string(),
            dir.path().join("bak").display().to_string(),
            plan,
            false,
        )
        .unwrap();
        assert_eq!(report.history_result, "成功");
        let rows = commands::history(db.display().to_string()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].result, "成功");
        let rolled = commands::rollback(
            db.display().to_string(),
            dir.path().display().to_string(),
            "exec-1".into(),
            hetianxia_core::exec::RollbackChoice::ProceedBackupCurrent,
        )
        .unwrap();
        assert_eq!(rolled.state, "ROLLED_BACK");

        let hosts = commands::connections("Host build-01\n  HostName 10.0.8.15\n  User deploy\n".into()).unwrap();
        assert_eq!(hosts[0].alias, "build-01");
        assert!(hosts[0].available());
    }
}
