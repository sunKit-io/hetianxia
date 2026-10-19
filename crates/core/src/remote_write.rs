//! 远程写入的第一步：先在壳层备份目录留下原文件和清单，再经临时文件改名覆盖。
//! 事务目录是 `app_data_dir()/bak/<id>/`，和本地引擎的 `backup_root/<id>` 是同一个根。
//! `app_data_dir()` 由 Tauri 壳按 bundle id 解析，这里不再拼产品名目录。
//! 取消时不碰远端，也不创建这个目录。

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::brand::{self, TEMP_TAG};
use crate::error::{Error, ErrorCode, Result};
use crate::hashutil::{self, sha256_hex};
use crate::remote::RuntimeLog;
use crate::sftp::{self, RemoteNames};

pub const DIALOG_TITLE: &str = "远程写入暂不支持回滚";
pub const CANCEL_LABEL: &str = "取消";
pub const PROCEED_LABEL: &str = "仍然写入";
pub const VERIFY_FAILED_TITLE: &str = "远端文件写完后核对不一致";
pub const VERIFY_FAILED_BUTTON: &str = "好";
/// 确认对话框发出的写入许可，十分钟内只能用一次。
pub const GRANT_TTL_MS: i64 = 10 * 60 * 1000;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWritePrompt {
    pub title: String,
    pub body: String,
    pub backup_dir: String,
    pub cancel_label: String,
    pub proceed_label: String,
    pub default_button: String,
    pub exec_id: String,
}

