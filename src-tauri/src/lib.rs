//! 合天下桌面壳。界面只通过命令层调用核心。
//! macOS 正式包用 universal-apple-darwin 打出一份双架构二进制。

pub mod commands;

pub const MACOS_TARGET: &str = "universal-apple-darwin";

pub fn run() {
    hetianxia_core::boot();
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            commands::scan,
            commands::compare,
            commands::diff,
            commands::merge,
            commands::execute,
            commands::rollback,
            commands::history,
            commands::connections,
        ])
        .run(tauri::generate_context!())
        .expect("合天下启动失败");
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
        assert_eq!(MACOS_TARGET, "universal-apple-darwin");
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
