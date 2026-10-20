//! SSH 连接策略。第一版不连跳板机，远程只允许 `sha256sum`，系统识别不执行命令。

use crate::error::{Error, ErrorCode, Result};
use crate::osinfo::{self, RemoteOs};
use rusqlite::{params, Connection};

pub const CLEAR_FINGERPRINT: &str = "清除指纹";
pub const PROXY_BADGE: &str = "第二版支持";
pub const PROXY_REASON: &str = "需要跳板机，第二版支持";
pub const MAX_HASH_PATHS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedHost {
    pub alias: String,
    pub host_name: String,
    pub user: Option<String>,
    pub port: u16,
    pub proxy_jump: Option<String>,
    /// 展开过 `~` 的私钥路径。没有则是空字符串。
    #[serde(default)]
    pub identity_file: String,
}

impl ImportedHost {
    pub fn available(&self) -> bool {
        self.proxy_jump.is_none()
    }
    pub fn badge(&self) -> Option<&'static str> {
        if self.available() { None } else { Some(PROXY_BADGE) }
    }
    pub fn reason(&self) -> Option<&'static str> {
        if self.available() { None } else { Some(PROXY_REASON) }
    }
}

/// 解析 OpenSSH config 里的 Host 块。含 ProxyJump 的主机照常导入，但标成不可用。
pub fn parse_ssh_config(text: &str) -> Vec<ImportedHost> {
    parse_ssh_config_with_home(text, None)
}

fn is_wildcard_host(pattern: &str) -> bool {
    pattern.is_empty() || pattern.contains('*') || pattern.contains('?')
}

fn unquote(value: &str) -> &str {
    let value = value.trim();
    let bytes = value.as_bytes();
    if value.len() >= 2
        && ((bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\''))
    {
        return &value[1..value.len() - 1];
    }
    value
}

fn expand_tilde(value: &str, home: Option<&std::path::Path>) -> String {
    let value = unquote(value);
    let Some(home) = home else {
        return value.to_string();
    };
    if value == "~" {
        return home.display().to_string();
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return home.join(rest).display().to_string();
    }
    value.to_string()
}

/// 只读解析。`home` 用来展开 IdentityFile 里的 `~`。通配 Host（`*`、`?`）整段忽略。
pub fn parse_ssh_config_with_home(text: &str, home: Option<&std::path::Path>) -> Vec<ImportedHost> {
    let mut out = Vec::new();
    let mut cur: Option<ImportedHost> = None;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let key = parts.next().unwrap_or("");
        let value = parts.collect::<Vec<_>>().join(" ");
        if key.eq_ignore_ascii_case("Host") {
            if let Some(done) = cur.take() {
                if !is_wildcard_host(&done.alias) {
                    out.push(done);
                }
            }
            let alias = value
                .split_whitespace()
                .find(|pattern| !is_wildcard_host(pattern))
                .unwrap_or("")
                .to_string();
            if alias.is_empty() {
                cur = None;
                continue;
            }
            cur = Some(ImportedHost {
                alias: alias.clone(),
                host_name: alias,
                user: None,
                port: 22,
                proxy_jump: None,
                identity_file: String::new(),
            });
            continue;
        }
        let Some(host) = cur.as_mut() else { continue };
        if key.eq_ignore_ascii_case("HostName") {
            host.host_name = value;
        } else if key.eq_ignore_ascii_case("User") {
            host.user = Some(value);
        } else if key.eq_ignore_ascii_case("Port") {
            if let Ok(p) = value.parse() {
                host.port = p;
            }
        } else if key.eq_ignore_ascii_case("ProxyJump") {
            host.proxy_jump = Some(value);
        } else if key.eq_ignore_ascii_case("IdentityFile") && host.identity_file.is_empty() {
            host.identity_file = expand_tilde(&value, home);
        }
    }
    if let Some(done) = cur {
        if !is_wildcard_host(&done.alias) {
            out.push(done);
        }
    }
    out
}

