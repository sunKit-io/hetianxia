//! SSH 凭据。密码和私钥口令只进钥匙串，服务名固定，账号是连接 id。

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;

use crate::error::{Error, ErrorCode, Result};

pub const KEYRING_SERVICE: &str = "com.hetianxia.app.ssh";

/// 交给界面的只有有没有保存。密码本身留在钥匙串里。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretStatus {
    pub saved: bool,
}

pub fn status(store: &impl SecretStore, account: &str) -> Result<SecretStatus> {
    Ok(SecretStatus { saved: store.get(account)?.is_some() })
}

pub trait SecretStore {
    fn get(&self, account: &str) -> Result<Option<String>>;
    fn set(&self, account: &str, secret: &str) -> Result<()>;
    fn delete(&self, account: &str) -> Result<()>;
}

/// 测试用。不碰系统钥匙串。
#[derive(Default)]
pub struct MemoryStore {
    items: Mutex<HashMap<String, String>>,
}

impl SecretStore for MemoryStore {
    fn get(&self, account: &str) -> Result<Option<String>> {
        let guard = self.items.lock().map_err(|_| Error::new(ErrorCode::Internal, "凭据表被占用"))?;
        Ok(guard.get(account).cloned())
    }

    fn set(&self, account: &str, secret: &str) -> Result<()> {
        let mut guard = self.items.lock().map_err(|_| Error::new(ErrorCode::Internal, "凭据表被占用"))?;
        guard.insert(account.to_string(), secret.to_string());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<()> {
        let mut guard = self.items.lock().map_err(|_| Error::new(ErrorCode::Internal, "凭据表被占用"))?;
        guard.remove(account);
        Ok(())
    }
}

/// macOS 钥匙串 / Windows 凭据管理器 / Linux 内核钥匙串。
pub struct KeyringStore;

fn entry(account: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, account).map_err(|_| Error::new(ErrorCode::Io, "钥匙串不可用"))
}

impl SecretStore for KeyringStore {
    fn get(&self, account: &str) -> Result<Option<String>> {
        match entry(account)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(Error::new(ErrorCode::Io, "钥匙串读取失败")),
        }
    }

    fn set(&self, account: &str, secret: &str) -> Result<()> {
        entry(account)?
            .set_password(secret)
            .map_err(|_| Error::new(ErrorCode::Io, "钥匙串写入失败"))
    }

    fn delete(&self, account: &str) -> Result<()> {
        match entry(account)?.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(Error::new(ErrorCode::Io, "钥匙串删除失败")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trip_uses_the_connection_id() {
        let store = MemoryStore::default();
        assert_eq!(store.get("build-01").unwrap(), None);
        store.set("build-01", "s3cret").unwrap();
        assert_eq!(store.get("build-01").unwrap().as_deref(), Some("s3cret"));
        assert_eq!(store.get("other").unwrap(), None);
        store.delete("build-01").unwrap();
        assert_eq!(store.get("build-01").unwrap(), None);
        assert_eq!(KEYRING_SERVICE, "com.hetianxia.app.ssh");
    }

    #[test]
    fn status_tells_the_frontend_saved_and_not_the_secret() {
        let store = MemoryStore::default();
        let secret = "pw-do-not-leak-9f3a";
        store.set("build-01", secret).unwrap();
        let saved = status(&store, "build-01").unwrap();
        assert!(saved.saved);
        let json = serde_json::to_string(&saved).unwrap();
        assert_eq!(json, r#"{"saved":true}"#);
        assert!(!json.contains(secret));
        let missing = status(&store, "other").unwrap();
        assert!(!missing.saved);
        let err = Error::new(ErrorCode::Io, "钥匙串读取失败");
        assert!(!err.to_string().contains(secret));
        assert!(!serde_json::to_string(&err).unwrap().contains(secret));
    }
}
