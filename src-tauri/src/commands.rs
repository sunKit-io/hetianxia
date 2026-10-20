use hetianxia_core::error::{Error, ErrorCode};
use tauri::Manager;
use hetianxia_core::exec::{self, ExecOpts, ExecReport, HistoryRow, ResumeChoice, ResumeOutcome, RollbackChoice};
use hetianxia_core::model::ExecPlan;
use hetianxia_core::merge::{self, Decision, Rendered};
use hetianxia_core::pathutil::Ignore;
use hetianxia_core::remote::{self, ImportedHost};
use hetianxia_core::scan::{self, ScanOptions, ScanOutput};
use hetianxia_core::secrets::{self, KeyringStore, SecretStatus, SecretStore};
use hetianxia_core::connections::{self, SavedConnection};
use hetianxia_core::remote_write::{self, RemoteWritePrompt, WritePromptInput};
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

pub fn execute_in(db_path: &Path, root: &Path, backup_root: &Path, mut plan: ExecPlan, allow_readonly: bool) -> Result<ExecReport, Error> {
    remember_project_root(db_path, root);
    plan.project_root = root.display().to_string();
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

/// 按 exec_id 取 journal.root，不读壳层当前项目。
pub fn rollback_recorded(db_path: &Path, exec_id: &str, choice: RollbackChoice) -> Result<ExecReport, Error> {
    let root = exec::stored_project_root(&exec::open_db(db_path)?, exec_id)?;
    rollback_in(db_path, &root, exec_id, choice)
}

pub fn resume_recorded(
    db_path: &Path,
    backup_root: &Path,
    exec_id: &str,
    choice: ResumeChoice,
) -> Result<ResumeOutcome, Error> {
    let root = exec::stored_project_root(&exec::open_db(db_path)?, exec_id)?;
    let db = exec::open_db(db_path)?;
    let fsys = LocalFs::new(root);
    exec::resume(
        &db,
        &fsys,
        exec_id,
        choice,
        &local_opts(backup_root.display().to_string(), false),
        hetianxia_core::fault::FaultPlan::default(),
    )
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
    ready(&session)?;
    reject_finished(&session.db_path, &exec_id, true)?;
    rollback_recorded(&session.db_path, &exec_id, choice)
}

#[tauri::command]
pub fn resume_run(
    session: tauri::State<crate::Session>,
    exec_id: String,
    choice: ResumeChoice,
) -> Result<ResumeOutcome, Error> {
    ready(&session)?;
    reject_finished(&session.db_path, &exec_id, false)?;
    resume_recorded(&session.db_path, &session.backup_root, &exec_id, choice)
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
    let home = std::env::var_os("HOME").filter(|value| !value.is_empty()).map(PathBuf::from);
    Ok(remote::parse_ssh_config_with_home(&ssh_config, home.as_deref()))
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

pub fn host_key_status_at(
    db_path: &Path,
    known_hosts_path: &Path,
    host: &str,
    port: u16,
    fingerprint: &str,
) -> Result<String, Error> {
    let db = exec::open_db(db_path)?;
    let status = remote::host_key_with_file(&db, known_hosts_path, host, port, fingerprint)?;
    remote::reject_if_host_key_changed(status, fingerprint)?;
    Ok(match status {
        remote::HostKeyStatus::New => "new",
        remote::HostKeyStatus::Match => "match",
        remote::HostKeyStatus::Changed => "changed",
    }
    .to_string())
}

#[tauri::command]
pub fn host_key_status(
    session: tauri::State<crate::Session>,
    known_hosts_path: String,
    host: String,
    port: u16,
    fingerprint: String,
) -> Result<String, Error> {
    ready(&session)?;
    host_key_status_at(&session.db_path, Path::new(&known_hosts_path), &host, port, &fingerprint)
}

pub fn clear_host_fingerprint_at(db_path: &Path, host: &str, port: u16) -> Result<(), Error> {
    let db = exec::open_db(db_path)?;
    remote::clear_fingerprint(&db, host, port)
}

#[tauri::command]
pub fn clear_host_fingerprint(session: tauri::State<crate::Session>, host: String, port: u16) -> Result<(), Error> {
    ready(&session)?;
    clear_host_fingerprint_at(&session.db_path, &host, port)
}

pub fn trust_host_fingerprint_at(db_path: &Path, host: &str, port: u16, fingerprint: &str) -> Result<(), Error> {
    let db = exec::open_db(db_path)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    remote::trust_host_key(&db, host, port, fingerprint, now)
}

#[tauri::command]
#[allow(dead_code)]
pub fn trust_host_fingerprint(
    session: tauri::State<crate::Session>,
    host: String,
    port: u16,
    fingerprint: String,
) -> Result<(), Error> {
    ready(&session)?;
    trust_host_fingerprint_at(&session.db_path, &host, port, &fingerprint)
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
pub fn app_bak_dir(session: tauri::State<crate::Session>) -> Result<String, Error> {
    ready(&session)?;
    if !session.backup_root.is_absolute() {
        return Err(Error::new(ErrorCode::InvalidState, "本机备份目录还没准备好"));
    }
    Ok(session.backup_root.display().to_string())
}

fn ready_db(session: &crate::Session) -> Result<(), Error> {
    ready(session)?;
    if !session.db_path.is_absolute() {
        return Err(Error::new(ErrorCode::InvalidState, "本机数据库还没准备好"));
    }
    Ok(())
}

#[tauri::command]
pub fn list_connections(session: tauri::State<crate::Session>) -> Result<Vec<SavedConnection>, Error> {
    ready_db(&session)?;
    let db = exec::open_db(&session.db_path)?;
    connections::list(&db)
}

#[tauri::command]
pub fn save_connection(session: tauri::State<crate::Session>, conn: SavedConnection) -> Result<SavedConnection, Error> {
    ready_db(&session)?;
    let db = exec::open_db(&session.db_path)?;
    let saved = connections::save(&db, &conn)?;
    if saved.auth == "agent" {
        let _ = KeyringStore.delete(&saved.id);
    }
    Ok(saved)
}

#[tauri::command]
pub fn delete_connection(session: tauri::State<crate::Session>, id: String) -> Result<(), Error> {
    ready_db(&session)?;
    let db = exec::open_db(&session.db_path)?;
    connections::delete(&db, &id)?;
    let _ = KeyringStore.delete(&id);
    Ok(())
}

#[tauri::command]
pub fn clear_connection_fingerprint(session: tauri::State<crate::Session>, id: String) -> Result<(), Error> {
    ready_db(&session)?;
    let db = exec::open_db(&session.db_path)?;
    let saved = connections::get(&db, &id)?;
    remote::clear_fingerprint(&db, &saved.host, saved.port)?;
    connections::set_fingerprint(&db, &id, "")
}

#[tauri::command]
pub fn test_connection(
    app: tauri::AppHandle,
    session: tauri::State<crate::Session>,
    id: String,
    accept_new: bool,
    confirmed_fingerprint: Option<String>,
) -> Result<ProbeReport, Error> {
    ready_db(&session)?;
    let db = exec::open_db(&session.db_path)?;
    let saved = connections::get(&db, &id)?;
    if saved.auth == "agent" {
        return Err(Error::new(ErrorCode::Unsupported, "ssh-agent 这一版还不能用来连接"));
    }
    let confirmed = confirmed_fingerprint.as_deref().map(str::trim).filter(|value| !value.is_empty()).map(str::to_string);
    if accept_new && confirmed.is_none() {
        return Err(Error::new(ErrorCode::InvalidState, "确认新指纹时必须带上看到的指纹"));
    }
    let app_data = app.path().app_data_dir().map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?;
    let mut probe = probe_for_saved(&saved, &app_data, &session, accept_new, confirmed)?;
    let report = sftp::probe_connection(&mut probe)?;
    if report.auth_ok && !report.fingerprint.is_empty() {
        connections::set_fingerprint(&db, &saved.id, &report.fingerprint)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        remote::trust_host_key(&db, &saved.host, saved.port, &report.fingerprint, now)?;
    }
    Ok(report)
}

fn probe_for_saved(
    saved: &SavedConnection,
    app_data: &std::path::Path,
    session: &crate::Session,
    accept_new: bool,
    confirmed_fingerprint: Option<String>,
) -> Result<ProbeRequest, Error> {
    let mut probe = ProbeRequest {
        host: saved.host.clone(),
        port: saved.port,
        user: saved.user.clone(),
        proxy_jump: None,
        auth: saved.auth.clone(),
        secret: String::new(),
        account: Some(saved.id.clone()),
        key_path: if saved.key_path.is_empty() { None } else { Some(saved.key_path.clone()) },
        known_hosts_path: app_data.join("known_hosts").display().to_string(),
        db_path: session.db_path.display().to_string(),
        accept_new,
        confirmed_fingerprint,
        strict_fingerprint: None,
    };
    probe.secret.clear();
    if saved.auth != "agent" {
        if let Some(secret) = KeyringStore.get(&saved.id)? {
            probe.secret = secret;
        }
    }
    Ok(probe)
}

const SSH_CONFIG_EMPTY: &str = "没有在 ~/.ssh/config 里找到可导入的主机";

#[tauri::command]
pub fn read_ssh_config() -> Result<Vec<ImportedHost>, Error> {
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| Error::new(ErrorCode::NotFound, SSH_CONFIG_EMPTY))?;
    let path = home.join(".ssh").join("config");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(_) => return Err(Error::new(ErrorCode::NotFound, SSH_CONFIG_EMPTY)),
    };
    let hosts = connections(text)?;
    if hosts.is_empty() {
        return Err(Error::new(ErrorCode::NotFound, SSH_CONFIG_EMPTY));
    }
    Ok(hosts)
}

#[tauri::command]
pub fn remote_write_prompt(
    app: tauri::AppHandle,
    session: tauri::State<crate::Session>,
    connection_id: String,
    remote_path: String,
    content: Vec<u8>,
    content_sha256: Option<String>,
) -> Result<RemoteWritePrompt, Error> {
    ready(&session)?;
    let app_data = app.path().app_data_dir().map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?;
    if crate::app_backup_root(&app_data) != session.backup_root {
        return Err(Error::new(ErrorCode::InvalidState, "备份目录和本次运行不一致"));
    }
    let db = exec::open_db(&session.db_path)?;
    let saved = connections::get(&db, &connection_id)?;
    if saved.auth == "agent" {
        return Err(Error::new(ErrorCode::Unsupported, "ssh-agent 这一版还不能用来连接"));
    }
    let key_path = if saved.key_path.is_empty() { None } else { Some(saved.key_path.clone()) };
    let remote_path = connections::path_inside_remote_root(saved.remote_root.as_deref(), &remote_path)?;
    let input = WritePromptInput {
        host: &saved.host,
        port: saved.port,
        user: &saved.user,
        auth: &saved.auth,
        fingerprint: &saved.fingerprint,
        remote_path: &remote_path,
        content: &content,
        content_sha256: content_sha256.as_deref(),
        account: Some(saved.id.as_str()),
        key_path: key_path.as_deref(),
    };
    remote_write::issue_write_prompt(&app_data, &input)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWriteOutcome {
    pub exec_id: String,
    pub remote_path: String,
    pub backup_path: Option<String>,
}

#[tauri::command]
pub fn commit_remote_overwrite(
    app: tauri::AppHandle,
    session: tauri::State<crate::Session>,
    exec_id: String,
) -> Result<RemoteWriteOutcome, Error> {
    ready(&session)?;
    let app_data = app.path().app_data_dir().map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?;
    if crate::app_backup_root(&app_data) != session.backup_root {
        return Err(Error::new(ErrorCode::InvalidState, "备份目录和本次运行不一致"));
    }
    let stored = remote_write::borrow_granted_connection(&exec_id, remote_write::now_ms())?;
    let mut probe = ProbeRequest {
        host: stored.host.clone(),
        port: stored.port,
        user: stored.user.clone(),
        proxy_jump: None,
        auth: stored.auth.clone(),
        secret: String::new(),
        account: stored.account.clone(),
        key_path: stored.key_path.clone(),
        known_hosts_path: app_data.join("known_hosts").display().to_string(),
        db_path: session.db_path.display().to_string(),
        accept_new: !stored.fingerprint.is_empty(),
        confirmed_fingerprint: if stored.fingerprint.is_empty() { None } else { Some(stored.fingerprint) },
        strict_fingerprint: None,
    };
    probe.secret.clear();
    if probe.auth != "agent" {
        if let Some(account) = probe.account.clone() {
            if let Some(secret) = KeyringStore.get(&account)? {
                probe.secret = secret;
            }
        }
    }
    let remote_path = stored.remote_path.clone();
    let committed = sftp::RemoteSession::commit_overwrite_blocking(&mut probe, &exec_id, app_data)?;
    let remote_write::CommitResult::Written(staged) = committed;
    Ok(RemoteWriteOutcome {
        exec_id,
        remote_path,
        backup_path: staged.backup_file.map(|path| path.display().to_string()),
    })
}