/// 只读取 `~/.ssh/config`，不创建、不改写。
pub fn read_user_ssh_config(home: &std::path::Path) -> Result<Vec<ImportedHost>> {
    let path = home.join(".ssh").join("config");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::new(ErrorCode::NotFound, "没有找到 ~/.ssh/config"));
        }
        Err(err) => return Err(Error::new(ErrorCode::Io, format!("读取 ~/.ssh/config 失败：{err}"))),
    };
    Ok(parse_ssh_config_with_home(&text, Some(home)))
}

pub const COMMIT_FINGERPRINT_MISMATCH: &str = "服务器指纹已变化";

/// 提交写入时只认许可证里的指纹。空指纹或任何不一致都拒绝，不进入信任确认。
pub fn strict_permit_fingerprint(permit: &str, presented: &str) -> Result<()> {
    if !permit.is_empty() && permit == presented {
        Ok(())
    } else {
        Err(Error::new(ErrorCode::HostkeyChanged, COMMIT_FINGERPRINT_MISMATCH))
    }
}

pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS known_hosts (
            host TEXT NOT NULL,
            port INTEGER NOT NULL,
            fingerprint TEXT NOT NULL,
            accepted_at INTEGER,
            PRIMARY KEY (host, port)
        );
        CREATE TABLE IF NOT EXISTS known_host_clears (
            host TEXT NOT NULL,
            port INTEGER NOT NULL,
            PRIMARY KEY (host, port)
        );",
    )?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyStatus {
    /// 没有记录，按首次连接，要再给用户看一次指纹。
    New,
    Match,
    /// 指纹变了，阻止连接，直到用户清除指纹。
    Changed,
}

pub fn check_host_key(conn: &Connection, host: &str, port: u16, fingerprint: &str) -> Result<HostKeyStatus> {
    ensure_schema(conn)?;
    let row: Option<String> = conn
        .query_row(
            "SELECT fingerprint FROM known_hosts WHERE host=?1 AND port=?2",
            params![host, port],
            |r| r.get(0),
        )
        .ok();
    Ok(match row {
        None => HostKeyStatus::New,
        Some(saved) if saved == fingerprint => HostKeyStatus::Match,
        Some(_) => HostKeyStatus::Changed,
    })
}

pub fn trust_host_key(conn: &Connection, host: &str, port: u16, fingerprint: &str, now_ms: i64) -> Result<()> {
    ensure_schema(conn)?;
    conn.execute(
        "INSERT INTO known_hosts (host, port, fingerprint, accepted_at) VALUES (?1,?2,?3,?4)
         ON CONFLICT(host, port) DO UPDATE SET fingerprint=excluded.fingerprint, accepted_at=excluded.accepted_at",
        params![host, port, fingerprint, now_ms],
    )?;
    conn.execute("DELETE FROM known_host_clears WHERE host=?1 AND port=?2", params![host, port])?;
    Ok(())
}

/// 清除后下次连接按首次连接处理，需要重新确认指纹。
/// 只删应用自己的记录。`~/.ssh/known_hosts` 不写。
pub fn clear_fingerprint(conn: &Connection, host: &str, port: u16) -> Result<()> {
    ensure_schema(conn)?;
    conn.execute("DELETE FROM known_hosts WHERE host=?1 AND port=?2", params![host, port])?;
    conn.execute(
        "INSERT INTO known_host_clears (host, port) VALUES (?1, ?2)
         ON CONFLICT(host, port) DO NOTHING",
        params![host, port],
    )?;
    Ok(())
}

pub(crate) fn saved_fingerprint(conn: &Connection, host: &str, port: u16) -> Result<Option<String>> {
    ensure_schema(conn)?;
    let row = conn
        .query_row(
            "SELECT fingerprint FROM known_hosts WHERE host=?1 AND port=?2",
            params![host, port],
            |r| r.get(0),
        )
        .ok();
    Ok(row)
}

pub(crate) fn fingerprint_was_cleared(conn: &Connection, host: &str, port: u16) -> Result<bool> {
    ensure_schema(conn)?;
    let hit = conn
        .query_row(
            "SELECT 1 FROM known_host_clears WHERE host=?1 AND port=?2",
            params![host, port],
            |_| Ok(()),
        )
        .is_ok();
    Ok(hit)
}