#[derive(Debug, Clone)]
pub struct RemoteWriteRequest {
    pub exec_id: String,
    pub host: String,
    pub port: u16,
    pub remote_path: String,
    pub bytes: Vec<u8>,
    /// Tauri `app.path().app_data_dir()` 的绝对路径。
    pub app_data_dir: PathBuf,
    pub posix_rename: bool,
    pub index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedBackup {
    pub backup_dir: PathBuf,
    /// 覆盖写入时，原文件落在这个绝对路径。新建文件没有原件。
    pub backup_file: Option<PathBuf>,
    pub op: String,
    pub hash: Option<String>,
}

/// 核对失败时的对话框。两条路径都由壳层在运行时填入。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerifyFailureDialog {
    pub title: String,
    pub body: String,
    pub code: String,
    pub remote_path: String,
    pub backup_path: String,
    pub button: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitResult {
    Written(StagedBackup),
}

struct Grant {
    host: String,
    port: u16,
    remote_path: String,
    expires_at_ms: i64,
}

fn grants() -> &'static Mutex<HashMap<String, Grant>> {
    static GRANTS: std::sync::LazyLock<Mutex<HashMap<String, Grant>>> = std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &GRANTS
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub trait RemoteBytes {
    fn exists(&mut self, path: &str) -> Result<bool>;
    fn read(&mut self, path: &str) -> Result<Vec<u8>>;
    fn write_new(&mut self, path: &str, bytes: &[u8]) -> Result<()>;
    fn rename(&mut self, from: &str, to: &str) -> Result<()>;
    fn remove(&mut self, path: &str) -> Result<()>;
    fn paths(&self) -> Vec<String>;
}

/// 壳层传下来的备份根：`app_data_dir()/bak`。本地 `ExecOpts.backup_root` 用同一个目录。
pub fn shell_backup_root(app_data_dir: &Path) -> Result<PathBuf> {
    if !app_data_dir.is_absolute() {
        return Err(Error::new(ErrorCode::InvalidLocation, "应用数据目录必须是绝对路径"));
    }
    Ok(app_data_dir.join("bak"))
}

/// `app_data_dir()/bak/<id>/`。macOS 上 `app_data_dir` 是
/// `$HOME/Library/Application Support/com.hetianxia.app`。
pub fn tx_backup_dir(app_data_dir: &Path, exec_id: &str) -> Result<PathBuf> {
    validate_exec_id(exec_id)?;
    Ok(shell_backup_root(app_data_dir)?.join(exec_id))
}

pub fn dialog_body(backup_dir: &Path) -> String {
    format!(
        "写入前会把远端原文件备份到本机（{}），出问题可以从这里手工恢复。",
        backup_dir.display()
    )
}

pub fn prompt_for(app_data_dir: &Path, exec_id: &str) -> Result<RemoteWritePrompt> {
    let dir = tx_backup_dir(app_data_dir, exec_id)?;
    Ok(prompt_at(&dir, exec_id))
}

fn prompt_at(dir: &Path, exec_id: &str) -> RemoteWritePrompt {
    RemoteWritePrompt {
        title: DIALOG_TITLE.into(),
        body: dialog_body(dir),
        backup_dir: dir.display().to_string(),
        cancel_label: CANCEL_LABEL.into(),
        proceed_label: PROCEED_LABEL.into(),
        default_button: CANCEL_LABEL.into(),
        exec_id: exec_id.to_string(),
    }
}

/// 壳层发出一次性写入许可，并返回带真实备份目录的确认对话框。
pub fn issue_write_prompt(app_data_dir: &Path, host: &str, port: u16, remote_path: &str) -> Result<RemoteWritePrompt> {
    let exec_id = uuid::Uuid::new_v4().simple().to_string();
    issue_prompt_for(app_data_dir, &exec_id, host, port, remote_path, now_ms())
}

pub fn issue_prompt_for(
    app_data_dir: &Path,
    exec_id: &str,
    host: &str,
    port: u16,
    remote_path: &str,
    now: i64,
) -> Result<RemoteWritePrompt> {
    let prompt = prompt_for(app_data_dir, exec_id)?;
    remember_grant(exec_id, host, port, remote_path, now)?;
    Ok(prompt)
}

fn remember_grant(exec_id: &str, host: &str, port: u16, remote_path: &str, now: i64) -> Result<()> {
    if host.is_empty() || remote_path.is_empty() {
        return Err(Error::new(ErrorCode::InvalidLocation, "远程主机或路径为空"));
    }
    let mut map = grants().lock().map_err(|_| Error::new(ErrorCode::Internal, "写入确认锁被占用"))?;
    map.insert(
        exec_id.to_string(),
        Grant {
            host: host.to_string(),
            port,
            remote_path: remote_path.to_string(),
            expires_at_ms: now.saturating_add(GRANT_TTL_MS),
        },
    );
    Ok(())
}

/// 用掉 `remote_write_prompt` 发出的许可。过期、主机或路径不一致、用过一次，都拒绝。
pub fn authorize_commit(exec_id: &str, host: &str, port: u16, remote_path: &str, now: i64) -> Result<()> {
    let mut map = grants().lock().map_err(|_| Error::new(ErrorCode::Internal, "写入确认锁被占用"))?;
    let Some(grant) = map.get(exec_id) else {
        return Err(Error::new(ErrorCode::InvalidState, "没有这次写入的确认"));
    };
    if now > grant.expires_at_ms {
        map.remove(exec_id);
        return Err(Error::new(ErrorCode::InvalidState, "写入确认已过期"));
    }
    if grant.host != host || grant.port != port || grant.remote_path != remote_path {
        return Err(Error::new(ErrorCode::InvalidState, "写入确认和主机或路径不一致"));
    }
    map.remove(exec_id);
    Ok(())
}

/// 把远端原文件落到 `bak/<id>/files/` 并写上清单。必须在远端改名之前调用。
pub fn stage_before_write(req: &RemoteWriteRequest, original: Option<&[u8]>) -> Result<StagedBackup> {
    let backup_dir = tx_backup_dir(&req.app_data_dir, &req.exec_id)?;
    let rel = local_rel(&req.remote_path)?;
    let op = if original.is_some() { "remote_overwrite" } else { "remote_create" };
    let hash = if let Some(bytes) = original {
        let dest = backup_dir.join("files").join(&rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&dest)
            .map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?;
        file.write_all(bytes).map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?;
        file.sync_all().map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?;
        drop(file);
        let got = hashutil::hash_file_sha256(&dest).map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?;
        let expect = sha256_hex(bytes);
        if got != expect {
            let _ = fs::remove_file(&dest);
            return Err(Error::new(
                ErrorCode::VerifyFailed,
                format!("{rel} 的备份和原文件哈希不一致，原文件未改"),
            ));
        }
        Some(got)
    } else {
        fs::create_dir_all(backup_dir.join("files")).map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?;
        None
    };
    let backup_file = hash.as_ref().map(|_| backup_dir.join("files").join(&rel));
    let backup_rel = backup_file.as_ref().map(|_| format!("files/{rel}"));
    let entry = serde_json::json!({
        "execId": req.exec_id,
        "projectKey": req.host,
        "index": req.index,
        "path": rel,
        "backup": backup_rel,
        "hash": hash,
        "op": op,
        "host": req.host,
        "port": req.port,
        "remotePath": req.remote_path,
        "backupPath": backup_file.as_ref().map(|path| path.display().to_string()),
    });
    append_manifest(&backup_dir, &entry)?;
    Ok(StagedBackup { backup_dir, backup_file, op: op.into(), hash })
}

pub fn verify_failure_body(remote_path: &str, backup_path: &str) -> String {
    format!("{remote_path} 写入后的内容和预期不一致。写入前的原文件已备份到本机（{backup_path}），可以从这里手工恢复。")
}

pub fn verify_failure_dialog(remote_path: &str, backup_file: &Path) -> VerifyFailureDialog {
    let backup_path = backup_file.display().to_string();
    VerifyFailureDialog {
        title: VERIFY_FAILED_TITLE.into(),
        body: verify_failure_body(remote_path, &backup_path),
        code: "E_VERIFY_FAILED".into(),
        remote_path: remote_path.to_string(),
        backup_path,
        button: VERIFY_FAILED_BUTTON.into(),
    }
}

pub fn verify_after_rename(got: &[u8], expect: &[u8], dest: &str, backup_file: &Path) -> Result<()> {
    if sha256_hex(got) == sha256_hex(expect) {
        return Ok(());
    }
    let dialog = verify_failure_dialog(dest, backup_file);
    Err(Error::new(ErrorCode::VerifyFailed, dialog.body).with_paths(vec![dialog.backup_path]))
}

pub fn remote_temp_path(remote_path: &str) -> Result<String> {
    let (dir, name) = split_remote(remote_path)?;
    let temp = brand::temp_file_name(name, &uuid::Uuid::new_v4().simple().to_string());
    Ok(join_remote(dir, &temp))
}

pub fn remote_backup_name(remote_path: &str) -> Result<String> {
    let (dir, name) = split_remote(remote_path)?;
    let bak = format!(".{name}{TEMP_TAG}bak-{}", uuid::Uuid::new_v4().simple());
    Ok(join_remote(dir, &bak))
}

pub fn commit_remote_write(
    store: &mut dyn RemoteBytes,
    log: &mut RuntimeLog,
    req: &RemoteWriteRequest,
) -> Result<CommitResult> {
    authorize_commit(&req.exec_id, &req.host, req.port, &req.remote_path, now_ms())?;
    let dest = req.remote_path.clone();
    let original = if store.exists(&dest)? { Some(store.read(&dest)?) } else { None };
    let staged = stage_before_write(req, original.as_deref())?;
    let temp = remote_temp_path(&dest)?;
    let backup = remote_backup_name(&dest)?;
    if let Err(err) = store.write_new(&temp, &req.bytes) {
        let _ = remove_if_present(store, &temp);
        return Err(err);
    }
    let held = {
        let mut names = Names { inner: store };
        sftp::apply_rename(&mut names, log, req.posix_rename, &temp, &dest, &backup)?
    };
    let got = store.read(&dest)?;
    let anchor = staged.backup_file.clone().unwrap_or_else(|| staged.backup_dir.clone());
    verify_after_rename(&got, &req.bytes, &dest, &anchor)?;
    if held {
        let mut names = Names { inner: store };
        sftp::discard_verified_backup(&mut names, &backup)?;
    }
    if store.paths().iter().any(|path| path.contains(TEMP_TAG)) {
        return Err(Error::new(ErrorCode::Io, "远程临时文件还在"));
    }
    Ok(CommitResult::Written(staged))
}

fn remove_if_present(store: &mut dyn RemoteBytes, path: &str) -> Result<()> {
    if store.exists(path)? {
        store.remove(path)?;
    }
    Ok(())
}

struct Names<'a> {
    inner: &'a mut dyn RemoteBytes,
}

