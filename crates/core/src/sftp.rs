//! 远程会话。只执行 `sha256sum`，文件读写走 SFTP。
//! 不支持 `posix-rename` 时按「改名备份 → 改名替换 → 删除备份」退回，并写入运行日志。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client::Handler;
use russh::keys::PrivateKeyWithHashAlg;
use russh_sftp::client::RawSftpSession;
use russh_sftp::protocol::{FileAttributes, OpenFlags, StatusCode};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::error::{Error, ErrorCode, Result};
use crate::exec;
use crate::hashutil;
use crate::remote::{
    self, classify_presented_key, fingerprints_in_file, fingerprint_was_cleared, saved_fingerprint, HostKeyStatus,
    PresentedKey, RuntimeLog, PROXY_REASON,
};

const POSIX_RENAME: &str = "posix-rename@openssh.com";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeReport {
    pub fingerprint: String,
    pub host_key: String,
    pub auth_ok: bool,
    pub sftp_ok: bool,
    pub sha256sum_ok: bool,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeRequest {
    pub host: String,
    pub port: u16,
    pub user: String,
    #[serde(default)]
    pub proxy_jump: Option<String>,
    /// `password`、`key` 或 `agent`。
    pub auth: String,
    #[serde(default)]
    pub secret: String,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub key_path: Option<String>,
    pub known_hosts_path: String,
    pub db_path: String,
    #[serde(default)]
    pub accept_new: bool,
    /// 用户在探测之后确认的指纹。`accept_new` 重连时必须带上，并且要和服务器出示的一致。
    #[serde(default)]
    pub confirmed_fingerprint: Option<String>,
    /// 提交写入时带上许可证里的指纹。有值就只做严格比较，不再询问信任。
    #[serde(default)]
    pub strict_fingerprint: Option<String>,
}

pub fn remote_backup_dir(home: &str, exec_id: &str) -> Result<String> {
    if exec_id.is_empty()
        || exec_id.contains('/')
        || exec_id.contains('\\')
        || exec_id.contains("..")
        || exec_id.contains('\0')
    {
        return Err(Error::new(ErrorCode::InvalidLocation, "备份编号不合法"));
    }
    let home = home.trim_end_matches('/');
    Ok(format!("{home}/.hetianxia-backups/{exec_id}"))
}

pub fn rename_steps(posix_rename: bool) -> &'static [&'static str] {
    if posix_rename {
        &["posix-rename"]
    } else {
        &["rename-backup", "rename-replace", "delete-backup"]
    }
}

pub fn log_rename_steps(log: &mut RuntimeLog, posix_rename: bool) {
    log.record_sftp_rename(&rename_steps(posix_rename).join(" → "));
}

pub trait RemoteNames {
    fn exists(&mut self, path: &str) -> Result<bool>;
    fn rename(&mut self, from: &str, to: &str) -> Result<()>;
    fn remove(&mut self, path: &str) -> Result<()>;
}

/// 和线上 SFTP 退回同一套顺序。测试不需要 sshd。
/// 非 posix 成功时如果留下了 `.htx-bak`，返回 true。目标核对通过之后才能删它。
/// 第二步失败时先把 `.htx-bak` 改回原名，这一步成功之后才删临时文件。
pub fn apply_rename(
    ops: &mut dyn RemoteNames,
    log: &mut RuntimeLog,
    posix_rename: bool,
    temp: &str,
    dest: &str,
    backup: &str,
) -> Result<bool> {
    log_rename_steps(log, posix_rename);
    if posix_rename {
        ops.rename(temp, dest)?;
        return Ok(false);
    }
    let had_dest = ops.exists(dest)?;
    if had_dest {
        ops.rename(dest, backup)?;
    }
    if let Err(err) = ops.rename(temp, dest) {
        if had_dest {
            ops.rename(backup, dest)?;
        }
        ops.remove(temp)?;
        return Err(err);
    }
    Ok(had_dest)
}

/// 只在目标内容已经核对通过之后调用。
pub fn discard_verified_backup(ops: &mut dyn RemoteNames, backup: &str) -> Result<()> {
    if ops.exists(backup)? {
        ops.remove(backup)?;
    }
    Ok(())
}

