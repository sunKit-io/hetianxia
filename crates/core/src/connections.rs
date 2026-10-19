//! 已保存的 SSH 连接。密码和口令不进这张表，只留在钥匙串里。

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorCode, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SavedConnection {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// `password`、`key` 或 `agent`。
    pub auth: String,
    pub key_path: String,
    pub fingerprint: String,
}

pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ssh_connections (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            host TEXT NOT NULL,
            port INTEGER NOT NULL,
            user TEXT NOT NULL,
            auth TEXT NOT NULL,
            key_path TEXT NOT NULL,
            fingerprint TEXT NOT NULL
        );",
    )?;
    Ok(())
}

pub fn list(conn: &Connection) -> Result<Vec<SavedConnection>> {
    ensure_schema(conn)?;
    let mut stmt = conn
        .prepare("SELECT id, name, host, port, user, auth, key_path, fingerprint FROM ssh_connections ORDER BY name, id")
        .map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(SavedConnection {
                id: row.get(0)?,
                name: row.get(1)?,
                host: row.get(2)?,
                port: row.get(3)?,
                user: row.get(4)?,
                auth: row.get(5)?,
                key_path: row.get(6)?,
                fingerprint: row.get(7)?,
            })
        })
        .map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?;
    rows.collect::<std::result::Result<Vec<_>, _>>().map_err(|err| Error::new(ErrorCode::Io, err.to_string()))
}

pub fn get(conn: &Connection, id: &str) -> Result<SavedConnection> {
    list(conn)?
        .into_iter()
        .find(|item| item.id == id)
        .ok_or_else(|| Error::new(ErrorCode::NotFound, "没有这个连接"))
}

pub fn save(conn: &Connection, input: &SavedConnection) -> Result<SavedConnection> {
    ensure_schema(conn)?;
    let mut saved = input.clone();
    if saved.id.is_empty() {
        saved.id = uuid::Uuid::new_v4().simple().to_string();
    }
    validate(&saved)?;
    conn.execute(
        "INSERT INTO ssh_connections (id, name, host, port, user, auth, key_path, fingerprint)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            host = excluded.host,
            port = excluded.port,
            user = excluded.user,
            auth = excluded.auth,
            key_path = excluded.key_path,
            fingerprint = excluded.fingerprint",
        params![saved.id, saved.name, saved.host, saved.port, saved.user, saved.auth, saved.key_path, saved.fingerprint],
    )
    .map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?;
    Ok(saved)
}

pub fn set_fingerprint(conn: &Connection, id: &str, fingerprint: &str) -> Result<()> {
    ensure_schema(conn)?;
    let changed = conn
        .execute(
            "UPDATE ssh_connections SET fingerprint = ?1 WHERE id = ?2",
            params![fingerprint, id],
        )
        .map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?;
    if changed == 0 {
        return Err(Error::new(ErrorCode::NotFound, "没有这个连接"));
    }
    Ok(())
}

pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    ensure_schema(conn)?;
    conn.execute("DELETE FROM ssh_connections WHERE id = ?1", params![id])
        .map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?;
    Ok(())
}

fn validate(input: &SavedConnection) -> Result<()> {
    if input.id.is_empty() || input.id.contains('/') || input.id.contains('\\') || input.id.contains("..") {
        return Err(Error::new(ErrorCode::InvalidLocation, "连接编号不合法"));
    }
    if input.name.is_empty() || input.host.is_empty() || input.user.is_empty() {
        return Err(Error::new(ErrorCode::InvalidLocation, "连接的名称、主机或用户名为空"));
    }
    if input.port == 0 {
        return Err(Error::new(ErrorCode::InvalidLocation, "端口不合法"));
    }
    if input.auth != "password" && input.auth != "key" && input.auth != "agent" {
        return Err(Error::new(ErrorCode::InvalidLocation, "认证方式不合法"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec;

    #[test]
    fn connections_round_trip_without_a_secret_column() {
        let dir = tempfile::tempdir().unwrap();
        let db = exec::open_db(&dir.path().join("hetianxia.sqlite")).unwrap();
        let saved = save(
            &db,
            &SavedConnection {
                id: String::new(),
                name: "build".into(),
                host: "db.example".into(),
                port: 22,
                user: "deploy".into(),
                auth: "key".into(),
                key_path: String::new(),
                fingerprint: "SHA256:abc".into(),
            },
        )
        .unwrap();
        assert!(!saved.id.is_empty());
        let listed = list(&db).unwrap();
        assert_eq!(listed, vec![saved.clone()]);
        let json = serde_json::to_string(&listed).unwrap();
        assert!(!json.contains("secret"));
        assert!(!json.contains("passphrase"));
        assert!(json.contains("db.example"));
        let mut columns = Vec::new();
        let mut stmt = db.prepare("PRAGMA table_info(ssh_connections)").unwrap();
        let rows = stmt.query_map([], |row| row.get::<_, String>(1)).unwrap();
        for name in rows {
            columns.push(name.unwrap());
        }
        assert!(columns.iter().all(|name| name != "secret" && name != "password" && name != "passphrase"));
        set_fingerprint(&db, &saved.id, "SHA256:next").unwrap();
        assert_eq!(get(&db, &saved.id).unwrap().fingerprint, "SHA256:next");
        delete(&db, &saved.id).unwrap();
        assert!(list(&db).unwrap().is_empty());
    }
}