impl RemoteNames for Names<'_> {
    fn exists(&mut self, path: &str) -> Result<bool> {
        self.inner.exists(path)
    }
    fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        self.inner.rename(from, to)
    }
    fn remove(&mut self, path: &str) -> Result<()> {
        self.inner.remove(path)
    }
}

fn append_manifest(backup: &Path, entry: &serde_json::Value) -> Result<()> {
    fs::create_dir_all(backup).map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?;
    let path = backup.join("manifest.json");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?;
    writeln!(file, "{entry}").map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?;
    file.sync_all().map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?;
    Ok(())
}

fn validate_exec_id(exec_id: &str) -> Result<()> {
    if exec_id.is_empty()
        || exec_id.contains('/')
        || exec_id.contains('\\')
        || exec_id.contains("..")
        || exec_id.contains('\0')
    {
        return Err(Error::new(ErrorCode::InvalidLocation, "备份编号不合法"));
    }
    Ok(())
}

fn local_rel(remote_path: &str) -> Result<String> {
    let (dir, name) = split_remote(remote_path)?;
    let joined = join_remote(dir, name);
    let rel = joined.trim_start_matches('/');
    if rel.is_empty() {
        return Err(Error::new(ErrorCode::InvalidLocation, "远程路径不合法"));
    }
    Ok(rel.to_string())
}

