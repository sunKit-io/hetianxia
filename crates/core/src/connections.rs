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
    /// 远端绝对目录。旧数据没有这一列时是 `None`。
    #[serde(default)]
    pub remote_root: Option<String>,
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
    let mut stmt = conn
        .prepare("PRAGMA table_info(ssh_connections)")
        .map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?;
    drop(stmt);
    if !names.iter().any(|name| name == "remote_root") {
        conn.execute("ALTER TABLE ssh_connections ADD COLUMN remote_root TEXT", [])
            .map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?;
    }
    Ok(())
}

/// 空字符串表示还没填。填了就必须是绝对 POSIX 路径：以 `/` 开头，不含 `..`，
/// 去掉 `.` 和重复斜杠，除了根目录 `/` 以外不保留结尾斜杠。
pub fn normalize_remote_root(raw: &str) -> Result<Option<String>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    if raw.contains('\0') || raw.contains('\\') || !raw.starts_with('/') {
        return Err(Error::new(ErrorCode::InvalidLocation, "远程目录必须是绝对路径"));
    }
    let mut parts = Vec::new();
    for part in raw.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return Err(Error::new(ErrorCode::InvalidLocation, "远程目录必须是绝对路径"));
        }
        parts.push(part);
    }
    if parts.is_empty() {
        return Ok(Some("/".to_string()));
    }
    Ok(Some(format!("/{}", parts.join("/"))))
}

const WRITE_OUTSIDE_ROOT: &str = "写入路径不在连接的远程目录下";

fn outside_root() -> Error {
    Error::new(ErrorCode::InvalidLocation, WRITE_OUTSIDE_ROOT)
}

/// 前端已经拼好绝对路径。这里只做 POSIX 规范化，并要求它严格落在远程目录里面。
/// `..` 直接拒绝，不解析。空段和 `.` 折叠掉。目录本身不能当文件目标。
pub fn path_inside_remote_root(root: Option<&str>, received: &str) -> Result<String> {
    let Some(root) = root.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err(Error::new(ErrorCode::InvalidState, "先在连接里填远程目录"));
    };
    let Some(root) = normalize_remote_root(root)? else {
        return Err(Error::new(ErrorCode::InvalidState, "先在连接里填远程目录"));
    };
    let file = absolute_components(received)?;
    let base: Vec<&str> = if root == "/" {
        Vec::new()
    } else {
        root.split('/').filter(|part| !part.is_empty()).collect()
    };
    if file.len() <= base.len() || file[..base.len()] != base[..] {
        return Err(outside_root());
    }
    Ok(format!("/{}", file.join("/")))
}

fn absolute_components(raw: &str) -> Result<Vec<&str>> {
    let raw = raw.trim();
    if raw.contains('\0') || raw.contains('\\') || !raw.starts_with('/') {
        return Err(outside_root());
    }
    let mut parts = Vec::new();
    for part in raw.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return Err(outside_root());
        }
        parts.push(part);
    }
    Ok(parts)
}

