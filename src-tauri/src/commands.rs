use hetianxia_core::error::{Error, ErrorCode};
use hetianxia_core::exec::{self, ExecOpts, ExecReport, HistoryRow, ResumeChoice, ResumeOutcome, RollbackChoice};
use hetianxia_core::model::ExecPlan;
use hetianxia_core::merge::{self, Decision, Rendered};
use hetianxia_core::pathutil::Ignore;
use hetianxia_core::remote::{self, ImportedHost};
use hetianxia_core::scan::{self, ScanOptions, ScanOutput};
use hetianxia_core::secrets::{self, KeyringStore, SecretStatus, SecretStore};
use hetianxia_core::sftp::{self, ProbeReport, ProbeRequest};
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

fn remember_project_root(db_path: &Path, root: &Path) {
    let path = db_path.with_file_name("project-root");
    let _ = std::fs::write(path, root.display().to_string());
}

fn ready(session: &crate::Session) -> Result<(), Error> {
    if let Some(message) = &session.startup_error {
        return Err(Error::new(ErrorCode::InvalidState, message.clone()));
    }
    Ok(())
}

fn project_root(session: &crate::Session) -> Result<PathBuf, Error> {
    ready(session)?;
    session
        .project_root
        .lock()
        .map(|guard| guard.clone())
        .map_err(|_| Error::new(ErrorCode::Internal, "项目目录锁被占用"))
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

pub fn execute_in(db_path: &Path, root: &Path, backup_root: &Path, plan: ExecPlan, allow_readonly: bool) -> Result<ExecReport, Error> {
    remember_project_root(db_path, root);
    let db = exec::open_db(db_path)?;
    let fsys = LocalFs::new(root);
    let kinds: Vec<_> = plan.ops.iter().map(|op| op.kind).collect();
    let fault = hetianxia_core::fault::load_from_env_or_exit(&kinds);
    exec::execute(&db, &fsys, &plan, &local_opts(backup_root.display().to_string(), allow_readonly), fault)
}

#[tauri::command]
pub fn execute(
    session: tauri::State<crate::Session>,
    root: String,
    plan: ExecPlan,
    allow_readonly: bool,
) -> Result<ExecReport, Error> {
    ready(&session)?;
    let project = PathBuf::from(&root);
    {
        let mut saved = session.project_root.lock().map_err(|_| Error::new(ErrorCode::Internal, "项目目录锁被占用"))?;
        *saved = project.clone();
    }
    execute_in(&session.db_path, &project, &session.backup_root, plan, allow_readonly)
}

pub fn rollback_in(db_path: &Path, root: &Path, exec_id: &str, choice: RollbackChoice) -> Result<ExecReport, Error> {
    let db = exec::open_db(db_path)?;
    let fsys = LocalFs::new(root);
    exec::rollback(&db, &fsys, exec_id, choice, hetianxia_core::fault::FaultPlan::default())
}

#[tauri::command]
pub fn rollback(session: tauri::State<crate::Session>, root: String, exec_id: String, choice: RollbackChoice) -> Result<ExecReport, Error> {
    ready(&session)?;
    rollback_in(&session.db_path, Path::new(&root), &exec_id, choice)
}

/// 已回滚的不能再回滚。已完成的升级可以回滚，所以已提交不在这里拒绝。
pub fn reject_finished(db_path: &Path, exec_id: &str, rollback: bool) -> Result<(), Error> {
    let db = exec::open_db(db_path)?;
    let state = exec::read_journal_state(&db, exec_id)?;
    if state == "ROLLED_BACK" {
        return Err(Error::new(
            ErrorCode::InvalidState,
            if rollback { "这次回滚已经结束" } else { "这次升级已经结束，不能继续" },
        ));
    }
    if state == "COMMITTED" && !rollback {
        return Err(Error::new(ErrorCode::InvalidState, "这次升级已经结束，不能继续"));
    }
    Ok(())
}

#[tauri::command]
pub fn rollback_run(session: tauri::State<crate::Session>, exec_id: String, choice: RollbackChoice) -> Result<ExecReport, Error> {
    reject_finished(&session.db_path, &exec_id, true)?;
    let root = project_root(&session)?;
    rollback_in(&session.db_path, &root, &exec_id, choice)
}

#[tauri::command]
pub fn resume_run(
    session: tauri::State<crate::Session>,
    exec_id: String,
    choice: ResumeChoice,
) -> Result<ResumeOutcome, Error> {
    reject_finished(&session.db_path, &exec_id, false)?;
    let root = project_root(&session)?;
    let db = exec::open_db(&session.db_path)?;
    let fsys = LocalFs::new(root);
    exec::resume(
        &db,
        &fsys,
        &exec_id,
        choice,
        &local_opts(session.backup_root.display().to_string(), false),
        hetianxia_core::fault::FaultPlan::default(),
    )
}

pub fn history_in(db_path: &Path) -> Result<Vec<HistoryRow>, Error> {
    let db = exec::open_db(db_path)?;
    exec::list_history(&db)
}

#[tauri::command]
pub fn history(session: tauri::State<crate::Session>) -> Result<Vec<HistoryRow>, Error> {
    ready(&session)?;
    history_in(&session.db_path)
}

pub fn journal_runs_in(db_path: &Path) -> Result<Vec<exec::JournalRun>, Error> {
    let db = exec::open_db(db_path)?;
    exec::list_journal_runs(&db)
}

#[tauri::command]
pub fn journal_runs(session: tauri::State<crate::Session>) -> Result<Vec<exec::JournalRun>, Error> {
    ready(&session)?;
    journal_runs_in(&session.db_path)
}

pub fn rollback_preview_in(db_path: &Path, exec_id: &str) -> Result<exec::RollbackPreview, Error> {
    let db = exec::open_db(db_path)?;
    exec::rollback_preview(&db, exec_id)
}

#[tauri::command]
pub fn rollback_preview(session: tauri::State<crate::Session>, exec_id: String) -> Result<exec::RollbackPreview, Error> {
    ready(&session)?;
    rollback_preview_in(&session.db_path, &exec_id)
}

#[tauri::command]
pub fn startup_failure(session: tauri::State<crate::Session>) -> Option<String> {
    session.startup_error.clone()
}

#[tauri::command]
pub fn startup_offers(session: tauri::State<crate::Session>) -> Vec<hetianxia_core::exec::RecoveryOffer> {
    session.offers.clone()
}

#[tauri::command]
pub fn connections(ssh_config: String) -> Result<Vec<ImportedHost>, Error> {
    Ok(remote::parse_ssh_config(&ssh_config))
}

#[tauri::command]
pub fn ssh_secret_service() -> String {
    secrets::KEYRING_SERVICE.to_string()
}

#[tauri::command]
pub fn save_ssh_secret(account: String, secret: String) -> Result<SecretStatus, Error> {
    KeyringStore.set(&account, &secret)?;
    Ok(SecretStatus { saved: true })
}

#[tauri::command]
pub fn ssh_secret_status(account: String) -> Result<SecretStatus, Error> {
    secrets::status(&KeyringStore, &account)
}

#[tauri::command]
pub fn delete_ssh_secret(account: String) -> Result<(), Error> {
    KeyringStore.delete(&account)
}

#[tauri::command]
pub fn host_key_status(
    db_path: String,
    known_hosts_path: String,
    host: String,
    port: u16,
    fingerprint: String,
) -> Result<String, Error> {
    let db = exec::open_db(Path::new(&db_path))?;
    let status = remote::host_key_with_file(&db, Path::new(&known_hosts_path), &host, port, &fingerprint)?;
    remote::reject_if_host_key_changed(status, &fingerprint)?;
    Ok(match status {
        remote::HostKeyStatus::New => "new",
        remote::HostKeyStatus::Match => "match",
        remote::HostKeyStatus::Changed => "changed",
    }
    .to_string())
}

#[tauri::command]
pub fn clear_host_fingerprint(db_path: String, host: String, port: u16) -> Result<(), Error> {
    let db = exec::open_db(Path::new(&db_path))?;
    remote::clear_fingerprint(&db, &host, port)
}

#[tauri::command]
pub fn trust_host_fingerprint(db_path: String, host: String, port: u16, fingerprint: String) -> Result<(), Error> {
    let db = exec::open_db(Path::new(&db_path))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    remote::trust_host_key(&db, &host, port, &fingerprint, now)
}

#[tauri::command]
pub fn remote_backup_path(home: String, exec_id: String) -> Result<String, Error> {
    sftp::remote_backup_dir(&home, &exec_id)
}

#[tauri::command]
pub fn remote_rename_plan(posix_rename: bool) -> Vec<String> {
    sftp::rename_steps(posix_rename).iter().map(|s| (*s).to_string()).collect()
}

#[tauri::command]
pub fn probe_remote(mut req: ProbeRequest) -> Result<ProbeReport, Error> {
    // 界面带上来的口令一律丢掉。连接时只从钥匙串按连接 id 读取。
    req.secret.clear();
    if let Some(account) = req.account.clone() {
        if let Some(secret) = KeyringStore.get(&account)? {
            req.secret = secret;
        }
    }
    sftp::probe_connection(&mut req)
}