/// 应用记录优先。没有记录时再看文件里的指纹。
/// 用户点过「清除指纹」之后，在重新确认前不采用文件里的旧指纹。
pub fn decide_host_key(saved: Option<&str>, cleared: bool, file_fps: &[String], presented: &str) -> HostKeyStatus {
    if let Some(saved) = saved {
        return if saved == presented { HostKeyStatus::Match } else { HostKeyStatus::Changed };
    }
    if cleared {
        return HostKeyStatus::New;
    }
    if file_fps.iter().any(|fp| fp == presented) {
        return HostKeyStatus::Match;
    }
    if !file_fps.is_empty() {
        return HostKeyStatus::Changed;
    }
    HostKeyStatus::New
}

/// 探测阶段记下的指纹，和重连时服务器出示的指纹。
/// 只有 `Accept` 之后才可以送出口令。指纹被换掉时是 `Reject`，口令不会发出去。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresentedKey {
    Accept(HostKeyStatus),
    Reject(String),
    Confirm(String),
}

impl PresentedKey {
    pub fn sends_credentials(&self) -> bool {
        matches!(self, PresentedKey::Accept(_))
    }
}

/// `accept_new` 时必须带上用户确认过的指纹。服务器出示另一把钥匙就拒绝。
pub fn classify_presented_key(
    saved: Option<&str>,
    cleared: bool,
    file_fps: &[String],
    presented: &str,
    accept_new: bool,
    confirmed: Option<&str>,
) -> PresentedKey {
    if accept_new {
        let want = confirmed.map(str::trim).filter(|value| !value.is_empty());
        return match want {
            Some(want) if want == presented => {
                let status = if saved == Some(presented) {
                    HostKeyStatus::Match
                } else {
                    HostKeyStatus::New
                };
                PresentedKey::Accept(status)
            }
            Some(_) => PresentedKey::Reject(presented.to_string()),
            None => PresentedKey::Confirm(presented.to_string()),
        };
    }
    match decide_host_key(saved, cleared, file_fps, presented) {
        HostKeyStatus::Changed => PresentedKey::Reject(presented.to_string()),
        HostKeyStatus::New => PresentedKey::Confirm(presented.to_string()),
        HostKeyStatus::Match => PresentedKey::Accept(HostKeyStatus::Match),
    }
}

pub fn remote_write_unsupported() -> Error {
    Error::new(ErrorCode::InvalidState, "远程写入还没支持")
}

/// 只读 `known_hosts` 文件，不创建、不追加。
pub fn fingerprints_in_file(path: &std::path::Path, host: &str, port: u16) -> Result<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let keys = russh::keys::known_hosts::known_host_keys_path(host, port, path).map_err(|err| {
        Error::new(ErrorCode::Io, format!("读取 known_hosts 失败：{err}"))
    })?;
    Ok(keys
        .into_iter()
        .map(|(_, key)| format!("{}", key.fingerprint(russh::keys::HashAlg::Sha256)))
        .collect())
}

pub fn host_key_with_file(
    conn: &Connection,
    path: &std::path::Path,
    host: &str,
    port: u16,
    presented: &str,
) -> Result<HostKeyStatus> {
    let saved = saved_fingerprint(conn, host, port)?;
    let cleared = fingerprint_was_cleared(conn, host, port)?;
    let file_fps = fingerprints_in_file(path, host, port)?;
    Ok(decide_host_key(saved.as_deref(), cleared, &file_fps, presented))
}