pub fn parse_sha256sum_output(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((hex, rest)) = line.split_once(' ') else { continue };
        if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        let path = rest.trim_start_matches([' ', '*']);
        if path.is_empty() {
            continue;
        }
        out.push((path.to_string(), hex.to_string()));
    }
    out
}

pub fn sftp_unavailable() -> Error {
    Error::new(
        ErrorCode::SftpUnavailable,
        "服务器未开启 SFTP（检查 sshd 配置中的 Subsystem sftp）",
    )
}

#[derive(Debug)]
enum SessFail {
    Ssh(russh::Error),
    Changed(String),
    Confirm(String),
    PermitMismatch,
}

impl From<russh::Error> for SessFail {
    fn from(value: russh::Error) -> Self {
        SessFail::Ssh(value)
    }
}

struct Observed {
    fingerprint: String,
    status: HostKeyStatus,
}

struct KeyHandler {
    saved: Option<String>,
    cleared: bool,
    file_fps: Vec<String>,
    accept_new: bool,
    confirmed: Option<String>,
    strict_fingerprint: Option<String>,
    seen: Arc<Mutex<Option<Observed>>>,
}

impl Handler for KeyHandler {
    type Error = SessFail;

    async fn check_server_key(&mut self, server_public_key: &russh::keys::PublicKey) -> std::result::Result<bool, Self::Error> {
        let fingerprint = format!("{}", server_public_key.fingerprint(russh::keys::HashAlg::Sha256));
        if let Some(permit) = &self.strict_fingerprint {
            if crate::remote::strict_permit_fingerprint(permit, &fingerprint).is_err() {
                return Err(SessFail::PermitMismatch);
            }
            if let Ok(mut slot) = self.seen.lock() {
                *slot = Some(Observed {
                    fingerprint,
                    status: HostKeyStatus::Match,
                });
            }
            return Ok(true);
        }
        let decision = classify_presented_key(
            self.saved.as_deref(),
            self.cleared,
            &self.file_fps,
            &fingerprint,
            self.accept_new,
            self.confirmed.as_deref(),
        );
        let status = match &decision {
            PresentedKey::Accept(status) => *status,
            PresentedKey::Reject(_) => HostKeyStatus::Changed,
            PresentedKey::Confirm(_) => HostKeyStatus::New,
        };
        if let Ok(mut slot) = self.seen.lock() {
            *slot = Some(Observed { fingerprint: fingerprint.clone(), status });
        }
        // 拒绝和待确认都在这里返回。`authenticate` 只在连接成功之后调用，所以口令不会发出去。
        match decision {
            PresentedKey::Accept(_) => Ok(true),
            PresentedKey::Reject(fp) => Err(SessFail::Changed(fp)),
            PresentedKey::Confirm(fp) => Err(SessFail::Confirm(fp)),
        }
    }
}

fn block_on<T>(fut: impl std::future::Future<Output = T>) -> Result<T> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|err| Error::new(ErrorCode::Internal, err.to_string()))?;
    Ok(runtime.block_on(fut))
}

fn map_net(err: impl std::fmt::Display) -> Error {
    let _ = err;
    Error::new(ErrorCode::Net, "SSH 连接失败")
}

fn map_sftp(err: russh_sftp::client::error::Error) -> Error {
    match err {
        russh_sftp::client::error::Error::Status(status) => match status.status_code {
            StatusCode::NoSuchFile => Error::new(ErrorCode::NotFound, "远程文件不存在"),
            StatusCode::PermissionDenied => Error::new(ErrorCode::Permission, "远程文件没有权限"),
            _ => Error::new(ErrorCode::Io, "SFTP 操作失败"),
        },
        russh_sftp::client::error::Error::Timeout => Error::new(ErrorCode::Net, "SFTP 超时"),
        _ => Error::new(ErrorCode::Io, "SFTP 操作失败"),
    }
}

fn status_name(status: HostKeyStatus) -> &'static str {
    match status {
        HostKeyStatus::New => "new",
        HostKeyStatus::Match => "match",
        HostKeyStatus::Changed => "changed",
    }
}

