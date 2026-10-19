use hetianxia_core::error::{Error, ErrorCode};
use hetianxia_core::exec::{self, ExecOpts, ExecReport, HistoryRow, RollbackChoice};
use hetianxia_core::model::ExecPlan;
use hetianxia_core::merge::{self, Decision, Rendered};
use hetianxia_core::pathutil::Ignore;
use hetianxia_core::remote::{self, ImportedHost};
use hetianxia_core::scan::{self, ScanOptions, ScanOutput};
use hetianxia_core::vfs::LocalFs;
use std::path::{Path, PathBuf};

fn local_opts(backup_root: String, allow_readonly: bool) -> ExecOpts {
    ExecOpts {
        allow_readonly,
        backup_root: PathBuf::from(backup_root),
        remote: false,
        #[cfg(feature = "fault-inject")]
        stop_after_log: None,
        #[cfg(feature = "fault-inject")]
        corrupt_temp: false,
    }
}

#[tauri::command]
pub fn scan(left: String, right: String) -> Result<ScanOutput, Error> {
    let left = LocalFs::new(left);
    let right = LocalFs::new(right);
    scan::scan_local(&left, &right, &Ignore::defaults(), &ScanOptions::default())
}

#[tauri::command]
pub fn compare(left: String, right: String) -> Result<ScanOutput, Error> {
    scan(left, right)
}

#[tauri::command]
pub fn diff(left: String, right: String) -> Result<merge::FileDiff, Error> {
    Ok(merge::diff_texts(merge::decode(left.as_bytes()), merge::decode(right.as_bytes())))
}

#[tauri::command]
pub fn merge(left: String, right: String, decision: String) -> Result<Rendered, Error> {
    let file = merge::diff_texts(merge::decode(left.as_bytes()), merge::decode(right.as_bytes()));
    let mut state = merge::default_state(&file);
    let decision = match decision.as_str() {
        "keepLeft" => Decision::KeepLeft,
        "both" => Decision::Both { right_first: false },
        _ => Decision::TakeRight,
    };
    merge::set_all(&mut state, decision, true).map_err(|_| {
        Error::new(ErrorCode::UnconfirmedHunks, "还有没确认的差异块")
    })?;
    Ok(merge::render(&file, &state))
}

#[tauri::command]
pub fn execute(
    db_path: String,
    root: String,
    backup_root: String,
    plan: ExecPlan,
    allow_readonly: bool,
) -> Result<ExecReport, Error> {
    let db = exec::open_db(Path::new(&db_path))?;
    let fsys = LocalFs::new(root);
    let kinds: Vec<_> = plan.ops.iter().map(|op| op.kind).collect();
    let fault = hetianxia_core::fault::load_from_env_or_exit(&kinds);
    exec::execute(&db, &fsys, &plan, &local_opts(backup_root, allow_readonly), fault)
}

#[tauri::command]
pub fn rollback(db_path: String, root: String, exec_id: String, choice: RollbackChoice) -> Result<ExecReport, Error> {
    let db = exec::open_db(Path::new(&db_path))?;
    let fsys = LocalFs::new(root);
    exec::rollback(&db, &fsys, &exec_id, choice, hetianxia_core::fault::FaultPlan::default())
}

#[tauri::command]
pub fn history(db_path: String) -> Result<Vec<HistoryRow>, Error> {
    let db = exec::open_db(Path::new(&db_path))?;
    exec::list_history(&db)
}

#[tauri::command]
pub fn connections(ssh_config: String) -> Result<Vec<ImportedHost>, Error> {
    Ok(remote::parse_ssh_config(&ssh_config))
}
