//! 预演勾选。按项目目录和升级包记住取消勾选的相对路径。
//! 重新扫描后，还在列表里的路径保持未选，其余默认勾上。

use crate::error::{Error, ErrorCode, Result};
use rusqlite::{params, Connection};

fn ensure(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS file_pick_skip (
            project_root TEXT NOT NULL,
            package_path TEXT NOT NULL,
            rel TEXT NOT NULL,
            PRIMARY KEY (project_root, package_path, rel)
        );",
    )?;
    Ok(())
}

/// 相对路径用 `/`。拒绝空段、`.`、`..` 和绝对路径。
pub fn normalize_rel(rel: &str) -> Option<String> {
    let slash = rel.replace('\\', "/");
    if slash.is_empty() || slash.starts_with('/') {
        return None;
    }
    let mut parts = Vec::new();
    for part in slash.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return None;
        }
        parts.push(part);
    }
    Some(parts.join("/"))
}

fn key(value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(Error::new(ErrorCode::InvalidLocation, "项目目录和升级包都不能是空的"));
    }
    Ok(trimmed.to_string())
}

/// 用这一次的取消勾选集合换掉同一对项目目录、升级包下的旧记录。
pub fn save_skipped(db: &Connection, project_root: &str, package_path: &str, skipped: &[String]) -> Result<()> {
    let project_root = key(project_root)?;
    let package_path = key(package_path)?;
    let mut rels = Vec::new();
    for rel in skipped {
        let Some(norm) = normalize_rel(rel) else {
            return Err(Error::new(ErrorCode::InvalidLocation, "勾选记录里的相对路径不合法"));
        };
        if !rels.contains(&norm) {
            rels.push(norm);
        }
    }
    rels.sort();
    ensure(db)?;
    let tx = db.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM file_pick_skip WHERE project_root = ?1 AND package_path = ?2",
        params![project_root, package_path],
    )?;
    for rel in rels {
        tx.execute(
            "INSERT INTO file_pick_skip (project_root, package_path, rel) VALUES (?1, ?2, ?3)",
            params![project_root, package_path, rel],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn load_skipped(db: &Connection, project_root: &str, package_path: &str) -> Result<Vec<String>> {
    let project_root = key(project_root)?;
    let package_path = key(package_path)?;
    ensure(db)?;
    let mut stmt = db.prepare(
        "SELECT rel FROM file_pick_skip WHERE project_root = ?1 AND package_path = ?2 ORDER BY rel",
    )?;
    let rows = stmt.query_map(params![project_root, package_path], |row| row.get(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn skipped_paths_are_remembered_per_project_and_package() {
        let conn = db();
        save_skipped(&conn, "/work/app", "/pkg/a.zip", &["src/old.ts".into(), "src\\b.ts".into()]).unwrap();
        save_skipped(&conn, "/work/app", "/pkg/b.zip", &["other.ts".into()]).unwrap();
        assert_eq!(load_skipped(&conn, "/work/app", "/pkg/a.zip").unwrap(), vec!["src/b.ts", "src/old.ts"]);
        assert_eq!(load_skipped(&conn, "/work/app", "/pkg/b.zip").unwrap(), vec!["other.ts"]);
        save_skipped(&conn, " /work/app ", "/pkg/a.zip", &["keep.ts".into()]).unwrap();
        assert_eq!(load_skipped(&conn, "/work/app", "/pkg/a.zip").unwrap(), vec!["keep.ts"]);
        save_skipped(&conn, "/work/app", "/pkg/a.zip", &[]).unwrap();
        assert!(load_skipped(&conn, "/work/app", "/pkg/a.zip").unwrap().is_empty());
        assert_eq!(load_skipped(&conn, "/work/app", "/pkg/b.zip").unwrap(), vec!["other.ts"]);
    }

    #[test]
    fn rescan_restores_unchecked_rels_and_checks_new_files() {
        let conn = db();
        save_skipped(
            &conn,
            "/work/app",
            "/pkg/a.zip",
            &["src/new.ts".into(), "gone.ts".into()],
        )
        .unwrap();
        let scanned = ["readme.txt", "src/new.ts", "src/util/c.ts", "extra.txt"];
        let skipped = load_skipped(&conn, "/work/app", "/pkg/a.zip").unwrap();
        let picked: Vec<_> = scanned
            .iter()
            .copied()
            .filter(|path| !skipped.iter().any(|rel| rel == path))
            .collect();
        assert_eq!(picked, vec!["readme.txt", "src/util/c.ts", "extra.txt"]);
        assert!(skipped.iter().any(|rel| rel == "gone.ts"));
    }

    #[test]
    fn reopened_database_keeps_unchecked_rels_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("picks.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            save_skipped(
                &conn,
                "/work/app",
                "/pkg/a.zip",
                &["src/new.ts".into(), "src/util/c.ts".into()],
            )
            .unwrap();
        }
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            load_skipped(&conn, "/work/app", "/pkg/a.zip").unwrap(),
            vec!["src/new.ts", "src/util/c.ts"]
        );
        let scanned = ["readme.txt", "src/new.ts", "extra.txt"];
        let skipped = load_skipped(&conn, "/work/app", "/pkg/a.zip").unwrap();
        let picked: Vec<_> = scanned
            .iter()
            .copied()
            .filter(|path| !skipped.iter().any(|rel| rel == path))
            .collect();
        assert_eq!(picked, vec!["readme.txt", "extra.txt"]);
    }

    #[test]
    fn dotdot_and_absolute_rels_are_rejected() {
        let conn = db();
        assert!(normalize_rel("../x").is_none());
        assert!(normalize_rel("/abs").is_none());
        assert!(normalize_rel("a/../b").is_none());
        let err = save_skipped(&conn, "/work/app", "/pkg/a.zip", &["a/../b".into()]).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidLocation);
        assert!(load_skipped(&conn, "/work/app", "/pkg/a.zip").unwrap().is_empty());
    }
}