fn split_remote(path: &str) -> Result<(&str, &str)> {
    if path.is_empty() || path.contains('\0') || path.contains('\\') {
        return Err(Error::new(ErrorCode::InvalidLocation, "远程路径不合法"));
    }
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(Error::new(ErrorCode::InvalidLocation, "远程路径不合法"));
    }
    let mut parts = trimmed.split('/');
    if trimmed.starts_with('/') {
        parts.next();
    }
    if parts.clone().any(|part| part.is_empty() || part == "." || part == "..") {
        return Err(Error::new(ErrorCode::InvalidLocation, "远程路径不合法"));
    }
    match trimmed.rfind('/') {
        Some(index) => Ok((&trimmed[..index], &trimmed[index + 1..])),
        None => Ok(("", trimmed)),
    }
}

fn join_remote(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct Mem {
        files: BTreeMap<String, Vec<u8>>,
        calls: u32,
        fail_write: bool,
        tamper_verify: bool,
        fail_replace: bool,
        fail_restore: bool,
    }

    impl Mem {
        fn new(files: BTreeMap<String, Vec<u8>>) -> Self {
            Self {
                files,
                calls: 0,
                fail_write: false,
                tamper_verify: false,
                fail_replace: false,
                fail_restore: false,
            }
        }
    }

    impl RemoteBytes for Mem {
        fn exists(&mut self, path: &str) -> Result<bool> {
            self.calls += 1;
            Ok(self.files.contains_key(path))
        }
        fn read(&mut self, path: &str) -> Result<Vec<u8>> {
            self.calls += 1;
            let bytes = self.files.get(path).cloned().ok_or_else(|| Error::new(ErrorCode::NotFound, "远程文件不存在"))?;
            if self.tamper_verify && bytes == b"new-bytes" {
                return Ok(b"tampered".to_vec());
            }
            Ok(bytes)
        }
        fn write_new(&mut self, path: &str, bytes: &[u8]) -> Result<()> {
            self.calls += 1;
            if self.fail_write {
                return Err(Error::new(ErrorCode::Io, "写入中途失败"));
            }
            if self.files.contains_key(path) {
                return Err(Error::new(ErrorCode::Io, "临时文件已存在"));
            }
            self.files.insert(path.to_string(), bytes.to_vec());
            Ok(())
        }
        fn rename(&mut self, from: &str, to: &str) -> Result<()> {
            self.calls += 1;
            if self.fail_replace && from.contains(TEMP_TAG) && !from.contains("bak-") {
                return Err(Error::new(ErrorCode::Io, "改名失败"));
            }
            if self.fail_restore && from.contains("bak-") {
                return Err(Error::new(ErrorCode::Io, "还原失败"));
            }
            let bytes = self.files.remove(from).ok_or_else(|| Error::new(ErrorCode::NotFound, "远程文件不存在"))?;
            self.files.insert(to.to_string(), bytes);
            Ok(())
        }
        fn remove(&mut self, path: &str) -> Result<()> {
            self.calls += 1;
            self.files.remove(path);
            Ok(())
        }
        fn paths(&self) -> Vec<String> {
            self.files.keys().cloned().collect()
        }
    }

    fn request(app_data: &Path, exec_id: &str, posix: bool) -> RemoteWriteRequest {
        RemoteWriteRequest {
            exec_id: exec_id.into(),
            host: "build-01".into(),
            port: 22,
            remote_path: "/opt/gateway/app.conf".into(),
            bytes: b"new-bytes".to_vec(),
            app_data_dir: app_data.to_path_buf(),
            posix_rename: posix,
            index: 1,
        }
    }

    fn grant(app_data: &Path, exec_id: &str) {
        issue_prompt_for(app_data, exec_id, "build-01", 22, "/opt/gateway/app.conf", now_ms()).unwrap();
    }

    #[test]
    fn prompt_uses_the_shell_backup_directory() {
        let home = tempfile::tempdir().unwrap();
        let app_data = home.path().join("app-data");
        let prompt = prompt_for(&app_data, "tx-100").unwrap();
        let dir = tx_backup_dir(&app_data, "tx-100").unwrap();
        assert_eq!(prompt.backup_dir, dir.display().to_string());
        assert_eq!(dir, app_data.join("bak").join("tx-100"));
        assert_eq!(prompt.title, "远程写入暂不支持回滚");
        assert_eq!(
            prompt.body,
            format!("写入前会把远端原文件备份到本机（{}），出问题可以从这里手工恢复。", dir.display())
        );
        assert_eq!(prompt.cancel_label, "取消");
        assert_eq!(prompt.proceed_label, "仍然写入");
        assert_eq!(prompt.default_button, "取消");
        assert!(!prompt.body.contains("~/"));
        assert!(!prompt.body.contains("HeTianXia"));
    }

    #[test]
    fn macos_path_is_bundle_id_under_application_support() {
        let app_data = PathBuf::from("/Users/me/Library/Application Support").join(brand::BUNDLE_ID);
        let dir = tx_backup_dir(&app_data, "tx-100").unwrap();
        assert_eq!(
            dir,
            PathBuf::from("/Users/me/Library/Application Support/com.hetianxia.app/bak/tx-100")
        );
        let body = dialog_body(&dir);
        assert!(body.contains("/Users/me/Library/Application Support/com.hetianxia.app/bak/tx-100"));
        assert!(!body.contains("HeTianXia"));
    }

    #[test]
    fn missing_grant_writes_nothing() {
        let home = tempfile::tempdir().unwrap();
        let mut mem = Mem::new(BTreeMap::from([("/opt/gateway/app.conf".into(), b"old-bytes".to_vec())]));
        let before = mem.files.clone();
        let mut log = RuntimeLog::default();
        let err = commit_remote_write(&mut mem, &mut log, &request(home.path(), "tx-missing", true)).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidState);
        assert_eq!(err.code_str(), "E_INVALID_STATE");
        assert_eq!(mem.calls, 0);
        assert_eq!(mem.files, before);
        assert!(log.lines.is_empty());
        assert!(!tx_backup_dir(home.path(), "tx-missing").unwrap().exists());
    }

    #[test]
    fn grant_must_match_host_path_and_is_one_time() {
        let home = tempfile::tempdir().unwrap();
        issue_prompt_for(home.path(), "tx-match", "build-01", 22, "/opt/gateway/app.conf", now_ms()).unwrap();
        let mut wrong = request(home.path(), "tx-match", true);
        wrong.remote_path = "/opt/other.conf".into();
        let mut mem = Mem::new(BTreeMap::from([("/opt/gateway/app.conf".into(), b"old-bytes".to_vec())]));
        let mut log = RuntimeLog::default();
        let err = commit_remote_write(&mut mem, &mut log, &wrong).unwrap_err();
        assert_eq!(err.message, "写入确认和主机或路径不一致");
        assert_eq!(mem.calls, 0);
        let mut mem = Mem::new(BTreeMap::from([("/opt/gateway/app.conf".into(), b"old-bytes".to_vec())]));
        commit_remote_write(&mut mem, &mut log, &request(home.path(), "tx-match", true)).unwrap();
        let err = commit_remote_write(&mut mem, &mut log, &request(home.path(), "tx-match", true)).unwrap_err();
        assert_eq!(err.message, "没有这次写入的确认");
    }

    #[test]
    fn expired_grant_is_refused() {
        let home = tempfile::tempdir().unwrap();
        issue_prompt_for(home.path(), "tx-expired", "build-01", 22, "/opt/gateway/app.conf", 0).unwrap();
        let mut mem = Mem::new(BTreeMap::new());
        let mut log = RuntimeLog::default();
        let err = commit_remote_write(&mut mem, &mut log, &request(home.path(), "tx-expired", true)).unwrap_err();
        assert_eq!(err.message, "写入确认已过期");
        assert_eq!(mem.calls, 0);
    }

    #[test]
    fn successful_write_backs_up_and_leaves_no_temp() {
        for posix in [true, false] {
            let home = tempfile::tempdir().unwrap();
            let exec_id = if posix { "tx-posix" } else { "tx-fallback" };
            grant(home.path(), exec_id);
            let mut mem = Mem::new(BTreeMap::from([("/opt/gateway/app.conf".into(), b"old-bytes".to_vec())]));
            let mut log = RuntimeLog::default();
            let prompt = prompt_for(home.path(), exec_id).unwrap();
            let result = commit_remote_write(&mut mem, &mut log, &request(home.path(), exec_id, posix)).unwrap();
            let CommitResult::Written(staged) = result;
            assert_eq!(staged.backup_dir.display().to_string(), prompt.backup_dir);
            assert_eq!(staged.op, "remote_overwrite");
            let expect = sha256_hex(b"old-bytes");
            assert_eq!(staged.hash.as_deref(), Some(expect.as_str()));
            let saved = staged.backup_dir.join("files").join("opt/gateway/app.conf");
            assert_eq!(fs::read(&saved).unwrap(), b"old-bytes");
            assert_eq!(hashutil::hash_file_sha256(&saved).unwrap(), expect);
            let manifest = fs::read_to_string(staged.backup_dir.join("manifest.json")).unwrap();
            let entry: serde_json::Value = serde_json::from_str(manifest.lines().next().unwrap()).unwrap();
            assert_eq!(entry["op"], "remote_overwrite");
            assert_eq!(entry["host"], "build-01");
            assert_eq!(entry["port"], 22);
            assert_eq!(entry["remotePath"], "/opt/gateway/app.conf");
            assert_eq!(entry["hash"], expect);
            assert_eq!(entry["backupPath"], saved.display().to_string());
            assert_eq!(mem.files.get("/opt/gateway/app.conf").map(Vec::as_slice), Some(b"new-bytes".as_slice()));
            assert!(mem.paths().iter().all(|path| !path.contains(TEMP_TAG)));
            assert_eq!(mem.paths(), vec!["/opt/gateway/app.conf".to_string()]);
            assert!(!log.lines.is_empty());
            assert_eq!(log.lines[0].category, "sftp-rename");
        }
    }

    #[test]
    fn failed_write_leaves_the_remote_file_and_no_temp() {
        let home = tempfile::tempdir().unwrap();
        grant(home.path(), "tx-fail-write");
        let mut mem = Mem::new(BTreeMap::from([("/opt/gateway/app.conf".into(), b"old-bytes".to_vec())]));
        mem.fail_write = true;
        let mut log = RuntimeLog::default();
        let err = commit_remote_write(&mut mem, &mut log, &request(home.path(), "tx-fail-write", true)).unwrap_err();
        assert_eq!(err.code, ErrorCode::Io);
        assert_eq!(mem.files.get("/opt/gateway/app.conf").map(Vec::as_slice), Some(b"old-bytes".as_slice()));
        assert!(mem.paths().iter().all(|path| !path.contains(TEMP_TAG)));
        let saved = tx_backup_dir(home.path(), "tx-fail-write").unwrap().join("files").join("opt/gateway/app.conf");
        assert_eq!(fs::read(saved).unwrap(), b"old-bytes");
    }

    #[test]
    fn hash_mismatch_after_rename_names_the_local_backup() {
        let home = tempfile::tempdir().unwrap();
        grant(home.path(), "tx-verify");
        let mut mem = Mem::new(BTreeMap::from([("/opt/gateway/app.conf".into(), b"old-bytes".to_vec())]));
        mem.tamper_verify = true;
        let mut log = RuntimeLog::default();
        let err = commit_remote_write(&mut mem, &mut log, &request(home.path(), "tx-verify", false)).unwrap_err();
        let saved = tx_backup_dir(home.path(), "tx-verify").unwrap().join("files").join("opt/gateway/app.conf");
        let dialog = verify_failure_dialog("/opt/gateway/app.conf", &saved);
        assert_eq!(err.code, ErrorCode::VerifyFailed);
        assert_eq!(err.code_str(), "E_VERIFY_FAILED");
        assert_eq!(dialog.title, "远端文件写完后核对不一致");
        assert_eq!(dialog.button, "好");
        assert_eq!(err.message, dialog.body);
        assert_eq!(
            dialog.body,
            "/opt/gateway/app.conf 写入后的内容和预期不一致。写入前的原文件已备份到本机（".to_string()
                + saved.to_str().unwrap()
                + "），可以从这里手工恢复。"
        );
        assert_eq!(err.paths, vec![saved.display().to_string()]);
        assert!(mem.paths().iter().any(|path| path.contains("bak-")), "核对失败不能删掉远端 .htx-bak");
    }

    #[test]
    fn failed_second_rename_restores_the_original() {
        let home = tempfile::tempdir().unwrap();
        grant(home.path(), "tx-rename");
        let mut mem = Mem::new(BTreeMap::from([("/opt/gateway/app.conf".into(), b"old-bytes".to_vec())]));
        mem.fail_replace = true;
        let mut log = RuntimeLog::default();
        let err = commit_remote_write(&mut mem, &mut log, &request(home.path(), "tx-rename", false)).unwrap_err();
        assert_eq!(err.code, ErrorCode::Io);
        assert_eq!(mem.files.get("/opt/gateway/app.conf").map(Vec::as_slice), Some(b"old-bytes".as_slice()));
        assert!(mem.paths().iter().all(|path| !path.contains(TEMP_TAG)));
        let saved = tx_backup_dir(home.path(), "tx-rename").unwrap().join("files").join("opt/gateway/app.conf");
        assert_eq!(fs::read(saved).unwrap(), b"old-bytes");
    }

    #[test]
    fn failed_restore_does_not_delete_the_remote_bak() {
        let home = tempfile::tempdir().unwrap();
        grant(home.path(), "tx-restore");
        let mut mem = Mem::new(BTreeMap::from([("/opt/gateway/app.conf".into(), b"old-bytes".to_vec())]));
        mem.fail_replace = true;
        mem.fail_restore = true;
        let mut log = RuntimeLog::default();
        let err = commit_remote_write(&mut mem, &mut log, &request(home.path(), "tx-restore", false)).unwrap_err();
        assert_eq!(err.message, "还原失败");
        let bak = mem.paths().into_iter().find(|path| path.contains("bak-")).expect("原文件还在 .htx-bak");
        assert_eq!(mem.files.get(&bak).map(Vec::as_slice), Some(b"old-bytes".as_slice()));
        assert!(mem.paths().iter().any(|path| path.contains(TEMP_TAG) && !path.contains("bak-")));
    }
}
