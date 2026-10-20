use serde::Serialize;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    NotFound,
    Permission,
    Busy,
    HostkeyChanged,
    SftpUnavailable,
    Auth,
    Net,
    ChangedSinceScan,
    CaseConflict,
    NoSpace,
    ZipSlip,
    ZipBomb,
    ZipEncoding,
    Cancelled,
    SymlinkInPath,
    SymlinkRefused,
    UnconfirmedHunks,
    InvalidLocation,
    Fault,
    LogWrite,
    VerifyFailed,
    RemoteHashUnavailable,
    BackupPurged,
    RollbackConflict,
    RollbackPartial,
    Conflict,
    Unsupported,
    Io,
    InvalidState,
    Internal,
}

#[derive(Debug, Clone, Serialize)]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), paths: Vec::new() }
    }
    pub fn with_paths(mut self, paths: Vec<String>) -> Self {
        self.paths = paths;
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code_str(), self.message)
    }
}

impl std::error::Error for Error {}

impl Error {
    pub fn code_str(&self) -> &'static str {
        match self.code {
            ErrorCode::NotFound => "E_NOT_FOUND",
            ErrorCode::Permission => "E_PERMISSION",
            ErrorCode::Busy => "E_BUSY",
            ErrorCode::HostkeyChanged => "E_HOSTKEY_CHANGED",
            ErrorCode::SftpUnavailable => "E_SFTP_UNAVAILABLE",
            ErrorCode::Auth => "E_AUTH",
            ErrorCode::Net => "E_NET",
            ErrorCode::ChangedSinceScan => "E_CHANGED_SINCE_SCAN",
            ErrorCode::CaseConflict => "E_CASE_CONFLICT",
            ErrorCode::NoSpace => "E_NO_SPACE",
            ErrorCode::ZipSlip => "E_ZIP_SLIP",
            ErrorCode::ZipBomb => "E_ZIP_BOMB",
            ErrorCode::ZipEncoding => "E_ZIP_ENCODING",
            ErrorCode::Cancelled => "E_CANCELLED",
            ErrorCode::SymlinkInPath => "E_SYMLINK_IN_PATH",
            ErrorCode::SymlinkRefused => "E_SYMLINK_REFUSED",
            ErrorCode::UnconfirmedHunks => "E_UNCONFIRMED_HUNKS",
            ErrorCode::InvalidLocation => "E_INVALID_LOCATION",
            ErrorCode::Fault => "E_FAULT",
            ErrorCode::LogWrite => "E_LOG_WRITE",
            ErrorCode::VerifyFailed => "E_VERIFY_FAILED",
            ErrorCode::RemoteHashUnavailable => "E_REMOTE_HASH_UNAVAILABLE",
            ErrorCode::BackupPurged => "E_BACKUP_PURGED",
            ErrorCode::RollbackConflict => "E_ROLLBACK_CONFLICT",
            ErrorCode::RollbackPartial => "E_ROLLBACK_PARTIAL",
            ErrorCode::Conflict => "E_CONFLICT",
            ErrorCode::Unsupported => "E_UNSUPPORTED",
            ErrorCode::Io => "E_IO",
            ErrorCode::InvalidState => "E_INVALID_STATE",
            ErrorCode::Internal => "E_INTERNAL",
        }
    }
}

/// 内容已经回到备份，但权限或修改时间没有恢复。不算未恢复，也不单独变成「部分回滚」。
pub const ATTR_RESTORE_FAILED: &str = "内容已恢复，权限 / 修改时间未恢复";

/// 预演之后目标上出现了文件。确认框和执行器共用这一句，避免两处文案不一致。
pub fn occupied_after_preview(path: &str) -> Error {
    Error::new(ErrorCode::Conflict, format!("{path} 在预演后被新建或修改过，为了不覆盖你的文件，这一项没有写入。"))
}

/// 编辑或合并时读不到文本，或文件是二进制、超过 1MB。确认和执行共用这一句，并且还没写盘。
pub fn unreadable_merge(path: &str) -> Error {
    Error::new(ErrorCode::Unsupported, format!("{path} 无法合并：没有读到文件内容，这一项没有写入。"))
}

/// 设计文档 12.11 的错误码。界面按这些码出文案。
pub const SPEC_CODES: &[&str] = &[
    "E_NOT_FOUND",
    "E_PERMISSION",
    "E_BUSY",
    "E_NO_SPACE",
    "E_IO",
    "E_CHANGED_SINCE_SCAN",
    "E_CASE_CONFLICT",
    "E_SYMLINK_IN_PATH",
    "E_VERIFY_FAILED",
    "E_LOG_WRITE",
    "E_UNCONFIRMED_HUNKS",
    "E_ZIP_SLIP",
    "E_ZIP_BOMB",
    "E_ZIP_ENCODING",
    "E_AUTH",
    "E_HOSTKEY_CHANGED",
    "E_SFTP_UNAVAILABLE",
    "E_NET",
    "E_REMOTE_HASH_UNAVAILABLE",
    "E_BACKUP_PURGED",
    "E_ROLLBACK_CONFLICT",
    "E_ROLLBACK_PARTIAL",
    "E_CANCELLED",
    "E_INVALID_STATE",
    "E_INTERNAL",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_table_codes_are_all_implemented() {
        let have: Vec<&str> = [
            ErrorCode::NotFound,
            ErrorCode::Permission,
            ErrorCode::Busy,
            ErrorCode::NoSpace,
            ErrorCode::Io,
            ErrorCode::ChangedSinceScan,
            ErrorCode::CaseConflict,
            ErrorCode::SymlinkInPath,
            ErrorCode::VerifyFailed,
            ErrorCode::LogWrite,
            ErrorCode::UnconfirmedHunks,
            ErrorCode::ZipSlip,
            ErrorCode::ZipBomb,
            ErrorCode::ZipEncoding,
            ErrorCode::Auth,
            ErrorCode::HostkeyChanged,
            ErrorCode::SftpUnavailable,
            ErrorCode::Net,
            ErrorCode::RemoteHashUnavailable,
            ErrorCode::BackupPurged,
            ErrorCode::RollbackConflict,
            ErrorCode::RollbackPartial,
            ErrorCode::Cancelled,
            ErrorCode::InvalidState,
            ErrorCode::Internal,
        ]
        .into_iter()
        .map(|c| Error::new(c, "").code_str())
        .collect();
        assert_eq!(have, SPEC_CODES);
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::new(ErrorCode::Internal, e.to_string())
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        let code = match e.kind() {
            std::io::ErrorKind::NotFound => ErrorCode::NotFound,
            std::io::ErrorKind::PermissionDenied => ErrorCode::Permission,
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => ErrorCode::Busy,
            _ if e.raw_os_error() == Some(28) => ErrorCode::NoSpace,
            _ => ErrorCode::Io,
        };
        Error::new(code, e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