pub struct RemoteSession {
    handle: russh::client::Handle<KeyHandler>,
    seen: Arc<Mutex<Option<Observed>>>,
    sftp: RawSftpSession,
    posix_rename: bool,
    pub log: RuntimeLog,
}

impl RemoteSession {
    pub async fn connect(req: &ProbeRequest) -> Result<Self> {
        if req.proxy_jump.as_ref().is_some_and(|jump| !jump.is_empty()) {
            return Err(Error::new(ErrorCode::Unsupported, PROXY_REASON));
        }
        let db = exec::open_db(Path::new(&req.db_path))?;
        let known = PathBuf::from(&req.known_hosts_path);
        let saved = saved_fingerprint(&db, &req.host, req.port)?;
        let cleared = fingerprint_was_cleared(&db, &req.host, req.port)?;
        let file_fps = fingerprints_in_file(&known, &req.host, req.port)?;
        let seen = Arc::new(Mutex::new(None));
        let handler = KeyHandler {
            saved,
            cleared,
            file_fps,
            accept_new: req.accept_new,
            confirmed: req.confirmed_fingerprint.clone(),
            strict_fingerprint: req.strict_fingerprint.clone(),
            seen: seen.clone(),
        };
        let config = Arc::new(russh::client::Config {
            inactivity_timeout: Some(Duration::from_secs(60)),
            ..Default::default()
        });
        let connected = tokio::time::timeout(
            CONNECT_TIMEOUT,
            russh::client::connect(config, (req.host.as_str(), req.port), handler),
        )
        .await
        .map_err(|_| Error::new(ErrorCode::Net, "连接超时"))?
        .map_err(|err| match err {
            SessFail::Changed(fp) => Error::new(
                ErrorCode::HostkeyChanged,
                format!("主机指纹变了，需要清除指纹后重新确认：{fp}"),
            ),
            SessFail::PermitMismatch => {
                Error::new(ErrorCode::HostkeyChanged, crate::remote::COMMIT_FINGERPRINT_MISMATCH)
            }
            SessFail::Confirm(fp) => Error::new(ErrorCode::HostkeyChanged, format!("需要确认主机指纹：{fp}")),
            SessFail::Ssh(err) => {
                let _ = err;
                map_net("ssh")
            }
        })?;
        let mut handle = connected;
        authenticate(&mut handle, req).await?;
        let channel = handle.channel_open_session().await.map_err(|_| sftp_unavailable())?;
        channel.request_subsystem(true, "sftp").await.map_err(|_| sftp_unavailable())?;
        let raw = RawSftpSession::new(channel.into_stream());
        let version = raw.init().await.map_err(|_| sftp_unavailable())?;
        let posix_rename = version.extensions.contains_key(POSIX_RENAME);
        Ok(Self {
            handle,
            seen,
            sftp: raw,
            posix_rename,
            log: RuntimeLog::default(),
        })
    }

    fn observed(&self) -> (String, String) {
        let Some(guard) = self.seen.lock().ok() else {
            return (String::new(), "match".into());
        };
        match guard.as_ref() {
            Some(obs) => (obs.fingerprint.clone(), status_name(obs.status).into()),
            None => (String::new(), "match".into()),
        }
    }

    pub fn posix_rename(&self) -> bool {
        self.posix_rename
    }