pub fn reject_if_host_key_changed(status: HostKeyStatus, fingerprint: &str) -> Result<()> {
    if status == HostKeyStatus::Changed {
        Err(Error::new(
            ErrorCode::HostkeyChanged,
            format!("主机指纹变了，需要清除指纹后重新确认：{fingerprint}"),
        ))
    } else {
        Ok(())
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn path_cannot_be_shell_quoted(path: &str) -> bool {
    path.chars().any(|c| c == '\n' || c == '\r' || c == '\0')
}

/// 能放进单引号的路径合成一条 `sha256sum`；换行、回车、空字节无法安全转义，单独走 SFTP。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteHashPlan {
    pub command: Option<String>,
    pub sftp_paths: Vec<String>,
}

pub fn plan_remote_hashes(paths: &[String]) -> Result<RemoteHashPlan> {
    if paths.is_empty() {
        return Err(Error::new(ErrorCode::InvalidLocation, "没有要计算哈希的路径"));
    }
    if paths.len() > MAX_HASH_PATHS {
        return Err(Error::new(ErrorCode::InvalidLocation, format!("一次最多 {MAX_HASH_PATHS} 个路径")));
    }
    let mut quoted = Vec::new();
    let mut sftp_paths = Vec::new();
    for path in paths {
        if path_cannot_be_shell_quoted(path) {
            sftp_paths.push(path.clone());
        } else {
            quoted.push(path.clone());
        }
    }
    let command = if quoted.is_empty() {
        None
    } else {
        let mut cmd = String::from("sha256sum --");
        for path in &quoted {
            cmd.push(' ');
            cmd.push_str(&shell_quote(path));
        }
        if !command_allowed(&cmd) {
            return Err(Error::new(ErrorCode::Unsupported, "生成的命令不在白名单里"));
        }
        Some(cmd)
    };
    Ok(RemoteHashPlan { command, sftp_paths })
}

/// 服务器上唯一允许的命令。路径全部都能转义时返回这一条命令。
pub fn remote_hashes_command(paths: &[String]) -> Result<String> {
    let plan = plan_remote_hashes(paths)?;
    if !plan.sftp_paths.is_empty() {
        return Err(Error::new(ErrorCode::InvalidLocation, "路径里不能有换行或空字节"));
    }
    plan.command.ok_or_else(|| Error::new(ErrorCode::InvalidLocation, "没有要计算哈希的路径"))
}

/// 单引号里的字符按字面量看。`$`、`;`、`uname`、`cp` 只有出现在引号外才拒绝。
pub fn command_allowed(cmd: &str) -> bool {
    let cmd = cmd.trim();
    if !cmd.starts_with("sha256sum -- ") {
        return false;
    }
    let chars: Vec<char> = cmd.chars().collect();
    let mut unquoted = String::new();
    let mut in_single = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_single {
            if c == '\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if c == '\\' {
            let Some(next) = chars.get(i + 1).copied() else { return false };
            unquoted.push(next);
            i += 2;
            continue;
        }
        if c == '\'' {
            in_single = true;
            i += 1;
            continue;
        }
        unquoted.push(c);
        i += 1;
    }
    if in_single {
        return false;
    }
    if unquoted.contains(';')
        || unquoted.contains('|')
        || unquoted.contains('`')
        || unquoted.contains('$')
        || unquoted.contains('\n')
        || unquoted.contains('\r')
    {
        return false;
    }
    let lower = unquoted.to_ascii_lowercase();
    if lower.contains("uname") || lower.contains("sudo") || unquoted.split_whitespace().any(|w| w == "cp") {
        return false;
    }
    true
}

pub fn classify_remote(os_release: Option<&str>, plist: Option<&str>) -> RemoteOs {
    osinfo::classify(os_release, plist)
}

pub const REMOTE_EXEC_CATEGORY: &str = "remote-exec";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub category: String,
    pub message: String,
}

/// 客户端运行日志。服务器上每条命令都记一条 `remote-exec`，路径脱敏。
#[derive(Debug, Default, Clone)]
pub struct RuntimeLog {
    pub lines: Vec<LogLine>,
}

impl RuntimeLog {
    pub fn record_remote_command(&mut self, cmd: &str) {
        self.lines.push(LogLine {
            category: REMOTE_EXEC_CATEGORY.into(),
            message: redact_remote_command(cmd),
        });
    }

    /// SFTP 改名路径。不用 `rename:` 加编号，避免和故障注入阶段串在一起。
    pub fn record_sftp_rename(&mut self, message: &str) {
        self.lines.push(LogLine {
            category: "sftp-rename".into(),
            message: message.to_string(),
        });
    }

    pub fn remote_exec_lines(&self) -> impl Iterator<Item = &LogLine> {
        self.lines.iter().filter(|l| l.category == REMOTE_EXEC_CATEGORY)
    }
}

pub fn redact_remote_command(cmd: &str) -> String {
    let Some(rest) = cmd.strip_prefix("sha256sum -- ") else {
        return cmd.to_string();
    };
    let n = shell_arg_count(rest);
    let mut out = String::from("sha256sum --");
    for _ in 0..n {
        out.push_str(" '<path>'");
    }
    out
}

