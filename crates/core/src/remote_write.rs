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
    pub user: String,
    /// 发许可时算出的新内容 sha256。清单和写后核对都用这一份。
    pub content_sha256: String,
    /// Tauri `app.path().app_data_dir()` 的绝对路径。
    pub app_data_dir: PathBuf,
    pub posix_rename: bool,
    pub index: u32,
}

/// 发许可时一次性交给壳层的连接、路径和新内容。
/// `content_sha256` 有值时必须等于 `content` 的 sha256，否则不发许可。
#[derive(Debug, Clone)]
pub struct WritePromptInput<'a> {
    pub host: &'a str,
    pub port: u16,
    pub user: &'a str,
    pub auth: &'a str,
    pub fingerprint: &'a str,
    pub remote_path: &'a str,
    pub content: &'a [u8],
    pub content_sha256: Option<&'a str>,
    pub account: Option<&'a str>,
    pub key_path: Option<&'a str>,
}

/// 连接参数。提交时只按 `exec_id` 取出，不再接受前端另传的主机或路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredConnection {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: String,
    pub fingerprint: String,
    pub remote_path: String,
    pub account: Option<String>,
    pub key_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantedWrite {
    pub connection: StoredConnection,
    pub bytes: Vec<u8>,
    pub content_sha256: String,
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
    connection: StoredConnection,
    bytes: Vec<u8>,
    content_sha256: String,
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
/// 主机、端口、用户、认证方式、指纹、完整远程路径和新内容都存在这条许可里。
pub fn issue_write_prompt(app_data_dir: &Path, input: &WritePromptInput<'_>) -> Result<RemoteWritePrompt> {
    let exec_id = uuid::Uuid::new_v4().simple().to_string();
    issue_prompt_for(app_data_dir, &exec_id, input, now_ms())
}

pub fn issue_prompt_for(
    app_data_dir: &Path,
    exec_id: &str,
    input: &WritePromptInput<'_>,
    now: i64,
) -> Result<RemoteWritePrompt> {
    let prompt = prompt_for(app_data_dir, exec_id)?;
    let grant = grant_from(input, now)?;
    let mut map = grants().lock().map_err(|_| Error::new(ErrorCode::Internal, "写入确认锁被占用"))?;
    map.insert(exec_id.to_string(), grant);
    Ok(prompt)
}

fn grant_from(input: &WritePromptInput<'_>, now: i64) -> Result<Grant> {
    if input.host.is_empty() || input.user.is_empty() || input.auth.is_empty() {
        return Err(Error::new(ErrorCode::InvalidLocation, "远程主机、用户或认证方式为空"));
    }
    if input.fingerprint.trim().is_empty() {
        return Err(Error::new(ErrorCode::InvalidState, "请先测试连接并确认指纹"));
    }
    split_remote(input.remote_path)?;
    let content_sha256 = sha256_hex(input.content);
    if let Some(given) = input.content_sha256 {
        if given != content_sha256 {
            return Err(Error::new(ErrorCode::VerifyFailed, "暂存的新内容和 sha256 不一致"));
        }
    }
    Ok(Grant {
        connection: StoredConnection {
            host: input.host.to_string(),
            port: input.port,
            user: input.user.to_string(),
            auth: input.auth.to_string(),
            fingerprint: input.fingerprint.to_string(),
            remote_path: input.remote_path.to_string(),
            account: input.account.map(str::to_string),
            key_path: input.key_path.map(str::to_string),
        },
        bytes: input.content.to_vec(),
        content_sha256,
        expires_at_ms: now.saturating_add(GRANT_TTL_MS),
    })
}

fn lock_grants() -> Result<std::sync::MutexGuard<'static, HashMap<String, Grant>>> {
    grants().lock().map_err(|_| Error::new(ErrorCode::Internal, "写入确认锁被占用"))
}

fn fresh_grant<'a>(map: &'a mut HashMap<String, Grant>, exec_id: &str, now: i64) -> Result<&'a Grant> {
    let expired = match map.get(exec_id) {
        None => return Err(Error::new(ErrorCode::InvalidState, "没有这次写入的确认")),
        Some(grant) if now > grant.expires_at_ms => true,
        Some(_) => false,
    };
    if expired {
        map.remove(exec_id);
        return Err(Error::new(ErrorCode::InvalidState, "写入确认已过期"));
    }
    map.get(exec_id).ok_or_else(|| Error::new(ErrorCode::InvalidState, "没有这次写入的确认"))
}

/// 读出许可里的连接参数，不消耗。提交会先用掉许可再连接；连接失败后这条许可已经作废。
pub fn borrow_granted_connection(exec_id: &str, now: i64) -> Result<StoredConnection> {
    let mut map = lock_grants()?;
    Ok(fresh_grant(&mut map, exec_id, now)?.connection.clone())
}

/// 用掉一次性许可，返回当时存下的连接、路径和新内容。
pub fn take_granted_write(exec_id: &str, now: i64) -> Result<GrantedWrite> {
    let mut map = lock_grants()?;
    let _ = fresh_grant(&mut map, exec_id, now)?;
    let grant = map.remove(exec_id).ok_or_else(|| Error::new(ErrorCode::InvalidState, "没有这次写入的确认"))?;
    if sha256_hex(&grant.bytes) != grant.content_sha256 {
        return Err(Error::new(ErrorCode::VerifyFailed, "暂存的新内容和 sha256 不一致"));
    }
    Ok(GrantedWrite {
        connection: grant.connection,
        bytes: grant.bytes,
        content_sha256: grant.content_sha256,
    })
}

/// 连接失败时作废许可。用户必须重新确认才能再写。
pub fn void_granted_write(exec_id: &str) -> Result<()> {
    let mut map = lock_grants()?;
    map.remove(exec_id);
    Ok(())
}