pub fn list(conn: &Connection) -> Result<Vec<SavedConnection>> {
    ensure_schema(conn)?;
    let mut stmt = conn
        .prepare("SELECT id, name, host, port, user, auth, key_path, fingerprint, remote_root FROM ssh_connections ORDER BY name, id")
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
                remote_root: row.get(8)?,
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
    // 前端带上来的指纹一律丢掉。只有测试连接确认过的指纹能写进来。
    saved.fingerprint.clear();
    if saved.id.is_empty() {
        saved.id = uuid::Uuid::new_v4().simple().to_string();
    } else if let Ok(previous) = get(conn, &saved.id) {
        if previous.host == saved.host && previous.port == saved.port && previous.user == saved.user {
            saved.fingerprint = previous.fingerprint;
        }
    }
    saved.remote_root = match saved.remote_root.as_deref() {
        Some(raw) => normalize_remote_root(raw)?,
        None => None,
    };
    validate(&saved)?;
    conn.execute(
        "INSERT INTO ssh_connections (id, name, host, port, user, auth, key_path, fingerprint, remote_root)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            host = excluded.host,
            port = excluded.port,
            user = excluded.user,
            auth = excluded.auth,
            key_path = excluded.key_path,
            fingerprint = excluded.fingerprint,
            remote_root = excluded.remote_root",
        params![
            saved.id,
            saved.name,
            saved.host,
            saved.port,
            saved.user,
            saved.auth,
            saved.key_path,
            saved.fingerprint,
            saved.remote_root
        ],
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
    if input.auth != "password" && input.auth != "key" {
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
                remote_root: None,
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

    #[test]
    fn save_ignores_a_frontend_fingerprint_and_clears_it_when_the_endpoint_changes() {
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
                fingerprint: "SHA256:from-the-form".into(),
                remote_root: None,
            },
        )
        .unwrap();
        assert!(saved.fingerprint.is_empty());
        set_fingerprint(&db, &saved.id, "SHA256:confirmed").unwrap();
        let kept = save(
            &db,
            &SavedConnection {
                fingerprint: "SHA256:replaced-by-form".into(),
                ..get(&db, &saved.id).unwrap()
            },
        )
        .unwrap();
        assert_eq!(kept.fingerprint, "SHA256:confirmed");
        for (host, port, user) in [("other.example", 22u16, "deploy"), ("db.example", 2222, "deploy"), ("db.example", 22, "root")] {
            set_fingerprint(&db, &saved.id, "SHA256:confirmed").unwrap();
            let cleared = save(
                &db,
                &SavedConnection {
                    host: host.into(),
                    port,
                    user: user.into(),
                    fingerprint: "SHA256:still-from-form".into(),
                    ..get(&db, &saved.id).unwrap()
                },
            )
            .unwrap();
            assert!(cleared.fingerprint.is_empty(), "{host}:{port} {user}");
        }
    }

    #[test]
    fn agent_auth_cannot_be_saved() {
        let dir = tempfile::tempdir().unwrap();
        let db = exec::open_db(&dir.path().join("hetianxia.sqlite")).unwrap();
        let err = save(
            &db,
            &SavedConnection {
                id: String::new(),
                name: "build".into(),
                host: "db.example".into(),
                port: 22,
                user: "deploy".into(),
                auth: "agent".into(),
                key_path: String::new(),
                fingerprint: String::new(),
                remote_root: None,
            },
        )
        .unwrap_err();
        assert_eq!(err.message, "认证方式不合法");
        assert!(list(&db).unwrap().is_empty());
    }

    #[test]
    fn remote_root_must_be_an_absolute_posix_path_and_old_rows_stay_null() {
        assert_eq!(normalize_remote_root("  ").unwrap(), None);
        assert_eq!(normalize_remote_root("/opt/app").unwrap().as_deref(), Some("/opt/app"));
        assert_eq!(normalize_remote_root("/opt/app/").unwrap().as_deref(), Some("/opt/app"));
        assert_eq!(normalize_remote_root("/opt//./app").unwrap().as_deref(), Some("/opt/app"));
        assert_eq!(normalize_remote_root("/").unwrap().as_deref(), Some("/"));
        for bad in ["opt/app", "opt", "/opt/../etc", "/opt/foo/../../etc", "C:/opt", "\\opt"] {
            let err = normalize_remote_root(bad).unwrap_err();
            assert_eq!(err.message, "远程目录必须是绝对路径", "{bad}");
        }

        let dir = tempfile::tempdir().unwrap();
        let db = exec::open_db(&dir.path().join("hetianxia.sqlite")).unwrap();
        db.execute_batch(
            "CREATE TABLE ssh_connections (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                host TEXT NOT NULL,
                port INTEGER NOT NULL,
                user TEXT NOT NULL,
                auth TEXT NOT NULL,
                key_path TEXT NOT NULL,
                fingerprint TEXT NOT NULL
            );",
        )
        .unwrap();
        db.execute(
            "INSERT INTO ssh_connections (id, name, host, port, user, auth, key_path, fingerprint) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params!["old", "old", "db.example", 22, "deploy", "key", "", "SHA256:kept"],
        )
        .unwrap();
        let migrated = get(&db, "old").unwrap();
        assert_eq!(migrated.remote_root, None);
        assert_eq!(migrated.fingerprint, "SHA256:kept");

        let err = save(
            &db,
            &SavedConnection {
                remote_root: Some("rel/path".into()),
                ..migrated.clone()
            },
        )
        .unwrap_err();
        assert_eq!(err.message, "远程目录必须是绝对路径");
        assert_eq!(get(&db, "old").unwrap().fingerprint, "SHA256:kept");

        let saved = save(
            &db,
            &SavedConnection {
                remote_root: Some("/srv/app/".into()),
                fingerprint: "SHA256:from-form".into(),
                ..migrated
            },
        )
        .unwrap();
        assert_eq!(saved.remote_root.as_deref(), Some("/srv/app"));
        assert_eq!(saved.fingerprint, "SHA256:kept");
        let json = serde_json::to_string(&saved).unwrap();
        assert!(json.contains("\"remoteRoot\":\"/srv/app\""));
    }

    #[test]
    fn write_path_must_stay_strictly_inside_the_remote_root() {
        assert_eq!(
            path_inside_remote_root(Some("/srv/app"), "/srv/app//conf/./app.conf").unwrap(),
            "/srv/app/conf/app.conf"
        );
        assert_eq!(
            path_inside_remote_root(Some("/srv/app/"), "/srv/app/conf/app.conf/").unwrap(),
            "/srv/app/conf/app.conf"
        );
        for bad in [
            "/srv/app/../app2/file",
            "/srv/app/foo/../../etc/passwd",
            "/srv/app2",
            "/srv/app2/file",
            "conf/app.conf",
            "srv/app/conf/app.conf",
            "/srv/app",
            "/srv/app/",
        ] {
            let err = path_inside_remote_root(Some("/srv/app"), bad).unwrap_err();
            assert_eq!(err.message, "写入路径不在连接的远程目录下", "{bad}");
        }
        assert_eq!(
            path_inside_remote_root(None, "/srv/app/conf/app.conf").unwrap_err().message,
            "先在连接里填远程目录"
        );
        assert_eq!(
            path_inside_remote_root(Some("  "), "/srv/app/conf/app.conf").unwrap_err().message,
            "先在连接里填远程目录"
        );
    }
}