    pub async fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        let opened = self
            .sftp
            .open(path, OpenFlags::READ, FileAttributes::empty())
            .await
            .map_err(map_sftp)?;
        let mut out = Vec::new();
        let mut offset = 0u64;
        loop {
            match self.sftp.read(&opened.handle, offset, 32 * 1024).await {
                Ok(data) => {
                    if data.data.is_empty() {
                        break;
                    }
                    offset += data.data.len() as u64;
                    out.extend(data.data);
                }
                Err(russh_sftp::client::error::Error::Status(status)) if status.status_code == StatusCode::Eof => {
                    break;
                }
                Err(err) => {
                    let _ = self.sftp.close(&opened.handle).await;
                    return Err(map_sftp(err));
                }
            }
        }
        let _ = self.sftp.close(&opened.handle).await;
        Ok(out)
    }

    pub async fn write_file(&self, _path: &str, _bytes: &[u8]) -> Result<()> {
        Err(remote::remote_write_unsupported())
    }

    pub async fn exists(&self, path: &str) -> Result<bool> {
        match self.sftp.lstat(path).await {
            Ok(_) => Ok(true),
            Err(russh_sftp::client::error::Error::Status(status)) if status.status_code == StatusCode::NoSuchFile => {
                Ok(false)
            }
            Err(err) => Err(map_sftp(err)),
        }
    }

    /// 备份是 SFTP 读再写。第一版不写远程文件。
    pub async fn copy_file(&self, _from: &str, _to: &str) -> Result<()> {
        Err(remote::remote_write_unsupported())
    }

    pub async fn replace(&mut self, _temp: &str, _dest: &str, _backup: &str) -> Result<()> {
        Err(remote::remote_write_unsupported())
    }

    /// 只执行已经过白名单的 `sha256sum`。命令不存在时返回 `E_REMOTE_HASH_UNAVAILABLE`。
    pub async fn exec_sha256sum(&mut self, cmd: &str) -> Result<String> {
        if !remote::command_allowed(cmd) {
            return Err(Error::new(ErrorCode::Unsupported, "服务器上只允许执行 sha256sum"));
        }
        self.log.record_remote_command(cmd);
        let mut channel = self.handle.channel_open_session().await.map_err(map_net)?;
        channel.exec(true, cmd.as_bytes().to_vec()).await.map_err(map_net)?;
        let mut out = Vec::new();
        let mut code = None;
        let mut eof = false;
        loop {
            match channel.wait().await {
                Some(russh::ChannelMsg::Data { data }) => out.extend_from_slice(&data),
                Some(russh::ChannelMsg::ExitStatus { exit_status }) => code = Some(exit_status),
                Some(russh::ChannelMsg::Eof) => eof = true,
                Some(russh::ChannelMsg::Close) | None => break,
                Some(_) => {}
            }
            if eof && code.is_some() {
                break;
            }
        }
        if code != Some(0) {
            return Err(Error::new(
                ErrorCode::RemoteHashUnavailable,
                "服务器上算不了哈希，改为通过 SFTP 读回本地计算",
            ));
        }
        String::from_utf8(out).map_err(|_| Error::new(ErrorCode::Io, "sha256sum 输出不是文本"))
    }

    pub async fn hashes(&mut self, paths: &[String]) -> Result<Vec<(String, String)>> {
        let plan = remote::plan_remote_hashes(paths)?;
        let mut found = Vec::new();
        if let Some(cmd) = plan.command.clone() {
            match self.exec_sha256sum(&cmd).await {
                Ok(text) => found.extend(parse_sha256sum_output(&text)),
                Err(err) if err.code == ErrorCode::RemoteHashUnavailable => {
                    let quoted = paths.iter().filter(|p| !plan.sftp_paths.iter().any(|s| s == *p)).cloned().collect::<Vec<_>>();
                    found.extend(self.hash_by_sftp(&quoted).await?);
                }
                Err(err) => return Err(err),
            }
        }
        found.extend(self.hash_by_sftp(&plan.sftp_paths).await?);
        Ok(found)
    }

    async fn hash_by_sftp(&self, paths: &[String]) -> Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        for path in paths {
            let bytes = self.read_file(path).await?;
            out.push((path.clone(), hashutil::sha256_hex(&bytes)));
        }
        Ok(out)
    }

    pub async fn close(self) -> Result<()> {
        self.handle
            .disconnect(russh::Disconnect::ByApplication, "", "")
            .await
            .map_err(map_net)
    }

    /// 确认之后才写。先把远端原文件落到本机 `bak/<id>/`，再写临时文件并改名。
    /// 成功返回时，远端不应再留下 `.htx-` 临时文件。
    pub async fn commit_overwrite(
        &mut self,
        req: crate::remote_write::RemoteWriteRequest,
    ) -> Result<crate::remote_write::CommitResult> {
        let dest = req.remote_path.clone();
        let original = if self.exists(&dest).await? { Some(self.read_file(&dest).await?) } else { None };
        let staged = crate::remote_write::stage_before_write(&req, original.as_deref())?;
        let temp = crate::remote_write::remote_temp_path(&dest)?;
        let backup = crate::remote_write::remote_backup_name(&dest)?;
        if let Err(err) = self.write_new(&temp, &req.bytes).await {
            let _ = self.remove_quiet(&temp).await;
            return Err(err);
        }
        let held = self.replace_temp(&temp, &dest, &backup, req.posix_rename).await?;
        let got = self.read_file(&dest).await?;
        let anchor = staged.backup_file.clone().unwrap_or_else(|| staged.backup_dir.clone());
        crate::remote_write::verify_after_rename(&got, &req.bytes, &dest, &anchor)?;
        if held {
            self.remove_quiet(&backup).await?;
        }
        if self.exists(&temp).await? || self.exists(&backup).await? {
            return Err(Error::new(ErrorCode::Io, "远程临时文件还在"));
        }
        Ok(crate::remote_write::CommitResult::Written(staged))
    }

    /// 给壳层用的同步入口。口令在返回前清掉。
    pub fn commit_overwrite_blocking(
        req: &mut ProbeRequest,
        exec_id: &str,
        app_data_dir: PathBuf,
    ) -> Result<crate::remote_write::CommitResult> {
        let granted = match crate::remote_write::take_granted_write(exec_id, crate::remote_write::now_ms()) {
            Ok(granted) => granted,
            Err(err) => {
                req.secret.zeroize();
                return Err(err);
            }
        };
        if !crate::remote_write::grant_binds(&granted.connection, &req.host, req.port, &req.user, &granted.connection.remote_path) {
            req.secret.zeroize();
            return Err(Error::new(ErrorCode::InvalidState, "写入确认和主机或路径不一致"));
        }
        lock_commit_fingerprint(req, &granted.connection.fingerprint);
        let write = crate::remote_write::request_from_grant(exec_id, &app_data_dir, true, 0, granted)?;
        let result = block_on(async {
            let connected = RemoteSession::connect(req).await;
            let mut session = match connected {
                Ok(session) => session,
                Err(err) => {
                    return Err(err);
                }
            };
            let committed = session.commit_overwrite(write).await?;
            let _ = session.close().await;
            Ok(committed)
        });
        if result.as_ref().is_err() {
            let _ = crate::remote_write::void_granted_write(exec_id);
        }
        req.secret.zeroize();
        result.and_then(|inner| inner)
    }

    /// 同一个连接里按顺序写。某一项失败就停，后面的文件保持原样。
    pub fn commit_batch_blocking(
        req: &mut ProbeRequest,
        exec_id: &str,
        app_data_dir: PathBuf,
    ) -> Result<crate::remote_write::RemoteBatchReport> {
        let granted = match crate::remote_write::take_batch(exec_id, crate::remote_write::now_ms()) {
            Ok(granted) => granted,
            Err(err) => {
                req.secret.zeroize();
                return Err(err);
            }
        };
        if granted.host != req.host || granted.port != req.port || granted.user != req.user {
            req.secret.zeroize();
            return Err(Error::new(ErrorCode::InvalidState, "写入确认和主机不一致"));
        }
        lock_commit_fingerprint(req, &granted.fingerprint);
        let host = granted.host.clone();
        let user = granted.user.clone();
        let port = granted.port;
        let paths: Vec<String> = granted.files.iter().map(|file| file.remote_path.clone()).collect();
        let backup_dir = crate::remote_write::tx_backup_dir(&app_data_dir, exec_id)?.display().to_string();
        let result = block_on(async {
            let connected = RemoteSession::connect(req).await;
            let mut session = match connected {
                Ok(session) => session,
                Err(err) => {
                    let failed = if paths.is_empty() { None } else { Some((0, err.code_str().to_string())) };
                    return Ok(crate::remote_write::assemble_report(&backup_dir, &paths, failed));
                }
            };
            let mut failed = None;
            for (index, file) in granted.files.into_iter().enumerate() {
                let write = match crate::remote_write::request_from_stored(
                    exec_id,
                    &app_data_dir,
                    true,
                    index as u32,
                    &host,
                    port,
                    &user,
                    file,
                ) {
                    Ok(write) => write,
                    Err(err) => {
                        failed = Some((index, err.code_str().to_string()));
                        break;
                    }
                };
                if let Err(err) = session.commit_overwrite(write).await {
                    failed = Some((index, err.code_str().to_string()));
                    break;
                }
            }
            let _ = session.close().await;
            Ok(crate::remote_write::assemble_report(&backup_dir, &paths, failed))
        });
        req.secret.zeroize();
        result.and_then(|inner| inner)
    }

    async fn write_new(&self, path: &str, bytes: &[u8]) -> Result<()> {
        let flags = OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE;
        let opened = self.sftp.open(path, flags, FileAttributes::empty()).await.map_err(map_sftp)?;
        let mut offset = 0usize;
        while offset < bytes.len() {
            let end = (offset + 32 * 1024).min(bytes.len());
            if let Err(err) = self.sftp.write(&opened.handle, offset as u64, bytes[offset..end].to_vec()).await {
                let _ = self.sftp.close(&opened.handle).await;
                let _ = self.sftp.remove(path).await;
                return Err(map_sftp(err));
            }
            offset = end;
        }
        self.sftp.close(&opened.handle).await.map_err(map_sftp)?;
        Ok(())
    }

    async fn replace_temp(&mut self, temp: &str, dest: &str, backup: &str, posix_rename: bool) -> Result<bool> {
        let use_posix = posix_rename && self.posix_rename;
        log_rename_steps(&mut self.log, use_posix);
        if use_posix {
            self.posix_rename_over(temp, dest).await?;
            return Ok(false);
        }
        let had_dest = self.exists(dest).await?;
        if had_dest {
            self.rename_path(dest, backup).await?;
        }
        if let Err(err) = self.rename_path(temp, dest).await {
            if had_dest {
                self.rename_path(backup, dest).await?;
            }
            self.remove_quiet(temp).await?;
            return Err(err);
        }
        Ok(had_dest)
    }

    async fn posix_rename_over(&self, from: &str, to: &str) -> Result<()> {
        let mut data = ssh_string(from);
        data.extend(ssh_string(to));
        let packet = self.sftp.extended(POSIX_RENAME, data).await.map_err(map_sftp)?;
        match packet {
            russh_sftp::protocol::Packet::Status(status) if status.status_code == StatusCode::Ok => Ok(()),
            russh_sftp::protocol::Packet::Status(status) => {
                Err(map_sftp(russh_sftp::client::error::Error::Status(status)))
            }
            _ => Err(Error::new(ErrorCode::Io, "SFTP 操作失败")),
        }
    }

    async fn rename_path(&self, from: &str, to: &str) -> Result<()> {
        self.sftp.rename(from, to).await.map_err(map_sftp)?;
        Ok(())
    }

    async fn remove_quiet(&self, path: &str) -> Result<()> {
        match self.sftp.remove(path).await {
            Ok(_) => Ok(()),
            Err(russh_sftp::client::error::Error::Status(status)) if status.status_code == StatusCode::NoSuchFile => Ok(()),
            Err(err) => Err(map_sftp(err)),
        }
    }
}