pub fn grant_binds(connection: &StoredConnection, host: &str, port: u16, user: &str, remote_path: &str) -> bool {
    connection.host == host && connection.port == port && connection.user == user && connection.remote_path == remote_path
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileOutcome {
    pub remote_path: String,
    pub state: String,
    pub code: Option<String>,
    /// 这一项写成功，并且远端原来就有文件、原件已落到本机。新增文件是 false。
    pub backed_up: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteBatchReport {
    pub backup_dir: String,
    pub total: u32,
    pub failed_index: Option<u32>,
    pub failed_path: Option<String>,
    pub code: Option<String>,
    pub files: Vec<RemoteFileOutcome>,
}

#[derive(Debug, Clone)]
pub struct BatchFileInput<'a> {
    pub remote_path: &'a str,
    pub content: &'a [u8],
    pub content_sha256: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct BatchPromptInput<'a> {
    pub host: &'a str,
    pub port: u16,
    pub user: &'a str,
    pub auth: &'a str,
    pub fingerprint: &'a str,
    pub account: Option<&'a str>,
    pub key_path: Option<&'a str>,
    pub files: &'a [BatchFileInput<'a>],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBatchFile {
    pub remote_path: String,
    /// 编辑或合并后的内容。`None` 表示按扫描记录从升级包流式读取。
    pub bytes: Option<Vec<u8>>,
    pub content_sha256: String,
    pub size: u64,
    pub rel: String,
    pub source: Option<crate::model::ContentSource>,
}

/// 和执行计划同一个形状：有 `bytes` 就写这份内容，否则只带路径、哈希、大小和扫描来源。
#[derive(Debug, Clone)]
pub struct UploadFile {
    pub remote_path: String,
    pub path: String,
    pub bytes: Option<Vec<u8>>,
    pub content_hash: Option<String>,
    pub size: Option<u64>,
    pub source: Option<crate::model::ContentSource>,
}

#[derive(Debug, Clone)]
pub struct UploadBatch<'a> {
    pub host: &'a str,
    pub port: u16,
    pub user: &'a str,
    pub auth: &'a str,
    pub fingerprint: &'a str,
    pub account: Option<&'a str>,
    pub key_path: Option<&'a str>,
    pub files: &'a [UploadFile],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBatch {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: String,
    pub fingerprint: String,
    pub account: Option<String>,
    pub key_path: Option<String>,
    pub files: Vec<StoredBatchFile>,
}

struct BatchGrant {
    host: String,
    port: u16,
    user: String,
    auth: String,
    fingerprint: String,
    account: Option<String>,
    key_path: Option<String>,
    files: Vec<StoredBatchFile>,
    expires_at_ms: i64,
}

fn batch_grants() -> &'static Mutex<HashMap<String, BatchGrant>> {
    static GRANTS: std::sync::LazyLock<Mutex<HashMap<String, BatchGrant>>> = std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &GRANTS
}

/// 一次确认写计划里的每一个文件。备份目录仍是这一次的 `bak/<id>/`。
pub fn issue_batch_prompt(app_data_dir: &Path, input: &BatchPromptInput<'_>) -> Result<RemoteWritePrompt> {
    let exec_id = uuid::Uuid::new_v4().simple().to_string();
    issue_batch_for(app_data_dir, &exec_id, input, now_ms())
}

pub fn issue_batch_for(app_data_dir: &Path, exec_id: &str, input: &BatchPromptInput<'_>, now: i64) -> Result<RemoteWritePrompt> {
    let prompt = prompt_for(app_data_dir, exec_id)?;
    let grant = batch_from(input, now)?;
    let mut map = batch_grants().lock().map_err(|_| Error::new(ErrorCode::Internal, "写入确认锁被占用"))?;
    map.insert(exec_id.to_string(), grant);
    Ok(prompt)
}

fn batch_from(input: &BatchPromptInput<'_>, now: i64) -> Result<BatchGrant> {
    if input.host.is_empty() || input.user.is_empty() || input.auth.is_empty() {
        return Err(Error::new(ErrorCode::InvalidLocation, "远程主机、用户或认证方式为空"));
    }
    if input.fingerprint.trim().is_empty() {
        return Err(Error::new(ErrorCode::InvalidState, "请先测试连接并确认指纹"));
    }
    if input.files.is_empty() {
        return Err(Error::new(ErrorCode::InvalidState, "至少选一个文件"));
    }
    let mut files = Vec::new();
    for file in input.files {
        split_remote(file.remote_path)?;
        let content_sha256 = sha256_hex(file.content);
        if let Some(given) = file.content_sha256 {
            if given != content_sha256 {
                return Err(Error::new(ErrorCode::VerifyFailed, "暂存的新内容和 sha256 不一致"));
            }
        }
        files.push(StoredBatchFile {
            remote_path: file.remote_path.to_string(),
            bytes: Some(file.content.to_vec()),
            content_sha256,
            size: file.content.len() as u64,
            rel: String::new(),
            source: None,
        });
    }
    Ok(BatchGrant {
        host: input.host.to_string(),
        port: input.port,
        user: input.user.to_string(),
        auth: input.auth.to_string(),
        fingerprint: input.fingerprint.to_string(),
        account: input.account.map(str::to_string),
        key_path: input.key_path.map(str::to_string),
        files,
        expires_at_ms: now.saturating_add(GRANT_TTL_MS),
    })
}

pub fn borrow_batch(exec_id: &str, now: i64) -> Result<StoredBatch> {
    let mut map = batch_grants().lock().map_err(|_| Error::new(ErrorCode::Internal, "写入确认锁被占用"))?;
    let Some(grant) = map.get(exec_id) else {
        return Err(Error::new(ErrorCode::InvalidState, "没有这次写入的确认"));
    };
    if now > grant.expires_at_ms {
        map.remove(exec_id);
        return Err(Error::new(ErrorCode::InvalidState, "写入确认已过期"));
    }
    Ok(stored_from(grant))
}

fn stored_from(grant: &BatchGrant) -> StoredBatch {
    StoredBatch {
        host: grant.host.clone(),
        port: grant.port,
        user: grant.user.clone(),
        auth: grant.auth.clone(),
        fingerprint: grant.fingerprint.clone(),
        account: grant.account.clone(),
        key_path: grant.key_path.clone(),
        files: grant.files.clone(),
    }
}

pub fn take_batch(exec_id: &str, now: i64) -> Result<StoredBatch> {
    let mut map = batch_grants().lock().map_err(|_| Error::new(ErrorCode::Internal, "写入确认锁被占用"))?;
    let Some(grant) = map.remove(exec_id) else {
        return Err(Error::new(ErrorCode::InvalidState, "没有这次写入的确认"));
    };
    if now > grant.expires_at_ms {
        return Err(Error::new(ErrorCode::InvalidState, "写入确认已过期"));
    }
    for file in &grant.files {
        if let Some(bytes) = &file.bytes {
            if sha256_hex(bytes) != file.content_sha256 {
                return Err(Error::new(ErrorCode::VerifyFailed, "暂存的新内容和 sha256 不一致"));
            }
        }
    }
    Ok(stored_from(&grant))
}

pub fn issue_upload(app_data_dir: &Path, input: &UploadBatch<'_>) -> Result<RemoteWritePrompt> {
    let exec_id = uuid::Uuid::new_v4().simple().to_string();
    issue_upload_for(app_data_dir, &exec_id, input, now_ms())
}

pub fn issue_upload_for(app_data_dir: &Path, exec_id: &str, input: &UploadBatch<'_>, now: i64) -> Result<RemoteWritePrompt> {
    let prompt = prompt_for(app_data_dir, exec_id)?;
    let grant = grant_from_upload(input, now)?;
    let mut map = batch_grants().lock().map_err(|_| Error::new(ErrorCode::Internal, "写入确认锁被占用"))?;
    map.insert(exec_id.to_string(), grant);
    Ok(prompt)
}

