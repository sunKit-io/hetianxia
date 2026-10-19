use hetianxia_core::error::{Error, ErrorCode};
use hetianxia_core::exec::{self, ExecOpts, ExecReport, HistoryRow, RollbackChoice};
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

fn remember_project_root(db_path: &str, root: &str) {
    let path = Path::new(db_path).with_file_name("project-root");
    let _ = std::fs::write(path, root);
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
    remember_project_root(&db_path, &root);
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
