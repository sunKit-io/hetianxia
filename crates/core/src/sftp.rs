//! 远程会话。只执行 `sha256sum`，文件读写走 SFTP。
//! 不支持 `posix-rename` 时按「改名备份 → 改名替换 → 删除备份」退回，并写入运行日志。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client::Handler;
use russh::keys::PrivateKeyWithHashAlg;
use russh_sftp::client::RawSftpSession;
use russh_sftp::protocol::{FileAttributes, OpenFlags, Packet, StatusCode};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::error::{Error, ErrorCode, Result};
use crate::exec;
use crate::hashutil;
use crate::remote::{
    self, decide_host_key, fingerprints_in_file, fingerprint_was_cleared, saved_fingerprint, HostKeyStatus,
    RuntimeLog, PROXY_REASON,
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
    /// `password` 或 `key`。
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
pub fn apply_rename(
    ops: &mut dyn RemoteNames,
    log: &mut RuntimeLog,
    posix_rename: bool,
    temp: &str,
    dest: &str,
    backup: &str,
) -> Result<()> {
    log_rename_steps(log, posix_rename);
    if posix_rename {
        return ops.rename(temp, dest);
    }
    if ops.exists(dest)? {
        ops.rename(dest, backup)?;
    }
    ops.rename(temp, dest)?;
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
    seen: Arc<Mutex<Option<Observed>>>,
}

impl Handler for KeyHandler {
    type Error = SessFail;

    async fn check_server_key(&mut self, server_public_key: &russh::keys::PublicKey) -> std::result::Result<bool, Self::Error> {
        let fingerprint = format!("{}", server_public_key.fingerprint(russh::keys::HashAlg::Sha256));
        let status = decide_host_key(self.saved.as_deref(), self.cleared, &self.file_fps, &fingerprint);
        if let Ok(mut slot) = self.seen.lock() {
            *slot = Some(Observed { fingerprint: fingerprint.clone(), status });
        }
        match status {
            HostKeyStatus::Changed => Err(SessFail::Changed(fingerprint)),
            HostKeyStatus::New if !self.accept_new => Err(SessFail::Confirm(fingerprint)),
            HostKeyStatus::New | HostKeyStatus::Match => Ok(true),
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

    pub async fn write_file(&self, path: &str, bytes: &[u8]) -> Result<()> {
        let flags = OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE;
        let opened = self.sftp.open(path, flags, FileAttributes::empty()).await.map_err(map_sftp)?;
        let mut offset = 0usize;
        while offset < bytes.len() {
            let end = (offset + 32 * 1024).min(bytes.len());
            self.sftp
                .write(&opened.handle, offset as u64, bytes[offset..end].to_vec())
                .await
                .map_err(map_sftp)?;
            offset = end;
        }
        self.sftp.close(&opened.handle).await.map_err(map_sftp)?;
        Ok(())
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

    /// 备份是 SFTP 读再写，不在服务器上执行 `cp`。
    pub async fn copy_file(&self, from: &str, to: &str) -> Result<()> {
        let bytes = self.read_file(from).await?;
        self.write_file(to, &bytes).await
    }

    pub async fn replace(&mut self, temp: &str, dest: &str, backup: &str) -> Result<()> {
        log_rename_steps(&mut self.log, self.posix_rename);
        if self.posix_rename {
            return self.posix_rename_to(temp, dest).await;
        }
        if self.exists(dest).await? {
            self.sftp.rename(dest, backup).await.map_err(map_sftp)?;
        }
        self.sftp.rename(temp, dest).await.map_err(map_sftp)?;
        if self.exists(backup).await? {
            self.sftp.remove(backup).await.map_err(map_sftp)?;
        }
        Ok(())
    }

    async fn posix_rename_to(&self, from: &str, to: &str) -> Result<()> {
        #[derive(Serialize)]
        struct PosixRenameExt {
            oldpath: String,
            newpath: String,
        }
        let data = russh_sftp::ser::to_bytes(&PosixRenameExt {
            oldpath: from.to_string(),
            newpath: to.to_string(),
        })
        .map_err(|_| Error::new(ErrorCode::Io, "无法编码远程改名"))?;
        match self.sftp.extended(POSIX_RENAME, data.to_vec()).await.map_err(map_sftp)? {
            Packet::Status(status) if status.status_code == StatusCode::Ok => Ok(()),
            Packet::Status(_) => Err(Error::new(ErrorCode::Io, "远程改名失败")),
            Packet::ExtendedReply(_) => Ok(()),
            _ => Err(Error::new(ErrorCode::Io, "远程改名失败")),
        }
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
}

async fn authenticate(handle: &mut russh::client::Handle<KeyHandler>, req: &ProbeRequest) -> Result<()> {
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
        apply_rename(&mut mem, &mut log, false, "dir/.a.txt.htx-1", "dir/a.txt", "dir/a.txt.htx-bak").unwrap();
        assert_eq!(
            mem.ops,
            vec![
                "rename dir/a.txt dir/a.txt.htx-bak".to_string(),
                "rename dir/.a.txt.htx-1 dir/a.txt".into(),
                "remove dir/a.txt.htx-bak".into(),
            ]
        );
        assert_eq!(mem.files.get("dir/a.txt").map(Vec::as_slice), Some(b"new".as_slice()));
        assert!(!mem.files.contains_key("dir/a.txt.htx-bak"));
        assert_eq!(log.lines[0].category, "sftp-rename");
        assert_eq!(log.lines[0].message, "rename-backup → rename-replace → delete-backup");
        assert!(!log.lines[0].message.contains("rename:"));

        let mut log = RuntimeLog::default();
        let mut mem = Mem {
            files: BTreeMap::from([("t".into(), b"n".to_vec())]),
            ops: Vec::new(),
        };
        apply_rename(&mut mem, &mut log, true, "t", "a", "b").unwrap();
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
            secret: "nope".into(),
            account: None,
            key_path: None,
            known_hosts_path: "/tmp/does-not-matter".into(),
            db_path: std::env::temp_dir().join("hetianxia-probe-unused.sqlite").display().to_string(),
            accept_new: false,
        };
        let err = probe_connection(&mut req).unwrap_err();
        assert_eq!(err.code_str(), "E_UNSUPPORTED");
        assert_eq!(err.message, "需要跳板机，第二版支持");
        assert!(req.secret.is_empty());
    }
}