fn grant_from_upload(input: &UploadBatch<'_>, now: i64) -> Result<BatchGrant> {
    if input.host.is_empty() || input.user.is_empty() || input.auth.is_empty() {
        return Err(Error::new(ErrorCode::InvalidLocation, "远程主机、用户或认证方式为空"));
    }
    if input.fingerprint.trim().is_empty() {
        return Err(Error::new(ErrorCode::InvalidState, "请先测试连接并确认指纹"));
    }
    if input.files.is_empty() {
        return Err(Error::new(ErrorCode::InvalidState, "至少选一个文件"));
    }
    let mut files = Vec::new();
    for file in input.files {
        split_remote(&file.remote_path)?;
        files.push(stored_upload(file)?);
    }
    Ok(BatchGrant {
        host: input.host.to_string(),
        port: input.port,
        user: input.user.to_string(),
        auth: input.auth.to_string(),
        fingerprint: input.fingerprint.to_string(),
        account: input.account.map(str::to_string),
        key_path: input.key_path.map(str::to_string),
        files,
        expires_at_ms: now.saturating_add(GRANT_TTL_MS),
    })
}

/// 编辑或合并后的内容如果来自二进制、超过 1MB、或读不出来的升级包文件，在写入前拒绝。
/// 没有内联内容的文件仍按哈希和大小流式读取，这里不拦。
pub fn reject_merged_upload(db: &rusqlite::Connection, file: &UploadFile) -> Result<()> {
    if file.bytes.is_none() {
        return Ok(());
    }
    let Some(source) = &file.source else {
        return Ok(());
    };
    if crate::pkg::source_unmergeable(db, source)? {
        return Err(crate::error::unreadable_merge(&file.path));
    }
    Ok(())
}

fn stored_upload(file: &UploadFile) -> Result<StoredBatchFile> {
    if let Some(bytes) = &file.bytes {
        let content_sha256 = sha256_hex(bytes);
        if let Some(given) = file.content_hash.as_deref().filter(|hash| !hash.is_empty()) {
            if given != content_sha256 {
                return Err(Error::new(ErrorCode::VerifyFailed, "暂存的新内容和 sha256 不一致"));
            }
        }
        return Ok(StoredBatchFile {
            remote_path: file.remote_path.clone(),
            bytes: Some(bytes.clone()),
            content_sha256,
            size: bytes.len() as u64,
            rel: file.path.clone(),
            source: None,
        });
    }
    let content_sha256 = file
        .content_hash
        .as_deref()
        .filter(|hash| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| Error::new(ErrorCode::InvalidState, "计划缺少内容哈希，不能写入"))?
        .to_string();
    let size = file.size.ok_or_else(|| Error::new(ErrorCode::InvalidState, "计划缺少文件大小，不能写入"))?;
    let source = file
        .source
        .clone()
        .filter(|source| !source.scan_id.is_empty() && !source.entry.is_empty())
        .ok_or_else(|| Error::new(ErrorCode::InvalidState, "计划缺少升级包来源，不能写入"))?;
    crate::pathutil::RelPath::parse(&file.path)?;
    Ok(StoredBatchFile {
        remote_path: file.remote_path.clone(),
        bytes: None,
        content_sha256,
        size,
        rel: file.path.clone(),
        source: Some(source),
    })
}

/// 有内容就用内容。没有内容就调用本机执行同一条 `stream_plan_file`，按原始字节流式读出并核对哈希。
pub fn materialize_batch_file(db: &rusqlite::Connection, file: StoredBatchFile) -> Result<StoredBatchFile> {
    if file.bytes.is_some() {
        let bytes = file.bytes.as_deref().unwrap_or_default();
        if sha256_hex(bytes) != file.content_sha256 {
            return Err(Error::new(ErrorCode::VerifyFailed, "暂存的新内容和 sha256 不一致"));
        }
        return Ok(file);
    }
    {
        let source = file.source.clone().ok_or_else(|| Error::new(ErrorCode::InvalidState, "计划缺少升级包来源，不能写入"))?;
        let op = crate::model::PlanOp {
            index: 1,
            kind: crate::model::OpKind::Overwrite,
            path: file.rel.clone(),
            from: None,
            bytes: None,
            expect: None,
            mode: None,
            source_mtime_ms: None,
            merged: false,
            keep_source: false,
            readonly: false,
            label: String::new(),
            decision: None,
            content_hash: Some(file.content_sha256.clone()),
            size: Some(file.size),
            source: Some(source),
        };
        let dest = std::env::temp_dir().join(format!("hetianxia-stream-{}", uuid::Uuid::new_v4().simple()));
        let streamed = crate::pkg::stream_plan_file(db, &op, &dest);
        let read = match streamed {
            Ok(()) => std::fs::read(&dest),
            Err(err) => {
                let _ = std::fs::remove_file(&dest);
                return Err(err);
            }
        };
        let _ = std::fs::remove_file(&dest);
        let bytes = read.map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?;
        if bytes.len() as u64 != file.size || sha256_hex(&bytes) != file.content_sha256 {
            return Err(Error::new(
                ErrorCode::VerifyFailed,
                format!("{} 的升级包内容和计划里的哈希不一致，已中止", file.rel),
            ));
        }
        return Ok(StoredBatchFile { bytes: Some(bytes), ..file });
    }
}

pub fn request_from_stored(
    exec_id: &str,
    app_data_dir: &Path,
    posix_rename: bool,
    index: u32,
    host: &str,
    port: u16,
    user: &str,
    file: StoredBatchFile,
) -> Result<RemoteWriteRequest> {
    let Some(bytes) = file.bytes else {
        return Err(Error::new(ErrorCode::InvalidState, "升级包内容还没有读出来"));
    };
    if sha256_hex(&bytes) != file.content_sha256 {
        return Err(Error::new(ErrorCode::VerifyFailed, "暂存的新内容和 sha256 不一致"));
    }
    Ok(RemoteWriteRequest {
        exec_id: exec_id.to_string(),
        host: host.to_string(),
        port,
        user: user.to_string(),
        remote_path: file.remote_path,
        bytes,
        content_sha256: file.content_sha256,
        app_data_dir: app_data_dir.to_path_buf(),
        posix_rename,
        index,
    })
}

pub fn assemble_report(backup_dir: &str, paths: &[String], failed_at: Option<(usize, String)>, backed_up: &[bool]) -> RemoteBatchReport {
    let files = paths
        .iter()
        .enumerate()
        .map(|(index, remote_path)| {
            let (state, code) = match &failed_at {
                Some((failed, code)) if index == *failed => ("failed", Some(code.clone())),
                Some((failed, _)) if index > *failed => ("pending", None),
                _ => ("written", None),
            };
            let kept = state == "written" && backed_up.get(index).copied().unwrap_or(false);
            RemoteFileOutcome { remote_path: remote_path.clone(), state: state.to_string(), code, backed_up: kept }
        })
        .collect();
    let (failed_index, failed_path, code) = match failed_at {
        Some((index, code)) => (Some(index as u32 + 1), paths.get(index).cloned(), Some(code)),
        None => (None, None, None),
    };
    RemoteBatchReport {
        backup_dir: backup_dir.to_string(),
        total: paths.len() as u32,
        failed_index,
        failed_path,
        code,
        files,
    }
}