fn shell_arg_count(s: &str) -> usize {
    let mut n = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        n += 1;
        if c == '\'' {
            while let Some(ch) = chars.next() {
                if ch != '\'' {
                    continue;
                }
                let mut look = chars.clone();
                if look.next() == Some('\\') && look.next() == Some('\'') && look.next() == Some('\'') {
                    chars.next();
                    chars.next();
                    chars.next();
                    continue;
                }
                break;
            }
        } else {
            while chars.peek().is_some_and(|ch| !ch.is_whitespace()) {
                chars.next();
            }
        }
    }
    n
}

/// 准备要在服务器上执行的哈希命令，并写入 `remote-exec` 日志。
/// 无法转义的路径在 `sftp_paths` 里，不进入这条命令，也不让整批失败。
pub fn prepare_remote_hashes(log: &mut RuntimeLog, paths: &[String]) -> Result<RemoteHashPlan> {
    let plan = plan_remote_hashes(paths)?;
    if let Some(cmd) = &plan.command {
        log.record_remote_command(cmd);
    }
    Ok(plan)
}

/// 服务器不能算哈希时不中断扫描，返回 `E_REMOTE_HASH_UNAVAILABLE`，调用方改走 SFTP。
/// 能算哈希时，个别无法转义的文件名仍放在 `sftp_paths`，其余合成一条命令。
pub fn remote_hash_or_sftp(log: &mut RuntimeLog, sha256sum_ok: bool, paths: &[String]) -> Result<RemoteHashPlan> {
    if !sha256sum_ok {
        return Err(Error::new(
            ErrorCode::RemoteHashUnavailable,
            "服务器上算不了哈希，改为通过 SFTP 读回本地计算",
        ));
    }
    prepare_remote_hashes(log, paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osinfo::{system_type_label, UNIDENTIFIED_LABEL};

    #[test]
    fn proxyjump_imports_but_stays_unavailable() {
        let text = "\
Host build-01\n  HostName 10.0.8.15\n  User deploy\n\n\
Host intranet-jump\n  HostName 10.1.0.4\n  User ops\n  ProxyJump jump.example\n";
        let hosts = parse_ssh_config(text);
        assert!(hosts[0].available());
        assert_eq!(hosts[0].badge(), None);
        assert!(!hosts[1].available());
        assert_eq!(hosts[1].badge(), Some("第二版支持"));
        assert_eq!(hosts[1].reason(), Some("需要跳板机，第二版支持"));
    }

    #[test]
    fn clear_fingerprint_makes_the_next_connect_first_time() {
        let dir = tempfile::tempdir().unwrap();
        let conn = Connection::open(dir.path().join("k.sqlite")).unwrap();
        trust_host_key(&conn, "10.0.8.15", 22, "SHA256:aaa", 1).unwrap();
        assert_eq!(check_host_key(&conn, "10.0.8.15", 22, "SHA256:bbb").unwrap(), HostKeyStatus::Changed);
        clear_fingerprint(&conn, "10.0.8.15", 22).unwrap();
        assert_eq!(check_host_key(&conn, "10.0.8.15", 22, "SHA256:bbb").unwrap(), HostKeyStatus::New);
        assert_eq!(CLEAR_FINGERPRINT, "清除指纹");
    }

    #[test]
    fn only_sha256sum_is_allowed_and_uname_cannot_be_built() {
        let cmd = remote_hashes_command(&["a b".into(), "it's".into()]).unwrap();
        assert_eq!(cmd, "sha256sum -- 'a b' 'it'\\''s'");
        assert!(command_allowed(&cmd));
        assert!(!command_allowed("uname -a"));
        assert!(!command_allowed("sudo sha256sum -- 'a'"));
        assert!(!command_allowed("cp a b"));
        assert!(!command_allowed("sha256sum -- 'a'; uname"));
        assert!(!cmd.to_ascii_lowercase().contains("uname"));
        let label = system_type_label(&classify_remote(None, None));
        assert_eq!(label, UNIDENTIFIED_LABEL);
    }

    #[test]
    fn remote_exec_log_redacts_paths_and_keeps_sha256sum() {
        let mut log = RuntimeLog::default();
        let cmd = prepare_remote_hashes(&mut log, &["secret/license.key".into(), "a b".into()])
            .unwrap()
            .command
            .unwrap();
        assert!(cmd.contains("license.key"));
        let line = log.remote_exec_lines().next().unwrap();
        assert_eq!(line.category, "remote-exec");
        assert_eq!(line.message, "sha256sum -- '<path>' '<path>'");
        assert!(!line.message.contains("license"));
        assert!(!line.message.contains("uname"));
        let err = remote_hash_or_sftp(&mut log, false, &["secret/license.key".into()]).unwrap_err();
        assert_eq!(err.code_str(), "E_REMOTE_HASH_UNAVAILABLE");
        assert_eq!(log.remote_exec_lines().count(), 1);
    }

    #[test]
    fn scan17_quotes_shell_metacharacters_and_falls_back_per_file() {
        let paths = vec![
            "Foo$1.class".into(),
            "$(touch pwned).txt".into(),
            "a cp b".into(),
            "pseudo-uname.md".into(),
            "a;uname".into(),
            "ok.txt".into(),
            "has\nnewline".into(),
            "nul\0name".into(),
            "cr\rname".into(),
        ];
        let plan = plan_remote_hashes(&paths).unwrap();
        let cmd = plan.command.as_deref().unwrap();
        assert_eq!(
            cmd,
            "sha256sum -- 'Foo$1.class' '$(touch pwned).txt' 'a cp b' 'pseudo-uname.md' 'a;uname' 'ok.txt'"
        );
        assert!(command_allowed(cmd));
        assert_eq!(plan.sftp_paths, vec!["has\nnewline".to_string(), "nul\0name".into(), "cr\rname".into()]);
        assert!(!command_allowed("sha256sum -- 'a'; uname"));
        assert!(!command_allowed("sha256sum -- 'a' $(touch pwned)"));
        assert!(!command_allowed("sha256sum -- 'a' | uname"));
        let only = plan_remote_hashes(&["a\nb".into()]).unwrap();
        assert!(only.command.is_none());
        assert_eq!(only.sftp_paths, vec!["a\nb".to_string()]);
        assert!(plan_remote_hashes(&["Foo$1.class".into()]).is_ok());
        let mut log = RuntimeLog::default();
        let planned = prepare_remote_hashes(&mut log, &paths).unwrap();
        assert_eq!(planned.sftp_paths.len(), 3);
        let line = log.remote_exec_lines().next().unwrap();
        assert_eq!(line.message, "sha256sum -- '<path>' '<path>' '<path>' '<path>' '<path>' '<path>'");
        assert!(!line.message.contains("Foo$1"));
        assert!(!line.message.contains("pwned"));
        assert!(!line.message.contains("newline"));
        let via = remote_hash_or_sftp(&mut RuntimeLog::default(), true, &paths).unwrap();
        assert!(via.command.is_some());
        assert_eq!(via.sftp_paths.len(), 3);
    }

    const SAMPLE_FP: &str = "SHA256:ldyiXa1JQakitNU5tErauu8DvWQ1dZ7aXu+rm7KQuog";
    const SAMPLE_LINE: &str =
        "build-01 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILagOJFgwaMNhBWQINinKOXmqS4Gh5NgxgriXwdOoINJ\n";

    #[test]
    fn known_hosts_file_is_read_and_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        std::fs::write(&path, SAMPLE_LINE).unwrap();
        let before = std::fs::read(&path).unwrap();
        let conn = Connection::open(dir.path().join("k.sqlite")).unwrap();

        let fps = fingerprints_in_file(&path, "build-01", 22).unwrap();
        assert_eq!(fps, vec![SAMPLE_FP.to_string()]);
        assert_eq!(
            host_key_with_file(&conn, &path, "build-01", 22, SAMPLE_FP).unwrap(),
            HostKeyStatus::Match
        );
        let changed = host_key_with_file(&conn, &path, "build-01", 22, "SHA256:other").unwrap();
        assert_eq!(changed, HostKeyStatus::Changed);
        assert_eq!(reject_if_host_key_changed(changed, "SHA256:other").unwrap_err().code_str(), "E_HOSTKEY_CHANGED");

        clear_fingerprint(&conn, "build-01", 22).unwrap();
        assert_eq!(
            host_key_with_file(&conn, &path, "build-01", 22, "SHA256:other").unwrap(),
            HostKeyStatus::New
        );
        trust_host_key(&conn, "build-01", 22, "SHA256:other", 2).unwrap();
        assert_eq!(
            host_key_with_file(&conn, &path, "build-01", 22, "SHA256:other").unwrap(),
            HostKeyStatus::Match
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);

        assert_eq!(
            decide_host_key(Some(SAMPLE_FP), false, &[], "SHA256:nope"),
            HostKeyStatus::Changed
        );
        assert_eq!(decide_host_key(None, false, &[], SAMPLE_FP), HostKeyStatus::New);
    }

    #[test]
    fn swapped_fingerprint_between_probe_and_reconnect_is_rejected() {
        let probed = "SHA256:probe-key";
        let swapped = "SHA256:mitm-key";
        let probe = classify_presented_key(None, false, &[], probed, false, None);
        assert!(!probe.sends_credentials());
        assert!(matches!(probe, PresentedKey::Confirm(fp) if fp == probed));

        let mitm = classify_presented_key(None, false, &[], swapped, true, Some(probed));
        assert!(!mitm.sends_credentials(), "指纹被换掉之后不能送出口令");
        assert!(matches!(mitm, PresentedKey::Reject(fp) if fp == swapped));

        let rebound = classify_presented_key(None, false, &[], probed, true, Some(probed));
        assert!(matches!(rebound, PresentedKey::Accept(HostKeyStatus::New)));
        assert!(rebound.sends_credentials());

        let unbound = classify_presented_key(None, false, &[], probed, true, None);
        assert!(!unbound.sends_credentials());
    }

    #[test]
    fn ssh_config_skips_wildcards_expands_identity_and_never_writes() {
        let text = "\
Host *\n  User nobody\n  IdentityFile ~/.ssh/id_ed25519\n\n\
Host *.internal\n  HostName inside.example\n\n\
Host web\n  HostName web.example\n  User deploy\n  Port 2222\n  IdentityFile \"~/keys/id\"\n";
        let home = std::path::Path::new("/home/me");
        let hosts = parse_ssh_config_with_home(text, Some(home));
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].alias, "web");
        assert_eq!(hosts[0].host_name, "web.example");
        assert_eq!(hosts[0].user.as_deref(), Some("deploy"));
        assert_eq!(hosts[0].port, 2222);
        assert_eq!(hosts[0].identity_file, "/home/me/keys/id");

        let dir = tempfile::tempdir().unwrap();
        let ssh = dir.path().join(".ssh");
        std::fs::create_dir(&ssh).unwrap();
        let cfg = ssh.join("config");
        let body = "Host web\n  HostName web.example\n  IdentityFile ~/id\n";
        std::fs::write(&cfg, body).unwrap();
        let before = std::fs::read(&cfg).unwrap();
        let read = read_user_ssh_config(dir.path()).unwrap();
        assert_eq!(std::fs::read(&cfg).unwrap(), before);
        let names: Vec<_> = std::fs::read_dir(&ssh).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("config")]);
        assert_eq!(read[0].identity_file, dir.path().join("id").display().to_string());
        assert_eq!(read[0].host_name, "web.example");
    }

    #[test]
    fn commit_fingerprint_must_match_the_permit_exactly() {
        assert!(strict_permit_fingerprint("SHA256:permit", "SHA256:permit").is_ok());
        let mismatch = strict_permit_fingerprint("SHA256:permit", "SHA256:other").unwrap_err();
        assert_eq!(mismatch.message, "服务器指纹已变化");
        assert!(!mismatch.message.contains("需要确认"));
        let empty = strict_permit_fingerprint("", "SHA256:other").unwrap_err();
        assert_eq!(empty.message, COMMIT_FINGERPRINT_MISMATCH);
        assert_eq!(empty.code_str(), "E_HOSTKEY_CHANGED");
    }
}