fn ssh_string(value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    let mut out = (bytes.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(bytes);
    out
}

/// 提交只比对许可证里的指纹，并关掉探测用的信任确认。
fn lock_commit_fingerprint(req: &mut ProbeRequest, permit_fp: &str) {
    req.strict_fingerprint = Some(permit_fp.to_string());
    req.accept_new = false;
    req.confirmed_fingerprint = None;
}

async fn authenticate(handle: &mut russh::client::Handle<KeyHandler>, req: &ProbeRequest) -> Result<()> {
    if req.auth == "agent" {
        return Err(Error::new(ErrorCode::Unsupported, "ssh-agent 这一版还不能用来连接"));
    }
    let result = if req.auth == "key" {
        let path = req.key_path.as_deref().filter(|p| !p.is_empty()).ok_or_else(|| {
            Error::new(ErrorCode::Auth, "没有私钥路径")
        })?;
        let passphrase = if req.secret.is_empty() { None } else { Some(req.secret.as_str()) };
        let key = russh::keys::load_secret_key(path, passphrase).map_err(|_| Error::new(ErrorCode::Auth, "私钥无法读取"))?;
        handle
            .authenticate_publickey(req.user.as_str(), PrivateKeyWithHashAlg::new(Arc::new(key), None))
            .await
            .map_err(|_| Error::new(ErrorCode::Auth, "身份认证失败"))?
    } else {
        handle
            .authenticate_password(req.user.as_str(), req.secret.as_str())
            .await
            .map_err(|_| Error::new(ErrorCode::Auth, "身份认证失败"))?
    };
    match result {
        russh::client::AuthResult::Success => Ok(()),
        russh::client::AuthResult::Failure { .. } => Err(Error::new(ErrorCode::Auth, "身份认证失败")),
    }
}

/// 连接测试。跳板机直接拒绝。没有 SFTP 报 `E_SFTP_UNAVAILABLE`。
/// 没有 `sha256sum` 不算失败。
pub fn probe_connection(req: &mut ProbeRequest) -> Result<ProbeReport> {
    let result = block_on(probe_async(req)).and_then(|inner| inner);
    req.secret.zeroize();
    result
}

async fn probe_async(req: &ProbeRequest) -> Result<ProbeReport> {
    if req.proxy_jump.as_ref().is_some_and(|jump| !jump.is_empty()) {
        return Err(Error::new(ErrorCode::Unsupported, PROXY_REASON));
    }
    let session = match RemoteSession::connect(req).await {
        Ok(session) => session,
        Err(err) if err.message.starts_with("需要确认主机指纹") => {
            let fingerprint = err.message.rsplit('：').next().unwrap_or("").to_string();
            return Ok(ProbeReport {
                fingerprint,
                host_key: "new".into(),
                auth_ok: false,
                sftp_ok: false,
                sha256sum_ok: false,
            });
        }
        Err(err) => return Err(err),
    };
    let (fingerprint, host_key) = session.observed();
    let sha256sum_ok = session_sha_probe(session).await?;
    Ok(ProbeReport {
        fingerprint,
        host_key,
        auth_ok: true,
        sftp_ok: true,
        sha256sum_ok,
    })
}

async fn session_sha_probe(mut session: RemoteSession) -> Result<bool> {
    let cmd = "sha256sum -- /etc/os-release";
    let ok = session.exec_sha256sum(cmd).await.is_ok();
    let _ = session.close().await;
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct Mem {
        files: BTreeMap<String, Vec<u8>>,
        ops: Vec<String>,
    }

    impl RemoteNames for Mem {
        fn exists(&mut self, path: &str) -> Result<bool> {
            Ok(self.files.contains_key(path))
        }
        fn rename(&mut self, from: &str, to: &str) -> Result<()> {
            self.ops.push(format!("rename {from} {to}"));
            let bytes = self.files.remove(from).ok_or_else(|| Error::new(ErrorCode::NotFound, "missing"))?;
            self.files.insert(to.to_string(), bytes);
            Ok(())
        }
        fn remove(&mut self, path: &str) -> Result<()> {
            self.ops.push(format!("remove {path}"));
            self.files.remove(path);
            Ok(())
        }
    }

    #[test]
    fn rename_falls_back_when_posix_rename_is_missing() {
        let mut mem = Mem {
            files: BTreeMap::from([
                ("dir/a.txt".into(), b"old".to_vec()),
                ("dir/.a.txt.htx-1".into(), b"new".to_vec()),
            ]),
            ops: Vec::new(),
        };
        let mut log = RuntimeLog::default();
        let held = apply_rename(&mut mem, &mut log, false, "dir/.a.txt.htx-1", "dir/a.txt", "dir/a.txt.htx-bak").unwrap();
        assert!(held);
        assert_eq!(
            mem.ops,
            vec![
                "rename dir/a.txt dir/a.txt.htx-bak".to_string(),
                "rename dir/.a.txt.htx-1 dir/a.txt".into(),
            ]
        );
        assert_eq!(mem.files.get("dir/a.txt").map(Vec::as_slice), Some(b"new".as_slice()));
        assert_eq!(mem.files.get("dir/a.txt.htx-bak").map(Vec::as_slice), Some(b"old".as_slice()));
        discard_verified_backup(&mut mem, "dir/a.txt.htx-bak").unwrap();
        assert!(!mem.files.contains_key("dir/a.txt.htx-bak"));
        assert_eq!(log.lines[0].category, "sftp-rename");
        assert_eq!(log.lines[0].message, "rename-backup → rename-replace → delete-backup");
        assert!(!log.lines[0].message.contains("rename:"));

        let mut log = RuntimeLog::default();
        let mut mem = Mem {
            files: BTreeMap::from([("t".into(), b"n".to_vec())]),
            ops: Vec::new(),
        };
        assert!(!apply_rename(&mut mem, &mut log, true, "t", "a", "b").unwrap());
        assert_eq!(log.lines[0].message, "posix-rename");
        assert_eq!(mem.ops, vec!["rename t a".to_string()]);
    }

    #[test]
    fn backup_dir_and_hash_lines() {
        assert_eq!(
            remote_backup_dir("/home/tester", "exec-1").unwrap(),
            "/home/tester/.hetianxia-backups/exec-1"
        );
        assert!(remote_backup_dir("/home/tester", "../x").is_err());
        let parsed = parse_sha256sum_output("abc\n0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  a b.txt\n");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "a b.txt");
    }

    #[test]
    fn proxy_jump_never_opens_a_socket() {
        let mut req = ProbeRequest {
            host: "10.1.0.4".into(),
            port: 22,
            user: "ops".into(),
            proxy_jump: Some("jump.example".into()),
            auth: "password".into(),
            secret: "pw-do-not-leak-9f3a".into(),
            account: None,
            key_path: None,
            known_hosts_path: "/tmp/does-not-matter".into(),
            db_path: std::env::temp_dir().join("hetianxia-probe-unused.sqlite").display().to_string(),
            accept_new: false,
            confirmed_fingerprint: None,
            strict_fingerprint: None,
        };
        let err = probe_connection(&mut req).unwrap_err();
        assert_eq!(err.code_str(), "E_UNSUPPORTED");
        assert_eq!(err.message, "需要跳板机，第二版支持");
        assert!(req.secret.is_empty());
        assert!(!format!("{err}").contains("pw-do-not-leak-9f3a"));
        let report = ProbeReport {
            fingerprint: "SHA256:abc".into(),
            host_key: "new".into(),
            auth_ok: false,
            sftp_ok: false,
            sha256sum_ok: false,
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("pw-do-not-leak-9f3a"));
        assert_eq!(remote::remote_write_unsupported().code_str(), "E_INVALID_STATE");
    }

    #[test]
    fn commit_locks_the_permit_fingerprint_and_drops_the_trust_prompt() {
        let mut req = ProbeRequest {
            host: "web.example".into(),
            port: 22,
            user: "deploy".into(),
            proxy_jump: None,
            auth: "key".into(),
            secret: String::new(),
            account: None,
            key_path: None,
            known_hosts_path: String::new(),
            db_path: String::new(),
            accept_new: true,
            confirmed_fingerprint: Some("SHA256:clicked-later".into()),
            strict_fingerprint: None,
        };
        lock_commit_fingerprint(&mut req, "SHA256:permit");
        assert_eq!(req.strict_fingerprint.as_deref(), Some("SHA256:permit"));
        assert!(!req.accept_new);
        assert!(req.confirmed_fingerprint.is_none());
        let err = crate::remote::strict_permit_fingerprint("SHA256:permit", "SHA256:server").unwrap_err();
        assert_eq!(err.message, "服务器指纹已变化");
        assert!(!err.message.contains("需要确认"));
    }
}