/// 按顺序写。某一项失败后不再碰后面的文件。
pub fn run_batch(backup_dir: &str, paths: &[String], mut write_at: impl FnMut(usize) -> Result<bool>) -> RemoteBatchReport {
    let mut failed_at = None;
    let mut backed_up = vec![false; paths.len()];
    for index in 0..paths.len() {
        match write_at(index) {
            Ok(kept) => backed_up[index] = kept,
            Err(err) => {
                failed_at = Some((index, err.code_str().to_string()));
                break;
            }
        }
    }
    assemble_report(backup_dir, paths, failed_at, &backed_up)
}

pub fn commit_stored_batch(
    store: &mut dyn RemoteBytes,
    log: &mut RuntimeLog,
    exec_id: &str,
    app_data_dir: &Path,
    posix_rename: bool,
) -> Result<RemoteBatchReport> {
    let granted = take_batch(exec_id, now_ms())?;
    let backup_dir = tx_backup_dir(app_data_dir, exec_id)?.display().to_string();
    let paths: Vec<String> = granted.files.iter().map(|file| file.remote_path.clone()).collect();
    let host = granted.host.clone();
    let port = granted.port;
    let user = granted.user.clone();
    let mut files = granted.files.into_iter();
    let report = run_batch(&backup_dir, &paths, |index| {
        let file = files.next().ok_or_else(|| Error::new(ErrorCode::Internal, "写入项丢失"))?;
        let req = request_from_stored(exec_id, app_data_dir, posix_rename, index as u32, &host, port, &user, file)?;
        let staged = write_one(store, log, &req)?;
        Ok(staged.backup_file.is_some())
    });
    Ok(report)
}

/// 写入请求只能从已消耗的许可组装。主机、路径和内容都来自这一份。
pub fn request_from_grant(
    exec_id: &str,
    app_data_dir: &Path,
    posix_rename: bool,
    index: u32,
    granted: GrantedWrite,
) -> Result<RemoteWriteRequest> {
    if sha256_hex(&granted.bytes) != granted.content_sha256 {
        return Err(Error::new(ErrorCode::VerifyFailed, "暂存的新内容和 sha256 不一致"));
    }
    Ok(RemoteWriteRequest {
        exec_id: exec_id.to_string(),
        host: granted.connection.host,
        port: granted.connection.port,
        user: granted.connection.user,
        remote_path: granted.connection.remote_path,
        bytes: granted.bytes,
        content_sha256: granted.content_sha256,
        app_data_dir: app_data_dir.to_path_buf(),
        posix_rename,
        index,
    })
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
        "user": req.user,
        "backupPath": backup_file.as_ref().map(|path| path.display().to_string()),
        "contentSha256": req.content_sha256,
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

fn write_one(store: &mut dyn RemoteBytes, log: &mut RuntimeLog, req: &RemoteWriteRequest) -> Result<StagedBackup> {
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
    Ok(staged)
}

pub fn commit_remote_write(
    store: &mut dyn RemoteBytes,
    log: &mut RuntimeLog,
    exec_id: &str,
    app_data_dir: &Path,
    posix_rename: bool,
    index: u32,
) -> Result<CommitResult> {
    let granted = take_granted_write(exec_id, now_ms())?;
    let req = request_from_grant(exec_id, app_data_dir, posix_rename, index, granted)?;
    let staged = write_one(store, log, &req)?;
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

    #[test]
    fn merged_upload_refuses_png_and_oversized_package_files() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let package = dir.path().join("package");
        std::fs::create_dir(&project).unwrap();
        std::fs::create_dir(&package).unwrap();
        std::fs::write(package.join("logo.png"), b"\x89PNG\r\n\x1a\n\x00").unwrap();
        std::fs::write(package.join("big.bin"), vec![b'Q'; 1024 * 1024 + 1]).unwrap();
        std::fs::write(package.join("note.txt"), b"hello\n").unwrap();
        let db = crate::exec::open_db(&dir.path().join("t.sqlite")).unwrap();
        let scanned = crate::pkg::scan_recorded(&db, &project, &package).unwrap();
        let source_for = |name: &str| {
            let file = scanned.files.iter().find(|item| item.rel == name).unwrap();
            crate::model::ContentSource {
                scan_id: scanned.scan_id.clone(),
                kind: scanned.package_kind.clone(),
                entry: file.entry_name.clone().unwrap(),
                path: String::new(),
            }
        };
        let upload = |name: &str, bytes: Option<Vec<u8>>| UploadFile {
            remote_path: format!("/srv/{name}"),
            path: name.into(),
            bytes,
            content_hash: None,
            size: None,
            source: Some(source_for(name)),
        };
        let png_err = reject_merged_upload(&db, &upload("logo.png", Some(Vec::new()))).unwrap_err();
        assert_eq!(png_err.message, "logo.png 无法合并：没有读到文件内容，这一项没有写入。");
        let big_err = reject_merged_upload(&db, &upload("big.bin", Some(b"x".to_vec()))).unwrap_err();
        assert_eq!(big_err.message, "big.bin 无法合并：没有读到文件内容，这一项没有写入。");
        reject_merged_upload(&db, &upload("note.txt", Some(b"edited\n".to_vec()))).unwrap();
        reject_merged_upload(&db, &upload("logo.png", None)).unwrap();
        reject_merged_upload(&db, &upload("big.bin", None)).unwrap();
    }

    struct Mem {
        files: BTreeMap<String, Vec<u8>>,
        calls: u32,
        fail_write: bool,
        fail_after_writes: Option<u32>,
        writes_done: u32,
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
                fail_after_writes: None,
                writes_done: 0,
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
            if self.fail_write || self.fail_after_writes.is_some_and(|limit| self.writes_done >= limit) {
                return Err(Error::new(ErrorCode::Io, "写入中途失败"));
            }
            self.writes_done += 1;
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

    fn sample<'a>(host: &'a str, user: &'a str, remote_path: &'a str, content: &'a [u8]) -> WritePromptInput<'a> {
        WritePromptInput {
            host,
            port: 22,
            user,
            auth: "key",
            fingerprint: "SHA256:stored",
            remote_path,
            content,
            content_sha256: None,
            account: Some("acct-1"),
            key_path: None,
        }
    }

    #[test]
    fn permit_stores_the_normalized_path_inside_the_remote_root() {
        let path = crate::connections::path_inside_remote_root(Some("/srv/app"), "/srv/app//conf/./app.conf").unwrap();
        let home = tempfile::tempdir().unwrap();
        let input = WritePromptInput {
            host: "build-01",
            port: 22,
            user: "deploy",
            auth: "key",
            fingerprint: "SHA256:stored",
            remote_path: &path,
            content: b"new-bytes",
            content_sha256: None,
            account: None,
            key_path: None,
        };
        issue_prompt_for(home.path(), "tx-inside", &input, now_ms()).unwrap();
        let stored = borrow_granted_connection("tx-inside", now_ms()).unwrap();
        assert_eq!(stored.remote_path, "/srv/app/conf/app.conf");
        assert!(crate::connections::path_inside_remote_root(Some("/srv/app"), "../etc/passwd").is_err());
    }

    fn grant(app_data: &Path, exec_id: &str) {
        issue_prompt_for(app_data, exec_id, &sample("build-01", "deploy", "/opt/gateway/app.conf", b"new-bytes"), now_ms()).unwrap();
    }

    fn commit(mem: &mut Mem, log: &mut RuntimeLog, exec_id: &str, app_data: &Path, posix: bool) -> Result<CommitResult> {
        commit_remote_write(mem, log, exec_id, app_data, posix, 1)
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
        let err = commit(&mut mem, &mut log, "tx-missing", home.path(), true).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidState);
        assert_eq!(err.code_str(), "E_INVALID_STATE");
        assert_eq!(mem.calls, 0);
        assert_eq!(mem.files, before);
        assert!(log.lines.is_empty());
        assert!(!tx_backup_dir(home.path(), "tx-missing").unwrap().exists());
    }

    #[test]
    fn permit_is_bound_to_host_port_user_and_path_once() {
        let home = tempfile::tempdir().unwrap();
        issue_prompt_for(home.path(), "tx-a", &sample("build-01", "deploy", "/opt/gateway/app.conf", b"new-bytes"), now_ms()).unwrap();
        issue_prompt_for(home.path(), "tx-b", &sample("other-host", "root", "/etc/other.conf", b"other-bytes"), now_ms()).unwrap();
        let bound = borrow_granted_connection("tx-a", now_ms()).unwrap();
        assert!(grant_binds(&bound, "build-01", 22, "deploy", "/opt/gateway/app.conf"));
        assert!(!grant_binds(&bound, "other-host", 22, "deploy", "/opt/gateway/app.conf"));
        assert!(!grant_binds(&bound, "build-01", 22, "root", "/opt/gateway/app.conf"));
        assert!(!grant_binds(&bound, "build-01", 2222, "deploy", "/opt/gateway/app.conf"));
        assert!(!grant_binds(&bound, "build-01", 22, "deploy", "/etc/other.conf"));
        let mut mem = Mem::new(BTreeMap::from([
            ("/opt/gateway/app.conf".into(), b"old-bytes".to_vec()),
            ("/etc/other.conf".into(), b"keep".to_vec()),
        ]));
        let mut log = RuntimeLog::default();
        commit(&mut mem, &mut log, "tx-a", home.path(), true).unwrap();
        assert_eq!(mem.files.get("/opt/gateway/app.conf").map(Vec::as_slice), Some(b"new-bytes".as_slice()));
        assert_eq!(mem.files.get("/etc/other.conf").map(Vec::as_slice), Some(b"keep".as_slice()));
        let err = commit(&mut mem, &mut log, "tx-a", home.path(), true).unwrap_err();
        assert_eq!(err.message, "没有这次写入的确认");
        commit(&mut mem, &mut log, "tx-b", home.path(), true).unwrap();
        assert_eq!(mem.files.get("/etc/other.conf").map(Vec::as_slice), Some(b"other-bytes".as_slice()));
    }

    #[test]
    fn voided_permit_cannot_be_reused() {
        let home = tempfile::tempdir().unwrap();
        grant(home.path(), "tx-void");
        void_granted_write("tx-void").unwrap();
        let mut mem = Mem::new(BTreeMap::from([("/opt/gateway/app.conf".into(), b"old-bytes".to_vec())]));
        let mut log = RuntimeLog::default();
        let err = commit(&mut mem, &mut log, "tx-void", home.path(), true).unwrap_err();
        assert_eq!(err.message, "没有这次写入的确认");
        assert_eq!(mem.calls, 0);
    }

    #[test]
    fn staged_sha256_must_match_before_a_permit_is_issued() {
        let home = tempfile::tempdir().unwrap();
        let mut input = sample("build-01", "deploy", "/opt/gateway/app.conf", b"new-bytes");
        input.content_sha256 = Some("deadbeef");
        let err = issue_prompt_for(home.path(), "tx-sha", &input, now_ms()).unwrap_err();
        assert_eq!(err.code, ErrorCode::VerifyFailed);
        let err = borrow_granted_connection("tx-sha", now_ms()).unwrap_err();
        assert_eq!(err.message, "没有这次写入的确认");
    }

    #[test]
    fn expired_grant_is_refused() {
        let home = tempfile::tempdir().unwrap();
        issue_prompt_for(home.path(), "tx-expired", &sample("build-01", "deploy", "/opt/gateway/app.conf", b"new-bytes"), 0).unwrap();
        let mut mem = Mem::new(BTreeMap::new());
        let mut log = RuntimeLog::default();
        let err = commit(&mut mem, &mut log, "tx-expired", home.path(), true).unwrap_err();
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
            let result = commit(&mut mem, &mut log, exec_id, home.path(), posix).unwrap();
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
            assert_eq!(entry["user"], "deploy");
            assert_eq!(entry["contentSha256"], sha256_hex(b"new-bytes"));
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
        let err = commit(&mut mem, &mut log, "tx-fail-write", home.path(), true).unwrap_err();
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
        let err = commit(&mut mem, &mut log, "tx-verify", home.path(), false).unwrap_err();
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
        let err = commit(&mut mem, &mut log, "tx-rename", home.path(), false).unwrap_err();
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
        let err = commit(&mut mem, &mut log, "tx-restore", home.path(), false).unwrap_err();
        assert_eq!(err.message, "还原失败");
        let bak = mem.paths().into_iter().find(|path| path.contains("bak-")).expect("原文件还在 .htx-bak");
        assert_eq!(mem.files.get(&bak).map(Vec::as_slice), Some(b"old-bytes".as_slice()));
        assert!(mem.paths().iter().any(|path| path.contains(TEMP_TAG) && !path.contains("bak-")));
    }

    #[test]
    fn empty_fingerprint_refuses_a_permit() {
        let home = tempfile::tempdir().unwrap();
        let mut input = sample("build-01", "deploy", "/opt/app.conf", b"new-bytes");
        input.fingerprint = "";
        let err = issue_prompt_for(home.path(), "tx-empty-fp", &input, now_ms()).unwrap_err();
        assert_eq!(err.message, "请先测试连接并确认指纹");
        assert!(borrow_granted_connection("tx-empty-fp", now_ms()).is_err());
        input.fingerprint = "   ";
        assert!(issue_prompt_for(home.path(), "tx-blank-fp", &input, now_ms()).is_err());
        assert!(borrow_granted_connection("tx-blank-fp", now_ms()).is_err());
    }

    #[test]
    fn mismatched_permit_fingerprint_rejects_and_voids_without_a_trust_prompt() {
        let home = tempfile::tempdir().unwrap();
        let exec_id = "tx-fp-mismatch";
        issue_prompt_for(home.path(), exec_id, &sample("build-01", "deploy", "/opt/app.conf", b"new-bytes"), now_ms()).unwrap();
        let granted = take_granted_write(exec_id, now_ms()).unwrap();
        let err = crate::remote::strict_permit_fingerprint(&granted.connection.fingerprint, "SHA256:other").unwrap_err();
        assert_eq!(err.message, "服务器指纹已变化");
        assert!(!err.message.contains("需要确认"));
        assert_eq!(granted.connection.fingerprint, "SHA256:stored");
        void_granted_write(exec_id).unwrap();
        assert!(borrow_granted_connection(exec_id, now_ms()).is_err());
        assert!(take_granted_write(exec_id, now_ms()).is_err());
    }

    fn batch_input<'a>(files: &'a [BatchFileInput<'a>]) -> BatchPromptInput<'a> {
        BatchPromptInput {
            host: "build-01",
            port: 22,
            user: "deploy",
            auth: "key",
            fingerprint: "SHA256:stored",
            account: None,
            key_path: None,
            files,
        }
    }

    #[test]
    fn batch_writes_every_file_then_stops_when_one_fails() {
        let home = tempfile::tempdir().unwrap();
        let app_data = home.path().join("app-data");
        let files = [
            BatchFileInput { remote_path: "/opt/site/a.txt", content: b"A", content_sha256: None },
            BatchFileInput { remote_path: "/opt/site/b.txt", content: b"B", content_sha256: None },
            BatchFileInput { remote_path: "/opt/site/c.txt", content: b"C", content_sha256: None },
        ];
        issue_batch_for(&app_data, "tx-all", &batch_input(&files), now_ms()).unwrap();
        let mut mem = Mem::new(BTreeMap::from([
            ("/opt/site/a.txt".into(), b"old-a".to_vec()),
            ("/opt/site/b.txt".into(), b"old-b".to_vec()),
            ("/opt/site/c.txt".into(), b"old-c".to_vec()),
        ]));
        let mut log = RuntimeLog::default();
        let done = commit_stored_batch(&mut mem, &mut log, "tx-all", &app_data, true).unwrap();
        assert_eq!(done.failed_index, None);
        assert_eq!(done.files.iter().map(|file| file.state.as_str()).collect::<Vec<_>>(), ["written", "written", "written"]);
        assert!(done.files.iter().all(|file| file.backed_up));
        assert_eq!(mem.files.get("/opt/site/a.txt").map(Vec::as_slice), Some(b"A".as_slice()));
        assert_eq!(mem.files.get("/opt/site/b.txt").map(Vec::as_slice), Some(b"B".as_slice()));
        assert_eq!(mem.files.get("/opt/site/c.txt").map(Vec::as_slice), Some(b"C".as_slice()));
        let dir = tx_backup_dir(&app_data, "tx-all").unwrap();
        assert_eq!(fs::read(dir.join("files/opt/site/a.txt")).unwrap(), b"old-a");
        assert_eq!(fs::read(dir.join("files/opt/site/b.txt")).unwrap(), b"old-b");
        assert_eq!(fs::read(dir.join("files/opt/site/c.txt")).unwrap(), b"old-c");
        assert!(mem.paths().iter().all(|path| !path.contains(TEMP_TAG)));

        issue_batch_for(&app_data, "tx-mid", &batch_input(&files), now_ms()).unwrap();
        let mut again = Mem::new(BTreeMap::from([
            ("/opt/site/a.txt".into(), b"old-a".to_vec()),
            ("/opt/site/b.txt".into(), b"old-b".to_vec()),
            ("/opt/site/c.txt".into(), b"old-c".to_vec()),
        ]));
        again.fail_after_writes = Some(1);
        let stopped = commit_stored_batch(&mut again, &mut log, "tx-mid", &app_data, true).unwrap();
        assert_eq!(stopped.total, 3);
        assert_eq!(stopped.failed_index, Some(2));
        assert_eq!(stopped.failed_path.as_deref(), Some("/opt/site/b.txt"));
        assert_eq!(stopped.code.as_deref(), Some("E_IO"));
        assert_eq!(
            stopped.files.iter().map(|file| (file.state.as_str(), file.code.as_deref())).collect::<Vec<_>>(),
            [("written", None), ("failed", Some("E_IO")), ("pending", None)]
        );
        assert_eq!(stopped.files.iter().map(|file| file.backed_up).collect::<Vec<_>>(), [true, false, false]);
        assert_eq!(again.files.get("/opt/site/a.txt").map(Vec::as_slice), Some(b"A".as_slice()));
        assert_eq!(again.files.get("/opt/site/b.txt").map(Vec::as_slice), Some(b"old-b".as_slice()));
        assert_eq!(again.files.get("/opt/site/c.txt").map(Vec::as_slice), Some(b"old-c".as_slice()));
        let mid = tx_backup_dir(&app_data, "tx-mid").unwrap();
        assert_eq!(fs::read(mid.join("files/opt/site/a.txt")).unwrap(), b"old-a");
        // 失败项在改远端之前已经把原件落下，但不算「已写入并备份」。
        assert_eq!(fs::read(mid.join("files/opt/site/b.txt")).unwrap(), b"old-b");
        assert!(!stopped.files[1].backed_up);
        assert_eq!(stopped.backup_dir, mid.display().to_string());

        let mixed_files = [
            BatchFileInput { remote_path: "/opt/site/a.txt", content: b"A", content_sha256: None },
            BatchFileInput { remote_path: "/opt/site/new.txt", content: b"N", content_sha256: None },
            BatchFileInput { remote_path: "/opt/site/c.txt", content: b"C", content_sha256: None },
        ];
        issue_batch_for(&app_data, "tx-mix", &batch_input(&mixed_files), now_ms()).unwrap();
        let mut mix = Mem::new(BTreeMap::from([
            ("/opt/site/a.txt".into(), b"old-a".to_vec()),
            ("/opt/site/c.txt".into(), b"old-c".to_vec()),
        ]));
        mix.fail_after_writes = Some(2);
        let mixed = commit_stored_batch(&mut mix, &mut log, "tx-mix", &app_data, true).unwrap();
        assert_eq!(
            mixed.files.iter().map(|file| (file.state.as_str(), file.backed_up)).collect::<Vec<_>>(),
            [("written", true), ("written", false), ("failed", false)]
        );
        assert_eq!(mix.files.get("/opt/site/new.txt").map(Vec::as_slice), Some(b"N".as_slice()));
        assert_eq!(mix.files.get("/opt/site/c.txt").map(Vec::as_slice), Some(b"old-c".as_slice()));
        let mix_dir = tx_backup_dir(&app_data, "tx-mix").unwrap();
        assert_eq!(fs::read(mix_dir.join("files/opt/site/a.txt")).unwrap(), b"old-a");
        assert!(!mix_dir.join("files/opt/site/new.txt").exists());

        let created = [
            BatchFileInput { remote_path: "/opt/site/n1.txt", content: b"1", content_sha256: None },
            BatchFileInput { remote_path: "/opt/site/n2.txt", content: b"2", content_sha256: None },
            BatchFileInput { remote_path: "/opt/site/n3.txt", content: b"3", content_sha256: None },
        ];
        issue_batch_for(&app_data, "tx-new", &batch_input(&created), now_ms()).unwrap();
        let mut fresh = Mem::new(BTreeMap::new());
        fresh.fail_after_writes = Some(2);
        let only_new = commit_stored_batch(&mut fresh, &mut log, "tx-new", &app_data, true).unwrap();
        assert!(only_new.files.iter().all(|file| !file.backed_up));
        assert_eq!(
            only_new.files.iter().map(|file| file.state.as_str()).collect::<Vec<_>>(),
            ["written", "written", "failed"]
        );

        let calls = std::cell::Cell::new(0u32);
        let partial = run_batch("/bak/tx", &["/a".into(), "/b".into(), "/c".into()], |_| {
            let seen = calls.get();
            calls.set(seen + 1);
            if seen == 1 {
                return Err(Error::new(ErrorCode::Permission, "不能写"));
            }
            Ok(true)
        });
        assert_eq!(calls.get(), 2);
        assert_eq!(partial.failed_index, Some(2));
        assert_eq!(partial.files[2].state, "pending");
        assert_eq!(partial.files.iter().map(|file| file.backed_up).collect::<Vec<_>>(), [true, false, false]);
        let dropped = assemble_report("/bak/tx", &["/a".into(), "/b".into()], Some((1, "E_IO".into())), &[true, true]);
        assert!(dropped.files[0].backed_up);
        assert!(!dropped.files[1].backed_up);
    }

    #[test]
    fn sftp_upload_matches_package_bytes_for_binary_large_and_encoded_files() {
        use crate::model::ContentSource;
        use crate::sftp::{ProbeRequest, RemoteSession};
        use russh::server::{Auth, Msg, Server as _, Session};
        use russh::Channel;
        use russh::ChannelId;
        use russh_sftp::protocol::{Data, FileAttributes, Handle, OpenFlags, Status, StatusCode};
        use std::collections::HashMap;
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        use std::result::Result;
        use std::sync::{Arc, Mutex};

        let home = tempfile::tempdir().unwrap();
        let project = home.path().join("project");
        let package = home.path().join("package");
        fs::create_dir(&project).unwrap();
        fs::create_dir(&package).unwrap();
        let binary = b"\x00\x01\xff\xfe\n".to_vec();
        let wide = vec![0x5au8; 5 * 1024 * 1024];
        let bom = b"\xef\xbb\xbfhello\n".to_vec();
        let gbk = b"\xd6\xd0\xce\xc4".to_vec();
        let utf16 = b"\xff\xfe\x41\x00\x42\x00".to_vec();
        let package_note = b"package-note".to_vec();
        let samples = [
            ("bin.dat", binary.clone()),
            ("wide.bin", wide.clone()),
            ("bom.txt", bom.clone()),
            ("gbk.txt", gbk.clone()),
            ("utf16.txt", utf16.clone()),
            ("note.txt", package_note.clone()),
        ];
        for (name, bytes) in &samples {
            fs::write(package.join(name), bytes).unwrap();
        }
        let zip_path = home.path().join("pkg.zip");
        {
            let file = fs::File::create(&zip_path).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            writer.start_file("gbk.txt", zip::write::SimpleFileOptions::default()).unwrap();
            std::io::Write::write_all(&mut writer, &gbk).unwrap();
            writer.finish().unwrap();
        }
        let db_path = home.path().join("app.sqlite");
        let db = crate::exec::open_db(&db_path).unwrap();
        let scanned = crate::pkg::scan_recorded(&db, &project, &package).unwrap();
        let zipped = crate::pkg::scan_recorded(&db, &project, &zip_path).unwrap();
        let mut by_rel = HashMap::new();
        for file in &scanned.files {
            by_rel.insert(file.rel.clone(), file.clone());
        }
        let zip_file = zipped.files.iter().find(|file| file.rel == "gbk.txt").unwrap();
        let mut uploads = Vec::new();
        for name in ["bin.dat", "wide.bin", "bom.txt", "gbk.txt", "utf16.txt"] {
            let file = by_rel.get(name).unwrap();
            uploads.push(UploadFile {
                remote_path: format!("/srv/pkg/{name}"),
                path: name.to_string(),
                bytes: None,
                content_hash: file.content_sha256.clone(),
                size: file.right_size,
                source: Some(ContentSource {
                    scan_id: scanned.scan_id.clone(),
                    kind: scanned.package_kind.clone(),
                    entry: file.entry_name.clone().unwrap(),
                    path: String::new(),
                }),
            });
        }
        uploads.push(UploadFile {
            remote_path: "/srv/pkg/from-zip.txt".into(),
            path: "gbk.txt".into(),
            bytes: None,
            content_hash: zip_file.content_sha256.clone(),
            size: zip_file.right_size,
            source: Some(ContentSource {
                scan_id: zipped.scan_id.clone(),
                kind: zipped.package_kind.clone(),
                entry: zip_file.entry_name.clone().unwrap(),
                path: String::new(),
            }),
        });
        let edited = b"user-edited\n".to_vec();
        uploads.push(UploadFile {
            remote_path: "/srv/pkg/note.txt".into(),
            path: "note.txt".into(),
            bytes: Some(edited.clone()),
            content_hash: None,
            size: None,
            source: None,
        });

        let remote_files = Arc::new(Mutex::new(HashMap::<String, Vec<u8>>::new()));
        let host_key = russh::keys::PrivateKey::random(
            &mut russh::keys::ssh_key::rand_core::OsRng,
            russh::keys::Algorithm::Ed25519,
        )
        .unwrap();
        let fingerprint = format!("{}", host_key.public_key().fingerprint(russh::keys::HashAlg::Sha256));
        let client_key = russh::keys::PrivateKey::random(
            &mut russh::keys::ssh_key::rand_core::OsRng,
            russh::keys::Algorithm::Ed25519,
        )
        .unwrap();
        let key_path = home.path().join("client-key");
        client_key
            .write_openssh_file(&key_path, russh::keys::ssh_key::LineEnding::LF)
            .unwrap();

        #[derive(Clone)]
        struct SshServer {
            files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
        }
        struct SshClient {
            files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
            channels: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
        }
        impl russh::server::Server for SshServer {
            type Handler = SshClient;
            fn new_client(&mut self, _: Option<SocketAddr>) -> Self::Handler {
                SshClient {
                    files: self.files.clone(),
                    channels: Arc::new(Mutex::new(HashMap::new())),
                }
            }
        }
        impl russh::server::Handler for SshClient {
            type Error = russh::Error;
            async fn auth_publickey(&mut self, _user: &str, _key: &russh::keys::PublicKey) -> Result<Auth, Self::Error> {
                Ok(Auth::Accept)
            }
            async fn channel_open_session(&mut self, channel: Channel<Msg>, _session: &mut Session) -> Result<bool, Self::Error> {
                self.channels.lock().unwrap().insert(channel.id(), channel);
                Ok(true)
            }
            async fn subsystem_request(&mut self, channel_id: ChannelId, name: &str, session: &mut Session) -> Result<(), Self::Error> {
                if name != "sftp" {
                    let _ = session.channel_failure(channel_id);
                    return Ok(());
                }
                let channel = self.channels.lock().unwrap().remove(&channel_id).unwrap();
                session.channel_success(channel_id)?;
                let sftp = SftpDisk { files: self.files.clone(), handles: HashMap::new(), next: 0 };
                russh_sftp::server::run(channel.into_stream(), sftp).await;
                Ok(())
            }
        }
        struct SftpDisk {
            files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
            handles: HashMap<String, String>,
            next: u64,
        }
        fn status_ok(id: u32) -> Status {
            Status { id, status_code: StatusCode::Ok, error_message: "ok".into(), language_tag: String::new() }
        }
        fn regular(size: u64) -> FileAttributes {
            let mut attrs = FileAttributes::empty();
            attrs.size = Some(size);
            attrs.set_regular(true);
            attrs
        }
        impl russh_sftp::server::Handler for SftpDisk {
            type Error = StatusCode;
            fn unimplemented(&self) -> Self::Error {
                StatusCode::OpUnsupported
            }
            async fn open(&mut self, id: u32, filename: String, flags: OpenFlags, _attrs: FileAttributes) -> Result<Handle, Self::Error> {
                let mut files = self.files.lock().unwrap();
                let exists = files.contains_key(&filename);
                if flags.contains(OpenFlags::WRITE) {
                    if flags.contains(OpenFlags::EXCLUDE) && exists {
                        return Err(StatusCode::Failure);
                    }
                    files.entry(filename.clone()).or_default();
                } else if !exists {
                    return Err(StatusCode::NoSuchFile);
                }
                self.next += 1;
                let handle = self.next.to_string();
                self.handles.insert(handle.clone(), filename);
                Ok(Handle { id, handle })
            }
            async fn write(&mut self, id: u32, handle: String, offset: u64, data: Vec<u8>) -> Result<Status, Self::Error> {
                let Some(path) = self.handles.get(&handle).cloned() else {
                    return Err(StatusCode::Failure);
                };
                let mut files = self.files.lock().unwrap();
                let Some(buf) = files.get_mut(&path) else {
                    return Err(StatusCode::NoSuchFile);
                };
                let start = offset as usize;
                let end = start + data.len();
                if buf.len() < end {
                    buf.resize(end, 0);
                }
                buf[start..end].copy_from_slice(&data);
                Ok(status_ok(id))
            }
            async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> Result<Data, Self::Error> {
                let Some(path) = self.handles.get(&handle).cloned() else {
                    return Err(StatusCode::Failure);
                };
                let files = self.files.lock().unwrap();
                let Some(buf) = files.get(&path) else {
                    return Err(StatusCode::NoSuchFile);
                };
                let start = offset as usize;
                if start >= buf.len() {
                    return Err(StatusCode::Eof);
                }
                let end = (start + len as usize).min(buf.len());
                Ok(Data { id, data: buf[start..end].to_vec() })
            }
            async fn lstat(&mut self, id: u32, path: String) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
                let files = self.files.lock().unwrap();
                let Some(buf) = files.get(&path) else {
                    return Err(StatusCode::NoSuchFile);
                };
                Ok(russh_sftp::protocol::Attrs { id, attrs: regular(buf.len() as u64) })
            }
            async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
                self.handles.remove(&handle);
                Ok(status_ok(id))
            }
            async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
                let mut files = self.files.lock().unwrap();
                if files.remove(&filename).is_none() {
                    return Err(StatusCode::NoSuchFile);
                }
                Ok(status_ok(id))
            }
            async fn rename(&mut self, id: u32, oldpath: String, newpath: String) -> Result<Status, Self::Error> {
                let mut files = self.files.lock().unwrap();
                if files.contains_key(&newpath) {
                    return Err(StatusCode::Failure);
                }
                let Some(buf) = files.remove(&oldpath) else {
                    return Err(StatusCode::NoSuchFile);
                };
                files.insert(newpath, buf);
                Ok(status_ok(id))
            }
        }

        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let files_for_server = remote_files.clone();
        let port = rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let addr = listener.local_addr().unwrap();
            assert_eq!(addr.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
            let config = russh::server::Config {
                auth_rejection_time: std::time::Duration::from_millis(1),
                auth_rejection_time_initial: Some(std::time::Duration::from_millis(0)),
                keys: vec![host_key],
                ..Default::default()
            };
            let mut server = SshServer { files: files_for_server };
            tokio::spawn(async move {
                let _ = server.run_on_socket(Arc::new(config), &listener).await;
            });
            addr.port()
        });
        let app_data = home.path().join("app-data");
        issue_upload_for(
            &app_data,
            "tx-pkg",
            &UploadBatch {
                host: "127.0.0.1",
                port,
                user: "deploy",
                auth: "key",
                fingerprint: &fingerprint,
                account: None,
                key_path: Some(key_path.to_str().unwrap()),
                files: &uploads,
            },
            now_ms(),
        )
        .unwrap();
        let mut probe = ProbeRequest {
            host: "127.0.0.1".into(),
            port,
            user: "deploy".into(),
            proxy_jump: None,
            auth: "key".into(),
            secret: String::new(),
            account: None,
            key_path: Some(key_path.display().to_string()),
            known_hosts_path: home.path().join("known_hosts").display().to_string(),
            db_path: db_path.display().to_string(),
            accept_new: false,
            confirmed_fingerprint: None,
            strict_fingerprint: None,
        };
        let report = RemoteSession::commit_batch_blocking(&mut probe, "tx-pkg", app_data).unwrap();
        assert_eq!(report.failed_index, None, "{report:?}");
        let stored = remote_files.lock().unwrap();
        for (name, raw) in [("bin.dat", &binary), ("wide.bin", &wide), ("bom.txt", &bom), ("gbk.txt", &gbk), ("utf16.txt", &utf16)] {
            let got = stored.get(&format!("/srv/pkg/{name}")).unwrap_or_else(|| panic!("missing {name}"));
            assert_eq!(sha256_hex(got), sha256_hex(raw), "{name}");
            assert_eq!(got, raw, "{name}");
        }
        let from_zip = stored.get("/srv/pkg/from-zip.txt").unwrap();
        assert_eq!(sha256_hex(from_zip), sha256_hex(&gbk));
        assert_eq!(from_zip, &gbk);
        let note = stored.get("/srv/pkg/note.txt").unwrap();
        assert_eq!(note, &edited);
        assert_ne!(note, &package_note);
        drop(stored);
        drop(rt);
    }
}
