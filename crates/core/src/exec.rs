//! 执行引擎（写入、回滚、删除都只走这里）。
//!
//! 顺序对每个要写入的文件固定为：
//! 备份 → 写临时文件 `.<name>.htx-<rand>` → fsync → 读回校验（verify-pre）
//! → 原子改名 → 全部完成后再次校验（verify-post）。
//! 校验失败会删掉临时文件，原文件不动。
//!
//! 事务日志每次状态变更单独提交一次，序号从 1 开始：
//! 1. PREPARED（打开事务的第一次日志写入）
//! 2. BACKED_UP
//! 然后每个计划文件各两次：`doing`、`done`
//! 然后 VERIFYING，然后在写入 COMMITTED 之前做最后一次日志检查，最后 COMMITTED。
//! 临时文件名写入 `exec_temps`，不计入上面的 `log:N`。LOG_FAILED 的试写也不计入。
//! 继续时的第一次日志写入仍是 `BACKED_UP`。
//! 日志写失败立刻停止，不自动回滚。回滚依据是已写入的日志加上备份目录里的 `manifest.json`。
//! 下次启动由 `pending()` 提示未完成的升级。
//!
//! 继续：对还没 done 的操作重新做变动检测（只认哈希，touch 不算改动）。
//! 回滚冲突三个选择：继续（先把当前版本另存再覆盖）、跳过冲突文件、取消。
//! 回滚时只要有文件被跳过或没有恢复，状态就是 `ROLLBACK_PARTIAL`，历史显示「部分回滚」。
//!
//! 只读文件默认不覆盖，预演里以 E_PERMISSION 列出。勾选「覆盖只读文件」后
//! 先去掉只读（chmod，macOS 再 chflags nouchg），写完再恢复只读。
//! 删除开关关闭时，移动变成复制，界面文案是「移动（保留旧文件）」。
//! 直接覆盖保留升级包的 mtime；合并结果用写入时刻。

use crate::error::{Error, ErrorCode, Result, ATTR_RESTORE_FAILED};
use crate::fault::{FaultPlan, FaultRuntime};
#[cfg(feature = "fault-inject")]
use crate::fault::Stage;
use crate::hashutil::sha256_hex;
use crate::model::{ExecPlan, OpKind, PlanOp};
use crate::pathutil::RelPath;
use crate::vfs::{now_ms, LocalFs};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 当前进程这一次运行的 `run_id`。`begin_run` 写入，干净退出时清掉。
static ACTIVE_RUN: Mutex<Option<String>> = Mutex::new(None);

fn set_active_run(id: Option<String>) {
    if let Ok(mut guard) = ACTIVE_RUN.lock() {
        *guard = id;
    }
}

fn active_run_id() -> Option<String> {
    ACTIVE_RUN.lock().ok().and_then(|guard| guard.clone())
}

#[derive(Debug, Clone)]
pub struct ExecOpts {
    pub allow_readonly: bool,
    pub backup_root: PathBuf,
    pub remote: bool,
    /// 测试或故障注入构建：写出这条日志后立刻返回，模拟进程退出。
    #[cfg(any(test, feature = "fault-inject"))]
    pub stop_after_log: Option<String>,
    /// 测试或故障注入构建：临时文件写完后改一个字节，验证原文件不动。
    #[cfg(any(test, feature = "fault-inject"))]
    pub corrupt_temp: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileResult {
    pub index: u32,
    pub path: String,
    pub kind: String,
    pub state: String,
    pub error: Option<String>,
    pub code: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecReport {
    pub exec_id: String,
    pub state: String,
    pub files: Vec<FileResult>,
    pub backup_root: String,
    pub log_stopped: bool,
    /// 历史记录结果。整次成功是「成功」，单文件失败停住是「部分失败」。
    pub history_result: String,
    /// 失败 Sheet 只有「回滚」和「稍后处理」。「继续」不在这张 Sheet 上。
    pub sheet_actions: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingTx {
    pub exec_id: String,
    pub state: String,
    pub project_key: String,
    pub backup_root: String,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RollbackChoice {
    Cancel,
    SkipConflicts,
    ProceedBackupCurrent,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictFile {
    pub path: String,
    pub detail: String,
}

/// 已写完的文件后来被改过。设计 21 的行标签。
pub const DETAIL_MODIFIED: &str = "升级后被修改";
/// 已写完的文件后来被删掉。
pub const DETAIL_DELETED: &str = "升级后被删除";
/// 撤销移动时，原路径被外部重新创建，而且内容和备份不同。
pub const DETAIL_RECREATED: &str = "原位置被重新创建，和备份不同";
/// 记在原事务日志里的“当时是删除”状态。不是文件哈希。
pub const KEPT_DELETED: &str = "deleted";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeAdmission {
    /// 可以继续升级。
    Upgrade,
    /// 只能接着回滚。
    RollbackOnly,
    /// 拒绝。
    Reject,
}

/// 单一入口。不改写事务原状态。
pub fn resume_admission(state: &str) -> ResumeAdmission {
    match state {
        "STOPPED" | "LOG_FAILED" | "PREPARED" | "BACKED_UP" | "VERIFYING" => ResumeAdmission::Upgrade,
        "ROLLING_BACK" | "ROLLBACK_PARTIAL" => ResumeAdmission::RollbackOnly,
        _ => ResumeAdmission::Reject,
    }
}

pub fn resume_actions(state: &str) -> Vec<&'static str> {
    match resume_admission(state) {
        ResumeAdmission::Upgrade => vec!["继续", "回滚", "稍后处理"],
        ResumeAdmission::RollbackOnly => vec!["继续回滚", "稍后处理"],
        ResumeAdmission::Reject => Vec::new(),
    }
}

pub fn resume_conflict_title(files: &[ConflictFile]) -> String {
    let n = files.len();
    if files.iter().any(|f| f.detail == DETAIL_DELETED) {
        format!("有 {n} 个已经写完的文件被改过或删除")
    } else {
        format!("有 {n} 个已经写完的文件被改过")
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeConflict {
    pub title: String,
    pub files: Vec<ConflictFile>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryOffer {
    pub exec_id: String,
    pub state: String,
    pub pre_recovery_state: String,
    pub actions: Vec<String>,
    pub conflicts: Vec<ConflictFile>,
    /// 启动试写失败时为 E_LOG_WRITE。这一笔事务的数据库和临时文件都没有改。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeHit {
    pub path: String,
    pub hash_changed: bool,
    pub detail: String,
}

pub fn open_db(path: &Path) -> Result<Connection> {
    if let Some(p) = path.parent() {
        crate::vfs::create_dir_all_synced(p)?;
    }
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS exec_journal (
            exec_id TEXT PRIMARY KEY,
            state TEXT NOT NULL,
            project_key TEXT,
            plan_json TEXT,
            backup_root TEXT,
            started_at INTEGER,
            finished_at INTEGER
         );
         CREATE TABLE IF NOT EXISTS exec_ops (
            exec_id TEXT,
            op_index INTEGER,
            op_json TEXT,
            state TEXT,
            error TEXT,
            code TEXT,
            before_hash TEXT,
            after_hash TEXT,
            rollback_state TEXT,
            rollback_error TEXT,
            PRIMARY KEY (exec_id, op_index)
         );
         CREATE TABLE IF NOT EXISTS history (
            exec_id TEXT PRIMARY KEY,
            project_key TEXT,
            summary_json TEXT,
            result TEXT,
            paths_json TEXT,
            created_at INTEGER,
            backup_available INTEGER
         );",
    )?;
    ensure_column(&conn, "exec_ops", "written", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(&conn, "exec_ops", "source_removed", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(&conn, "exec_ops", "needs_verify", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(&conn, "exec_ops", "needs_verify_run", "TEXT")?;
    ensure_column(&conn, "exec_ops", "attr_restore_failed", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(&conn, "exec_ops", "kept_hash", "TEXT")?;
    ensure_column(&conn, "history", "attr_failed_json", "TEXT")?;
    ensure_column(&conn, "exec_journal", "pre_recovery_state", "TEXT")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS exec_temps (
            exec_id TEXT NOT NULL,
            path TEXT NOT NULL,
            PRIMARY KEY (exec_id, path)
        );",
    )?;
    Ok(conn)
}

fn ensure_column(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(());
        }
    }
    conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"), [])?;
    Ok(())
}

pub fn detect_changes(fsys: &LocalFs, ops: &[PlanOp]) -> Result<Vec<ChangeHit>> {
    let mut hits = Vec::new();
    for op in ops {
        let Some(expect) = &op.expect else {
            let rel = RelPath::parse(&op.path)?;
            if fsys.stat(&rel).is_ok() && matches!(op.kind, OpKind::Create) {
                hits.push(ChangeHit {
                    path: op.path.clone(),
                    hash_changed: true,
                    detail: "目标路径在扫描后已经出现文件".into(),
                });
            }
            continue;
        };
        let rel = RelPath::parse(&op.path)?;
        let st = match fsys.stat(&rel) {
            Ok(s) => s,
            Err(_) => {
                hits.push(ChangeHit {
                    path: op.path.clone(),
                    hash_changed: true,
                    detail: "文件在扫描后消失了".into(),
                });
                continue;
            }
        };
        if st.kind == crate::vfs::EntryKind::Symlink {
            continue;
        }
        let size_same = st.size == expect.size;
        let mtime_same = st.mtime_ms == expect.mtime_ms;
        if size_same && mtime_same {
            continue;
        }
        let hash = fsys.hash(&rel, true).unwrap_or_default();
        if hash == expect.hash {
            // 只 touch 过，内容没变，不算改动。
            continue;
        }
        hits.push(ChangeHit {
            path: op.path.clone(),
            hash_changed: true,
            detail: format!("扫描时 {} 字节，现在 {} 字节，内容哈希已变", expect.size, st.size),
        });
    }
    Ok(hits)
}

struct OpRow {
    state: String,
    written: bool,
    source_removed: bool,
    needs_verify: bool,
    kept_hash: Option<String>,
    after_hash: Option<String>,
    rollback_state: Option<String>,
}

fn load_rows(db: &Connection, exec_id: &str) -> Result<HashMap<u32, OpRow>> {
    let mut stmt = db.prepare(
        "SELECT op_index, state, COALESCE(written,0), kept_hash, after_hash, rollback_state, COALESCE(source_removed,0), COALESCE(needs_verify,0) FROM exec_ops WHERE exec_id=?1",
    )?;
    let rows = stmt.query_map(params![exec_id], |r| {
        Ok((
            r.get::<_, i64>(0)? as u32,
            OpRow {
                state: r.get(1)?,
                written: r.get::<_, i64>(2)? != 0,
                source_removed: r.get::<_, i64>(6)? != 0,
                needs_verify: r.get::<_, i64>(7)? != 0,
                kept_hash: r.get(3)?,
                after_hash: r.get(4)?,
                rollback_state: r.get(5)?,
            },
        ))
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let (index, op) = row?;
        map.insert(index, op);
    }
    Ok(map)
}

fn remember_temp(db: &Connection, exec_id: &str, rel: &RelPath) -> Result<()> {
    db.execute(
        "INSERT OR REPLACE INTO exec_temps (exec_id, path) VALUES (?1,?2)",
        params![exec_id, rel.as_str()],
    )?;
    Ok(())
}

fn forget_temp(db: &Connection, exec_id: &str, rel: &RelPath) -> Result<()> {
    db.execute("DELETE FROM exec_temps WHERE exec_id=?1 AND path=?2", params![exec_id, rel.as_str()])?;
    Ok(())
}

/// 写失败时删掉这次的临时文件。删掉了才从日志里消掉；删不掉就留着，崩溃恢复再试。
fn discard_temp(fsys: &LocalFs, db: &Connection, exec_id: &str, rel: &RelPath) {
    let _ = fsys.remove_path(rel);
    if fsys.stat(rel).is_err() {
        let _ = forget_temp(db, exec_id, rel);
    }
}

pub fn recover_registered_temps(db: &Connection, fsys: &LocalFs, exec_id: &str) -> Result<Vec<String>> {
    recover_registered_temps_in(db, fsys, exec_id, None)
}

fn recover_registered_temps_in(db: &Connection, fsys: &LocalFs, exec_id: &str, backup: Option<&Path>) -> Result<Vec<String>> {
    let mut stmt = db.prepare("SELECT path FROM exec_temps WHERE exec_id=?1")?;
    let rows = stmt.query_map(params![exec_id], |r| r.get::<_, String>(0))?;
    let mut removed = Vec::new();
    let mut paths = Vec::new();
    for row in rows {
        paths.push(row?);
    }
    drop(stmt);
    for path in paths {
        if let Some(rest) = path.strip_prefix("bak:") {
            if let Some(root) = backup {
                if !manifest_references(root, rest) {
                    let abs = root.join(rest);
                    if abs.is_file() {
                        let _ = fs::remove_file(&abs);
                        removed.push(path.clone());
                    }
                }
            }
            db.execute("DELETE FROM exec_temps WHERE exec_id=?1 AND path=?2", params![exec_id, path])?;
            continue;
        }
        let rel = RelPath::parse(&path)?;
        if fsys.stat(&rel).is_ok() {
            fsys.remove_path(&rel)?;
            removed.push(path.clone());
        }
        db.execute("DELETE FROM exec_temps WHERE exec_id=?1 AND path=?2", params![exec_id, path])?;
    }
    Ok(removed)
}

fn mark_written(db: &Connection, exec_id: &str, index: u32) -> Result<()> {
    db.execute(
        "UPDATE exec_ops SET written=1 WHERE exec_id=?1 AND op_index=?2",
        params![exec_id, index],
    )?;
    Ok(())
}

fn mark_source_removed(db: &Connection, exec_id: &str, index: u32) -> Result<()> {
    db.execute(
        "UPDATE exec_ops SET source_removed=1 WHERE exec_id=?1 AND op_index=?2",
        params![exec_id, index],
    )?;
    Ok(())
}

fn mark_rollback_done(db: &Connection, exec_id: &str, index: u32) -> Result<()> {
    db.execute(
        "UPDATE exec_ops SET rollback_state='done', rollback_error=NULL, needs_verify=0 WHERE exec_id=?1 AND op_index=?2",
        params![exec_id, index],
    )?;
    Ok(())
}

fn record_restored_io(db: &Connection, exec_id: &str, index: u32, err: &Error) -> Result<()> {
    let run = active_run_id();
    db.execute(
        "UPDATE exec_ops SET rollback_state='done', needs_verify=1, needs_verify_run=?5, rollback_error=?3, code=?4 WHERE exec_id=?1 AND op_index=?2",
        params![exec_id, index, err.message, err.code_str(), run],
    )?;
    Ok(())
}

/// 干净退出时调用，而且必须在把 `run.lock` 写成 `clean` 之前。
/// 清掉这次 `run_id` 的标记，也清掉还没带 `run_id` 的旧标记。别的运行留下的标记不动。
pub(crate) fn clear_needs_verify_for_run(db: &Connection, run_id: &str) -> Result<()> {
    db.execute(
        "UPDATE exec_ops SET needs_verify=0, needs_verify_run=NULL WHERE needs_verify=1 AND (needs_verify_run IS NULL OR needs_verify_run=?1)",
        params![run_id],
    )?;
    if let Ok(mut guard) = ACTIVE_RUN.lock() {
        if guard.as_deref() == Some(run_id) {
            *guard = None;
        }
    }
    Ok(())
}

fn mark_rollback_unrestored(db: &Connection, exec_id: &str, index: u32, err: &Error) -> Result<()> {
    db.execute(
        "UPDATE exec_ops SET rollback_state='failed', needs_verify=0, attr_restore_failed=0, rollback_error=?3, code=?4 WHERE exec_id=?1 AND op_index=?2",
        params![exec_id, index, err.message, err.code_str()],
    )?;
    Ok(())
}

fn record_attr_restore_failed(db: &Connection, exec_id: &str, index: u32, err: &Error) -> Result<()> {
    db.execute(
        "UPDATE exec_ops SET rollback_state='done', needs_verify=0, attr_restore_failed=1, rollback_error=?3, code=?4 WHERE exec_id=?1 AND op_index=?2",
        params![exec_id, index, err.message, err.code_str()],
    )?;
    Ok(())
}

fn clear_needs_verify(db: &Connection, exec_id: &str, index: u32) -> Result<()> {
    db.execute(
        "UPDATE exec_ops SET needs_verify=0 WHERE exec_id=?1 AND op_index=?2",
        params![exec_id, index],
    )?;
    Ok(())
}

fn rollback_is_done(db: &Connection, exec_id: &str, index: u32) -> Result<bool> {
    let state: Option<String> = db.query_row(
        "SELECT rollback_state FROM exec_ops WHERE exec_id=?1 AND op_index=?2",
        params![exec_id, index],
        |r| r.get(0),
    )?;
    Ok(state.as_deref() == Some("done"))
}

fn move_owes_source_removal(op: &PlanOp) -> bool {
    op.kind == OpKind::Move && !op.keep_source && op.from.is_some()
}

fn file_hash(path: &Path) -> Option<String> {
    fs::read(path).ok().map(|b| sha256_hex(&b))
}

/// 备份还能用：哈希等于升级前快照，或者等于当前原文件。
/// 和快照一致时，即使原文件后来被改过也不删，回滚还要靠它。
/// 对不上快照、也对不上当前文件的，是坏备份，删掉。原文件已经不在时不删。
fn backup_still_valid(fsys: &LocalFs, backup: &Path, rel: &RelPath, expect_hash: Option<&str>) -> Result<bool> {
    let dest = backup_file(backup, rel.as_str());
    let Some(got) = file_hash(&dest) else {
        return Ok(false);
    };
    if expect_hash.is_some_and(|expect| got == expect) {
        return Ok(true);
    }
    let Ok(src) = fsys.hash(rel, true) else {
        return Ok(false);
    };
    if got == src {
        return Ok(true);
    }
    let _ = fs::remove_file(&dest);
    Ok(false)
}

fn manifest_has(backup: &Path, path: &str) -> Result<bool> {
    Ok(read_manifest(backup)?.iter().any(|v| v.get("path").and_then(|p| p.as_str()) == Some(path)))
}

fn manifest_entry(plan: &ExecPlan, op: &PlanOp, rel: &RelPath, backup_rel: &str, hash: &str) -> serde_json::Value {
    json!({
        "execId": plan.id,
        "projectKey": plan.project_key,
        "index": op.index,
        "path": rel.as_str(),
        "backup": backup_rel,
        "hash": hash,
    })
}

fn manifest_backup_rel(backup: &Path, project_rel: &str) -> Option<String> {
    let rows = read_manifest(backup).ok()?;
    rows.iter().rev().find(|v| v.get("path").and_then(|p| p.as_str()) == Some(project_rel)).and_then(|v| {
        v.get("backup").and_then(|p| p.as_str()).map(|s| s.to_string())
    })
}

fn backup_file(backup: &Path, project_rel: &str) -> PathBuf {
    if let Some(rel) = manifest_backup_rel(backup, project_rel) {
        let path = backup.join(&rel);
        if path.is_file() {
            return path;
        }
    }
    backup.join("files").join(project_rel)
}

fn manifest_references(backup: &Path, stored_rel: &str) -> bool {
    read_manifest(backup)
        .ok()
        .into_iter()
        .flatten()
        .any(|v| v.get("backup").and_then(|p| p.as_str()) == Some(stored_rel))
}

fn fresh_backup_rel(backup: &Path, index: u32) -> String {
    let mut gen = 2u32;
    loop {
        let rel = format!("refresh/{index}.r{gen}");
        if !backup.join(&rel).exists() {
            return rel;
        }
        gen += 1;
    }
}

fn fsync_dir(dir: &Path, fault: &mut FaultRuntime) -> Result<()> {
    fault.hit_fsync_dir()?;
    let file = fs::File::open(dir)?;
    file.sync_all()?;
    Ok(())
}

/// 逐层创建还不存在的目录。每新建一层，在真正 fsync 父目录之前走目录同步故障点。
fn create_dirs_synced(path: &Path, fault: &mut FaultRuntime) -> Result<()> {
    if path.as_os_str().is_empty() || path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            create_dirs_synced(parent, fault)?;
        }
    }
    fs::create_dir(path)?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fsync_dir(parent, fault)?;
        }
    }
    Ok(())
}

fn remember_backup_staging(db: &Connection, exec_id: &str, rel: &str) -> Result<()> {
    db.execute(
        "INSERT OR REPLACE INTO exec_temps (exec_id, path) VALUES (?1,?2)",
        params![exec_id, format!("bak:{rel}")],
    )?;
    Ok(())
}

fn forget_backup_staging(db: &Connection, exec_id: &str, rel: &str) -> Result<()> {
    db.execute("DELETE FROM exec_temps WHERE exec_id=?1 AND path=?2", params![exec_id, format!("bak:{rel}")])?;
    Ok(())
}

#[cfg(test)]
thread_local! {
    static FAIL_MANIFEST_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static MUTATE_BEFORE_WRITE: std::cell::RefCell<Option<(String, Vec<u8>)>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn fail_manifest_rename() -> bool {
    FAIL_MANIFEST_RENAME.with(|flag| flag.get())
}

#[cfg(test)]
fn set_fail_manifest_rename(value: bool) {
    FAIL_MANIFEST_RENAME.with(|flag| flag.set(value));
}

#[cfg(test)]
fn set_mutate_before_write(path: Option<(&str, &[u8])>) {
    MUTATE_BEFORE_WRITE.with(|slot| {
        *slot.borrow_mut() = path.map(|(name, bytes)| (name.to_string(), bytes.to_vec()));
    });
}

#[cfg(test)]
fn mutate_before_write(fsys: &LocalFs, rel: &RelPath) {
    MUTATE_BEFORE_WRITE.with(|slot| {
        let Some((name, bytes)) = slot.borrow().clone() else { return };
        if rel.as_str() == name {
            let _ = fs::write(fsys.root().join(name), bytes);
        }
    });
}

/// 新备份写成备份根下的 `refresh/<id>.rN`，和 `files/` 并列，不会盖住项目里同名文件的备份。
/// 校验通过后才改 manifest 指向它，最后才删旧备份。
/// manifest 没写成功时，旧备份和旧记录都还在。
/// manifest 已经指向新备份之后，目录 fsync 失败则留下新备份，以 E_IO 停止。
fn install_refreshed_backup(
    db: &Connection,
    exec_id: &str,
    backup: &Path,
    fsys: &LocalFs,
    plan: &ExecPlan,
    op: &PlanOp,
    rel: &RelPath,
    fault: &mut FaultRuntime,
) -> Result<()> {
    let current = fsys.hash(rel, true).unwrap_or_default();
    let new_rel = fresh_backup_rel(backup, op.index);
    let staging = backup.join(&new_rel);
    let manifest_rel = "manifest.json.htx-new";
    let manifest_tmp = backup.join(manifest_rel);
    remember_backup_staging(db, exec_id, &new_rel)?;
    remember_backup_staging(db, exec_id, manifest_rel)?;
    let abort = |err: Error| -> Result<()> {
        let _ = fs::remove_file(&staging);
        let _ = fs::remove_file(&manifest_tmp);
        let _ = forget_backup_staging(db, exec_id, &new_rel);
        let _ = forget_backup_staging(db, exec_id, manifest_rel);
        Err(err)
    };
    if let Some(parent) = staging.parent() {
        if let Err(err) = create_dirs_synced(parent, fault) {
            return abort(err);
        }
    }
    if let Err(err) = fsys.copy_to(rel, &staging) {
        return abort(err);
    }
    if let Some(parent) = staging.parent() {
        if let Err(err) = fsync_dir(parent, fault) {
            return abort(err);
        }
    }
    if fault.backup_should_corrupt(op.index) {
        if let Err(err) = flip_middle(&staging) {
            return abort(err);
        }
    }
    let got = file_hash(&staging).unwrap_or_default();
    if got != current {
        return abort(Error::new(
            ErrorCode::VerifyFailed,
            format!("{} 的备份和原文件哈希不一致，原文件未改", rel.as_str()),
        ));
    }
    let old_rel = manifest_backup_rel(backup, rel.as_str());
    if let Err(err) = upsert_manifest(backup, &manifest_entry(plan, op, rel, &new_rel, &current), fault) {
        if manifest_references(backup, &new_rel) {
            let _ = fs::remove_file(&manifest_tmp);
            let _ = forget_backup_staging(db, exec_id, manifest_rel);
            return Err(err);
        }
        return abort(err);
    }
    if let Some(old) = old_rel {
        if old != new_rel {
            let old_path = backup.join(&old);
            if old_path.is_file() {
                if let Err(err) = fs::remove_file(&old_path) {
                    return Err(err.into());
                }
                if let Some(parent) = old_path.parent() {
                    fsync_dir(parent, fault)?;
                }
            }
        }
    }
    let _ = forget_backup_staging(db, exec_id, &new_rel);
    let _ = forget_backup_staging(db, exec_id, manifest_rel);
    Ok(())
}

fn upsert_manifest(backup: &Path, entry: &serde_json::Value, fault: &mut FaultRuntime) -> Result<()> {
    let path = entry.get("path").and_then(|p| p.as_str()).unwrap_or("");
    let mut rows = read_manifest(backup)?;
    rows.retain(|v| v.get("path").and_then(|p| p.as_str()) != Some(path));
    rows.push(entry.clone());
    let tmp = backup.join("manifest.json.htx-new");
    {
        let mut f = fs::OpenOptions::new().create(true).write(true).truncate(true).open(&tmp)?;
        for row in &rows {
            writeln!(f, "{row}")?;
        }
        f.sync_all()?;
    }
    #[cfg(test)]
    if fail_manifest_rename() {
        return Err(Error::new(ErrorCode::Io, "manifest 替换失败"));
    }
    crate::vfs::atomic_rename_counted(&tmp, &backup.join("manifest.json"), || fault.hit_fsync_dir())?;
    Ok(())
}

fn backup_op(
    db: &Connection,
    fsys: &LocalFs,
    plan: &ExecPlan,
    backup: &Path,
    op: &PlanOp,
    fault: &mut FaultRuntime,
    refresh_current: bool,
) -> Result<()> {
    if !needs_backup(op) {
        return Ok(());
    }
    let rel = RelPath::parse(op.from.as_deref().unwrap_or(&op.path))?;
    if fsys.stat(&rel).is_err() {
        return Ok(());
    }
    let expect_hash = op.expect.as_ref().map(|s| s.hash.as_str());
    if !refresh_current && backup_still_valid(fsys, backup, &rel, expect_hash)? {
        if !manifest_has(backup, rel.as_str())? {
            let hash = file_hash(&backup_file(backup, rel.as_str())).unwrap_or_default();
            append_manifest(backup, &manifest_entry(plan, op, &rel, &format!("files/{}", rel.as_str()), &hash), fault)?;
        }
        return Ok(());
    }
    // 继续时补备份、以及「仍然覆盖」换备份，都走这一次 backup:N 钩子。计数按计划序号，本轮 FaultRuntime 从零计。
    if let Err(e) = fault.hit_file(
        #[cfg(feature = "fault-inject")]
        Stage::Backup,
        op.index,
    ) {
        return Err(real_code(e));
    }
    if refresh_current {
        return install_refreshed_backup(db, &plan.id, backup, fsys, plan, op, &rel, fault);
    }
    let dest = backup.join("files").join(rel.as_str());
    if let Some(parent) = dest.parent() {
        create_dirs_synced(parent, fault)?;
    }
    fsys.copy_to(&rel, &dest)?;
    if fault.backup_should_corrupt(op.index) {
        flip_middle(&dest)?;
    }
    let expect = fsys.hash(&rel, true).unwrap_or_default();
    let got = file_hash(&dest).unwrap_or_default();
    if got != expect {
        let _ = fs::remove_file(&dest);
        return Err(Error::new(
            ErrorCode::VerifyFailed,
            format!("{} 的备份和原文件哈希不一致，原文件未改", rel.as_str()),
        ));
    }
    if let Some(parent) = dest.parent() {
        fsync_dir(parent, fault)?;
    }
    append_manifest(backup, &manifest_entry(plan, op, &rel, &format!("files/{}", rel.as_str()), &expect), fault)?;
    let _ = db;
    Ok(())
}

fn require_verified_backup(fsys: &LocalFs, backup: &Path, op: &PlanOp) -> Result<()> {
    if !needs_backup(op) {
        return Ok(());
    }
    let rel = RelPath::parse(op.from.as_deref().unwrap_or(&op.path))?;
    if fsys.stat(&rel).is_err() {
        return Ok(());
    }
    let dest = backup_file(backup, rel.as_str());
    let Some(bak) = file_hash(&dest) else {
        return Err(Error::new(
            ErrorCode::VerifyFailed,
            format!("{} 没有已校验的备份，拒绝覆盖", rel.as_str()),
        ));
    };
    let cur = fsys.hash(&rel, true).unwrap_or_default();
    if bak != cur {
        return Err(Error::new(
            ErrorCode::ChangedSinceScan,
            format!("{} 文件在备份后被修改，已停止", rel.as_str()),
        ));
    }
    Ok(())
}

fn observed_state(db: &Connection, exec_id: &str) -> Result<String> {
    let state: String = db.query_row(
        "SELECT state FROM exec_journal WHERE exec_id=?1",
        params![exec_id],
        |r| r.get(0),
    )?;
    Ok(state)
}

fn note_pre_recovery(db: &Connection, exec_id: &str, observed: &str) -> Result<String> {
    db.execute(
        "UPDATE exec_journal SET pre_recovery_state=COALESCE(pre_recovery_state, ?2) WHERE exec_id=?1",
        params![exec_id, observed],
    )?;
    Ok(observed.to_string())
}

fn probe_log_write(db: &Connection, plan: &FaultPlan) -> Result<()> {
    #[cfg(feature = "fault-inject")]
    crate::fault::check_probe(plan)?;
    #[cfg(not(feature = "fault-inject"))]
    let _ = plan;
    db.execute_batch("CREATE TABLE IF NOT EXISTS exec_log_probe (id INTEGER PRIMARY KEY, note TEXT);")
        .map_err(|e| Error::new(ErrorCode::LogWrite, e.to_string()))?;
    db.execute("INSERT INTO exec_log_probe (note) VALUES ('probe')", [])
        .map_err(|e| Error::new(ErrorCode::LogWrite, e.to_string()))?;
    db.execute("DELETE FROM exec_log_probe", [])
        .map_err(|e| Error::new(ErrorCode::LogWrite, e.to_string()))?;
    Ok(())
}

enum WriteGuess {
    Written,
    NotWritten,
    Conflict(ConflictFile),
}

fn guess_written(fsys: &LocalFs, backup: &Path, op: &PlanOp) -> Result<WriteGuess> {
    let target_rel = RelPath::parse(&op.path)?;
    let target_hash = op.bytes.as_ref().map(|b| sha256_hex(b));
    let expect_hash = op.expect.as_ref().map(|s| s.hash.clone());
    let current = fsys.hash(&target_rel, true).ok();
    let source = op.from.as_deref().unwrap_or(op.path.as_str());
    let bak = file_hash(&backup_file(backup, source));
    if let (Some(cur), Some(target)) = (&current, &target_hash) {
        if cur == target {
            return Ok(WriteGuess::Written);
        }
    }
    if let (Some(cur), Some(expect)) = (&current, &expect_hash) {
        if cur == expect {
            return Ok(WriteGuess::NotWritten);
        }
    }
    if let (Some(cur), Some(bak)) = (&current, &bak) {
        if cur == bak {
            return Ok(WriteGuess::NotWritten);
        }
    }
    if current.is_none() && matches!(op.kind, OpKind::Create) {
        return Ok(WriteGuess::NotWritten);
    }
    // 日志里已经有这条删除的 doing 记录，目标也确实不在了：算做完，不报「升级后被删除」。
    if current.is_none() && matches!(op.kind, OpKind::Delete) {
        return Ok(WriteGuess::Written);
    }
    if current.is_none() {
        return Ok(WriteGuess::Conflict(ConflictFile {
            path: op.path.clone(),
            detail: DETAIL_DELETED.into(),
        }));
    }
    Ok(WriteGuess::Conflict(ConflictFile {
        path: op.path.clone(),
        detail: "无法判断是否已经写入".into(),
    }))
}

fn op_touched(row: &OpRow) -> bool {
    row.written || row.state == "done" || row.state == "doing"
}

fn restore_still_matches(fsys: &LocalFs, backup: &Path, op: &PlanOp) -> Result<bool> {
    match restored_hashes_match(fsys, backup, op) {
        Ok(()) => Ok(true),
        Err(e) if e.code == ErrorCode::NotFound || e.code == ErrorCode::Io => Ok(false),
        Err(e) => Err(e),
    }
}

/// 已恢复但目录同步失败的文件，只在上次进程异常退出后的第一次启动时和备份比哈希。
/// 只核对 `crashed_run` 那一次运行打上的标记。没有 `run_id` 的旧文件只核对未打标的行。
/// 一致就清掉 needs_verify。不一致则改成未恢复，历史记「部分回滚」，错误码用 E_IO。不改文件内容。
fn reconcile_unverified_restores(db: &Connection, fsys: &LocalFs, crashed_run: Option<&str>) -> Result<()> {
    let mut stmt = db.prepare(
        "SELECT o.exec_id, o.op_index, o.op_json, j.backup_root, j.state
         FROM exec_ops o
         JOIN exec_journal j ON j.exec_id = o.exec_id
         WHERE o.needs_verify = 1 AND o.rollback_state = 'done'
           AND ((?1 IS NULL AND o.needs_verify_run IS NULL) OR o.needs_verify_run = ?1)",
    )?;
    let rows = stmt.query_map(params![crashed_run], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)? as u32,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
        ))
    })?;
    let mut loaded = Vec::new();
    for row in rows {
        loaded.push(row?);
    }
    drop(stmt);
    let mut mismatched: HashMap<String, (String, Vec<serde_json::Value>)> = HashMap::new();
    for (exec_id, index, op_json, backup_root, state) in loaded {
        let Ok(op) = serde_json::from_str::<PlanOp>(&op_json) else {
            continue;
        };
        let backup = PathBuf::from(&backup_root);
        if restore_still_matches(fsys, &backup, &op)? {
            db.execute(
                "UPDATE exec_ops SET needs_verify=0, needs_verify_run=NULL WHERE exec_id=?1 AND op_index=?2",
                params![exec_id, index],
            )?;
            continue;
        }
        let reason = "回滚后内容和备份不一致";
        db.execute(
            "UPDATE exec_ops SET rollback_state='failed', needs_verify=0, code='E_IO', rollback_error=?3 WHERE exec_id=?1 AND op_index=?2",
            params![exec_id, index, reason],
        )?;
        mismatched.entry(exec_id).or_insert_with(|| (state, Vec::new())).1.push(json!({
            "path": op.path,
            "reason": reason,
            "code": "E_IO",
        }));
    }
    for (exec_id, (state, items)) in mismatched {
        let paths_json: String = db
            .query_row("SELECT paths_json FROM history WHERE exec_id=?1", params![exec_id], |r| r.get(0))
            .unwrap_or_else(|_| "[]".into());
        let mut paths: Vec<serde_json::Value> = serde_json::from_str(&paths_json).unwrap_or_default();
        for item in items {
            let path = item.get("path").and_then(|p| p.as_str()).unwrap_or("");
            if paths.iter().any(|v| v.get("path").and_then(|p| p.as_str()) == Some(path)) {
                continue;
            }
            paths.push(item);
        }
        let encoded = serde_json::to_string(&paths).unwrap_or_else(|_| "[]".into());
        if state == "ROLLED_BACK" {
            db.execute("UPDATE exec_journal SET state='ROLLBACK_PARTIAL' WHERE exec_id=?1", params![exec_id])?;
            db.execute(
                "UPDATE history SET result=?2, paths_json=?3 WHERE exec_id=?1",
                params![exec_id, "部分回滚", encoded],
            )?;
        } else {
            db.execute("UPDATE history SET paths_json=?2 WHERE exec_id=?1", params![exec_id, encoded])?;
        }
    }
    Ok(())
}

/// 启动时的崩溃恢复。不按文件名扫临时文件。原状态默认保留，并记下恢复前的状态。
/// LOG_FAILED 先试写日志，成功后才改数据库、删临时文件。试写失败则这一笔原样留下。
/// 不在这里核对 needs_verify。那一步只由 `begin_run` 在判定为异常退出之后、本函数之前做。
pub fn recover_on_startup(db: &Connection, fsys: &LocalFs) -> Result<Vec<RecoveryOffer>> {
    #[cfg(feature = "fault-inject")]
    let probe_plan = crate::fault::active_plan();
    #[cfg(not(feature = "fault-inject"))]
    let probe_plan = FaultPlan::default();
    let mut stmt = db.prepare(
        "SELECT exec_id, state, plan_json, backup_root FROM exec_journal ORDER BY started_at, exec_id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let mut loaded = Vec::new();
    for row in rows {
        loaded.push(row?);
    }
    drop(stmt);
    let mut offers = Vec::new();
    for (exec_id, state, plan_json, backup_root) in loaded {
        if matches!(resume_admission(&state), ResumeAdmission::Reject) {
            continue;
        }
        if state == "LOG_FAILED" {
            if let Err(err) = probe_log_write(db, &probe_plan) {
                offers.push(RecoveryOffer {
                    exec_id,
                    state: state.clone(),
                    pre_recovery_state: state,
                    actions: resume_actions("LOG_FAILED").into_iter().map(|s| s.to_string()).collect(),
                    conflicts: Vec::new(),
                    code: Some(err.code_str().to_string()),
                    error: Some(err.message),
                });
                continue;
            }
        }
        let kept = note_pre_recovery(db, &exec_id, &state)?;
        let backup = PathBuf::from(&backup_root);
        let _ = recover_registered_temps_in(db, fsys, &exec_id, Some(&backup))?;
        let mut conflicts = Vec::new();
        if resume_admission(&kept) == ResumeAdmission::Upgrade {
            if let Ok(plan) = serde_json::from_str::<ExecPlan>(&plan_json) {
                let progress = load_rows(db, &exec_id)?;
                for op in &plan.ops {
                    let Some(row) = progress.get(&op.index) else { continue };
                    // 只有 doing 的日志不完整。failed 且 written=0 表示这次写入没有落地。
                    if row.state != "doing" || row.written {
                        continue;
                    }
                    match guess_written(fsys, &backup, op)? {
                        WriteGuess::Written => mark_written(db, &exec_id, op.index)?,
                        WriteGuess::NotWritten => {}
                        WriteGuess::Conflict(c) => conflicts.push(c),
                    }
                }
            }
        }
        offers.push(RecoveryOffer {
            exec_id,
            state: kept.clone(),
            pre_recovery_state: state,
            actions: resume_actions(&kept).into_iter().map(|s| s.to_string()).collect(),
            conflicts,
            code: None,
            error: None,
        });
    }
    Ok(offers)
}

pub enum RunStart {
    /// 另一个进程正拿着 run.lock。不要读、写或删除这个文件，也不要做崩溃恢复。
    AnotherInstance,
    Started {
        lock: crate::runlock::RunLock,
        abnormal_exit: bool,
        offers: Vec<RecoveryOffer>,
    },
}

/// 占住 run.lock。状态是 `running` 时先核对 needs_verify，核对完才写入新的 `run_id`。
/// 然后做崩溃恢复。锁一直拿到调用方放开为止。
pub fn begin_run(db: &Connection, fsys: &LocalFs, data_dir: &Path) -> Result<RunStart> {
    match crate::runlock::acquire_run_lock(data_dir)? {
        crate::runlock::RunAcquire::AnotherInstance => Ok(RunStart::AnotherInstance),
        crate::runlock::RunAcquire::Held(lock) => {
            let abnormal_exit = lock.abnormal_exit();
            if abnormal_exit {
                reconcile_unverified_restores(db, fsys, lock.crashed_run())?;
            }
            lock.publish_running()?;
            set_active_run(Some(lock.run_id().to_string()));
            let offers = recover_on_startup(db, fsys)?;
            Ok(RunStart::Started { lock, abnormal_exit, offers })
        }
    }
}

pub fn execute(db: &Connection, fsys: &LocalFs, plan: &ExecPlan, opts: &ExecOpts, fault: FaultPlan) -> Result<ExecReport> {
    if opts.remote {
        return Err(crate::remote::remote_write_unsupported());
    }
    #[cfg(feature = "fault-inject")]
    if !opts.remote && fault.items.iter().any(|i| matches!(i, crate::fault::Item::At { stage: Stage::SftpDisconnect, .. })) {
        let msg = "本地目标不能使用断开连接这种故障".to_string();
        eprintln!("UPG_FAULT: {msg}");
        #[cfg(test)]
        return Err(Error::new(ErrorCode::Internal, msg));
        #[cfg(not(test))]
        std::process::exit(1);
    }
    let mut fault = FaultRuntime::new(fault);
    let mut recorded_temps: Vec<RelPath> = Vec::new();
    let backup = opts.backup_root.join(&plan.id);
    create_dirs_synced(&backup.join("files"), &mut fault)?;
    let mut files = Vec::new();
    let started = now_ms();
    log_write(db, &mut fault, &plan.id, "prepared", || {
        db.execute(
            "INSERT INTO exec_journal (exec_id, state, project_key, plan_json, backup_root, started_at) VALUES (?1,?2,?3,?4,?5,?6)",
            params![plan.id, "PREPARED", plan.project_key, serde_json::to_string(plan).unwrap_or_default(), backup.display().to_string(), started],
        )?;
        for op in &plan.ops {
            db.execute(
                "INSERT INTO exec_ops (exec_id, op_index, op_json, state) VALUES (?1,?2,?3,'pending')",
                params![plan.id, op.index, serde_json::to_string(op).unwrap_or_default()],
            )?;
        }
        Ok(())
    })?;
    if stop(opts, "prepared") {
        return Ok(partial(plan, &backup, "PREPARED", files, true));
    }

    for op in &plan.ops {
        if let Err(e) = backup_op(db, fsys, plan, &backup, op, &mut fault, false) {
            return finish_stopped(db, plan, &backup, files, op, &real_code(e));
        }
    }
    log_write(db, &mut fault, &plan.id, "backed_up", || {
        db.execute("UPDATE exec_journal SET state='BACKED_UP' WHERE exec_id=?1", params![plan.id])?;
        Ok(())
    })?;
    if stop(opts, "backed_up") {
        return Ok(partial(plan, &backup, "BACKED_UP", files, true));
    }

    for op in &plan.ops {
        if !will_apply(op, opts.allow_readonly) {
            set_op(db, &plan.id, op.index, "skipped", Some("只读文件未勾选覆盖"), Some("E_PERMISSION"))?;
            files.push(fr(op, "skipped", Some("E_PERMISSION"), Some("跳过（只读）")));
            continue;
        }
        let doing = format!("doing:{}", op.index);
        log_write(db, &mut fault, &plan.id, &doing, || {
            set_op(db, &plan.id, op.index, "doing", None, None)
        })?;
        if stop(opts, &doing) {
            files.push(fr(op, "doing", None, None));
            return Ok(partial(plan, &backup, "BACKED_UP", files, true));
        }
        if let Ok(rel) = RelPath::parse(op.from.as_deref().unwrap_or(&op.path)) {
            #[cfg(test)]
            mutate_before_write(fsys, &rel);
            #[cfg(not(test))]
            let _ = rel;
        }
        if let Err(e) = require_verified_backup(fsys, &backup, op) {
            return finish_stopped(db, plan, &backup, files, op, &real_code(e));
        }
        let res = apply_one(db, &plan.id, fsys, op, opts, &mut fault, &mut recorded_temps);
        match res {
            Ok(()) => {
                let done = format!("done:{}", op.index);
                let wrote = op.bytes.as_ref().map(|b| sha256_hex(b));
                log_write(db, &mut fault, &plan.id, &done, || {
                    set_op(db, &plan.id, op.index, "done", None, None)?;
                    if let Some(h) = &wrote {
                        db.execute(
                            "UPDATE exec_ops SET after_hash=?3 WHERE exec_id=?1 AND op_index=?2",
                            params![plan.id, op.index, h],
                        )?;
                    }
                    Ok(())
                })?;
                files.push(fr(op, "done", None, None));
                if stop(opts, &done) {
                    return Ok(partial(plan, &backup, "BACKED_UP", files, true));
                }
            }
            Err(e) if e.code == ErrorCode::LogWrite => {
                return Err(e);
            }
            Err(e) => {
                // 任何一个文件失败就停，不再写后面的文件。错误码用真实失败码，不用 E_FAULT。
                return finish_stopped(db, plan, &backup, files, op, &real_code(e));
            }
        }
    }

    log_write(db, &mut fault, &plan.id, "verifying", || {
        db.execute("UPDATE exec_journal SET state='VERIFYING' WHERE exec_id=?1", params![plan.id])?;
        Ok(())
    })?;
    if stop(opts, "verifying") {
        return Ok(partial(plan, &backup, "VERIFYING", files, true));
    }
    for op in &plan.ops {
        if !will_apply(op, opts.allow_readonly) {
            continue;
        }
        if let Some(bytes) = &op.bytes {
            if let Err(e) = fault.hit_file(
                #[cfg(feature = "fault-inject")]
                Stage::VerifyPost,
                op.index,
            ) {
                return finish_stopped(db, plan, &backup, files, op, &real_code(e));
            }
            let rel = RelPath::parse(&op.path)?;
            let got = fsys.hash(&rel, true).unwrap_or_default();
            let expect = sha256_hex(bytes);
            if got != expect {
                let err = Error::new(ErrorCode::VerifyFailed, format!("{} 写入后的哈希和升级包不一致", op.path));
                return finish_stopped(db, plan, &backup, files, op, &err);
            }
        }
    }
    if let Err(e) = fault.hit_commit() {
        let err = Error::new(ErrorCode::LogWrite, e.message);
        let _ = db.execute("UPDATE exec_journal SET state='LOG_FAILED' WHERE exec_id=?1", params![plan.id]);
        return Err(err);
    }
    log_write(db, &mut fault, &plan.id, "committed", || {
        db.execute("UPDATE exec_journal SET state='COMMITTED', finished_at=?2 WHERE exec_id=?1", params![plan.id, now_ms()])?;
        let summary = serde_json::to_string(&plan.summary).unwrap_or_else(|_| "{}".into());
        db.execute(
            "INSERT OR REPLACE INTO history (exec_id, project_key, summary_json, result, paths_json, created_at, backup_available) VALUES (?1,?2,?3,?4,?5,?6,1)",
            params![plan.id, plan.project_key, summary, "成功", format!("{} ↔ {}", plan.project_key, plan.id), now_ms()],
        )?;
        Ok(())
    })?;
    cleanup_recorded(fsys, &recorded_temps)?;
    Ok(base_report(&plan.id, "COMMITTED", files, &backup, false, "成功", false))
}

fn apply_one(
    db: &Connection,
    exec_id: &str,
    fsys: &LocalFs,
    op: &PlanOp,
    opts: &ExecOpts,
    fault: &mut FaultRuntime,
    temps: &mut Vec<RelPath>,
) -> Result<()> {
    if opts.remote {
        if let Err(e) = fault.hit_file(
            #[cfg(feature = "fault-inject")]
            Stage::SftpDisconnect,
            op.index,
        ) {
            return Err(real_code(e));
        }
    }
    match op.kind {
        OpKind::Delete => {
            fault.hit_file(
                #[cfg(feature = "fault-inject")]
                Stage::Rename,
                op.index,
            )
            .map_err(real_code)?;
            let rel = RelPath::parse(&op.path)?;
            let restore = op.readonly && opts.allow_readonly;
            if restore {
                fsys.set_readonly(&rel, false)?;
            }
            let removed = fsys.remove_path(&rel);
            if removed.is_err() && restore {
                fsys.set_readonly(&rel, true)?;
            }
            removed?;
            mark_written(db, exec_id, op.index)?;
            Ok(())
        }
        OpKind::Create | OpKind::Overwrite | OpKind::Move => {
            let bytes = op.bytes.clone().unwrap_or_default();
            let dest = RelPath::parse(&op.path)?;
            let restore = op.readonly && opts.allow_readonly && op.kind == OpKind::Overwrite;
            if restore {
                fsys.set_readonly(&dest, false)?;
            }
            let wrote = write_one(db, exec_id, fsys, op, opts, fault, temps, &dest, &bytes);
            if wrote.is_err() && restore {
                fsys.set_readonly(&dest, true)?;
            }
            wrote
        }
    }
}

fn write_one(
    db: &Connection,
    exec_id: &str,
    fsys: &LocalFs,
    op: &PlanOp,
    opts: &ExecOpts,
    fault: &mut FaultRuntime,
    temps: &mut Vec<RelPath>,
    dest: &RelPath,
    bytes: &[u8],
) -> Result<()> {
    if let Err(e) = fault.hit_file(
        #[cfg(feature = "fault-inject")]
        Stage::Write,
        op.index,
    ) {
        return Err(real_code(e));
    }
    let tmp = fsys.allocate_temp(dest)?;
    remember_temp(db, exec_id, &tmp)?;
    if let Err(e) = fsys.create_temp_file(&tmp, bytes) {
        discard_temp(fsys, db, exec_id, &tmp);
        return Err(e);
    }
    temps.push(tmp.clone());
    let bail = |e: Error| -> Result<()> {
        discard_temp(fsys, db, exec_id, &tmp);
        Err(e)
    };
    #[cfg(any(test, feature = "fault-inject"))]
    if opts.corrupt_temp {
        let p = fsys.abs(&tmp)?;
        let mut data = fs::read(&p)?;
        if data.is_empty() { data.push(0); } else { data[0] ^= 0xff; }
        fs::write(&p, &data)?;
    }
    if let Err(e) = fault.hit_file(
        #[cfg(feature = "fault-inject")]
        Stage::VerifyPre,
        op.index,
    ) {
        return bail(real_code(e));
    }
    let got = fsys.read_temp_hash(&tmp)?;
    let expect = sha256_hex(bytes);
    if got != expect {
        return bail(Error::new(ErrorCode::VerifyFailed, format!("{} 临时文件校验失败，原文件未改", op.path)));
    }
    if let Some(mode) = op.mode {
        if let Err(e) = fsys.set_meta(&tmp, Some(mode), None) {
            return bail(e);
        }
    }
    if let Err(e) = fault.hit_file(
        #[cfg(feature = "fault-inject")]
        Stage::Rename,
        op.index,
    ) {
        return bail(real_code(e));
    }
    if let Err(e) = fsys.rename_over_placed(&tmp, dest) {
        return bail(e);
    }
    // 目标内容已经换上。目录 fsync 失败也必须留下 written=1，回滚才知道要恢复。
    mark_written(db, exec_id, op.index)?;
    forget_temp(db, exec_id, &tmp)?;
    if let Err(e) = fsys.fsync_parent_of(dest, || fault.hit_fsync_dir()) {
        return Err(e);
    }
    if !op.merged {
        if let Some(ms) = op.source_mtime_ms {
            fsys.set_meta(dest, None, Some(ms))?;
        }
    }
    if op.readonly && opts.allow_readonly && op.kind == OpKind::Overwrite {
        fsys.set_readonly(dest, true)?;
    }
    if move_owes_source_removal(op) {
        finish_source_removal(db, exec_id, fsys, op, fault)?;
    }
    Ok(())
}

/// 写入已经落地、目录同步却失败时，继续执行要补上修改时间、只读属性和父目录同步。
/// 失败不把 written 清掉。
fn reapply_written_side(
    db: &Connection,
    exec_id: &str,
    fsys: &LocalFs,
    op: &PlanOp,
    opts: &ExecOpts,
    fault: &mut FaultRuntime,
    source_removed: bool,
) -> Result<()> {
    let dest = RelPath::parse(&op.path)?;
    if !op.merged {
        if let Some(ms) = op.source_mtime_ms {
            fsys.set_meta(&dest, None, Some(ms))?;
        }
    }
    if op.readonly && opts.allow_readonly && op.kind == OpKind::Overwrite {
        fsys.set_readonly(&dest, true)?;
    }
    fsys.fsync_parent_of(&dest, || fault.hit_fsync_dir())?;
    if move_owes_source_removal(op) && !source_removed {
        finish_source_removal(db, exec_id, fsys, op, fault)?;
    }
    Ok(())
}

/// 移动的后半段：源文件已经删掉就记 source_removed，再同步目录。失败不把前半段的 written 清掉。
fn finish_source_removal(
    db: &Connection,
    exec_id: &str,
    fsys: &LocalFs,
    op: &PlanOp,
    fault: &mut FaultRuntime,
) -> Result<()> {
    let Some(from) = &op.from else { return Ok(()) };
    let src = RelPath::parse(from)?;
    if fsys.stat(&src).is_ok() {
        fsys.remove_path(&src)?;
    }
    mark_source_removed(db, exec_id, op.index)?;
    fsys.fsync_parent_of(&src, || fault.hit_fsync_dir())?;
    Ok(())
}

fn needs_backup(op: &PlanOp) -> bool {
    match op.kind {
        OpKind::Overwrite | OpKind::Delete => true,
        OpKind::Move => !op.keep_source,
        OpKind::Create => false,
    }
}

/// 备份清单是 JSON 行文件：每成功备份一个文件就追加一行并 fsync。
/// 最后一行如果写到一半读不出来，前面的行仍然有效。
fn append_manifest(backup: &Path, entry: &serde_json::Value, fault: &mut FaultRuntime) -> Result<()> {
    let path = backup.join("manifest.json");
    let mut f = fs::OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(f, "{entry}")?;
    f.sync_all()?;
    fsync_dir(backup, fault)?;
    Ok(())
}

pub fn read_manifest(backup: &Path) -> Result<Vec<serde_json::Value>> {
    let path = backup.join("manifest.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(&path)?;
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(v) => out.push(v),
            Err(_) if i + 1 == lines.len() => {}
            Err(e) => return Err(Error::new(ErrorCode::Internal, format!("manifest.json 第 {} 行无法解析：{e}", i + 1))),
        }
    }
    Ok(out)
}

fn real_code(e: Error) -> Error {
    if e.code == ErrorCode::Fault {
        Error::new(ErrorCode::Internal, e.message)
    } else {
        e
    }
}

fn flip_middle(path: &Path) -> Result<()> {
    let mut data = fs::read(path)?;
    if data.is_empty() {
        data.push(0xff);
    } else {
        let i = data.len() / 2;
        data[i] ^= 0xff;
    }
    let mut f = fs::OpenOptions::new().write(true).truncate(true).open(path)?;
    f.write_all(&data)?;
    f.sync_all()?;
    Ok(())
}

fn cleanup_recorded(fsys: &LocalFs, temps: &[RelPath]) -> Result<()> {
    for t in temps {
        if fsys.stat(t).is_ok() {
            fsys.remove_path(t)?;
        }
    }
    Ok(())
}

fn base_report(exec_id: &str, state: &str, files: Vec<FileResult>, backup: &Path, log_stopped: bool, history_result: &str, sheet: bool) -> ExecReport {
    ExecReport {
        exec_id: exec_id.to_string(),
        state: state.into(),
        files,
        backup_root: backup.display().to_string(),
        log_stopped,
        history_result: history_result.into(),
        sheet_actions: if sheet { vec!["回滚".into(), "稍后处理".into()] } else { Vec::new() },
    }
}

fn failed_paths_json(files: &[FileResult]) -> String {
    let list: Vec<_> = files.iter().filter(|f| f.state == "failed").map(|f| json!({"path": f.path, "code": f.code})).collect();
    serde_json::to_string(&list).unwrap_or_else(|_| "[]".into())
}

fn finish_stopped(db: &Connection, plan: &ExecPlan, backup: &Path, mut files: Vec<FileResult>, op: &PlanOp, e: &Error) -> Result<ExecReport> {
    set_op(db, &plan.id, op.index, "failed", Some(&e.message), Some(e.code_str()))?;
    if let Some(f) = files.iter_mut().find(|f| f.index == op.index) {
        f.state = "failed".into();
        f.error = Some(e.message.clone());
        f.code = Some(e.code_str().to_string());
    } else {
        files.push(fr(op, "failed", Some(e.code_str()), Some(&e.message)));
    }
    db.execute("UPDATE exec_journal SET state='STOPPED' WHERE exec_id=?1", params![plan.id])?;
    let summary = serde_json::to_string(&plan.summary).unwrap_or_else(|_| "{}".into());
    db.execute(
        "INSERT OR REPLACE INTO history (exec_id, project_key, summary_json, result, paths_json, created_at, backup_available) VALUES (?1,?2,?3,?4,?5,?6,1)",
        params![plan.id, plan.project_key, summary, "部分失败", failed_paths_json(&files), now_ms()],
    )?;
    Ok(base_report(&plan.id, "STOPPED", files, backup, false, "部分失败", true))
}

fn live_content_hash(fsys: &LocalFs, project_rel: &str) -> Result<Option<String>> {
    let rel = RelPath::parse(project_rel)?;
    match fsys.hash(&rel, true) {
        Ok(hash) => Ok(Some(hash)),
        Err(e) if e.code == ErrorCode::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn backup_content_hash(backup: &Path, project_rel: &str) -> Option<String> {
    file_hash(&backup_file(backup, project_rel))
}

/// 恢复是否成立，只看哈希是否和备份一致。文件还在或已经不在，本身不算恢复成功。
fn restored_hashes_match(fsys: &LocalFs, backup: &Path, op: &PlanOp) -> Result<()> {
    match op.kind {
        OpKind::Create => {
            let backed = backup_content_hash(backup, &op.path);
            let live = live_content_hash(fsys, &op.path)?;
            if live.is_none() && backed.is_none() {
                return Ok(());
            }
            Err(Error::new(ErrorCode::Io, format!("{} 回滚后内容和备份不一致", op.path)))
        }
        OpKind::Overwrite | OpKind::Delete => require_matches_backup(fsys, backup, &op.path),
        OpKind::Move if op.keep_source => {
            // 保留旧文件时，原路径本来就要留下，用户可以改它。回滚只要求新路径已经不在。
            if live_content_hash(fsys, &op.path)?.is_some() {
                return Err(Error::new(ErrorCode::Io, format!("{} 回滚后内容和备份不一致", op.path)));
            }
            Ok(())
        }
        OpKind::Move => {
            let from_raw = op.from.as_deref().unwrap_or("");
            require_matches_backup(fsys, backup, from_raw)?;
            if live_content_hash(fsys, &op.path)?.is_some() {
                return Err(Error::new(ErrorCode::Io, format!("{} 回滚后内容和备份不一致", op.path)));
            }
            Ok(())
        }
    }
}

fn require_matches_backup(fsys: &LocalFs, backup: &Path, project_rel: &str) -> Result<()> {
    let Some(expect) = backup_content_hash(backup, project_rel) else {
        return Err(Error::new(ErrorCode::NotFound, format!("备份里没有 {project_rel}")));
    };
    match live_content_hash(fsys, project_rel)? {
        Some(hash) if hash == expect => Ok(()),
        _ => Err(Error::new(ErrorCode::Io, format!("{project_rel} 回滚后内容和备份不一致"))),
    }
}

fn verify_restored(fsys: &LocalFs, backup: &Path, op: &PlanOp) -> Result<()> {
    restored_hashes_match(fsys, backup, op)?;
    if op.kind == OpKind::Create {
        let rel = RelPath::parse(&op.path)?;
        let _ = remove_empty_parents(fsys, &rel);
    }
    Ok(())
}

fn remove_empty_parents(fsys: &LocalFs, rel: &RelPath) -> Result<()> {
    let mut dir = fsys.abs(rel)?.parent().map(|p| p.to_path_buf());
    let root = fsys.root().to_path_buf();
    while let Some(d) = dir {
        if d == root || !d.starts_with(&root) {
            break;
        }
        if fs::remove_dir(&d).is_err() {
            break;
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
    Ok(())
}

pub fn unfinished_ops(db: &Connection, exec_id: &str) -> Result<Vec<PlanOp>> {
    let mut stmt = db.prepare("SELECT op_json, state FROM exec_ops WHERE exec_id=?1 ORDER BY op_index")?;
    let rows = stmt.query_map(params![exec_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (json, state) = row?;
        if state == "done" || state == "skipped" {
            continue;
        }
        if let Ok(op) = serde_json::from_str::<PlanOp>(&json) {
            out.push(op);
        }
    }
    Ok(out)
}

/// 崩溃后选「继续」：先对还没做完的操作重新做变动检测。只认内容哈希，改修改时间不算。
pub fn continue_change_detection(db: &Connection, fsys: &LocalFs, exec_id: &str) -> Result<Vec<ChangeHit>> {
    let ops = unfinished_ops(db, exec_id)?;
    detect_changes(fsys, &ops)
}

/// 每个项目保留最近 `keep` 次，并且超过 `max_age_ms` 的备份删掉。
/// 历史行留着，只把回滚置为不可用（界面显示「备份已清理」）。
pub fn sweep_backups(db: &Connection, now_ms: i64, keep: u32, max_age_ms: i64) -> Result<Vec<String>> {
    let mut stmt = db.prepare(
        "SELECT h.exec_id, h.project_key, h.created_at, j.backup_root
         FROM history h
         LEFT JOIN exec_journal j ON j.exec_id = h.exec_id
         WHERE h.backup_available = 1
         ORDER BY h.project_key, h.created_at DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })?;
    let mut grouped: Vec<(String, String, i64, Option<String>)> = Vec::new();
    for row in rows {
        grouped.push(row?);
    }
    let mut cleaned = Vec::new();
    let mut seen_project = String::new();
    let mut index_in_project = 0u32;
    for (exec_id, project, created, root) in grouped {
        if project != seen_project {
            seen_project = project;
            index_in_project = 0;
        }
        let too_many = index_in_project >= keep;
        let too_old = now_ms.saturating_sub(created) > max_age_ms;
        index_in_project += 1;
        if too_many || too_old {
            if let Some(root) = root {
                let _ = fs::remove_dir_all(root);
            }
            mark_backup_cleaned(db, &exec_id)?;
            cleaned.push(exec_id);
        }
    }
    Ok(cleaned)
}

pub fn rollback_disabled_reason(backup_available: bool) -> Option<&'static str> {
    if backup_available { None } else { Some("备份已清理") }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PreviewCounts {
    pub create: u32,
    pub overwrite: u32,
    pub move_count: u32,
    pub delete: u32,
    /// 没勾选「覆盖只读文件」时，这些只读覆盖/删除不计入覆盖或删除。
    pub skip_readonly: u32,
    pub merged: u32,
}

/// 预演会写下去的操作。执行循环用同一个判断，保证清单和实际写入一致。
pub fn will_apply(op: &PlanOp, allow_readonly: bool) -> bool {
    !(op.readonly && !allow_readonly && matches!(op.kind, OpKind::Overwrite | OpKind::Delete))
}

pub fn preview_counts(ops: &[PlanOp], allow_readonly: bool) -> PreviewCounts {
    let mut c = PreviewCounts::default();
    for op in ops {
        if !will_apply(op, allow_readonly) {
            c.skip_readonly += 1;
            continue;
        }
        match op.kind {
            OpKind::Create => c.create += 1,
            OpKind::Overwrite => c.overwrite += 1,
            OpKind::Move => c.move_count += 1,
            OpKind::Delete => c.delete += 1,
        }
        if op.merged {
            c.merged += 1;
        }
    }
    c
}

pub fn skip_readonly_label(n: u32) -> String {
    format!("跳过（只读）{n}")
}

pub fn planned_write_paths(ops: &[PlanOp], allow_readonly: bool) -> Vec<String> {
    ops.iter().filter(|op| will_apply(op, allow_readonly)).map(|op| op.path.clone()).collect()
}

pub fn rollback_conflict_error(files: &[ConflictFile]) -> Option<Error> {
    if files.is_empty() {
        None
    } else {
        Some(
            Error::new(ErrorCode::RollbackConflict, format!("回滚发现 {} 个冲突文件", files.len()))
                .with_paths(files.iter().map(|f| f.path.clone()).collect()),
        )
    }
}

fn log_write<F: FnOnce() -> Result<()>>(db: &Connection, fault: &mut FaultRuntime, exec_id: &str, _label: &str, f: F) -> Result<()> {
    if let Err(e) = fault.hit_log() {
        // :panic 在注入点直接 abort，到不了这里。这里只处理日志写入失败。
        let err = Error::new(ErrorCode::LogWrite, format!("事务日志写入失败：{}", e.message));
        let _ = db.execute("UPDATE exec_journal SET state='LOG_FAILED' WHERE exec_id=?1", params![exec_id]);
        return Err(err);
    }
    f().map_err(|e| {
        let _ = db.execute("UPDATE exec_journal SET state='LOG_FAILED' WHERE exec_id=?1", params![exec_id]);
        Error::new(ErrorCode::LogWrite, e.message)
    })
}

fn set_op(db: &Connection, id: &str, index: u32, state: &str, error: Option<&str>, code: Option<&str>) -> Result<()> {
    db.execute(
        "UPDATE exec_ops SET state=?3, error=?4, code=?5 WHERE exec_id=?1 AND op_index=?2",
        params![id, index, state, error, code],
    )?;
    Ok(())
}

fn stop(opts: &ExecOpts, label: &str) -> bool {
    #[cfg(any(test, feature = "fault-inject"))]
    {
        opts.stop_after_log.as_deref() == Some(label)
    }
    #[cfg(not(any(test, feature = "fault-inject")))]
    {
        let _ = (opts, label);
        false
    }
}

fn partial(plan: &ExecPlan, backup: &Path, state: &str, files: Vec<FileResult>, stopped: bool) -> ExecReport {
    base_report(&plan.id, state, files, backup, stopped, "", false)
}

fn fr(op: &PlanOp, state: &str, code: Option<&str>, error: Option<&str>) -> FileResult {
    FileResult {
        index: op.index,
        path: op.path.clone(),
        kind: format!("{:?}", op.kind).to_lowercase(),
        state: state.into(),
        error: error.map(|s| s.to_string()),
        code: code.map(|s| s.to_string()),
    }
}

fn parse_attr_failed(raw: &str) -> (u32, Vec<String>) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return (0, Vec::new());
    };
    let count = value.get("count").and_then(|item| item.as_u64()).unwrap_or(0) as u32;
    let paths = value
        .get("paths")
        .and_then(|item| item.as_array())
        .map(|items| items.iter().filter_map(|item| item.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    (count, paths)
}

fn attr_failed_payload(paths: &[String]) -> String {
    serde_json::to_string(&json!({ "count": paths.len(), "paths": paths })).unwrap_or_else(|_| "{\"count\":0,\"paths\":[]}".into())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRow {
    pub exec_id: String,
    pub project_key: String,
    pub result: String,
    pub paths_json: String,
    pub created_at: i64,
    pub backup_available: bool,
    /// 内容已恢复、权限或修改时间没恢复的文件数。
    pub attr_failed_count: u32,
    pub attr_failed_paths: Vec<String>,
}

/// 只读历史。不核对 needs_verify，也不改回滚状态。
pub fn list_history(db: &Connection) -> Result<Vec<HistoryRow>> {
    let mut stmt = db.prepare(
        "SELECT exec_id, project_key, result, paths_json, created_at, backup_available, attr_failed_json FROM history ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        let attr_raw: Option<String> = r.get(6)?;
        let (attr_failed_count, attr_failed_paths) = parse_attr_failed(attr_raw.as_deref().unwrap_or(""));
        Ok(HistoryRow {
            exec_id: r.get(0)?,
            project_key: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            result: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            paths_json: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            created_at: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
            backup_available: r.get::<_, Option<i64>>(5)?.unwrap_or(0) != 0,
            attr_failed_count,
            attr_failed_paths,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn pending(db: &Connection) -> Result<Vec<PendingTx>> {
    let mut stmt = db.prepare("SELECT exec_id, state, project_key, backup_root, plan_json FROM exec_journal WHERE state NOT IN ('COMMITTED','ROLLED_BACK')")?;
    let rows = stmt.query_map([], |r| {
        Ok(PendingTx {
            exec_id: r.get(0)?,
            state: r.get(1)?,
            project_key: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            backup_root: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            summary: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn rollback(db: &Connection, fsys: &LocalFs, exec_id: &str, choice: RollbackChoice, fault: FaultPlan) -> Result<ExecReport> {
    if choice == RollbackChoice::Cancel {
        return Err(Error::new(ErrorCode::Cancelled, "已取消回滚"));
    }
    let state = observed_state(db, exec_id)?;
    if state == "ROLLED_BACK" {
        return Err(Error::new(ErrorCode::InvalidState, "这次回滚已经结束"));
    }
    let available: Option<i64> = db
        .query_row("SELECT backup_available FROM history WHERE exec_id=?1", params![exec_id], |r| r.get(0))
        .ok();
    if available == Some(0) {
        return Err(Error::new(ErrorCode::BackupPurged, "备份已清理"));
    }
    let (plan_json, backup): (String, String) = {
        let mut stmt = db.prepare("SELECT plan_json, backup_root FROM exec_journal WHERE exec_id=?1")?;
        stmt.query_row(params![exec_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
    };
    let plan: ExecPlan = serde_json::from_str(&plan_json).map_err(|e| Error::new(ErrorCode::Internal, e.to_string()))?;
    let backup = PathBuf::from(&backup);
    db.execute("UPDATE exec_journal SET state='ROLLING_BACK' WHERE exec_id=?1", params![exec_id])?;
    let conflicts = rollback_conflicts(fsys, db, exec_id, &plan)?;
    let states = load_rows(db, exec_id)?;
    let mut states = states;
    let mut fault = FaultRuntime::new(fault);
    let mut files = Vec::new();
    let mut any_fail = false;
    // 已写入的文件都要还原，包括改名之后才失败、状态被标成 failed 的。未执行的 Create 不能删目标。
    for op in plan.ops.iter().rev() {
        let Some(row) = states.get(&op.index) else { continue };
        if !op_touched(row) {
            continue;
        }
        if row.rollback_state.as_deref() == Some("done") && !move_other_side_present(fsys, op)? {
            if row.needs_verify {
                match restored_hashes_match(fsys, &backup, op) {
                    Ok(()) => {
                        clear_needs_verify(db, exec_id, op.index)?;
                        files.push(fr(op, "rolled-back", None, None));
                    }
                    Err(_) => {
                        // 用户后来改过。标成未恢复，不要用备份盖掉当前内容。
                        any_fail = true;
                        let err = Error::new(ErrorCode::Io, "回滚后内容和备份不一致");
                        mark_rollback_unrestored(db, exec_id, op.index, &err)?;
                        if let Some(row) = states.get_mut(&op.index) {
                            row.rollback_state = Some("failed".into());
                            row.needs_verify = false;
                        }
                        files.push(fr(op, "rollback-failed", Some(err.code_str()), Some(&err.message)));
                    }
                }
            } else {
                files.push(fr(op, "rolled-back", None, None));
            }
            continue;
        }
        let op_conflicts: Vec<&ConflictFile> = conflicts
            .iter()
            .filter(|c| c.path == op.path || op.from.as_deref() == Some(c.path.as_str()))
            .collect();
        if !op_conflicts.is_empty() && choice == RollbackChoice::SkipConflicts {
            files.push(fr(op, "skipped", None, Some("跳过冲突文件")));
            continue;
        }
        if !op_conflicts.is_empty() && choice == RollbackChoice::ProceedBackupCurrent {
            let mut saved = true;
            for hit in &op_conflicts {
                let rel = RelPath::parse(&hit.path)?;
                if fsys.stat(&rel).is_err() {
                    continue;
                }
                // 先把用户当前版本落到 conflict-current，同步目录并核对哈希。没核对上就不动原文件。
                if let Err(e) = save_conflict_current(fsys, &backup, &rel) {
                    any_fail = true;
                    mark_rollback_unrestored(db, exec_id, op.index, &e)?;
                    if let Some(row) = states.get_mut(&op.index) {
                        row.rollback_state = Some("failed".into());
                    }
                    files.push(fr(op, "rollback-failed", Some(e.code_str()), Some(&e.message)));
                    saved = false;
                    break;
                }
            }
            if !saved {
                continue;
            }
        }
        if let Err(e) = fault.hit_file(
            #[cfg(feature = "fault-inject")]
            Stage::Rollback,
            op.index,
        ) {
            let e = real_code(e);
            any_fail = true;
            db.execute(
                "UPDATE exec_ops SET rollback_state='failed', rollback_error=?3 WHERE exec_id=?1 AND op_index=?2",
                params![exec_id, op.index, e.message],
            )?;
            files.push(fr(op, "rollback-failed", Some(e.code_str()), Some(&e.message)));
            continue;
        }
        match restore_one(db, exec_id, fsys, &backup, op, &mut fault) {
            Ok(RestoreOutcome::Restored) => match verify_restored(fsys, &backup, op) {
                Ok(()) => {
                    db.execute(
                        "UPDATE exec_ops SET rollback_state='done' WHERE exec_id=?1 AND op_index=?2",
                        params![exec_id, op.index],
                    )?;
                    files.push(fr(op, "rolled-back", None, None));
                }
                Err(e) => {
                    any_fail = true;
                    db.execute(
                        "UPDATE exec_ops SET rollback_state='failed', rollback_error=?3 WHERE exec_id=?1 AND op_index=?2",
                        params![exec_id, op.index, e.message],
                    )?;
                    files.push(fr(op, "rollback-failed", Some(e.code_str()), Some(&e.message)));
                }
            },
            Ok(RestoreOutcome::RestoredIo(e)) => {
                // 内容已经回到备份。目录同步失败只报 E_IO，不能改回未恢复，也不中断后面的文件。
                record_restored_io(db, exec_id, op.index, &e)?;
                if let Some(row) = states.get_mut(&op.index) {
                    row.rollback_state = Some("done".into());
                }
                files.push(fr(op, "rolled-back", Some(e.code_str()), Some(&e.message)));
            }
            Ok(RestoreOutcome::AttrRestored(e)) => {
                record_attr_restore_failed(db, exec_id, op.index, &e)?;
                if let Some(row) = states.get_mut(&op.index) {
                    row.rollback_state = Some("done".into());
                }
                files.push(fr(op, "rolled-back", Some(e.code_str()), Some(&e.message)));
            }
            Err(e) => {
                if rollback_is_done(db, exec_id, op.index)? && e.message == ATTR_RESTORE_FAILED {
                    record_attr_restore_failed(db, exec_id, op.index, &e)?;
                    if let Some(row) = states.get_mut(&op.index) {
                        row.rollback_state = Some("done".into());
                    }
                    files.push(fr(op, "rolled-back", Some(e.code_str()), Some(&e.message)));
                } else if rollback_is_done(db, exec_id, op.index)? {
                    record_restored_io(db, exec_id, op.index, &e)?;
                    if let Some(row) = states.get_mut(&op.index) {
                        row.rollback_state = Some("done".into());
                    }
                    files.push(fr(op, "rolled-back", Some(e.code_str()), Some(&e.message)));
                } else {
                    any_fail = true;
                    mark_rollback_unrestored(db, exec_id, op.index, &e)?;
                    if let Some(row) = states.get_mut(&op.index) {
                        row.rollback_state = Some("failed".into());
                    }
                    files.push(fr(op, "rollback-failed", Some(e.code_str()), Some(&e.message)));
                }
            }
        }
    }
    let _ = any_fail;
    let mut unrestored = Vec::new();
    for op in &plan.ops {
        let Some(row) = states.get(&op.index) else { continue };
        if !op_touched(row) {
            continue;
        }
        let Some(reported) = files.iter_mut().find(|f| f.index == op.index) else {
            unrestored.push(json!({ "path": op.path, "reason": "未恢复" }));
            continue;
        };
        if reported.state == "rolled-back" {
            let already = states.get(&op.index).and_then(|row| row.rollback_state.as_deref()) == Some("done");
            if !already {
                if let Err(err) = verify_restored(fsys, &backup, op) {
                    reported.state = "rollback-failed".into();
                    reported.code = Some(err.code_str().to_string());
                    reported.error = Some(err.message.clone());
                    unrestored.push(json!({ "path": op.path, "reason": err.message, "code": err.code_str() }));
                }
            }
        } else {
            let reason = reported.error.clone().unwrap_or_else(|| "未恢复".into());
            let mut item = json!({ "path": op.path, "reason": reason });
            if let Some(code) = &reported.code {
                item["code"] = json!(code);
            }
            unrestored.push(item);
        }
    }
    // ROLLED_BACK 只在每个已触碰的文件都核对得上备份时使用。
    let state = if unrestored.is_empty() { "ROLLED_BACK" } else { "ROLLBACK_PARTIAL" };
    let history_result = if unrestored.is_empty() { "已回滚" } else { "部分回滚" };
    let paths = serde_json::to_string(&unrestored).unwrap_or_else(|_| "[]".into());
    let attr_paths: Vec<String> = files
        .iter()
        .filter(|f| f.error.as_deref() == Some(ATTR_RESTORE_FAILED))
        .map(|f| f.path.clone())
        .collect();
    let attr_json = attr_failed_payload(&attr_paths);
    db.execute("UPDATE exec_journal SET state=?2, finished_at=?3 WHERE exec_id=?1", params![exec_id, state, now_ms()])?;
    db.execute(
        "UPDATE history SET result=?2, paths_json=?3, attr_failed_json=?4 WHERE exec_id=?1",
        params![exec_id, history_result, paths, attr_json],
    )?;
    Ok(base_report(exec_id, state, files, &backup, false, history_result, false))
}

fn restore_one(db: &Connection, exec_id: &str, fsys: &LocalFs, backup: &Path, op: &PlanOp, fault: &mut FaultRuntime) -> Result<RestoreOutcome> {
    match op.kind {
        OpKind::Create => {
            let rel = RelPath::parse(&op.path)?;
            if fsys.stat(&rel).is_ok() {
                fsys.remove_path(&rel)?;
            }
            Ok(RestoreOutcome::Restored)
        }
        OpKind::Overwrite | OpKind::Delete => {
            let rel = RelPath::parse(&op.path)?;
            let src = backup_file(backup, rel.as_str());
            if !src.is_file() {
                return Err(Error::new(
                    ErrorCode::NotFound,
                    format!("备份里没有 {}，这个文件没有回滚", rel.as_str()),
                ));
            }
            let mode = op.mode;
            let mtime = op.expect.as_ref().and_then(|s| s.mtime_ms);
            copy_back(db, exec_id, fsys, &src, &rel, mode, mtime, op.index, fault)
        }
        OpKind::Move => undo_move(db, exec_id, fsys, backup, op, fault),
    }
}

/// 把冲突文件的当前内容写到 `conflict-current/<path>`，同步所在目录，再核对哈希。
/// 哈希不一致就返回错误，调用方不得接着删除或覆盖这个文件。
fn save_conflict_current(fsys: &LocalFs, backup: &Path, rel: &RelPath) -> Result<()> {
    let dest = backup.join("conflict-current").join(rel.as_str());
    if let Some(parent) = dest.parent() {
        crate::vfs::create_dir_all_synced(parent)?;
    }
    fsys.copy_to(rel, &dest)?;
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            let dir = fs::File::open(parent)?;
            dir.sync_all()?;
        }
    }
    let expect = fsys.hash(rel, true)?;
    let got = file_hash(&dest).unwrap_or_default();
    if got != expect {
        return Err(Error::new(
            ErrorCode::VerifyFailed,
            format!("{} 的当前版本备份和文件哈希不一致，未删除当前文件", rel.as_str()),
        ));
    }
    Ok(())
}

fn conflict_backup_verified(backup: &Path, rel: &RelPath, live_hash: Option<&str>) -> bool {
    let Some(live) = live_hash else { return false };
    let saved = backup.join("conflict-current").join(rel.as_str());
    file_hash(&saved).as_deref() == Some(live)
}

fn move_other_side_present(fsys: &LocalFs, op: &PlanOp) -> Result<bool> {
    if op.kind != OpKind::Move {
        return Ok(false);
    }
    let to = RelPath::parse(&op.path)?;
    Ok(fsys.stat(&to).is_ok())
}

fn fsync_rel_file(fsys: &LocalFs, rel: &RelPath) -> Result<()> {
    let file = fs::File::open(fsys.abs(rel)?)?;
    file.sync_all()?;
    Ok(())
}

/// 撤销移动。原路径写好并 fsync，删掉另一侧并 fsync 目录，这之后才记 rollback_state=done。
/// 崩溃落在写和删之间时，记录不能是 done，否则重试会跳过，两边都留着文件还报已回滚。
/// 目标或原路径和备份不一致时，必须先有核对过的当前版本备份，否则不动这两个路径。
fn undo_move(
    db: &Connection,
    exec_id: &str,
    fsys: &LocalFs,
    backup: &Path,
    op: &PlanOp,
    fault: &mut FaultRuntime,
) -> Result<RestoreOutcome> {
    let to = RelPath::parse(&op.path)?;
    if op.keep_source {
        // 保留旧文件：只删除新路径。不读、不哈希、不覆盖原路径。
        if fsys.stat(&to).is_ok() {
            fsys.remove_path(&to)?;
        }
        fsys.fsync_parent_of(&to, || fault.hit_fsync_dir())?;
        mark_rollback_done(db, exec_id, op.index)?;
        return Ok(RestoreOutcome::Restored);
    }
    let Some(from_raw) = &op.from else {
        return Err(Error::new(ErrorCode::Internal, "移动操作缺少原路径"));
    };
    let from = RelPath::parse(from_raw)?;
    let bak = backup_file(backup, from_raw);
    let bak_hash = file_hash(&bak);
    let from_hash = fsys.hash(&from, true).ok();
    let to_hash = fsys.hash(&to, true).ok();
    if to_hash.is_some() && to_hash != bak_hash && !conflict_backup_verified(backup, &to, to_hash.as_deref()) {
        return Err(Error::new(
            ErrorCode::VerifyFailed,
            format!("{} 还没有校验过的当前版本备份，未删除", to.as_str()),
        ));
    }
    if from_hash.is_some() && from_hash != bak_hash && !conflict_backup_verified(backup, &from, from_hash.as_deref()) {
        return Err(Error::new(
            ErrorCode::VerifyFailed,
            format!("{} 还没有校验过的当前版本备份，未覆盖", from.as_str()),
        ));
    }
    let mut attr_err = None;
    if !(bak_hash.is_some() && from_hash == bak_hash) {
        if from_hash.is_none() && bak_hash.is_some() && to_hash == bak_hash {
            fsys.rename_rel(&to, &from)?;
        } else if bak.is_file() {
            match fsys.copy_in_renamed(
                &bak,
                &from,
                op.mode,
                None,
                |tmp| remember_temp(db, exec_id, tmp),
                |tmp| {
                    let _ = forget_temp(db, exec_id, tmp);
                },
                || Ok(()),
            ) {
                Ok(()) => {}
                Err(e) if e.message == ATTR_RESTORE_FAILED => attr_err = Some(e),
                Err(e) => return Err(e),
            }
        } else {
            return Err(Error::new(
                ErrorCode::NotFound,
                format!("备份里没有 {}，这个文件没有回滚", from_raw),
            ));
        }
    }
    fsync_rel_file(fsys, &from)?;
    fsys.fsync_parent_of(&from, || fault.hit_fsync_dir())?;
    let removed_other = if fsys.stat(&to).is_ok() {
        fsys.remove_path(&to)?;
        true
    } else {
        false
    };
    if removed_other {
        fsys.fsync_parent_of(&to, || fault.hit_fsync_dir())?;
    }
    mark_rollback_done(db, exec_id, op.index)?;
    if let Some(e) = attr_err {
        return Ok(RestoreOutcome::AttrRestored(e));
    }
    Ok(RestoreOutcome::Restored)
}

enum RestoreOutcome {
    Restored,
    /// 内容已经回到备份，随后的目录同步失败。记录保持已恢复，并标 needs_verify。
    RestoredIo(Error),
    /// 内容已恢复，权限或修改时间没有恢复。不算未恢复，也不标 needs_verify。
    AttrRestored(Error),
}

fn copy_back(
    db: &Connection,
    exec_id: &str,
    fsys: &LocalFs,
    src: &Path,
    dest: &RelPath,
    mode: Option<u32>,
    mtime: Option<i64>,
    op_index: u32,
    fault: &mut FaultRuntime,
) -> Result<RestoreOutcome> {
    let placed = fsys.copy_in_atomic(
        src,
        dest,
        mode,
        mtime,
        || fault.hit_fsync_dir(),
        |tmp| remember_temp(db, exec_id, tmp),
        |tmp| {
            let _ = forget_temp(db, exec_id, tmp);
        },
        || mark_rollback_done(db, exec_id, op_index),
    );
    match placed {
        Ok(()) => Ok(RestoreOutcome::Restored),
        Err(e) => {
            if rollback_is_done(db, exec_id, op_index)? && e.message == ATTR_RESTORE_FAILED {
                Ok(RestoreOutcome::AttrRestored(e))
            } else if rollback_is_done(db, exec_id, op_index)? {
                Ok(RestoreOutcome::RestoredIo(e))
            } else {
                Err(e)
            }
        }
    }
}

pub fn rollback_conflicts(fsys: &LocalFs, db: &Connection, exec_id: &str, plan: &ExecPlan) -> Result<Vec<ConflictFile>> {
    let backup_root: String = db.query_row(
        "SELECT backup_root FROM exec_journal WHERE exec_id=?1",
        params![exec_id],
        |r| r.get(0),
    )?;
    let backup = PathBuf::from(backup_root);
    let states = load_rows(db, exec_id)?;
    let mut out = Vec::new();
    for op in &plan.ops {
        let Some(row) = states.get(&op.index) else { continue };
        if !op_touched(row) {
            continue;
        }
        if let Some(hit) = recreated_source_conflict(fsys, &backup, op, row)? {
            out.push(hit);
        }
        if let Some(hit) = written_file_conflict(fsys, &backup, op, row)? {
            if !out.iter().any(|c| c.path == hit.path) {
                out.push(hit);
            }
        }
    }
    Ok(out)
}

/// 和当初写下去的内容比。接受过的外部删除或外部哈希不拿来抵消回滚冲突。
/// 计划内删除、目标已经不在：不是冲突，回滚时用备份恢复。只有外部又把文件建回来且内容和备份不同才算冲突。
fn recreated_source_conflict(fsys: &LocalFs, backup: &Path, op: &PlanOp, row: &OpRow) -> Result<Option<ConflictFile>> {
    if op.kind != OpKind::Move || op.keep_source {
        return Ok(None);
    }
    if row.rollback_state.as_deref() == Some("done") && !move_other_side_present(fsys, op)? {
        return Ok(None);
    }
    let Some(from) = &op.from else { return Ok(None) };
    let rel = RelPath::parse(from)?;
    let Ok(hash) = fsys.hash(&rel, true) else { return Ok(None) };
    let bak = file_hash(&backup_file(backup, from));
    if bak.as_deref() == Some(hash.as_str()) {
        return Ok(None);
    }
    Ok(Some(ConflictFile { path: from.clone(), detail: DETAIL_RECREATED.into() }))
}

fn written_file_conflict(fsys: &LocalFs, backup: &Path, op: &PlanOp, row: &OpRow) -> Result<Option<ConflictFile>> {
    // done 但移动的另一侧还在：这项还没做完，要继续看冲突，不能直接跳过。
    if row.rollback_state.as_deref() == Some("done") && !move_other_side_present(fsys, op)? {
        return Ok(None);
    }
    if matches!(op.kind, OpKind::Delete) && (row.written || row.state == "done" || row.state == "doing") {
        let rel = RelPath::parse(&op.path)?;
        return match fsys.hash(&rel, true) {
            Err(_) => Ok(None),
            Ok(hash) => {
                let bak = file_hash(&backup_file(backup, rel.as_str()));
                if bak.as_deref() == Some(hash.as_str()) {
                    Ok(None)
                } else {
                    Ok(Some(ConflictFile { path: op.path.clone(), detail: DETAIL_MODIFIED.into() }))
                }
            }
        };
    }
    if op.bytes.is_none() && !row.written && row.state != "done" {
        return Ok(None);
    }
    let rel = RelPath::parse(&op.path)?;
    let wrote = row.after_hash.clone().or_else(|| op.bytes.as_ref().map(|b| sha256_hex(b)));
    match fsys.hash(&rel, true) {
        Ok(hash) => {
            if let Some(expect) = wrote {
                if hash != expect {
                    return Ok(Some(ConflictFile { path: op.path.clone(), detail: DETAIL_MODIFIED.into() }));
                }
            }
            Ok(None)
        }
        Err(_) => {
            if wrote.is_some() || row.written || row.kept_hash.as_deref() == Some(KEPT_DELETED) {
                return Ok(Some(ConflictFile { path: op.path.clone(), detail: DETAIL_DELETED.into() }));
            }
            Ok(None)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResumeChoice {
    /// 什么都不改，下次启动再提示。
    Cancel,
    /// 点「继续」。有未处理冲突就只返回清单，不写文件。
    Continue,
    /// 已写文件被外部改过时：不碰这些文件，从失败处接着写，并让以后的回滚把它们当冲突。
    KeepExternal,
    /// 进入已有的三选一回滚流程。这里不改文件。
    Rollback,
    /// 还没写的文件在扫描后变了：跳过这些文件。
    SkipChanged,
    /// 还没写的文件在扫描后变了：仍然覆盖。
    OverwriteChanged,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ResumeOutcome {
    Unchanged,
    Continued(ExecReport),
    RollbackPrompt(ResumeConflict),
    /// 失败文件或还没写的文件在扫描后又变了。选择前不写这些文件。
    ChangesSinceScan(Vec<ChangeHit>),
}

/// 用原来的事务和原来的备份继续。不新开事务，这样回滚仍覆盖前后两次写过的文件。
fn resume_seen_conflict(fsys: &LocalFs, op: &PlanOp, row: &OpRow) -> Result<Option<ConflictFile>> {
    if row.state != "done" && !row.written {
        return Ok(None);
    }
    let rel = RelPath::parse(&op.path)?;
    let current = fsys.hash(&rel, true).ok();
    if let Some(kept) = &row.kept_hash {
        if kept == KEPT_DELETED {
            return Ok(if current.is_none() {
                None
            } else {
                Some(ConflictFile { path: op.path.clone(), detail: DETAIL_MODIFIED.into() })
            });
        }
        return Ok(match current {
            None => Some(ConflictFile { path: op.path.clone(), detail: DETAIL_DELETED.into() }),
            Some(h) if &h == kept => None,
            Some(_) => Some(ConflictFile { path: op.path.clone(), detail: DETAIL_MODIFIED.into() }),
        });
    }
    let expect = row.after_hash.clone().or_else(|| op.bytes.as_ref().map(|b| sha256_hex(b)));
    Ok(match (current, expect) {
        (None, Some(_)) => Some(ConflictFile { path: op.path.clone(), detail: DETAIL_DELETED.into() }),
        (Some(h), Some(e)) if h != e => Some(ConflictFile { path: op.path.clone(), detail: DETAIL_MODIFIED.into() }),
        _ => None,
    })
}

fn record_kept(db: &Connection, fsys: &LocalFs, exec_id: &str, op: &PlanOp) -> Result<()> {
    let rel = RelPath::parse(&op.path)?;
    let (kept, detail) = match fsys.hash(&rel, true) {
        Ok(h) => (h, DETAIL_MODIFIED),
        Err(_) => (KEPT_DELETED.to_string(), DETAIL_DELETED),
    };
    db.execute(
        "UPDATE exec_ops SET kept_hash=?3, code='E_CHANGED_SINCE_SCAN', error=?4, rollback_state='conflict' WHERE exec_id=?1 AND op_index=?2",
        params![exec_id, op.index, kept, detail],
    )?;
    Ok(())
}

pub fn resume(db: &Connection, fsys: &LocalFs, exec_id: &str, choice: ResumeChoice, opts: &ExecOpts, fault: FaultPlan) -> Result<ResumeOutcome> {
    if choice == ResumeChoice::Cancel {
        return Ok(ResumeOutcome::Unchanged);
    }
    let observed = observed_state(db, exec_id)?;
    match resume_admission(&observed) {
        ResumeAdmission::Reject => {
            return Err(Error::new(ErrorCode::InvalidState, "这次升级已经结束，不能继续"));
        }
        ResumeAdmission::RollbackOnly => {
            return Err(Error::new(ErrorCode::InvalidState, "这次只能继续回滚，不能继续升级"));
        }
        ResumeAdmission::Upgrade => {}
    }
    if observed == "LOG_FAILED" {
        probe_log_write(db, &fault)?;
    }
    note_pre_recovery(db, exec_id, &observed)?;
    let backup_root_early: String = db.query_row(
        "SELECT backup_root FROM exec_journal WHERE exec_id=?1",
        params![exec_id],
        |r| r.get(0),
    )?;
    recover_registered_temps_in(db, fsys, exec_id, Some(Path::new(&backup_root_early)))?;
    let (plan_json, backup): (String, String) = {
        let mut stmt = db.prepare("SELECT plan_json, backup_root FROM exec_journal WHERE exec_id=?1")?;
        stmt.query_row(params![exec_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
    };
    let plan: ExecPlan = serde_json::from_str(&plan_json).map_err(|e| Error::new(ErrorCode::Internal, e.to_string()))?;
    if plan.id != exec_id {
        return Err(Error::new(ErrorCode::Internal, "事务 id 和计划不一致"));
    }
    let backup = PathBuf::from(backup);
    let mut recovery_conflicts = Vec::new();
    {
        let progress = load_rows(db, exec_id)?;
        for op in &plan.ops {
            let Some(row) = progress.get(&op.index) else { continue };
            if row.state != "doing" || row.written {
                continue;
            }
            match guess_written(fsys, &backup, op)? {
                WriteGuess::Written => mark_written(db, exec_id, op.index)?,
                WriteGuess::NotWritten => {}
                WriteGuess::Conflict(c) => recovery_conflicts.push(c),
            }
        }
    }
    let states = load_rows(db, exec_id)?;
    let mut external = recovery_conflicts;
    for op in &plan.ops {
        let Some(row) = states.get(&op.index) else { continue };
        if let Some(hit) = resume_seen_conflict(fsys, op, row)? {
            if !external.iter().any(|c| c.path == hit.path) {
                external.push(hit);
            }
        }
    }
    if choice == ResumeChoice::Rollback {
        if external.is_empty() {
            external = rollback_conflicts(fsys, db, exec_id, &plan)?;
        }
        return Ok(ResumeOutcome::RollbackPrompt(ResumeConflict {
            title: resume_conflict_title(&external),
            files: external,
        }));
    }
    let states = load_rows(db, exec_id)?;
    let remaining: Vec<PlanOp> = plan
        .ops
        .iter()
        .filter(|op| {
            let Some(row) = states.get(&op.index) else { return true };
            row.state != "done" && row.state != "skipped" && row.kept_hash.is_none() && !row.written
        })
        .cloned()
        .collect();
    let hits = detect_changes(fsys, &remaining)?;
    if choice == ResumeChoice::Continue && !external.is_empty() {
        return Ok(ResumeOutcome::RollbackPrompt(ResumeConflict {
            title: resume_conflict_title(&external),
            files: external,
        }));
    }
    if !hits.is_empty() && matches!(choice, ResumeChoice::Continue | ResumeChoice::KeepExternal) {
        return Ok(ResumeOutcome::ChangesSinceScan(hits));
    }
    let accepting = matches!(
        choice,
        ResumeChoice::KeepExternal | ResumeChoice::SkipChanged | ResumeChoice::OverwriteChanged
    );
    if !external.is_empty() && accepting {
        for op in &plan.ops {
            if external.iter().any(|c| c.path == op.path) {
                record_kept(db, fsys, exec_id, op)?;
            }
        }
    }
    let report = continue_same_transaction(db, fsys, &plan, &backup, opts, fault, choice, &hits)?;
    Ok(ResumeOutcome::Continued(report))
}

fn continue_same_transaction(
    db: &Connection,
    fsys: &LocalFs,
    plan: &ExecPlan,
    backup: &Path,
    opts: &ExecOpts,
    fault: FaultPlan,
    choice: ResumeChoice,
    hits: &[ChangeHit],
) -> Result<ExecReport> {
    let mut fault = FaultRuntime::new(fault);
    let states = load_rows(db, &plan.id)?;
    let skip_paths: HashSet<&str> = if choice == ResumeChoice::SkipChanged {
        hits.iter().map(|h| h.path.as_str()).collect()
    } else {
        HashSet::new()
    };
    let mut files = Vec::new();
    for op in &plan.ops {
        let Some(row) = states.get(&op.index) else { continue };
        if row.kept_hash.is_some() {
            files.push(fr(op, "done", Some("E_CHANGED_SINCE_SCAN"), None));
        } else if row.state == "done" {
            files.push(fr(op, "done", None, None));
        } else if row.state == "skipped" {
            files.push(fr(op, "skipped", Some("E_PERMISSION"), Some("跳过（只读）")));
        }
    }
    // 先把还没备份、并且这次真的要写的文件补上，校验通过才往下写。
    for op in &plan.ops {
        if !will_resume_write(&states, op, opts, &skip_paths) {
            continue;
        }
        let refresh = choice == ResumeChoice::OverwriteChanged && hits.iter().any(|h| h.path == op.path);
        if let Err(e) = backup_op(db, fsys, plan, backup, op, &mut fault, refresh) {
            if e.code == ErrorCode::LogWrite {
                return Err(e);
            }
            return finish_stopped(db, plan, backup, files, op, &real_code(e));
        }
    }
    log_write(db, &mut fault, &plan.id, "backed_up", || {
        db.execute("UPDATE exec_journal SET state='BACKED_UP' WHERE exec_id=?1", params![plan.id])?;
        Ok(())
    })?;
    if stop(opts, "backed_up") {
        return Ok(partial(plan, backup, "BACKED_UP", files, true));
    }

    let mut temps = Vec::new();
    for op in &plan.ops {
        let row = states.get(&op.index);
        if let Some(row) = row {
            if row.kept_hash.is_some() || row.state == "done" || row.state == "skipped" {
                continue;
            }
            if row.written {
                if let Err(e) = reapply_written_side(db, &plan.id, fsys, op, opts, &mut fault, row.source_removed) {
                    if e.code == ErrorCode::LogWrite {
                        return Err(e);
                    }
                    return finish_stopped(db, plan, backup, files, op, &real_code(e));
                }
                set_op(db, &plan.id, op.index, "done", None, None)?;
                if !files.iter().any(|f| f.index == op.index) {
                    files.push(fr(op, "done", None, None));
                }
                continue;
            }
        }
        if skip_paths.contains(op.path.as_str()) {
            set_op(db, &plan.id, op.index, "skipped", Some("扫描后内容已变，按选择跳过"), Some("E_CHANGED_SINCE_SCAN"))?;
            files.push(fr(op, "skipped", Some("E_CHANGED_SINCE_SCAN"), Some("扫描后内容已变，按选择跳过")));
            continue;
        }
        if !will_apply(op, opts.allow_readonly) {
            set_op(db, &plan.id, op.index, "skipped", Some("只读文件未勾选覆盖"), Some("E_PERMISSION"))?;
            files.push(fr(op, "skipped", Some("E_PERMISSION"), Some("跳过（只读）")));
            continue;
        }
        let doing = format!("doing:{}", op.index);
        log_write(db, &mut fault, &plan.id, &doing, || set_op(db, &plan.id, op.index, "doing", None, None))?;
        if stop(opts, &doing) {
            files.push(fr(op, "doing", None, None));
            return Ok(partial(plan, backup, "BACKED_UP", files, true));
        }
        if let Ok(rel) = RelPath::parse(op.from.as_deref().unwrap_or(&op.path)) {
            #[cfg(test)]
            mutate_before_write(fsys, &rel);
            #[cfg(not(test))]
            let _ = rel;
        }
        if let Err(e) = require_verified_backup(fsys, backup, op) {
            return finish_stopped(db, plan, backup, files, op, &real_code(e));
        }
        match apply_one(db, &plan.id, fsys, op, opts, &mut fault, &mut temps) {
            Ok(()) => {
                let done = format!("done:{}", op.index);
                let wrote = op.bytes.as_ref().map(|b| sha256_hex(b));
                log_write(db, &mut fault, &plan.id, &done, || {
                    set_op(db, &plan.id, op.index, "done", None, None)?;
                    if let Some(h) = &wrote {
                        db.execute(
                            "UPDATE exec_ops SET after_hash=?3 WHERE exec_id=?1 AND op_index=?2",
                            params![plan.id, op.index, h],
                        )?;
                    }
                    Ok(())
                })?;
                files.push(fr(op, "done", None, None));
                if stop(opts, &done) {
                    return Ok(partial(plan, backup, "BACKED_UP", files, true));
                }
            }
            Err(e) if e.code == ErrorCode::LogWrite => return Err(e),
            Err(e) => return finish_stopped(db, plan, backup, files, op, &real_code(e)),
        }
    }
    log_write(db, &mut fault, &plan.id, "verifying", || {
        db.execute("UPDATE exec_journal SET state='VERIFYING' WHERE exec_id=?1", params![plan.id])?;
        Ok(())
    })?;
    if stop(opts, "verifying") {
        return Ok(partial(plan, backup, "VERIFYING", files, true));
    }
    for op in &plan.ops {
        if files.iter().find(|f| f.index == op.index).map(|f| f.state.as_str()) != Some("done") {
            continue;
        }
        let code: Option<String> = db
            .query_row("SELECT code FROM exec_ops WHERE exec_id=?1 AND op_index=?2", params![plan.id, op.index], |r| r.get(0))
            .unwrap_or(None);
        if code.as_deref() == Some("E_CHANGED_SINCE_SCAN") {
            continue;
        }
        if let Some(bytes) = &op.bytes {
            if let Err(e) = fault.hit_file(
                #[cfg(feature = "fault-inject")]
                Stage::VerifyPost,
                op.index,
            ) {
                return finish_stopped(db, plan, backup, files, op, &real_code(e));
            }
            let rel = RelPath::parse(&op.path)?;
            let got = fsys.hash(&rel, true).unwrap_or_default();
            if got != sha256_hex(bytes) {
                let err = Error::new(ErrorCode::VerifyFailed, format!("{} 写入后的哈希和升级包不一致", op.path));
                return finish_stopped(db, plan, backup, files, op, &err);
            }
        }
    }
    if let Err(e) = fault.hit_commit() {
        let err = Error::new(ErrorCode::LogWrite, e.message);
        db.execute("UPDATE exec_journal SET state='LOG_FAILED' WHERE exec_id=?1", params![plan.id])?;
        return Err(err);
    }
    log_write(db, &mut fault, &plan.id, "committed", || {
        db.execute("UPDATE exec_journal SET state='COMMITTED', finished_at=?2 WHERE exec_id=?1", params![plan.id, now_ms()])?;
        let summary = serde_json::to_string(&plan.summary).unwrap_or_else(|_| "{}".into());
        db.execute(
            "INSERT OR REPLACE INTO history (exec_id, project_key, summary_json, result, paths_json, created_at, backup_available) VALUES (?1,?2,?3,?4,?5,?6,1)",
            params![plan.id, plan.project_key, summary, "成功", format!("{} ↔ {}", plan.project_key, plan.id), now_ms()],
        )?;
        Ok(())
    })?;
    cleanup_recorded(fsys, &temps)?;
    Ok(base_report(&plan.id, "COMMITTED", files, backup, false, "成功", false))
}

fn will_resume_write(states: &HashMap<u32, OpRow>, op: &PlanOp, opts: &ExecOpts, skip_paths: &HashSet<&str>) -> bool {
    if skip_paths.contains(op.path.as_str()) || !will_apply(op, opts.allow_readonly) {
        return false;
    }
    match states.get(&op.index) {
        Some(row) => row.state != "done" && row.state != "skipped" && row.kept_hash.is_none() && !row.written,
        None => true,
    }
}

pub fn mark_backup_cleaned(db: &Connection, exec_id: &str) -> Result<()> {
    db.execute("UPDATE history SET backup_available=0, result='backup-cleaned' WHERE exec_id=?1", params![exec_id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Snapshot, Summary};
    use std::os::unix::fs::PermissionsExt;

    fn plan(ops: Vec<PlanOp>) -> ExecPlan {
        ExecPlan {
            id: format!("ex-{}", uuid::Uuid::new_v4().simple()),
            project_key: "proj".into(),
            summary: Summary { create: 1, overwrite: 1, ..Summary::default() },
            ops,
        }
    }

    fn ov(index: u32, path: &str, old: &[u8], new: &[u8], mtime: Option<i64>) -> PlanOp {
        PlanOp {
            index,
            kind: OpKind::Overwrite,
            path: path.into(),
            from: None,
            bytes: Some(new.to_vec()),
            expect: Some(Snapshot { size: old.len() as u64, mtime_ms: mtime, hash: sha256_hex(old) }),
            mode: Some(0o644),
            source_mtime_ms: Some(1_700_000_000_000),
            merged: false,
            keep_source: false,
            readonly: false,
            label: "覆盖".into(),
        }
    }

    fn del(index: u32, path: &str, old: &[u8]) -> PlanOp {
        PlanOp {
            index,
            kind: OpKind::Delete,
            path: path.into(),
            from: None,
            bytes: None,
            expect: Some(Snapshot { size: old.len() as u64, mtime_ms: None, hash: sha256_hex(old) }),
            mode: Some(0o644),
            source_mtime_ms: None,
            merged: false,
            keep_source: false,
            readonly: false,
            label: "删除".into(),
        }
    }

    #[test]
    fn overwrite_matches_bytes_and_rollback_restores() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let dbp = dir.path().join("t.sqlite");
        let db = open_db(&dbp).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"new", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: None, corrupt_temp: false };
        let rep = execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        assert_eq!(rep.state, "COMMITTED");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"new");
        assert!(dir.path().join("bak").join(&p.id).join("manifest.json").exists());
        let back = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(back.state, "ROLLED_BACK");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old");
    }

    #[test]
    fn corrupt_temp_leaves_original() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"new", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: None, corrupt_temp: true };
        let rep = execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        assert_eq!(rep.state, "STOPPED");
        assert_eq!(rep.files[0].state, "failed");
        assert_eq!(rep.files[0].code.as_deref(), Some("E_VERIFY_FAILED"));
        assert_eq!(rep.history_result, "部分失败");
        assert_eq!(rep.sheet_actions, vec!["回滚".to_string(), "稍后处理".to_string()]);
        assert!(!rep.sheet_actions.iter().any(|a| a == "继续"));
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old");
        assert!(fsys.list_temps().unwrap().is_empty());
    }

    #[test]
    fn readonly_skipped_unless_allowed_then_restored() {
        let dir = tempfile::tempdir().unwrap();
        let pth = dir.path().join("a.txt");
        fs::write(&pth, b"old").unwrap();
        let mut perm = fs::metadata(&pth).unwrap().permissions();
        perm.set_mode(0o444);
        fs::set_permissions(&pth, perm).unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let mut op = ov(1, "a.txt", b"old", b"new", None);
        op.readonly = true;
        let p = plan(vec![op]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: None, corrupt_temp: false };
        let rep = execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        assert_eq!(rep.files[0].code.as_deref(), Some("E_PERMISSION"));
        assert_eq!(fs::read(&pth).unwrap(), b"old");

        let mut op = ov(1, "a.txt", b"old", b"new", None);
        op.readonly = true;
        let p = plan(vec![op]);
        let opts = ExecOpts { allow_readonly: true, backup_root: dir.path().join("bak"), remote: false, stop_after_log: None, corrupt_temp: false };
        // 文件仍是 0444，允许覆盖后应写成功并恢复只读。
        let rep = execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        assert_eq!(rep.state, "COMMITTED", "{:?}", rep.files);
        assert_eq!(fs::read(&pth).unwrap(), b"new");
        assert_eq!(fs::metadata(&pth).unwrap().permissions().mode() & 0o222, 0);
    }

    #[test]
    fn touch_is_not_a_change_but_content_is() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"same").unwrap();
        let fsys = LocalFs::new(dir.path());
        let op = ov(1, "a.txt", b"same", b"next", Some(1));
        filetime::set_file_mtime(dir.path().join("a.txt"), filetime::FileTime::from_unix_time(50, 0)).unwrap();
        let hits = detect_changes(&fsys, &[op]).unwrap();
        assert!(hits.is_empty(), "{hits:?}");
        fs::write(dir.path().join("a.txt"), b"changed").unwrap();
        let op = ov(1, "a.txt", b"same", b"next", Some(1));
        let hits = detect_changes(&fsys, &[op]).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].hash_changed);
    }

    #[test]
    fn move_without_delete_keeps_source() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("old.ts"), b"body").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let op = PlanOp {
            index: 1,
            kind: OpKind::Move,
            path: "new.ts".into(),
            from: Some("old.ts".into()),
            bytes: Some(b"body".to_vec()),
            expect: None,
            mode: Some(0o644),
            source_mtime_ms: None,
            merged: false,
            keep_source: true,
            readonly: false,
            label: "移动（保留旧文件）".into(),
        };
        let p = plan(vec![op]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: None, corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        assert_eq!(fs::read(dir.path().join("new.ts")).unwrap(), b"body");
        assert_eq!(fs::read(dir.path().join("old.ts")).unwrap(), b"body");
    }

    #[test]
    fn crash_after_doing_can_continue_or_rollback() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"new", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: Some("doing:1".into()), corrupt_temp: false };
        let rep = execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        assert_ne!(rep.state, "COMMITTED");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old");
        let pend = pending(&db).unwrap();
        assert_eq!(pend.len(), 1);
        let back = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(back.state, "ROLLED_BACK");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old");
    }

    #[test]
    fn log_failure_stops_without_autocommit() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        fs::write(dir.path().join("b.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![
            ov(1, "a.txt", b"old", b"A", None),
            ov(2, "b.txt", b"old", b"B", None),
        ]);
        // 第 4 次日志是第一个文件的 done。之前 prepared、backed_up、doing:1 已写入。
        #[cfg(feature = "fault-inject")]
        let fp = {
            let mut fp = FaultPlan::default();
            fp.items.push(crate::fault::Item::At { stage: Stage::Log, n: 4, mode: crate::fault::Mode::Error });
            fp
        };
        #[cfg(not(feature = "fault-inject"))]
        let fp = FaultPlan::default();
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: None, corrupt_temp: false };
        let err = execute(&db, &fsys, &p, &opts, fp);
        // feature 关闭时故障不生效，升级会成功。两种都接受，但开了 feature 必须停。
        #[cfg(feature = "fault-inject")]
        {
            let err = err.unwrap_err();
            assert_eq!(err.code, ErrorCode::LogWrite);
            let pend = pending(&db).unwrap();
            assert_eq!(pend.len(), 1);
            assert_ne!(pend[0].state, "COMMITTED");
            assert_ne!(pend[0].state, "ROLLED_BACK");
        }
        #[cfg(not(feature = "fault-inject"))]
        {
            assert!(err.is_ok());
        }
    }

    #[test]
    fn rollback_reports_the_file_that_failed() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"new", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: None, corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        fs::remove_file(dir.path().join("bak").join(&p.id).join("files").join("a.txt")).unwrap();
        let rep = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(rep.state, "ROLLBACK_PARTIAL", "{:?}", rep.files);
        assert!(rep.files.iter().any(|f| f.state == "rollback-failed" && f.path == "a.txt"), "{:?}", rep.files);
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"new");
    }

    #[test]
    fn manifest_is_one_json_line_per_backed_up_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        fs::write(dir.path().join("b.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"A", None), ov(2, "b.txt", b"old", b"B", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: None, corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        let manifest = dir.path().join("bak").join(&p.id).join("manifest.json");
        let rows = read_manifest(&manifest.parent().unwrap()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["path"], "a.txt");
        assert_eq!(rows[1]["path"], "b.txt");
        // 撕掉最后一行，前面的备份记录还在。
        let mut text = fs::read_to_string(&manifest).unwrap();
        text.push_str("{\"path\":");
        fs::write(&manifest, text).unwrap();
        let rows = read_manifest(manifest.parent().unwrap()).unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn continue_reruns_change_detection_for_unfinished_ops() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        fs::write(dir.path().join("b.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"A", None), ov(2, "b.txt", b"old", b"B", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: Some("done:1".into()), corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        fs::write(dir.path().join("b.txt"), b"touched-by-someone").unwrap();
        let hits = continue_change_detection(&db, &fsys, &p.id).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "b.txt");
        let unfinished = unfinished_ops(&db, &p.id).unwrap();
        assert!(unfinished.iter().all(|op| op.path != "a.txt"));
    }

    #[test]
    fn retention_keeps_history_but_disables_rollback() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: None, corrupt_temp: false };
        let mut ids = Vec::new();
        for i in 0..3 {
            fs::write(dir.path().join("a.txt"), format!("v{i}")).unwrap();
            let old = fs::read(dir.path().join("a.txt")).unwrap();
            let p = plan(vec![ov(1, "a.txt", &old, b"next", None)]);
            execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
            ids.push(p.id);
        }
        db.execute("UPDATE history SET created_at=?2 WHERE exec_id=?1", params![ids[0], 1_i64]).unwrap();
        let cleaned = sweep_backups(&db, crate::prefs::BACKUP_MAX_AGE_MS + 2, 10, crate::prefs::BACKUP_MAX_AGE_MS).unwrap();
        assert_eq!(cleaned, vec![ids[0].clone()]);
        let reason = db.query_row("SELECT backup_available FROM history WHERE exec_id=?1", params![ids[0]], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(reason, 0);
        assert_eq!(rollback_disabled_reason(false), Some("备份已清理"));
        let still = db.query_row("SELECT COUNT(*) FROM history", [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(still, 3);
        let cleaned = sweep_backups(&db, 10_000, 1, crate::prefs::BACKUP_MAX_AGE_MS).unwrap();
        assert!(cleaned.contains(&ids[1]));
        assert!(!cleaned.contains(&ids[2]));
    }

    #[test]
    fn conflict_sheet_sees_post_upgrade_edits() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"new", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: None, corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        fs::write(dir.path().join("a.txt"), b"edited-after").unwrap();
        let conflicts = rollback_conflicts(&fsys, &db, &p.id, &p).unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].path, "a.txt");
        let skipped = rollback(&db, &fsys, &p.id, RollbackChoice::SkipConflicts, FaultPlan::default()).unwrap();
        assert_eq!(skipped.state, "ROLLBACK_PARTIAL");
        assert_eq!(skipped.history_result, "部分回滚");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"edited-after");
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn backup_fault_stops_on_that_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"new", None)]);
        let mut fp = FaultPlan::default();
        fp.items.push(crate::fault::Item::At { stage: Stage::Backup, n: 1, mode: crate::fault::Mode::Error });
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: None, corrupt_temp: false };
        let rep = execute(&db, &fsys, &p, &opts, fp).unwrap();
        assert_eq!(rep.state, "STOPPED");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old");
    }

    #[test]
    fn preview_excludes_readonly_until_the_box_is_ticked_and_matches_writes() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        fs::write(dir.path().join("ro.txt"), b"old").unwrap();
        let mut perm = fs::metadata(dir.path().join("ro.txt")).unwrap().permissions();
        perm.set_mode(0o444);
        fs::set_permissions(dir.path().join("ro.txt"), perm).unwrap();
        let mut ops = vec![ov(1, "a.txt", b"old", b"NEW", None), ov(2, "ro.txt", b"old", b"NEW", None)];
        ops[1].readonly = true;
        let off = preview_counts(&ops, false);
        assert_eq!(off.overwrite, 1);
        assert_eq!(off.skip_readonly, 1);
        assert_eq!(skip_readonly_label(1), "跳过（只读）1");
        let on = preview_counts(&ops, true);
        assert_eq!(on.overwrite, 2);
        assert_eq!(on.skip_readonly, 0);

        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let expect = planned_write_paths(&ops, false);
        let p = plan(ops);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: None, corrupt_temp: false };
        let rep = execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        let wrote: Vec<_> = rep.files.iter().filter(|f| f.state == "done").map(|f| f.path.clone()).collect();
        assert_eq!(wrote, expect);
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"NEW");
        assert_eq!(fs::read(dir.path().join("ro.txt")).unwrap(), b"old");
    }

    #[test]
    fn remote_overwrite_copy_delete_and_move_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"keep").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let before = fs::read(dir.path().join("a.txt")).unwrap();
        for kind in [OpKind::Overwrite, OpKind::Create, OpKind::Delete, OpKind::Move] {
            let mut op = ov(1, "a.txt", b"keep", b"NEW", None);
            op.kind = kind;
            if kind == OpKind::Move {
                op.from = Some("a.txt".into());
                op.path = "b.txt".into();
            }
            let plan = plan(vec![op]);
            let mut opts = opts_at(dir.path());
            opts.remote = true;
            let err = execute(&db, &fsys, &plan, &opts, FaultPlan::default()).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidState);
            assert_eq!(err.code_str(), "E_INVALID_STATE");
            assert_eq!(err.message, "远程写入还没支持");
            assert!(!format!("{err}").contains("NEW"));
        }
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), before);
        assert!(!dir.path().join("b.txt").exists());
        let rows: i64 = db.query_row("SELECT COUNT(*) FROM exec_journal", [], |row| row.get(0)).unwrap();
        assert_eq!(rows, 0);
    }

    fn opts_at(dir: &Path) -> ExecOpts {
        ExecOpts { allow_readonly: false, backup_root: dir.join("bak"), remote: false, stop_after_log: None, corrupt_temp: false }
    }

    fn create_op(index: u32, path: &str, bytes: &[u8]) -> PlanOp {
        PlanOp {
            index,
            kind: OpKind::Create,
            path: path.into(),
            from: None,
            bytes: Some(bytes.to_vec()),
            expect: None,
            mode: Some(0o644),
            source_mtime_ms: None,
            merged: false,
            keep_source: false,
            readonly: false,
            label: "新增".into(),
        }
    }

    #[test]
    fn file_failure_stops_the_rest_and_history_is_partial() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        fs::write(dir.path().join("b.txt"), b"old").unwrap();
        fs::write(dir.path().join(".notes.htx-keep"), b"keep").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"A", None), ov(2, "b.txt", b"old", b"B", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: None, corrupt_temp: true };
        let rep = execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        assert_eq!(rep.state, "STOPPED");
        assert_eq!(rep.files.len(), 1);
        assert_eq!(rep.files[0].code.as_deref(), Some("E_VERIFY_FAILED"));
        assert_ne!(rep.files[0].code.as_deref(), Some("E_FAULT"));
        assert_eq!(rep.history_result, "部分失败");
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"old");
        assert_eq!(fs::read(dir.path().join(".notes.htx-keep")).unwrap(), b"keep");
        let result: String = db.query_row("SELECT result FROM history WHERE exec_id=?1", params![p.id], |r| r.get(0)).unwrap();
        assert_eq!(result, "部分失败");
        let paths: String = db.query_row("SELECT paths_json FROM history WHERE exec_id=?1", params![p.id], |r| r.get(0)).unwrap();
        assert!(paths.contains("E_VERIFY_FAILED"), "{paths}");
        assert!(paths.contains("a.txt"), "{paths}");
        assert_ne!(result, "ok");
    }

    #[test]
    fn rollback_does_not_delete_an_unexecuted_create() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", None), create_op(2, "brand-new.txt", b"created")]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: Some("backed_up".into()), corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        fs::write(dir.path().join("brand-new.txt"), b"preexisting").unwrap();
        let rep = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(rep.state, "ROLLED_BACK");
        assert_eq!(fs::read(dir.path().join("brand-new.txt")).unwrap(), b"preexisting");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old");
    }

    #[test]
    fn commit_deletes_only_temps_this_run_recorded() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        fs::write(dir.path().join("keep.htx-other"), b"leave").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"new", None)]);
        execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"new");
        assert_eq!(fs::read(dir.path().join("keep.htx-other")).unwrap(), b"leave");
        let temps = fsys.list_temps().unwrap();
        assert_eq!(temps, vec!["keep.htx-other".to_string()], "{temps:?}");
    }

    #[test]
    fn readonly_is_restored_when_the_write_fails() {
        let dir = tempfile::tempdir().unwrap();
        let pth = dir.path().join("a.txt");
        fs::write(&pth, b"old").unwrap();
        let mut perm = fs::metadata(&pth).unwrap().permissions();
        perm.set_mode(0o444);
        fs::set_permissions(&pth, perm).unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let mut op = ov(1, "a.txt", b"old", b"new", None);
        op.readonly = true;
        let p = plan(vec![op]);
        let opts = ExecOpts { allow_readonly: true, backup_root: dir.path().join("bak"), remote: false, stop_after_log: None, corrupt_temp: true };
        let rep = execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        assert_eq!(rep.state, "STOPPED");
        assert_eq!(fs::read(&pth).unwrap(), b"old");
        assert_eq!(fs::metadata(&pth).unwrap().permissions().mode() & 0o222, 0);
    }

    #[test]
    fn resume_reuses_the_same_transaction_and_rollback_covers_both_passes() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old-a").unwrap();
        fs::write(dir.path().join("b.txt"), b"old-b").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old-a", b"new-a", None), ov(2, "b.txt", b"old-b", b"new-b", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: Some("done:1".into()), corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"new-a");
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"old-b");
        let before: i64 = db.query_row("SELECT COUNT(*) FROM exec_journal", [], |r| r.get(0)).unwrap();
        let continued = resume(&db, &fsys, &p.id, ResumeChoice::KeepExternal, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let ResumeOutcome::Continued(rep) = continued else { panic!("expected continue"); };
        assert_eq!(rep.exec_id, p.id);
        assert_eq!(rep.state, "COMMITTED");
        let after: i64 = db.query_row("SELECT COUNT(*) FROM exec_journal", [], |r| r.get(0)).unwrap();
        assert_eq!(before, after);
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"new-b");
        let back = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(back.state, "ROLLED_BACK");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old-a");
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"old-b");
    }

    #[test]
    fn resume_conflict_keep_rollback_and_cancel() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old-a").unwrap();
        fs::write(dir.path().join("b.txt"), b"old-b").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old-a", b"new-a", None), ov(2, "b.txt", b"old-b", b"new-b", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: Some("done:1".into()), corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        fs::write(dir.path().join("a.txt"), b"external").unwrap();

        let cancelled = resume(&db, &fsys, &p.id, ResumeChoice::Cancel, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        assert!(matches!(cancelled, ResumeOutcome::Unchanged));
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"external");
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"old-b");
        assert_eq!(pending(&db).unwrap().len(), 1);

        let prompted = resume(&db, &fsys, &p.id, ResumeChoice::Rollback, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let ResumeOutcome::RollbackPrompt(prompt) = prompted else { panic!("expected rollback prompt"); };
        assert_eq!(prompt.files.len(), 1);
        assert_eq!(prompt.files[0].path, "a.txt");
        assert_eq!(prompt.files[0].detail, DETAIL_MODIFIED);
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"old-b");
        assert_eq!(pending(&db).unwrap().len(), 1);

        let kept = resume(&db, &fsys, &p.id, ResumeChoice::KeepExternal, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let ResumeOutcome::Continued(rep) = kept else { panic!("expected continue"); };
        assert_eq!(rep.state, "COMMITTED");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"external");
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"new-b");
        let conflicts = rollback_conflicts(&fsys, &db, &p.id, &p).unwrap();
        assert!(conflicts.iter().any(|c| c.path == "a.txt"), "{conflicts:?}");
        let skipped = rollback(&db, &fsys, &p.id, RollbackChoice::SkipConflicts, FaultPlan::default()).unwrap();
        assert_eq!(skipped.state, "ROLLBACK_PARTIAL");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"external");
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"old-b");
    }

    #[test]
    fn saving_current_version_failure_marks_rollback_failed() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"new", None)]);
        execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        fs::write(dir.path().join("a.txt"), b"external").unwrap();
        fs::write(dir.path().join("bak").join(&p.id).join("conflict-current"), b"not-a-directory").unwrap();
        let rep = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(rep.state, "ROLLBACK_PARTIAL", "{:?}", rep.files);
        assert!(rep.files.iter().any(|f| f.state == "rollback-failed" && f.path == "a.txt"), "{:?}", rep.files);
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"external");
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn verify_post_and_log_commit_use_real_codes() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        fs::write(dir.path().join("b.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"A", None), ov(2, "b.txt", b"old", b"B", None)]);
        let mut fp = FaultPlan::default();
        fp.items.push(crate::fault::Item::At { stage: Stage::VerifyPost, n: 1, mode: crate::fault::Mode::Error });
        let rep = execute(&db, &fsys, &p, &opts_at(dir.path()), fp).unwrap();
        assert_eq!(rep.state, "STOPPED");
        assert_eq!(rep.files.iter().find(|f| f.path == "a.txt").unwrap().code.as_deref(), Some("E_VERIFY_FAILED"));
        assert!(rep.files.iter().all(|f| f.code.as_deref() != Some("E_FAULT")));
        assert_ne!(rep.state, "COMMITTED");
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"B");

        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"A", None)]);
        let mut fp = FaultPlan::default();
        fp.items.push(crate::fault::Item::LogCommit { mode: crate::fault::Mode::Error });
        let err = execute(&db, &fsys, &p, &opts_at(dir.path()), fp).unwrap_err();
        assert_eq!(err.code, ErrorCode::LogWrite);
        let state: String = db.query_row("SELECT state FROM exec_journal WHERE exec_id=?1", params![p.id], |r| r.get(0)).unwrap();
        assert_eq!(state, "LOG_FAILED");
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn backup_corrupt_fails_verification_and_leaves_the_original() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old-bytes").unwrap();
        fs::write(dir.path().join("b.txt"), b"other").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old-bytes", b"NEW", None), ov(2, "b.txt", b"other", b"NEXT", None)]);
        let mut fp = FaultPlan::default();
        fp.items.push(crate::fault::Item::At { stage: Stage::Backup, n: 1, mode: crate::fault::Mode::Corrupt });
        let rep = execute(&db, &fsys, &p, &opts_at(dir.path()), fp).unwrap();
        assert_eq!(rep.state, "STOPPED");
        assert_eq!(rep.files[0].code.as_deref(), Some("E_VERIFY_FAILED"));
        assert_ne!(rep.files[0].code.as_deref(), Some("E_FAULT"));
        assert_eq!(rep.history_result, "部分失败");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old-bytes");
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"other");
        assert!(crate::error::SPEC_CODES.contains(&"E_VERIFY_FAILED"));
    }

    fn journal_state(db: &Connection, id: &str) -> String {
        db.query_row("SELECT state FROM exec_journal WHERE exec_id=?1", params![id], |r| r.get(0)).unwrap()
    }

    fn pre_recovery(db: &Connection, id: &str) -> Option<String> {
        db.query_row("SELECT pre_recovery_state FROM exec_journal WHERE exec_id=?1", params![id], |r| r.get(0)).unwrap()
    }

    struct DirMode<'a> {
        path: &'a Path,
    }

    impl Drop for DirMode<'_> {
        fn drop(&mut self) {
            if let Ok(meta) = fs::metadata(self.path) {
                let mut perm = meta.permissions();
                perm.set_mode(0o755);
                let _ = fs::set_permissions(self.path, perm);
            }
        }
    }

    #[test]
    fn resumable_states_are_one_whitelist() {
        assert_eq!(resume_admission("STOPPED"), ResumeAdmission::Upgrade);
        assert_eq!(resume_admission("LOG_FAILED"), ResumeAdmission::Upgrade);
        assert_eq!(resume_admission("PREPARED"), ResumeAdmission::Upgrade);
        assert_eq!(resume_admission("BACKED_UP"), ResumeAdmission::Upgrade);
        assert_eq!(resume_admission("VERIFYING"), ResumeAdmission::Upgrade);
        assert_eq!(resume_admission("ROLLING_BACK"), ResumeAdmission::RollbackOnly);
        assert_eq!(resume_admission("ROLLBACK_PARTIAL"), ResumeAdmission::RollbackOnly);
        assert_eq!(resume_admission("COMMITTED"), ResumeAdmission::Reject);
        assert_eq!(resume_admission("ROLLED_BACK"), ResumeAdmission::Reject);
        assert_eq!(resume_actions("STOPPED"), vec!["继续", "回滚", "稍后处理"]);
        assert_eq!(resume_actions("ROLLING_BACK"), vec!["继续回滚", "稍后处理"]);
        assert_eq!(resume_actions("ROLLBACK_PARTIAL"), vec!["继续回滚", "稍后处理"]);
        assert!(resume_actions("COMMITTED").is_empty());
        let files = vec![
            ConflictFile { path: "a".into(), detail: DETAIL_MODIFIED.into() },
            ConflictFile { path: "b".into(), detail: DETAIL_DELETED.into() },
        ];
        assert_eq!(resume_conflict_title(&files), "有 2 个已经写完的文件被改过或删除");
        assert_eq!(
            resume_conflict_title(&files[..1]),
            "有 1 个已经写完的文件被改过"
        );
    }

    #[test]
    fn move_source_delete_failure_still_rolls_back_the_written_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("old")).unwrap();
        fs::create_dir_all(dir.path().join("new")).unwrap();
        fs::write(dir.path().join("old/m.txt"), b"orig").unwrap();
        fs::write(dir.path().join("tail.txt"), b"tail").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let mv = PlanOp {
            index: 1,
            kind: OpKind::Move,
            path: "new/m.txt".into(),
            from: Some("old/m.txt".into()),
            bytes: Some(b"orig".to_vec()),
            expect: Some(Snapshot { size: 4, mtime_ms: None, hash: sha256_hex(b"orig") }),
            mode: Some(0o644),
            source_mtime_ms: None,
            merged: false,
            keep_source: false,
            readonly: false,
            label: "移动".into(),
        };
        let p = plan(vec![mv, ov(2, "tail.txt", b"tail", b"TAIL", None)]);
        let old = dir.path().join("old");
        let _guard = DirMode { path: &old };
        let mut perm = fs::metadata(&old).unwrap().permissions();
        perm.set_mode(0o555);
        fs::set_permissions(&old, perm).unwrap();
        let rep = execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        assert_eq!(rep.state, "STOPPED");
        assert_eq!(rep.files[0].code.as_deref(), Some("E_PERMISSION"));
        assert_ne!(rep.files[0].code.as_deref(), Some("E_FAULT"));
        let written: i64 = db
            .query_row("SELECT written FROM exec_ops WHERE exec_id=?1 AND op_index=1", params![p.id], |r| r.get(0))
            .unwrap();
        assert_eq!(written, 1);
        assert_eq!(fs::read(dir.path().join("new/m.txt")).unwrap(), b"orig");
        assert_eq!(fs::read(dir.path().join("old/m.txt")).unwrap(), b"orig");
        assert_eq!(fs::read(dir.path().join("tail.txt")).unwrap(), b"tail");
        let mut perm = fs::metadata(&old).unwrap().permissions();
        perm.set_mode(0o755);
        fs::set_permissions(&old, perm).unwrap();
        let back = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(back.state, "ROLLED_BACK");
        assert!(!dir.path().join("new/m.txt").exists());
        assert_eq!(fs::read(dir.path().join("old/m.txt")).unwrap(), b"orig");
        assert_eq!(fs::read(dir.path().join("tail.txt")).unwrap(), b"tail");
    }

    #[test]
    fn resume_uses_change_detection_and_keeps_or_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        fs::write(dir.path().join("b.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", None), ov(2, "b.txt", b"old", b"NEW", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: None, corrupt_temp: true };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        fs::write(dir.path().join("a.txt"), b"ext-a").unwrap();
        fs::write(dir.path().join("b.txt"), b"ext-b").unwrap();
        let listed = resume(&db, &fsys, &p.id, ResumeChoice::Continue, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let ResumeOutcome::ChangesSinceScan(hits) = listed else { panic!("expected scan hits, got {listed:?}"); };
        assert_eq!(hits.len(), 2);
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"ext-a");
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"ext-b");
        let skipped = resume(&db, &fsys, &p.id, ResumeChoice::SkipChanged, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let ResumeOutcome::Continued(rep) = skipped else { panic!("expected continue"); };
        assert_eq!(rep.state, "COMMITTED");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"ext-a");
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"ext-b");

        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        fs::write(dir.path().join("b.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", Some(1)), ov(2, "b.txt", b"old", b"NEW", Some(1))]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: None, corrupt_temp: true };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        filetime::set_file_mtime(dir.path().join("b.txt"), filetime::FileTime::from_unix_time(90, 0)).unwrap();
        let continued = resume(&db, &fsys, &p.id, ResumeChoice::Continue, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let ResumeOutcome::Continued(rep) = continued else { panic!("touch is not a change: {continued:?}"); };
        assert_eq!(rep.state, "COMMITTED");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"NEW");
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"NEW");

        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: Some("backed_up".into()), corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        fs::write(dir.path().join("a.txt"), b"ext").unwrap();
        let wrote = resume(&db, &fsys, &p.id, ResumeChoice::OverwriteChanged, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let ResumeOutcome::Continued(_) = wrote else { panic!("expected overwrite"); };
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"NEW");
        assert_eq!(fs::read(dir.path().join("bak").join(&p.id).join("refresh").join("1.r2")).unwrap(), b"ext");
        assert!(!dir.path().join("bak").join(&p.id).join("files").join("a.txt").exists());
        let back = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(back.state, "ROLLED_BACK");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"ext");
        let again = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap_err();
        assert_eq!(again.code, ErrorCode::InvalidState);
    }

    #[test]
    fn deleted_written_file_is_a_resume_conflict() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old-a").unwrap();
        fs::write(dir.path().join("b.txt"), b"old-b").unwrap();
        fs::write(dir.path().join("c.txt"), b"old-c").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![
            ov(1, "a.txt", b"old-a", b"new-a", None),
            ov(2, "b.txt", b"old-b", b"new-b", None),
            ov(3, "c.txt", b"old-c", b"new-c", None),
        ]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: Some("done:2".into()), corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        fs::remove_file(dir.path().join("a.txt")).unwrap();
        fs::write(dir.path().join("b.txt"), b"external-b").unwrap();
        let listed = resume(&db, &fsys, &p.id, ResumeChoice::Continue, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let ResumeOutcome::RollbackPrompt(prompt) = listed else { panic!("expected conflict"); };
        assert_eq!(prompt.title, "有 2 个已经写完的文件被改过或删除");
        assert!(prompt.files.iter().any(|f| f.path == "a.txt" && f.detail == DETAIL_DELETED));
        assert!(prompt.files.iter().any(|f| f.path == "b.txt" && f.detail == DETAIL_MODIFIED));
        assert_eq!(fs::read(dir.path().join("c.txt")).unwrap(), b"old-c");
        assert!(!dir.path().join("a.txt").exists());
    }

    #[test]
    fn resume_rejects_finished_and_rollback_only_states() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"new", None)]);
        execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let err = resume(&db, &fsys, &p.id, ResumeChoice::Continue, &opts_at(dir.path()), FaultPlan::default()).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidState);
        assert_eq!(err.message, "这次升级已经结束，不能继续");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"new");
        assert_eq!(journal_state(&db, &p.id), "COMMITTED");

        let back = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(back.state, "ROLLED_BACK");
        let err = resume(&db, &fsys, &p.id, ResumeChoice::Continue, &opts_at(dir.path()), FaultPlan::default()).unwrap_err();
        assert_eq!(err.message, "这次升级已经结束，不能继续");
        assert_eq!(journal_state(&db, &p.id), "ROLLED_BACK");

        db.execute("UPDATE exec_journal SET state='ROLLING_BACK' WHERE exec_id=?1", params![p.id]).unwrap();
        let err = resume(&db, &fsys, &p.id, ResumeChoice::Continue, &opts_at(dir.path()), FaultPlan::default()).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidState);
        assert_eq!(err.message, "这次只能继续回滚，不能继续升级");
        assert_eq!(journal_state(&db, &p.id), "ROLLING_BACK");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old");

        db.execute("UPDATE exec_journal SET state='ROLLBACK_PARTIAL' WHERE exec_id=?1", params![p.id]).unwrap();
        let err = resume(&db, &fsys, &p.id, ResumeChoice::Continue, &opts_at(dir.path()), FaultPlan::default()).unwrap_err();
        assert_eq!(err.message, "这次只能继续回滚，不能继续升级");
        assert_eq!(journal_state(&db, &p.id), "ROLLBACK_PARTIAL");
    }

    #[test]
    fn startup_recovery_keeps_state_and_only_registered_temps() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        fs::write(dir.path().join("b.txt"), b"old").unwrap();
        fs::write(dir.path().join(".a.txt.htx-userkeep"), b"user").unwrap();
        fs::write(dir.path().join(".a.txt.htx-registered"), b"temp").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"new", None), ov(2, "b.txt", b"old", b"new", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: Some("doing:1".into()), corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        fs::write(dir.path().join("a.txt"), b"neither").unwrap();
        db.execute(
            "INSERT INTO exec_temps (exec_id, path) VALUES (?1, '.a.txt.htx-registered')",
            params![p.id],
        )
        .unwrap();
        let offers = recover_on_startup(&db, &fsys).unwrap();
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].state, "BACKED_UP");
        assert_eq!(offers[0].pre_recovery_state, "BACKED_UP");
        assert_eq!(offers[0].actions, vec!["继续".to_string(), "回滚".to_string(), "稍后处理".to_string()]);
        assert!(offers[0].conflicts.iter().any(|c| c.path == "a.txt" && c.detail == "无法判断是否已经写入"));
        assert_eq!(journal_state(&db, &p.id), "BACKED_UP");
        assert!(!dir.path().join(".a.txt.htx-registered").exists());
        assert_eq!(fs::read(dir.path().join(".a.txt.htx-userkeep")).unwrap(), b"user");

        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak2"), remote: false, stop_after_log: Some("prepared".into()), corrupt_temp: false };
        let p2 = plan(vec![ov(1, "b.txt", b"old", b"new", None)]);
        execute(&db, &fsys, &p2, &opts, FaultPlan::default()).unwrap();
        let offers = recover_on_startup(&db, &fsys).unwrap();
        let prepared = offers.iter().find(|o| o.exec_id == p2.id).unwrap();
        assert_eq!(prepared.state, "PREPARED");
        assert_eq!(prepared.pre_recovery_state, "PREPARED");
        assert_eq!(journal_state(&db, &p2.id), "PREPARED");

        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak3"), remote: false, stop_after_log: Some("verifying".into()), corrupt_temp: false };
        fs::write(dir.path().join("b.txt"), b"old").unwrap();
        let p3 = plan(vec![ov(1, "b.txt", b"old", b"new", None)]);
        let rep = execute(&db, &fsys, &p3, &opts, FaultPlan::default()).unwrap();
        assert_eq!(rep.state, "VERIFYING");
        let offers = recover_on_startup(&db, &fsys).unwrap();
        let verifying = offers.iter().find(|o| o.exec_id == p3.id).unwrap();
        assert_eq!(verifying.state, "VERIFYING");
        assert_eq!(verifying.pre_recovery_state, "VERIFYING");
        assert_eq!(journal_state(&db, &p3.id), "VERIFYING");
    }

    #[test]
    fn inferred_write_is_not_rewritten_and_log_probe_gates_resume() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"new", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: Some("doing:1".into()), corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        fs::write(dir.path().join("a.txt"), b"new").unwrap();
        let offers = recover_on_startup(&db, &fsys).unwrap();
        assert!(offers[0].conflicts.is_empty(), "{:?}", offers[0].conflicts);
        let written: i64 = db
            .query_row("SELECT written FROM exec_ops WHERE exec_id=?1 AND op_index=1", params![p.id], |r| r.get(0))
            .unwrap();
        assert_eq!(written, 1);
        let continued = resume(&db, &fsys, &p.id, ResumeChoice::Continue, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let ResumeOutcome::Continued(rep) = continued else { panic!("expected continue"); };
        assert_eq!(rep.state, "COMMITTED");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"new");

        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"new", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: Some("backed_up".into()), corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        db.execute("UPDATE exec_journal SET state='LOG_FAILED' WHERE exec_id=?1", params![p.id]).unwrap();
        db.execute_batch("PRAGMA query_only=ON").unwrap();
        let err = resume(&db, &fsys, &p.id, ResumeChoice::Continue, &opts_at(dir.path()), FaultPlan::default()).unwrap_err();
        assert_eq!(err.code, ErrorCode::LogWrite);
        assert_ne!(err.code_str(), "E_FAULT");
        assert_eq!(journal_state(&db, &p.id), "LOG_FAILED");
        assert!(pre_recovery(&db, &p.id).is_none());
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old");
        db.execute_batch("PRAGMA query_only=OFF").unwrap();
        let continued = resume(&db, &fsys, &p.id, ResumeChoice::Continue, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let ResumeOutcome::Continued(rep) = continued else { panic!("expected continue"); };
        assert_eq!(rep.state, "COMMITTED");
        assert_eq!(pre_recovery(&db, &p.id).as_deref(), Some("LOG_FAILED"));
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"new");
    }

    #[test]
    fn kept_external_is_not_listed_again_until_it_changes() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old-a").unwrap();
        fs::write(dir.path().join("b.txt"), b"old-b").unwrap();
        fs::write(dir.path().join("c.txt"), b"old-c").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![
            ov(1, "a.txt", b"old-a", b"new-a", None),
            ov(2, "b.txt", b"old-b", b"new-b", None),
            ov(3, "c.txt", b"old-c", b"new-c", None),
        ]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: Some("done:1".into()), corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        fs::write(dir.path().join("a.txt"), b"external").unwrap();
        let mut mid = opts_at(dir.path());
        mid.stop_after_log = Some("done:2".into());
        let kept = resume(&db, &fsys, &p.id, ResumeChoice::KeepExternal, &mid, FaultPlan::default()).unwrap();
        let ResumeOutcome::Continued(rep) = kept else { panic!("expected partial continue"); };
        assert_ne!(rep.state, "COMMITTED");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"external");
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"new-b");
        assert_eq!(fs::read(dir.path().join("c.txt")).unwrap(), b"old-c");
        let again = resume(&db, &fsys, &p.id, ResumeChoice::Continue, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let ResumeOutcome::Continued(rep) = again else { panic!("kept file listed again: {again:?}"); };
        assert_eq!(rep.state, "COMMITTED");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"external");
        assert_eq!(fs::read(dir.path().join("c.txt")).unwrap(), b"new-c");
        let conflicts = rollback_conflicts(&fsys, &db, &p.id, &p).unwrap();
        assert!(conflicts.iter().any(|c| c.path == "a.txt"));

        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old-a").unwrap();
        fs::write(dir.path().join("b.txt"), b"old-b").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old-a", b"new-a", None), ov(2, "b.txt", b"old-b", b"new-b", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: Some("done:1".into()), corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        fs::write(dir.path().join("a.txt"), b"external").unwrap();
        let mut mid = opts_at(dir.path());
        mid.stop_after_log = Some("backed_up".into());
        resume(&db, &fsys, &p.id, ResumeChoice::KeepExternal, &mid, FaultPlan::default()).unwrap();
        fs::write(dir.path().join("a.txt"), b"external-again").unwrap();
        let listed = resume(&db, &fsys, &p.id, ResumeChoice::Continue, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let ResumeOutcome::RollbackPrompt(prompt) = listed else { panic!("expected relist, got {listed:?}"); };
        assert_eq!(prompt.files.len(), 1);
        assert_eq!(prompt.files[0].path, "a.txt");
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"old-b");
    }

    #[test]
    fn resume_rollback_choice_writes_nothing_until_rollback_runs() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old-a").unwrap();
        fs::write(dir.path().join("b.txt"), b"old-b").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old-a", b"new-a", None), ov(2, "b.txt", b"old-b", b"new-b", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: Some("done:1".into()), corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        fs::write(dir.path().join("a.txt"), b"external").unwrap();
        let prompted = resume(&db, &fsys, &p.id, ResumeChoice::Rollback, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let ResumeOutcome::RollbackPrompt(prompt) = prompted else { panic!("expected prompt"); };
        assert_eq!(prompt.files.len(), 1);
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"external");
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"old-b");
        let err = rollback(&db, &fsys, &p.id, RollbackChoice::Cancel, FaultPlan::default()).unwrap_err();
        assert_eq!(err.code, ErrorCode::Cancelled);
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"external");
        let skipped = rollback(&db, &fsys, &p.id, RollbackChoice::SkipConflicts, FaultPlan::default()).unwrap();
        assert_eq!(skipped.state, "ROLLBACK_PARTIAL");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"external");
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"old-b");

        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old-a").unwrap();
        fs::write(dir.path().join("b.txt"), b"old-b").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old-a", b"new-a", None), ov(2, "b.txt", b"old-b", b"new-b", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: Some("done:1".into()), corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        fs::write(dir.path().join("a.txt"), b"external").unwrap();
        resume(&db, &fsys, &p.id, ResumeChoice::Rollback, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let back = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(back.state, "ROLLED_BACK");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old-a");
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"old-b");
    }

    #[test]
    fn resume_cancel_records_nothing_and_prompts_again() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        fs::write(dir.path().join("b.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"new", None), ov(2, "b.txt", b"old", b"new", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: Some("done:1".into()), corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        let state_before = journal_state(&db, &p.id);
        fs::write(dir.path().join("a.txt"), b"external").unwrap();
        let cancelled = resume(&db, &fsys, &p.id, ResumeChoice::Cancel, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        assert!(matches!(cancelled, ResumeOutcome::Unchanged));
        let kept: Option<String> = db
            .query_row("SELECT kept_hash FROM exec_ops WHERE exec_id=?1 AND op_index=1", params![p.id], |r| r.get(0))
            .unwrap();
        assert!(kept.is_none());
        assert_eq!(journal_state(&db, &p.id), state_before);
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"external");
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"old");
        let listed = resume(&db, &fsys, &p.id, ResumeChoice::Continue, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        assert!(matches!(listed, ResumeOutcome::RollbackPrompt(_)));
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"old");
    }

    #[test]
    fn kept_deletion_stays_deleted_until_rollback_restores_it() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old-a").unwrap();
        fs::write(dir.path().join("b.txt"), b"old-b").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old-a", b"new-a", None), ov(2, "b.txt", b"old-b", b"new-b", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: Some("done:1".into()), corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        fs::remove_file(dir.path().join("a.txt")).unwrap();
        let kept = resume(&db, &fsys, &p.id, ResumeChoice::KeepExternal, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let ResumeOutcome::Continued(rep) = kept else { panic!("expected continue"); };
        assert_eq!(rep.state, "COMMITTED");
        assert!(!dir.path().join("a.txt").exists());
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"new-b");
        let kept_hash: String = db
            .query_row("SELECT kept_hash FROM exec_ops WHERE exec_id=?1 AND op_index=1", params![p.id], |r| r.get(0))
            .unwrap();
        assert_eq!(kept_hash, KEPT_DELETED);
        let skipped = rollback(&db, &fsys, &p.id, RollbackChoice::SkipConflicts, FaultPlan::default()).unwrap();
        assert_eq!(skipped.state, "ROLLBACK_PARTIAL");
        assert!(!dir.path().join("a.txt").exists());
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"old-b");

        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old-a").unwrap();
        fs::write(dir.path().join("b.txt"), b"old-b").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old-a", b"new-a", None), ov(2, "b.txt", b"old-b", b"new-b", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: Some("done:1".into()), corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        fs::remove_file(dir.path().join("a.txt")).unwrap();
        resume(&db, &fsys, &p.id, ResumeChoice::KeepExternal, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let back = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(back.state, "ROLLED_BACK");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old-a");
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"old-b");
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn verify_post_failure_rolls_back_the_written_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut ops = Vec::new();
        for i in 1..=5 {
            let name = format!("f{i}.txt");
            fs::write(dir.path().join(&name), b"old").unwrap();
            ops.push(ov(i, &name, b"old", b"new", None));
        }
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(ops);
        let mut fp = FaultPlan::default();
        fp.items.push(crate::fault::Item::At { stage: Stage::VerifyPost, n: 3, mode: crate::fault::Mode::Error });
        let rep = execute(&db, &fsys, &p, &opts_at(dir.path()), fp).unwrap();
        assert_eq!(rep.state, "STOPPED");
        assert_eq!(rep.files.iter().find(|f| f.path == "f3.txt").unwrap().code.as_deref(), Some("E_VERIFY_FAILED"));
        let written: i64 = db
            .query_row("SELECT written FROM exec_ops WHERE exec_id=?1 AND op_index=3", params![p.id], |r| r.get(0))
            .unwrap();
        assert_eq!(written, 1);
        for i in 1..=5 {
            assert_eq!(fs::read(dir.path().join(format!("f{i}.txt"))).unwrap(), b"new");
        }
        let back = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(back.state, "ROLLED_BACK");
        for i in 1..=5 {
            assert_eq!(fs::read(dir.path().join(format!("f{i}.txt"))).unwrap(), b"old");
        }
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn resume_backs_up_missing_files_and_replaces_corrupt_backups() {
        let dir = tempfile::tempdir().unwrap();
        let mut ops = Vec::new();
        for i in 1..=6 {
            let name = format!("f{i}.txt");
            fs::write(dir.path().join(&name), format!("old{i}")).unwrap();
            ops.push(ov(i, &name, format!("old{i}").as_bytes(), b"NEW", None));
        }
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(ops);
        let mut fp = FaultPlan::default();
        fp.items.push(crate::fault::Item::At { stage: Stage::Backup, n: 3, mode: crate::fault::Mode::Error });
        let rep = execute(&db, &fsys, &p, &opts_at(dir.path()), fp).unwrap();
        assert_eq!(rep.state, "STOPPED");
        assert_eq!(rep.files[0].code.as_deref(), Some("E_IO"));
        let rows = read_manifest(&dir.path().join("bak").join(&p.id)).unwrap();
        assert_eq!(rows.len(), 2);
        let continued = resume(&db, &fsys, &p.id, ResumeChoice::Continue, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let ResumeOutcome::Continued(rep) = continued else { panic!("expected continue"); };
        assert_eq!(rep.state, "COMMITTED");
        assert_eq!(rep.exec_id, p.id);
        let rows = read_manifest(&dir.path().join("bak").join(&p.id)).unwrap();
        let paths: std::collections::HashSet<_> = rows.iter().filter_map(|v| v["path"].as_str()).collect();
        assert_eq!(paths.len(), 6, "{rows:?}");
        for i in 1..=6 {
            let name = format!("f{i}.txt");
            assert_eq!(fs::read(dir.path().join(&name)).unwrap(), b"NEW");
            let bak = fs::read(dir.path().join("bak").join(&p.id).join("files").join(&name)).unwrap();
            assert_eq!(bak, format!("old{i}").into_bytes());
        }
        let back = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(back.state, "ROLLED_BACK");
        for i in 1..=6 {
            assert_eq!(fs::read(dir.path().join(format!("f{i}.txt"))).unwrap(), format!("old{i}").into_bytes());
        }

        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old-bytes").unwrap();
        fs::write(dir.path().join("b.txt"), b"other").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old-bytes", b"NEW", None), ov(2, "b.txt", b"other", b"NEXT", None)]);
        let mut fp = FaultPlan::default();
        fp.items.push(crate::fault::Item::At { stage: Stage::Backup, n: 1, mode: crate::fault::Mode::Corrupt });
        execute(&db, &fsys, &p, &opts_at(dir.path()), fp).unwrap();
        assert!(!dir.path().join("bak").join(&p.id).join("files").join("a.txt").exists());
        let continued = resume(&db, &fsys, &p.id, ResumeChoice::Continue, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        assert!(matches!(continued, ResumeOutcome::Continued(_)));
        assert_eq!(fs::read(dir.path().join("bak").join(&p.id).join("files").join("a.txt")).unwrap(), b"old-bytes");
        let back = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(back.state, "ROLLED_BACK");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old-bytes");
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"other");
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn injected_io_stages_use_real_codes() {
        for (stage, mode, code) in [
            (Stage::Write, crate::fault::Mode::Error, "E_IO"),
            (Stage::Rename, crate::fault::Mode::Error, "E_IO"),
            (Stage::Backup, crate::fault::Mode::Error, "E_IO"),
            (Stage::Rollback, crate::fault::Mode::Error, "E_IO"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("a.txt"), b"old").unwrap();
            let db = open_db(&dir.path().join("t.sqlite")).unwrap();
            let fsys = LocalFs::new(dir.path());
            let p = plan(vec![ov(1, "a.txt", b"old", b"new", None)]);
            let mut fp = FaultPlan::default();
            fp.items.push(crate::fault::Item::At { stage, n: 1, mode });
            if stage == Stage::Rollback {
                execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
                let rep = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, fp).unwrap();
                assert_eq!(rep.state, "ROLLBACK_PARTIAL");
                assert_eq!(rep.files[0].code.as_deref(), Some(code));
                assert_ne!(rep.files[0].code.as_deref(), Some("E_ROLLBACK_PARTIAL"));
                assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"new");
            } else {
                let rep = execute(&db, &fsys, &p, &opts_at(dir.path()), fp).unwrap();
                assert_eq!(rep.state, "STOPPED");
                assert_eq!(rep.files[0].code.as_deref(), Some(code), "{stage:?}");
                assert_ne!(rep.files[0].code.as_deref(), Some("E_FAULT"));
                assert_ne!(rep.files[0].code.as_deref(), Some("E_INTERNAL"));
                assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old");
            }
        }
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn panic_aborts_without_logging_internal() {
        if std::env::var("HTX_ABORT_CHILD").is_ok() {
            let dir = PathBuf::from(std::env::var("HTX_ABORT_DIR").unwrap());
            fs::write(dir.join("a.txt"), b"old").unwrap();
            fs::write(dir.join(".a.txt.htx-userkeep"), b"user").unwrap();
            let db = open_db(&dir.join("t.sqlite")).unwrap();
            let fsys = LocalFs::new(&dir);
            let p = plan(vec![ov(1, "a.txt", b"old", b"new", None)]);
            fs::write(dir.join("exec_id"), &p.id).unwrap();
            let mut fp = FaultPlan::default();
            fp.items.push(crate::fault::Item::At { stage: Stage::VerifyPre, n: 1, mode: crate::fault::Mode::Panic });
            let _ = execute(&db, &fsys, &p, &opts_at(&dir), fp);
            std::process::exit(2);
        }
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        let current = std::thread::current();
        let test_name = current.name().unwrap_or("panic_aborts_without_logging_internal");
        cmd.arg(test_name)
            .arg("--exact")
            .env("HTX_ABORT_CHILD", "1")
            .env("HTX_ABORT_DIR", dir.path());
        let status = cmd.status().unwrap();
        assert!(!status.success(), "panic injection returned instead of aborting: {status}");
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(6), "expected SIGABRT, got {status}");
        }
        let id = fs::read_to_string(dir.path().join("exec_id")).unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        assert_eq!(journal_state(&db, id.trim()), "BACKED_UP");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old");
        assert_eq!(fs::read(dir.path().join(".a.txt.htx-userkeep")).unwrap(), b"user");
        let temps: i64 = db.query_row("SELECT COUNT(*) FROM exec_temps WHERE exec_id=?1", params![id.trim()], |r| r.get(0)).unwrap();
        assert_eq!(temps, 1);
        let internal: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM exec_ops WHERE exec_id=?1 AND code='E_INTERNAL'",
                params![id.trim()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(internal, 0);
        let offers = recover_on_startup(&db, &fsys).unwrap();
        assert_eq!(offers[0].state, "BACKED_UP");
        assert_eq!(offers[0].pre_recovery_state, "BACKED_UP");
        assert_eq!(offers[0].actions, vec!["继续".to_string(), "回滚".to_string(), "稍后处理".to_string()]);
        let temps: i64 = db.query_row("SELECT COUNT(*) FROM exec_temps WHERE exec_id=?1", params![id.trim()], |r| r.get(0)).unwrap();
        assert_eq!(temps, 0);
        assert_eq!(fs::read(dir.path().join(".a.txt.htx-userkeep")).unwrap(), b"user");
    }

    #[test]
    fn interrupted_rollback_offers_continue_or_later() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"new", None)]);
        execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        for state in ["ROLLING_BACK", "ROLLBACK_PARTIAL"] {
            db.execute("UPDATE exec_journal SET state=?2, pre_recovery_state=NULL WHERE exec_id=?1", params![p.id, state]).unwrap();
            let offers = recover_on_startup(&db, &fsys).unwrap();
            let offer = offers.iter().find(|o| o.exec_id == p.id).unwrap();
            assert_eq!(offer.actions, vec!["继续回滚".to_string(), "稍后处理".to_string()]);
            assert!(!offer.actions.iter().any(|a| a.contains("继续升级") || a == "继续"));
            assert_eq!(offer.state, state);
            assert_eq!(offer.pre_recovery_state, state);
            assert_eq!(journal_state(&db, &p.id), state);
        }
    }

    #[test]
    fn overwrite_changed_rebackup_restores_the_user_version() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: Some("prepared".into()), corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        fs::write(dir.path().join("a.txt"), b"ext").unwrap();
        let wrote = resume(&db, &fsys, &p.id, ResumeChoice::OverwriteChanged, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let ResumeOutcome::Continued(rep) = wrote else { panic!("expected overwrite, got {wrote:?}"); };
        assert_eq!(rep.state, "COMMITTED");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"NEW");
        assert_eq!(fs::read(dir.path().join("bak").join(&p.id).join("refresh").join("1.r2")).unwrap(), b"ext");
        assert!(!dir.path().join("bak").join(&p.id).join("files").join("a.txt").exists());
        let back = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(back.state, "ROLLED_BACK", "{:?}", back.files);
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"ext");
        assert!(back.files.iter().all(|f| f.code.as_deref() != Some("E_IO")));
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn resume_rebackup_uses_the_backup_hook() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old-a").unwrap();
        fs::write(dir.path().join("b.txt"), b"old-b").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old-a", b"NEW", None), ov(2, "b.txt", b"old-b", b"NEW", None)]);
        let mut fp = FaultPlan::default();
        fp.items.push(crate::fault::Item::At { stage: Stage::Backup, n: 2, mode: crate::fault::Mode::Error });
        let rep = execute(&db, &fsys, &p, &opts_at(dir.path()), fp).unwrap();
        assert_eq!(rep.state, "STOPPED");
        assert!(!dir.path().join("bak").join(&p.id).join("files").join("b.txt").exists());
        let mut again = FaultPlan::default();
        again.items.push(crate::fault::Item::At { stage: Stage::Backup, n: 2, mode: crate::fault::Mode::Error });
        let continued = resume(&db, &fsys, &p.id, ResumeChoice::Continue, &opts_at(dir.path()), again).unwrap();
        let ResumeOutcome::Continued(rep) = continued else { panic!("backup hook did not fire: {continued:?}"); };
        assert_eq!(rep.state, "STOPPED");
        assert_eq!(rep.files.iter().find(|f| f.path == "b.txt").unwrap().code.as_deref(), Some("E_IO"));
        assert!(!dir.path().join("bak").join(&p.id).join("files").join("b.txt").exists());
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"old-b");

        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: Some("backed_up".into()), corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        fs::write(dir.path().join("a.txt"), b"ext").unwrap();
        let mut fp = FaultPlan::default();
        fp.items.push(crate::fault::Item::At { stage: Stage::Backup, n: 1, mode: crate::fault::Mode::Error });
        let continued = resume(&db, &fsys, &p.id, ResumeChoice::OverwriteChanged, &opts_at(dir.path()), fp).unwrap();
        let ResumeOutcome::Continued(rep) = continued else { panic!("rebackup hook did not fire: {continued:?}"); };
        assert_eq!(rep.files[0].code.as_deref(), Some("E_IO"));
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"ext");
        assert_eq!(fs::read(dir.path().join("bak").join(&p.id).join("files").join("a.txt")).unwrap(), b"old");

        fs::write(dir.path().join("a.txt"), b"ext").unwrap();
        let mut fp = FaultPlan::default();
        fp.items.push(crate::fault::Item::At { stage: Stage::Backup, n: 1, mode: crate::fault::Mode::Corrupt });
        let continued = resume(&db, &fsys, &p.id, ResumeChoice::OverwriteChanged, &opts_at(dir.path()), fp).unwrap();
        let ResumeOutcome::Continued(rep) = continued else { panic!("corrupt rebackup did not fire: {continued:?}"); };
        assert_eq!(rep.state, "STOPPED");
        assert_eq!(rep.files[0].code.as_deref(), Some("E_VERIFY_FAILED"));
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"ext");
        assert_eq!(fs::read(dir.path().join("bak").join(&p.id).join("files").join("a.txt")).unwrap(), b"old");
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn continue_rollback_skips_files_already_restored() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["f1.txt", "f2.txt", "f3.txt"] {
            fs::write(dir.path().join(name), b"old").unwrap();
        }
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![
            ov(1, "f1.txt", b"old", b"new", None),
            ov(2, "f2.txt", b"old", b"new", None),
            ov(3, "f3.txt", b"old", b"new", None),
        ]);
        execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let mut fp = FaultPlan::default();
        fp.items.push(crate::fault::Item::At { stage: Stage::Rollback, n: 2, mode: crate::fault::Mode::Error });
        let rep = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, fp).unwrap();
        assert_eq!(rep.state, "ROLLBACK_PARTIAL");
        assert_eq!(rep.files.iter().find(|f| f.path == "f2.txt").unwrap().code.as_deref(), Some("E_IO"));
        assert_eq!(fs::read(dir.path().join("f1.txt")).unwrap(), b"old");
        assert_eq!(fs::read(dir.path().join("f3.txt")).unwrap(), b"old");
        assert_eq!(fs::read(dir.path().join("f2.txt")).unwrap(), b"new");
        let conflicts = rollback_conflicts(&fsys, &db, &p.id, &p).unwrap();
        assert!(conflicts.iter().all(|c| c.path != "f1.txt" && c.path != "f3.txt"), "{conflicts:?}");
        let again = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(again.state, "ROLLED_BACK", "{:?}", again.files);
        assert_eq!(fs::read(dir.path().join("f1.txt")).unwrap(), b"old");
        assert_eq!(fs::read(dir.path().join("f2.txt")).unwrap(), b"old");
        assert_eq!(fs::read(dir.path().join("f3.txt")).unwrap(), b"old");
        assert!(!dir.path().join("bak").join(&p.id).join("conflict-current").exists());
    }

    #[test]
    fn planned_delete_missing_after_crash_is_done() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old-a").unwrap();
        fs::write(dir.path().join("b.txt"), b"old-b").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![del(1, "a.txt", b"old-a"), ov(2, "b.txt", b"old-b", b"NEW", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: Some("doing:1".into()), corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        fs::remove_file(dir.path().join("a.txt")).unwrap();
        let offers = recover_on_startup(&db, &fsys).unwrap();
        assert!(offers[0].conflicts.iter().all(|c| c.detail != DETAIL_DELETED), "{:?}", offers[0].conflicts);
        let written: i64 = db.query_row("SELECT written FROM exec_ops WHERE exec_id=?1 AND op_index=1", params![p.id], |r| r.get(0)).unwrap();
        assert_eq!(written, 1);
        let continued = resume(&db, &fsys, &p.id, ResumeChoice::Continue, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let ResumeOutcome::Continued(rep) = continued else { panic!("delete was listed as a conflict: {continued:?}"); };
        assert_eq!(rep.state, "COMMITTED");
        assert!(!dir.path().join("a.txt").exists());
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"NEW");
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn log_indices_for_file3_done_delete_done_and_resume() {
        // REC-21: 三个覆盖，第 3 个文件的 done 是第 8 次日志写入。
        // 1 prepared, 2 backed_up, 3 doing1, 4 done1, 5 doing2, 6 done2, 7 doing3, 8 done3.
        let dir = tempfile::tempdir().unwrap();
        for i in 1..=3 {
            fs::write(dir.path().join(format!("f{i}.txt")), b"old").unwrap();
        }
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![
            ov(1, "f1.txt", b"old", b"new", None),
            ov(2, "f2.txt", b"old", b"new", None),
            ov(3, "f3.txt", b"old", b"new", None),
        ]);
        let mut fp = FaultPlan::default();
        fp.items.push(crate::fault::Item::At { stage: Stage::Log, n: 8, mode: crate::fault::Mode::Error });
        let err = execute(&db, &fsys, &p, &opts_at(dir.path()), fp).unwrap_err();
        assert_eq!(err.code, ErrorCode::LogWrite);
        assert_eq!(journal_state(&db, &p.id), "LOG_FAILED");
        assert_eq!(fs::read(dir.path().join("f1.txt")).unwrap(), b"new");
        assert_eq!(fs::read(dir.path().join("f2.txt")).unwrap(), b"new");
        assert_eq!(fs::read(dir.path().join("f3.txt")).unwrap(), b"new");
        let state: String = db.query_row("SELECT state FROM exec_ops WHERE exec_id=?1 AND op_index=3", params![p.id], |r| r.get(0)).unwrap();
        assert_eq!(state, "doing");
        let written: i64 = db.query_row("SELECT written FROM exec_ops WHERE exec_id=?1 AND op_index=3", params![p.id], |r| r.get(0)).unwrap();
        assert_eq!(written, 1);

        let dir = tempfile::tempdir().unwrap();
        for i in 1..=3 {
            fs::write(dir.path().join(format!("f{i}.txt")), b"old").unwrap();
        }
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![
            ov(1, "f1.txt", b"old", b"new", None),
            ov(2, "f2.txt", b"old", b"new", None),
            ov(3, "f3.txt", b"old", b"new", None),
        ]);
        let mut fp = FaultPlan::default();
        fp.items.push(crate::fault::Item::At { stage: Stage::Log, n: 7, mode: crate::fault::Mode::Error });
        execute(&db, &fsys, &p, &opts_at(dir.path()), fp).unwrap_err();
        assert_eq!(fs::read(dir.path().join("f3.txt")).unwrap(), b"old");
        let state: String = db.query_row("SELECT state FROM exec_ops WHERE exec_id=?1 AND op_index=3", params![p.id], |r| r.get(0)).unwrap();
        assert_eq!(state, "pending");

        // REC-23: 第 3 个操作是计划内删除，它的 done 同样是 log:8。
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        fs::write(dir.path().join("b.txt"), b"old").unwrap();
        fs::write(dir.path().join("c.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", None), ov(2, "b.txt", b"old", b"NEW", None), del(3, "c.txt", b"old")]);
        let mut fp = FaultPlan::default();
        fp.items.push(crate::fault::Item::At { stage: Stage::Log, n: 8, mode: crate::fault::Mode::Error });
        execute(&db, &fsys, &p, &opts_at(dir.path()), fp).unwrap_err();
        assert!(!dir.path().join("c.txt").exists());
        let state: String = db.query_row("SELECT state FROM exec_ops WHERE exec_id=?1 AND op_index=3", params![p.id], |r| r.get(0)).unwrap();
        assert_eq!(state, "doing", "delete done record is not log:8");
        let written: i64 = db.query_row("SELECT written FROM exec_ops WHERE exec_id=?1 AND op_index=3", params![p.id], |r| r.get(0)).unwrap();
        assert_eq!(written, 1);

        // FI-21: 继续这一轮的第 1 次日志写入是 BACKED_UP。探测写不计数。
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old-a").unwrap();
        fs::write(dir.path().join("b.txt"), b"old-b").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old-a", b"NEW", None), ov(2, "b.txt", b"old-b", b"NEW", None)]);
        let mut fp = FaultPlan::default();
        fp.items.push(crate::fault::Item::At { stage: Stage::Backup, n: 2, mode: crate::fault::Mode::Error });
        execute(&db, &fsys, &p, &opts_at(dir.path()), fp).unwrap();
        assert!(!dir.path().join("bak").join(&p.id).join("files").join("b.txt").exists());
        db.execute("UPDATE exec_journal SET state='LOG_FAILED' WHERE exec_id=?1", params![p.id]).unwrap();
        let mut fp = FaultPlan::default();
        fp.items.push(crate::fault::Item::At { stage: Stage::Log, n: 1, mode: crate::fault::Mode::Error });
        // 试写规格在场，但不会占用 log:1。log:1 仍然打在 BACKED_UP 上。
        fp.items.push(crate::fault::Item::LogProbe { n: Some(u32::MAX) });
        let err = resume(&db, &fsys, &p.id, ResumeChoice::Continue, &opts_at(dir.path()), fp).unwrap_err();
        assert_eq!(err.code, ErrorCode::LogWrite);
        assert!(err.message.contains("事务日志写入失败"), "{}", err.message);
        assert_eq!(pre_recovery(&db, &p.id).as_deref(), Some("LOG_FAILED"));
        assert!(dir.path().join("bak").join(&p.id).join("files").join("b.txt").exists());
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old-a");
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"old-b");
        let state: String = db.query_row("SELECT state FROM exec_ops WHERE exec_id=?1 AND op_index=1", params![p.id], |r| r.get(0)).unwrap();
        assert_eq!(state, "pending");
    }

    #[cfg(feature = "fault-inject")]
    fn seed_log_failed(db: &Connection, dir: &Path, file: &str, body: &[u8]) -> String {
        fs::write(dir.join(file), body).unwrap();
        let fsys = LocalFs::new(dir);
        let p = plan(vec![ov(1, file, body, b"NEW", None)]);
        let mut opts = opts_at(dir);
        opts.stop_after_log = Some("backed_up".into());
        execute(db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        db.execute("UPDATE exec_journal SET state='LOG_FAILED' WHERE exec_id=?1", params![p.id]).unwrap();
        let temp = format!(".{file}.htx-probe");
        fs::write(dir.join(&temp), b"temp").unwrap();
        db.execute("INSERT INTO exec_temps (exec_id, path) VALUES (?1,?2)", params![p.id, temp]).unwrap();
        p.id
    }

    #[cfg(feature = "fault-inject")]
    fn started_at(db: &Connection, id: &str) -> i64 {
        db.query_row("SELECT started_at FROM exec_journal WHERE exec_id=?1", params![id], |r| r.get(0)).unwrap()
    }

    /// 先启动 A，再启动 B。两笔都停在 LOG_FAILED，试写才会发生。
    #[cfg(feature = "fault-inject")]
    fn two_log_failed(db: &Connection, dir: &Path) -> (String, String) {
        let a = seed_log_failed(db, dir, "a.txt", b"old-a");
        let a_at = started_at(db, &a);
        while crate::vfs::now_ms() <= a_at {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let b = seed_log_failed(db, dir, "b.txt", b"old-b");
        assert!(started_at(db, &a) < started_at(db, &b));
        assert_eq!(journal_state(db, &a), "LOG_FAILED");
        assert_eq!(journal_state(db, &b), "LOG_FAILED");
        (a, b)
    }

    #[test]
    fn planned_delete_is_restored_on_rollback_and_skip_is_partial() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["f1.txt", "f2.txt", "f3.txt", "f4.txt", "f5.txt"] {
            fs::write(dir.path().join(name), b"old").unwrap();
        }
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![
            ov(1, "f1.txt", b"old", b"new", None),
            ov(2, "f2.txt", b"old", b"new", None),
            del(3, "f3.txt", b"old"),
            ov(4, "f4.txt", b"old", b"new", None),
            ov(5, "f5.txt", b"old", b"new", None),
        ]);
        execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        assert!(!dir.path().join("f3.txt").exists());
        let conflicts = rollback_conflicts(&fsys, &db, &p.id, &p).unwrap();
        assert!(conflicts.is_empty(), "{conflicts:?}");
        let back = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(back.state, "ROLLED_BACK", "{:?}", back.files);
        assert_eq!(back.history_result, "已回滚");
        for name in ["f1.txt", "f2.txt", "f3.txt", "f4.txt", "f5.txt"] {
            assert_eq!(fs::read(dir.path().join(name)).unwrap(), b"old", "{name}");
        }
        let paths: String = db.query_row("SELECT paths_json FROM history WHERE exec_id=?1", params![p.id], |r| r.get(0)).unwrap();
        assert_eq!(paths, "[]");

        let dir = tempfile::tempdir().unwrap();
        for name in ["f1.txt", "f2.txt", "f3.txt", "f4.txt", "f5.txt"] {
            fs::write(dir.path().join(name), b"old").unwrap();
        }
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![
            ov(1, "f1.txt", b"old", b"new", None),
            ov(2, "f2.txt", b"old", b"new", None),
            del(3, "f3.txt", b"old"),
            ov(4, "f4.txt", b"old", b"new", None),
            ov(5, "f5.txt", b"old", b"new", None),
        ]);
        execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        fs::write(dir.path().join("f1.txt"), b"external").unwrap();
        let skipped = rollback(&db, &fsys, &p.id, RollbackChoice::SkipConflicts, FaultPlan::default()).unwrap();
        assert_eq!(skipped.state, "ROLLBACK_PARTIAL", "{:?}", skipped.files);
        assert_eq!(skipped.history_result, "部分回滚");
        assert_eq!(fs::read(dir.path().join("f1.txt")).unwrap(), b"external");
        assert_eq!(fs::read(dir.path().join("f3.txt")).unwrap(), b"old");
        let paths: String = db.query_row("SELECT paths_json FROM history WHERE exec_id=?1", params![p.id], |r| r.get(0)).unwrap();
        assert!(paths.contains("f1.txt"), "{paths}");
        assert!(!paths.contains("f3.txt"), "{paths}");
        fs::write(dir.path().join("f3.txt"), b"recreated").unwrap();
        db.execute("UPDATE exec_journal SET state='COMMITTED' WHERE exec_id=?1", params![p.id]).unwrap();
        db.execute("UPDATE exec_ops SET rollback_state=NULL WHERE exec_id=?1", params![p.id]).unwrap();
        let again = rollback_conflicts(&fsys, &db, &p.id, &p).unwrap();
        assert!(again.iter().any(|c| c.path == "f3.txt"), "{again:?}");
    }

    #[test]
    fn manifest_failure_keeps_the_old_backup_pair() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: Some("backed_up".into()), corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        let root = dir.path().join("bak").join(&p.id);
        let before = fs::read_to_string(root.join("manifest.json")).unwrap();
        fs::write(dir.path().join("a.txt"), b"ext").unwrap();
        set_fail_manifest_rename(true);
        let wrote = resume(&db, &fsys, &p.id, ResumeChoice::OverwriteChanged, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        set_fail_manifest_rename(false);
        let ResumeOutcome::Continued(rep) = wrote else { panic!("expected stop, got {wrote:?}"); };
        assert_eq!(rep.state, "STOPPED");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"ext");
        assert_eq!(fs::read(root.join("files").join("a.txt")).unwrap(), b"old");
        assert!(!root.join("files").join("1.r2").exists());
        assert!(!root.join("manifest.json.htx-new").exists());
        assert_eq!(fs::read_to_string(root.join("manifest.json")).unwrap(), before);
        let staged: i64 = db.query_row("SELECT COUNT(*) FROM exec_temps WHERE exec_id=?1 AND path LIKE 'bak:%'", params![p.id], |r| r.get(0)).unwrap();
        assert_eq!(staged, 0);
    }

    #[test]
    fn changed_after_backup_stops_before_write() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        fs::write(dir.path().join("b.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", None), ov(2, "b.txt", b"old", b"NEW", None)]);
        set_mutate_before_write(Some(("a.txt", b"ext")));
        let rep = execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        set_mutate_before_write(None);
        assert_eq!(rep.state, "STOPPED", "{:?}", rep.files);
        assert_eq!(rep.files.iter().find(|f| f.path == "a.txt").unwrap().code.as_deref(), Some("E_CHANGED_SINCE_SCAN"));
        assert!(rep.files.iter().any(|f| f.error.as_deref().unwrap_or("").contains("文件在备份后被修改，已停止")));
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"ext");
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"old");
        assert_eq!(fs::read(dir.path().join("bak").join(&p.id).join("files").join("a.txt")).unwrap(), b"old");
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn refreshed_backup_uses_refresh_dir_and_does_not_clobber_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old-a").unwrap();
        fs::write(dir.path().join("1.r2"), b"project-r2").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![
            ov(1, "a.txt", b"old-a", b"NEW-A", None),
            ov(2, "1.r2", b"project-r2", b"NEW-R", None),
        ]);
        let mut fp = FaultPlan::default();
        fp.items.push(crate::fault::Item::At { stage: Stage::Backup, n: 2, mode: crate::fault::Mode::Error });
        let rep = execute(&db, &fsys, &p, &opts_at(dir.path()), fp).unwrap();
        assert_eq!(rep.state, "STOPPED");
        let root = dir.path().join("bak").join(&p.id);
        assert_eq!(fs::read(root.join("files").join("a.txt")).unwrap(), b"old-a");
        assert!(!root.join("files").join("1.r2").exists());
        fs::write(dir.path().join("a.txt"), b"ext-a").unwrap();
        let continued = resume(&db, &fsys, &p.id, ResumeChoice::OverwriteChanged, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let ResumeOutcome::Continued(rep) = continued else { panic!("resume stopped before both backups: {continued:?}"); };
        assert_eq!(rep.state, "COMMITTED", "{:?}", rep.files);
        let rows = read_manifest(&root).unwrap();
        let backup_of = |path: &str| {
            rows.iter()
                .rev()
                .find(|v| v.get("path").and_then(|p| p.as_str()) == Some(path))
                .unwrap()
                .get("backup")
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        };
        let a_bak = backup_of("a.txt");
        let r_bak = backup_of("1.r2");
        assert_eq!(a_bak, "refresh/1.r2");
        assert_eq!(r_bak, "files/1.r2");
        assert_eq!(fs::read(root.join(&a_bak)).unwrap(), b"ext-a");
        assert_eq!(fs::read(root.join(&r_bak)).unwrap(), b"project-r2");
    }

    #[test]
    fn project_refresh_directory_stays_under_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("refresh")).unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        fs::write(dir.path().join("refresh").join("keep.txt"), b"note").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![
            ov(1, "a.txt", b"old", b"NEW", None),
            ov(2, "refresh/keep.txt", b"note", b"NEXT", None),
        ]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: Some("backed_up".into()), corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        let root = dir.path().join("bak").join(&p.id);
        assert_eq!(fs::read(root.join("files").join("refresh").join("keep.txt")).unwrap(), b"note");
        fs::write(dir.path().join("a.txt"), b"ext").unwrap();
        let continued = resume(&db, &fsys, &p.id, ResumeChoice::OverwriteChanged, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let ResumeOutcome::Continued(rep) = continued else { panic!("expected commit, got {continued:?}"); };
        assert_eq!(rep.state, "COMMITTED", "{:?}", rep.files);
        assert_eq!(fs::read(root.join("files").join("refresh").join("keep.txt")).unwrap(), b"note");
        assert_eq!(fs::read(root.join("refresh").join("1.r2")).unwrap(), b"ext");
        assert!(!root.join("refresh").join("keep.txt").exists());
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn fsync_dir_after_manifest_rename_is_three_and_keeps_the_new_backup() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", None)]);
        let opts = ExecOpts { allow_readonly: false, backup_root: dir.path().join("bak"), remote: false, stop_after_log: Some("backed_up".into()), corrupt_temp: false };
        execute(&db, &fsys, &p, &opts, FaultPlan::default()).unwrap();
        let root = dir.path().join("bak").join(&p.id);
        let resume_at = |spec: &str| {
            let _ = fs::remove_dir_all(root.join("refresh"));
            fs::write(dir.path().join("a.txt"), b"ext").unwrap();
            let continued = resume(&db, &fsys, &p.id, ResumeChoice::OverwriteChanged, &opts_at(dir.path()), crate::fault::parse_spec(spec).unwrap()).unwrap();
            let ResumeOutcome::Continued(rep) = continued else { panic!("expected stop, got {continued:?}"); };
            rep
        };
        let early = resume_at("fsync-dir:1");
        assert_eq!(early.files[0].code.as_deref(), Some("E_IO"));
        assert!(early.files[0].error.as_deref().unwrap_or("").contains("FsyncDir:1"), "{:?}", early.files[0].error);
        let rows = read_manifest(&root).unwrap();
        let pointed = rows.iter().rev().find(|v| v.get("path").and_then(|p| p.as_str()) == Some("a.txt")).unwrap().get("backup").unwrap().as_str().unwrap();
        assert_eq!(pointed, "files/a.txt");
        assert!(!root.join("refresh").join("1.r2").exists());
        assert_eq!(fs::read(root.join("files").join("a.txt")).unwrap(), b"old");

        let before_rename = resume_at("fsync-dir:2");
        assert_eq!(before_rename.state, "STOPPED");
        assert!(before_rename.files[0].error.as_deref().unwrap_or("").contains("FsyncDir:2"), "{:?}", before_rename.files[0].error);
        let rows = read_manifest(&root).unwrap();
        let pointed = rows.iter().rev().find(|v| v.get("path").and_then(|p| p.as_str()) == Some("a.txt")).unwrap().get("backup").unwrap().as_str().unwrap();
        assert_eq!(pointed, "files/a.txt");
        assert!(!root.join("refresh").join("1.r2").exists());

        let at_rename = resume_at("fsync-dir:3");
        assert_eq!(at_rename.state, "STOPPED");
        assert_eq!(at_rename.files[0].code.as_deref(), Some("E_IO"));
        assert!(at_rename.files[0].error.as_deref().unwrap_or("").contains("FsyncDir:3"), "{:?}", at_rename.files[0].error);
        let rows = read_manifest(&root).unwrap();
        let pointed = rows.iter().rev().find(|v| v.get("path").and_then(|p| p.as_str()) == Some("a.txt")).unwrap().get("backup").unwrap().as_str().unwrap();
        assert_eq!(pointed, "refresh/1.r2");
        assert_eq!(fs::read(root.join("refresh").join("1.r2")).unwrap(), b"ext");
        assert_eq!(fs::read(root.join("files").join("a.txt")).unwrap(), b"old");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"ext");
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn fsync_dir_after_atomic_rename_is_six() {
        // 备份根目录还不存在。M = 6。
        let run = |spec: &str| {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("a.txt"), b"old").unwrap();
            let db = open_db(&dir.path().join("t.sqlite")).unwrap();
            let fsys = LocalFs::new(dir.path());
            let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", None)]);
            let rep = execute(&db, &fsys, &p, &opts_at(dir.path()), crate::fault::parse_spec(spec).unwrap());
            (dir, db, p, rep)
        };
        let (dir, _db, p, err) = {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("a.txt"), b"old").unwrap();
            let db = open_db(&dir.path().join("t.sqlite")).unwrap();
            let fsys = LocalFs::new(dir.path());
            let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", None)]);
            let err = execute(&db, &fsys, &p, &opts_at(dir.path()), crate::fault::parse_spec("fsync-dir:1").unwrap()).unwrap_err();
            (dir, db, p, err)
        };
        assert!(err.message.contains("FsyncDir:1"), "{}", err.message);
        assert!(dir.path().join("bak").is_dir());
        assert!(!dir.path().join("bak").join(&p.id).exists());
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old");

        let (dir, _db, p, err) = {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("a.txt"), b"old").unwrap();
            let db = open_db(&dir.path().join("t.sqlite")).unwrap();
            let fsys = LocalFs::new(dir.path());
            let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", None)]);
            let err = execute(&db, &fsys, &p, &opts_at(dir.path()), crate::fault::parse_spec("fsync-dir:2").unwrap()).unwrap_err();
            (dir, db, p, err)
        };
        assert!(err.message.contains("FsyncDir:2"), "{}", err.message);
        assert!(dir.path().join("bak").join(&p.id).is_dir());
        assert!(!dir.path().join("bak").join(&p.id).join("files").exists());
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old");

        let (dir, _db, p, err) = {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("a.txt"), b"old").unwrap();
            let db = open_db(&dir.path().join("t.sqlite")).unwrap();
            let fsys = LocalFs::new(dir.path());
            let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", None)]);
            let err = execute(&db, &fsys, &p, &opts_at(dir.path()), crate::fault::parse_spec("fsync-dir:3").unwrap()).unwrap_err();
            (dir, db, p, err)
        };
        assert!(err.message.contains("FsyncDir:3"), "{}", err.message);
        assert!(dir.path().join("bak").join(&p.id).join("files").is_dir());
        assert!(!dir.path().join("bak").join(&p.id).join("files").join("a.txt").exists());
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old");

        let (dir, _db, p, rep) = run("fsync-dir:4");
        let rep = rep.unwrap();
        assert_eq!(rep.state, "STOPPED");
        assert!(rep.files[0].error.as_deref().unwrap_or("").contains("FsyncDir:4"), "{:?}", rep.files[0].error);
        assert_eq!(fs::read(dir.path().join("bak").join(&p.id).join("files").join("a.txt")).unwrap(), b"old");
        assert!(!dir.path().join("bak").join(&p.id).join("manifest.json").exists());
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old");

        let (dir, _db, p, rep) = run("fsync-dir:5");
        let rep = rep.unwrap();
        assert_eq!(rep.state, "STOPPED");
        assert!(rep.files[0].error.as_deref().unwrap_or("").contains("FsyncDir:5"));
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old");
        let rows = read_manifest(&dir.path().join("bak").join(&p.id)).unwrap();
        let pointed = rows.iter().rev().find(|v| v.get("path").and_then(|p| p.as_str()) == Some("a.txt")).unwrap().get("backup").unwrap().as_str().unwrap();
        assert_eq!(pointed, "files/a.txt");

        let (dir, db, p, rep) = run("fsync-dir:6");
        let rep = rep.unwrap();
        assert_eq!(rep.state, "STOPPED");
        assert_eq!(rep.files[0].code.as_deref(), Some("E_IO"));
        assert!(rep.files[0].error.as_deref().unwrap_or("").contains("FsyncDir:6"), "{:?}", rep.files[0].error);
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"NEW");
        let written: i64 = db
            .query_row("SELECT written FROM exec_ops WHERE exec_id=?1 AND op_index=1", params![p.id], |r| r.get(0))
            .unwrap();
        assert_eq!(written, 1);
        let fsys = LocalFs::new(dir.path());
        let back = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(back.state, "ROLLED_BACK", "{:?}", back.files);
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old");
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn fsync_dir_after_atomic_rename_is_five_when_backup_root_exists() {
        let run = |spec: &str| {
            let dir = tempfile::tempdir().unwrap();
            fs::create_dir(dir.path().join("bak")).unwrap();
            fs::write(dir.path().join("a.txt"), b"old").unwrap();
            let db = open_db(&dir.path().join("t.sqlite")).unwrap();
            let fsys = LocalFs::new(dir.path());
            let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", None)]);
            let rep = execute(&db, &fsys, &p, &opts_at(dir.path()), crate::fault::parse_spec(spec).unwrap());
            (dir, db, p, rep)
        };
        let (dir, _db, p, err) = {
            let dir = tempfile::tempdir().unwrap();
            fs::create_dir(dir.path().join("bak")).unwrap();
            fs::write(dir.path().join("a.txt"), b"old").unwrap();
            let db = open_db(&dir.path().join("t.sqlite")).unwrap();
            let fsys = LocalFs::new(dir.path());
            let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", None)]);
            let err = execute(&db, &fsys, &p, &opts_at(dir.path()), crate::fault::parse_spec("fsync-dir:1").unwrap()).unwrap_err();
            (dir, db, p, err)
        };
        assert!(err.message.contains("FsyncDir:1"), "{}", err.message);
        assert!(dir.path().join("bak").join(&p.id).is_dir());
        assert!(!dir.path().join("bak").join(&p.id).join("files").exists());
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old");

        let (dir, _db, p, err) = {
            let dir = tempfile::tempdir().unwrap();
            fs::create_dir(dir.path().join("bak")).unwrap();
            fs::write(dir.path().join("a.txt"), b"old").unwrap();
            let db = open_db(&dir.path().join("t.sqlite")).unwrap();
            let fsys = LocalFs::new(dir.path());
            let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", None)]);
            let err = execute(&db, &fsys, &p, &opts_at(dir.path()), crate::fault::parse_spec("fsync-dir:2").unwrap()).unwrap_err();
            (dir, db, p, err)
        };
        assert!(err.message.contains("FsyncDir:2"), "{}", err.message);
        assert!(dir.path().join("bak").join(&p.id).join("files").is_dir());
        assert!(!dir.path().join("bak").join(&p.id).join("files").join("a.txt").exists());
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old");

        let (dir, _db, p, rep) = run("fsync-dir:3");
        let rep = rep.unwrap();
        assert_eq!(rep.state, "STOPPED");
        assert!(rep.files[0].error.as_deref().unwrap_or("").contains("FsyncDir:3"), "{:?}", rep.files[0].error);
        assert_eq!(fs::read(dir.path().join("bak").join(&p.id).join("files").join("a.txt")).unwrap(), b"old");
        assert!(!dir.path().join("bak").join(&p.id).join("manifest.json").exists());
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old");

        let (dir, _db, p, rep) = run("fsync-dir:4");
        let rep = rep.unwrap();
        assert_eq!(rep.state, "STOPPED");
        assert!(rep.files[0].error.as_deref().unwrap_or("").contains("FsyncDir:4"));
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old");
        let rows = read_manifest(&dir.path().join("bak").join(&p.id)).unwrap();
        let pointed = rows.iter().rev().find(|v| v.get("path").and_then(|p| p.as_str()) == Some("a.txt")).unwrap().get("backup").unwrap().as_str().unwrap();
        assert_eq!(pointed, "files/a.txt");

        let (dir, db, p, rep) = run("fsync-dir:5");
        let rep = rep.unwrap();
        assert_eq!(rep.state, "STOPPED");
        assert!(rep.files[0].error.as_deref().unwrap_or("").contains("FsyncDir:5"), "{:?}", rep.files[0].error);
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"NEW");
        let written: i64 = db
            .query_row("SELECT written FROM exec_ops WHERE exec_id=?1 AND op_index=1", params![p.id], |r| r.get(0))
            .unwrap();
        assert_eq!(written, 1);
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn rollback_dir_fsync_failure_still_records_the_file_restored() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", None)]);
        execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let rep = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, crate::fault::parse_spec("fsync-dir:1").unwrap()).unwrap();
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old");
        assert_eq!(rep.files[0].state, "rolled-back");
        assert_eq!(rep.files[0].code.as_deref(), Some("E_IO"));
        assert!(rep.files[0].error.as_deref().unwrap_or("").contains("FsyncDir:1"), "{:?}", rep.files[0].error);
        assert_ne!(rep.state, "ROLLBACK_PARTIAL");
        assert_eq!(rep.state, "ROLLED_BACK", "{:?}", rep);
        assert_eq!(rep.history_result, "已回滚");
        let rollback_state: String = db
            .query_row("SELECT rollback_state FROM exec_ops WHERE exec_id=?1 AND op_index=1", params![p.id], |r| r.get(0))
            .unwrap();
        assert_eq!(rollback_state, "done");
        let paths: String = db.query_row("SELECT paths_json FROM history WHERE exec_id=?1", params![p.id], |r| r.get(0)).unwrap();
        assert_eq!(paths, "[]");
    }

    fn move_op(from: &str, to: &str, bytes: &[u8]) -> PlanOp {
        PlanOp {
            index: 1,
            kind: OpKind::Move,
            path: to.into(),
            from: Some(from.into()),
            bytes: Some(bytes.to_vec()),
            expect: Some(Snapshot { size: bytes.len() as u64, mtime_ms: None, hash: sha256_hex(bytes) }),
            mode: Some(0o644),
            source_mtime_ms: None,
            merged: false,
            keep_source: false,
            readonly: false,
            label: "移动".into(),
        }
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn undo_move_dir_fsync_failure_leaves_one_copy_and_retry_does_not_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("old.ts"), b"body").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![move_op("old.ts", "new.ts", b"body")]);
        execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        assert!(!dir.path().join("old.ts").exists());
        assert_eq!(fs::read(dir.path().join("new.ts")).unwrap(), b"body");
        let rep = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, crate::fault::parse_spec("fsync-dir:1").unwrap()).unwrap();
        assert!(dir.path().join("old.ts").exists());
        assert!(!dir.path().join("new.ts").exists());
        assert_eq!(fs::read(dir.path().join("old.ts")).unwrap(), b"body");
        assert_eq!(rep.state, "ROLLBACK_PARTIAL", "{:?}", rep.files);
        assert_eq!(rep.files[0].state, "rollback-failed");
        assert_eq!(rep.files[0].code.as_deref(), Some("E_IO"));
        let rollback_state: String = db
            .query_row("SELECT rollback_state FROM exec_ops WHERE exec_id=?1 AND op_index=1", params![p.id], |r| r.get(0))
            .unwrap();
        assert_ne!(rollback_state, "done");
        let again = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(again.state, "ROLLED_BACK", "{:?}", again.files);
        assert!(dir.path().join("old.ts").exists());
        assert!(!dir.path().join("new.ts").exists());
        assert_eq!(fs::read(dir.path().join("old.ts")).unwrap(), b"body");
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn move_resume_after_dir_fsync_failure_removes_the_source_once() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("old.ts"), b"body").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![move_op("old.ts", "new.ts", b"body")]);
        let rep = execute(&db, &fsys, &p, &opts_at(dir.path()), crate::fault::parse_spec("fsync-dir:6").unwrap()).unwrap();
        assert_eq!(rep.state, "STOPPED");
        assert_eq!(rep.files[0].code.as_deref(), Some("E_IO"));
        assert!(rep.files[0].error.as_deref().unwrap_or("").contains("FsyncDir:6"), "{:?}", rep.files[0].error);
        assert_eq!(fs::read(dir.path().join("new.ts")).unwrap(), b"body");
        assert_eq!(fs::read(dir.path().join("old.ts")).unwrap(), b"body");
        let written: i64 = db
            .query_row("SELECT written FROM exec_ops WHERE exec_id=?1 AND op_index=1", params![p.id], |r| r.get(0))
            .unwrap();
        let removed: i64 = db
            .query_row("SELECT source_removed FROM exec_ops WHERE exec_id=?1 AND op_index=1", params![p.id], |r| r.get(0))
            .unwrap();
        assert_eq!(written, 1);
        assert_eq!(removed, 0);
        let continued = resume(&db, &fsys, &p.id, ResumeChoice::Continue, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let ResumeOutcome::Continued(done) = continued else { panic!("expected resume to finish the move: {continued:?}"); };
        assert_eq!(done.state, "COMMITTED", "{:?}", done.files);
        assert!(!dir.path().join("old.ts").exists());
        assert_eq!(fs::read(dir.path().join("new.ts")).unwrap(), b"body");
        let removed: i64 = db
            .query_row("SELECT source_removed FROM exec_ops WHERE exec_id=?1 AND op_index=1", params![p.id], |r| r.get(0))
            .unwrap();
        assert_eq!(removed, 1);
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn continue_rollback_keeps_later_edits_to_restored_files() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["f1.txt", "f2.txt"] {
            fs::write(dir.path().join(name), b"old").unwrap();
        }
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "f1.txt", b"old", b"new", None), ov(2, "f2.txt", b"old", b"new", None)]);
        execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let mut fp = FaultPlan::default();
        fp.items.push(crate::fault::Item::At { stage: Stage::Rollback, n: 2, mode: crate::fault::Mode::Error });
        let rep = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, fp).unwrap();
        assert_eq!(rep.state, "ROLLBACK_PARTIAL");
        fs::write(dir.path().join("f1.txt"), b"user-kept").unwrap();
        let again = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(again.state, "ROLLED_BACK", "{:?}", again.files);
        assert_eq!(fs::read(dir.path().join("f1.txt")).unwrap(), b"user-kept");
        assert_eq!(fs::read(dir.path().join("f2.txt")).unwrap(), b"old");

        let dir = tempfile::tempdir().unwrap();
        for name in ["f1.txt", "f2.txt"] {
            fs::write(dir.path().join(name), b"old").unwrap();
        }
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "f1.txt", b"old", b"new", None), ov(2, "f2.txt", b"old", b"new", None)]);
        execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let mut fp = FaultPlan::default();
        fp.items.push(crate::fault::Item::At { stage: Stage::Rollback, n: 2, mode: crate::fault::Mode::Error });
        rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, fp).unwrap();
        db.execute("UPDATE exec_ops SET rollback_state='done' WHERE exec_id=?1", params![p.id]).unwrap();
        fs::write(dir.path().join("f1.txt"), b"user-one").unwrap();
        fs::write(dir.path().join("f2.txt"), b"user-two").unwrap();
        let done = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(done.state, "ROLLED_BACK", "{:?}", done.files);
        assert_eq!(done.history_result, "已回滚");
        assert_eq!(fs::read(dir.path().join("f1.txt")).unwrap(), b"user-one");
        assert_eq!(fs::read(dir.path().join("f2.txt")).unwrap(), b"user-two");
    }

    #[test]
    fn undo_move_of_a_modified_target_restores_the_backup_bytes() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("old.ts"), b"body").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![move_op("old.ts", "new.ts", b"body")]);
        execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        fs::write(dir.path().join("new.ts"), b"user-edit").unwrap();
        let back = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(back.state, "ROLLED_BACK", "{:?}", back.files);
        assert_eq!(back.history_result, "已回滚");
        assert_eq!(fs::read(dir.path().join("old.ts")).unwrap(), b"body");
        assert!(!dir.path().join("new.ts").exists());
        assert_eq!(fs::read(dir.path().join("bak").join(&p.id).join("conflict-current").join("new.ts")).unwrap(), b"user-edit");
    }

    #[test]
    fn undo_move_delete_failure_is_unrestored_and_retry_removes_the_copy() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("old")).unwrap();
        fs::create_dir_all(dir.path().join("new")).unwrap();
        fs::write(dir.path().join("old/m.txt"), b"orig").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![move_op("old/m.txt", "new/m.txt", b"orig")]);
        let old = dir.path().join("old");
        let new_dir = dir.path().join("new");
        let _old_guard = DirMode { path: &old };
        let _new_guard = DirMode { path: &new_dir };
        let mut perm = fs::metadata(&old).unwrap().permissions();
        perm.set_mode(0o555);
        fs::set_permissions(&old, perm).unwrap();
        let rep = execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        assert_eq!(rep.state, "STOPPED");
        assert_eq!(fs::read(dir.path().join("old/m.txt")).unwrap(), b"orig");
        assert_eq!(fs::read(dir.path().join("new/m.txt")).unwrap(), b"orig");
        let mut perm = fs::metadata(&old).unwrap().permissions();
        perm.set_mode(0o755);
        fs::set_permissions(&old, perm).unwrap();
        let mut perm = fs::metadata(&new_dir).unwrap().permissions();
        perm.set_mode(0o555);
        fs::set_permissions(&new_dir, perm).unwrap();
        let back = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(back.state, "ROLLBACK_PARTIAL", "{:?}", back.files);
        assert_eq!(back.history_result, "部分回滚");
        let failed = back.files.iter().find(|f| f.path == "new/m.txt").unwrap();
        assert_eq!(failed.state, "rollback-failed");
        assert_eq!(failed.code.as_deref(), Some("E_PERMISSION"));
        assert_eq!(fs::read(dir.path().join("old/m.txt")).unwrap(), b"orig");
        assert_eq!(fs::read(dir.path().join("new/m.txt")).unwrap(), b"orig");
        let paths: String = db.query_row("SELECT paths_json FROM history WHERE exec_id=?1", params![p.id], |r| r.get(0)).unwrap();
        assert!(paths.contains("new/m.txt"), "{paths}");
        assert!(paths.contains("E_PERMISSION"), "{paths}");
        let mut perm = fs::metadata(&new_dir).unwrap().permissions();
        perm.set_mode(0o755);
        fs::set_permissions(&new_dir, perm).unwrap();
        let again = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(again.state, "ROLLED_BACK", "{:?}", again.files);
        assert!(!dir.path().join("new/m.txt").exists());
        assert_eq!(fs::read(dir.path().join("old/m.txt")).unwrap(), b"orig");
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn rollback_dir_fsync_failure_continues_with_the_remaining_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        fs::write(dir.path().join("b.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", None), ov(2, "b.txt", b"old", b"NEXT", None)]);
        execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let rep = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, crate::fault::parse_spec("fsync-dir:1").unwrap()).unwrap();
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old");
        assert_eq!(fs::read(dir.path().join("b.txt")).unwrap(), b"old");
        assert_eq!(rep.state, "ROLLED_BACK", "{:?}", rep.files);
        assert_ne!(rep.state, "ROLLBACK_PARTIAL");
        assert_eq!(rep.history_result, "已回滚");
        let b = rep.files.iter().find(|f| f.path == "b.txt").unwrap();
        assert_eq!(b.state, "rolled-back");
        assert_eq!(b.code.as_deref(), Some("E_IO"));
        let a = rep.files.iter().find(|f| f.path == "a.txt").unwrap();
        assert_eq!(a.state, "rolled-back");
        assert!(a.code.is_none(), "{:?}", a.code);
        let nv = |index: u32| -> i64 {
            db.query_row(
                "SELECT needs_verify FROM exec_ops WHERE exec_id=?1 AND op_index=?2",
                params![p.id, index],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(nv(2), 1);
        assert_eq!(nv(1), 0);
        let paths: String = db.query_row("SELECT paths_json FROM history WHERE exec_id=?1", params![p.id], |r| r.get(0)).unwrap();
        assert_eq!(paths, "[]");
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn restored_io_is_rechecked_on_rollback_retry_without_rewriting() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", None)]);
        execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, crate::fault::parse_spec("fsync-dir:1").unwrap()).unwrap();
        let listed = list_history(&db).unwrap();
        assert_eq!(listed[0].result, "已回滚");
        assert_eq!(journal_state(&db, &p.id), "ROLLED_BACK");
        let nv: i64 = db
            .query_row("SELECT needs_verify FROM exec_ops WHERE exec_id=?1 AND op_index=1", params![p.id], |r| r.get(0))
            .unwrap();
        assert_eq!(nv, 1);
        assert!(recover_on_startup(&db, &fsys).unwrap().is_empty());
        let nv: i64 = db
            .query_row("SELECT needs_verify FROM exec_ops WHERE exec_id=?1 AND op_index=1", params![p.id], |r| r.get(0))
            .unwrap();
        assert_eq!(nv, 1);

        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", None)]);
        execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, crate::fault::parse_spec("fsync-dir:1").unwrap()).unwrap();
        fs::write(dir.path().join("a.txt"), b"user-edit").unwrap();
        db.execute("UPDATE exec_journal SET state='ROLLING_BACK' WHERE exec_id=?1", params![p.id]).unwrap();
        let again = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(again.state, "ROLLBACK_PARTIAL", "{:?}", again.files);
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"user-edit");
        assert_eq!(journal_state(&db, &p.id), "ROLLBACK_PARTIAL");
        let listed = list_history(&db).unwrap();
        assert_eq!(listed[0].result, "部分回滚");
        assert!(listed[0].paths_json.contains("a.txt"), "{}", listed[0].paths_json);
        assert!(listed[0].paths_json.contains("E_IO"), "{}", listed[0].paths_json);
        let state: String = db
            .query_row("SELECT rollback_state FROM exec_ops WHERE exec_id=?1 AND op_index=1", params![p.id], |r| r.get(0))
            .unwrap();
        assert_eq!(state, "failed");
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn continue_reapplies_mtime_after_dir_fsync_failure() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", None)]);
        let rep = execute(&db, &fsys, &p, &opts_at(dir.path()), crate::fault::parse_spec("fsync-dir:6").unwrap()).unwrap();
        assert_eq!(rep.state, "STOPPED");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"NEW");
        let rel = RelPath::parse("a.txt").unwrap();
        assert_ne!(fsys.stat(&rel).unwrap().mtime_ms, Some(1_700_000_000_000));
        let continued = resume(&db, &fsys, &p.id, ResumeChoice::Continue, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let ResumeOutcome::Continued(done) = continued else { panic!("expected resume to finish: {continued:?}"); };
        assert_eq!(done.state, "COMMITTED", "{:?}", done.files);
        assert_eq!(fsys.stat(&rel).unwrap().mtime_ms, Some(1_700_000_000_000));
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn log_probe_fails_one_transaction_and_leaves_log_numbers() {
        let _probes = crate::fault::probe_section();
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let (a, b) = two_log_failed(&db, dir.path());
        let _plan = crate::fault::override_plan(crate::fault::parse_spec("log:probe:1").unwrap());
        let offers = recover_on_startup(&db, &fsys).unwrap();
        assert_eq!(offers.len(), 2, "{offers:?}");
        assert_eq!(offers[0].exec_id, a);
        assert_eq!(offers[0].code.as_deref(), Some("E_LOG_WRITE"));
        assert_eq!(offers[1].exec_id, b);
        assert!(offers[1].code.is_none(), "{:?}", offers[1].code);
        assert!(pre_recovery(&db, &a).is_none());
        assert_eq!(pre_recovery(&db, &b).as_deref(), Some("LOG_FAILED"));
        assert!(dir.path().join(".a.txt.htx-probe").exists());
        assert!(!dir.path().join(".b.txt.htx-probe").exists());
        assert_eq!(journal_state(&db, &a), "LOG_FAILED");
        assert_eq!(journal_state(&db, &b), "LOG_FAILED");

        drop(_plan);
        crate::fault::reset_probe_count();
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let (a, b) = two_log_failed(&db, dir.path());
        let _plan = crate::fault::override_plan(crate::fault::parse_spec("log:probe:2").unwrap());
        let offers = recover_on_startup(&db, &fsys).unwrap();
        assert_eq!(offers[0].exec_id, a);
        assert!(offers[0].code.is_none(), "{:?}", offers[0]);
        assert_eq!(offers[1].exec_id, b);
        assert_eq!(offers[1].code.as_deref(), Some("E_LOG_WRITE"));
        assert_eq!(pre_recovery(&db, &a).as_deref(), Some("LOG_FAILED"));
        assert!(pre_recovery(&db, &b).is_none());
        assert!(!dir.path().join(".a.txt.htx-probe").exists());
        assert!(dir.path().join(".b.txt.htx-probe").exists());
    }

    #[test]
    fn skip_leaves_a_modified_move_target_untouched() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("old.ts"), b"body").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![move_op("old.ts", "new.ts", b"body")]);
        execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        fs::write(dir.path().join("new.ts"), b"user-edit").unwrap();
        let skipped = rollback(&db, &fsys, &p.id, RollbackChoice::SkipConflicts, FaultPlan::default()).unwrap();
        assert_eq!(skipped.state, "ROLLBACK_PARTIAL", "{:?}", skipped.files);
        assert_eq!(fs::read(dir.path().join("new.ts")).unwrap(), b"user-edit");
        assert!(!dir.path().join("old.ts").exists());
    }

    #[test]
    fn modified_move_target_is_kept_when_current_backup_cannot_be_saved() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("old.ts"), b"body").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![move_op("old.ts", "new.ts", b"body")]);
        execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        fs::write(dir.path().join("new.ts"), b"user-edit").unwrap();
        fs::write(dir.path().join("bak").join(&p.id).join("conflict-current"), b"not-a-dir").unwrap();
        let back = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(back.state, "ROLLBACK_PARTIAL", "{:?}", back.files);
        assert_eq!(fs::read(dir.path().join("new.ts")).unwrap(), b"user-edit");
        assert!(!dir.path().join("old.ts").exists());
    }

    #[test]
    fn rb_21_recreated_source_is_a_rollback_conflict() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("old.ts"), b"body").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![move_op("old.ts", "new.ts", b"body")]);
        execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        fs::write(dir.path().join("old.ts"), b"other").unwrap();
        let hits = rollback_conflicts(&fsys, &db, &p.id, &p).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].path, "old.ts");
        assert_eq!(hits[0].detail, DETAIL_RECREATED);
        let skipped = rollback(&db, &fsys, &p.id, RollbackChoice::SkipConflicts, FaultPlan::default()).unwrap();
        assert_eq!(skipped.state, "ROLLBACK_PARTIAL", "{:?}", skipped.files);
        assert_eq!(fs::read(dir.path().join("old.ts")).unwrap(), b"other");
        assert_eq!(fs::read(dir.path().join("new.ts")).unwrap(), b"body");

        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("old.ts"), b"body").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![move_op("old.ts", "new.ts", b"body")]);
        execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        fs::write(dir.path().join("old.ts"), b"other").unwrap();
        let back = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(back.state, "ROLLED_BACK", "{:?}", back.files);
        assert_eq!(fs::read(dir.path().join("old.ts")).unwrap(), b"body");
        assert!(!dir.path().join("new.ts").exists());
        assert_eq!(fs::read(dir.path().join("bak").join(&p.id).join("conflict-current").join("old.ts")).unwrap(), b"other");

        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("old.ts"), b"body").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![move_op("old.ts", "new.ts", b"body")]);
        execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        fs::write(dir.path().join("old.ts"), b"other").unwrap();
        fs::write(dir.path().join("new.ts"), b"edited").unwrap();
        let hits = rollback_conflicts(&fsys, &db, &p.id, &p).unwrap();
        assert!(hits.iter().any(|h| h.path == "old.ts" && h.detail == DETAIL_RECREATED), "{hits:?}");
        assert!(hits.iter().any(|h| h.path == "new.ts" && h.detail == DETAIL_MODIFIED), "{hits:?}");
        let back = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(back.state, "ROLLED_BACK", "{:?}", back.files);
        assert_eq!(fs::read(dir.path().join("old.ts")).unwrap(), b"body");
        assert!(!dir.path().join("new.ts").exists());
        let saved = dir.path().join("bak").join(&p.id).join("conflict-current");
        assert_eq!(fs::read(saved.join("old.ts")).unwrap(), b"other");
        assert_eq!(fs::read(saved.join("new.ts")).unwrap(), b"edited");
    }

    #[test]
    fn done_move_with_the_other_side_still_present_is_not_finished() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("old.ts"), b"body").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![move_op("old.ts", "new.ts", b"body")]);
        execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        fs::copy(dir.path().join("new.ts"), dir.path().join("old.ts")).unwrap();
        db.execute("UPDATE exec_ops SET rollback_state='done' WHERE exec_id=?1", params![p.id]).unwrap();
        let back = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(back.state, "ROLLED_BACK", "{:?}", back.files);
        assert_eq!(fs::read(dir.path().join("old.ts")).unwrap(), b"body");
        assert!(!dir.path().join("new.ts").exists());
    }

    #[test]
    fn attribute_restore_failure_lists_the_path_and_stays_rolled_back() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", None)]);
        execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        crate::vfs::fail_set_meta_once();
        let back = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(back.state, "ROLLED_BACK", "{:?}", back.files);
        assert_eq!(back.files[0].error.as_deref(), Some(crate::error::ATTR_RESTORE_FAILED));
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old");
        let nv: i64 = db.query_row("SELECT needs_verify FROM exec_ops WHERE exec_id=?1 AND op_index=1", params![p.id], |r| r.get(0)).unwrap();
        let attr: i64 = db.query_row("SELECT attr_restore_failed FROM exec_ops WHERE exec_id=?1 AND op_index=1", params![p.id], |r| r.get(0)).unwrap();
        assert_eq!(nv, 0);
        assert_eq!(attr, 1);
        let listed = list_history(&db).unwrap();
        assert_eq!(listed[0].attr_failed_count, 1);
        assert_eq!(listed[0].attr_failed_paths, vec!["a.txt".to_string()]);
        assert_eq!(listed[0].result, "已回滚");
    }

    fn needs_verify_after_user_edit(dir: &Path) -> (Connection, LocalFs, String) {
        fs::write(dir.join("a.txt"), b"old").unwrap();
        let db = open_db(&dir.join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir);
        let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", None)]);
        execute(&db, &fsys, &p, &opts_at(dir), FaultPlan::default()).unwrap();
        rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        fs::write(dir.join("a.txt"), b"user-edit").unwrap();
        db.execute("UPDATE exec_ops SET needs_verify=1, rollback_state='done' WHERE exec_id=?1", params![p.id]).unwrap();
        (db, fsys, p.id)
    }

    #[test]
    fn rec_27_clean_quit_leaves_status_clean() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let data = dir.path().join("data");
        let started = begin_run(&db, &fsys, &data).unwrap();
        let RunStart::Started { lock, .. } = started else { panic!("expected the lock"); };
        lock.release_clean(&db).unwrap();
        let path = data.join("run.lock");
        assert!(path.is_file());
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("status=clean\n"), "{text}");
        assert!(text.contains("pid="), "{text}");
        assert!(text.contains("start="), "{text}");
    }

    #[test]
    fn rec_28_kill_leaves_running_and_next_start_reverifies() {
        let dir = tempfile::tempdir().unwrap();
        let (db, fsys, id) = needs_verify_after_user_edit(dir.path());
        let data = dir.path().join("data");
        let started = begin_run(&db, &fsys, &data).unwrap();
        let RunStart::Started { abnormal_exit, lock, .. } = started else { panic!("expected the lock"); };
        assert!(!abnormal_exit);
        let run = lock.run_id().to_string();
        db.execute(
            "UPDATE exec_ops SET needs_verify_run=?2 WHERE exec_id=?1 AND needs_verify=1",
            params![id, run],
        )
        .unwrap();
        drop(lock);
        let left = fs::read_to_string(data.join("run.lock")).unwrap();
        assert!(left.starts_with("status=running\n"), "{left}");
        let again = begin_run(&db, &fsys, &data).unwrap();
        let RunStart::Started { abnormal_exit, lock, .. } = again else { panic!("expected the next start"); };
        assert!(abnormal_exit);
        assert_eq!(journal_state(&db, &id), "ROLLBACK_PARTIAL");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"user-edit");
        lock.release_clean(&db).unwrap();
    }

    #[test]
    fn rec_29_lock_failure_exits_without_touching_run_lock() {
        let dir = tempfile::tempdir().unwrap();
        let (db, fsys, id) = needs_verify_after_user_edit(dir.path());
        let data = dir.path().join("data");
        fs::create_dir_all(&data).unwrap();
        let prior = b"status=running\nrun=held\npid=1\nstart=1\n";
        fs::write(data.join("run.lock"), prior).unwrap();
        let first = crate::runlock::acquire_run_lock(&data).unwrap();
        let crate::runlock::RunAcquire::Held(lock) = first else { panic!("first lock"); };
        let body = fs::read(data.join("run.lock")).unwrap();
        assert_eq!(body, prior);
        let second = crate::runlock::acquire_run_lock(&data).unwrap();
        assert!(matches!(second, crate::runlock::RunAcquire::AnotherInstance));
        assert_eq!(fs::read(data.join("run.lock")).unwrap(), body);
        let started = begin_run(&db, &fsys, &data).unwrap();
        assert!(matches!(started, RunStart::AnotherInstance));
        assert_eq!(fs::read(data.join("run.lock")).unwrap(), body);
        assert_eq!(journal_state(&db, &id), "ROLLED_BACK");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"user-edit");
        let nv: i64 = db.query_row("SELECT needs_verify FROM exec_ops WHERE exec_id=?1", params![id], |r| r.get(0)).unwrap();
        assert_eq!(nv, 1);
        drop(lock);
    }

    #[test]
    fn rec_30_running_status_rechecks_needs_verify() {
        let dir = tempfile::tempdir().unwrap();
        let (db, fsys, id) = needs_verify_after_user_edit(dir.path());
        let data = dir.path().join("data");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("run.lock"), b"status=running\npid=4\nstart=4\n").unwrap();
        let started = begin_run(&db, &fsys, &data).unwrap();
        let RunStart::Started { abnormal_exit, offers, lock } = started else { panic!("expected the lock"); };
        assert!(abnormal_exit);
        assert_eq!(journal_state(&db, &id), "ROLLBACK_PARTIAL");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"user-edit");
        assert_eq!(offers.len(), 1, "{offers:?}");
        assert_eq!(offers[0].actions, vec!["继续回滚".to_string(), "稍后处理".to_string()]);
        lock.release_clean(&db).unwrap();
    }

    #[test]
    fn rec_31_clean_empty_or_missing_lock_skips_reverify() {
        let check = |prior: Option<&[u8]>| {
            let dir = tempfile::tempdir().unwrap();
            let (db, fsys, id) = needs_verify_after_user_edit(dir.path());
            let data = dir.path().join("data");
            fs::create_dir_all(&data).unwrap();
            if let Some(bytes) = prior {
                fs::write(data.join("run.lock"), bytes).unwrap();
            }
            let started = begin_run(&db, &fsys, &data).unwrap();
            let RunStart::Started { abnormal_exit, offers, lock } = started else { panic!("expected the lock"); };
            assert!(!abnormal_exit);
            assert!(offers.is_empty(), "{offers:?}");
            assert_eq!(journal_state(&db, &id), "ROLLED_BACK");
            assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"user-edit");
            let listed = list_history(&db).unwrap();
            assert_eq!(listed[0].result, "已回滚");
            lock.release_clean(&db).unwrap();
            let nv: i64 = db.query_row("SELECT needs_verify FROM exec_ops WHERE exec_id=?1", params![id], |r| r.get(0)).unwrap();
            assert_eq!(nv, 0);
            let text = fs::read_to_string(data.join("run.lock")).unwrap();
            assert!(text.starts_with("status=clean\n"), "{text}");
            assert!(data.join("run.lock").is_file());
        };
        check(Some(b"status=clean\npid=1\nstart=1\n"));
        check(Some(b""));
        check(None);
    }

    #[test]
    fn abnormal_start_keeps_a_recreated_create_and_marks_it_unrestored() {
        let dir = tempfile::tempdir().unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![create_op(1, "made.txt", b"new")]);
        execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        let back = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(back.state, "ROLLED_BACK", "{:?}", back.files);
        assert!(!dir.path().join("made.txt").exists());
        fs::write(dir.path().join("made.txt"), b"came-back").unwrap();
        db.execute("UPDATE exec_ops SET needs_verify=1, rollback_state='done' WHERE exec_id=?1", params![p.id]).unwrap();
        let data = dir.path().join("data");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("run.lock"), b"status=running\npid=9\nstart=9\n").unwrap();
        let started = begin_run(&db, &fsys, &data).unwrap();
        let RunStart::Started { abnormal_exit, lock, .. } = started else { panic!("lock"); };
        assert!(abnormal_exit);
        assert_eq!(journal_state(&db, &p.id), "ROLLBACK_PARTIAL");
        assert_eq!(fs::read(dir.path().join("made.txt")).unwrap(), b"came-back");
        lock.release_clean(&db).unwrap();
    }

    fn keep_move_op() -> PlanOp {
        PlanOp {
            index: 1,
            kind: OpKind::Move,
            path: "new.ts".into(),
            from: Some("old.ts".into()),
            bytes: Some(b"body".to_vec()),
            expect: Some(Snapshot { size: 4, mtime_ms: None, hash: sha256_hex(b"body") }),
            mode: Some(0o644),
            source_mtime_ms: None,
            merged: false,
            keep_source: true,
            readonly: false,
            label: "移动（保留旧文件）".into(),
        }
    }

    #[cfg(feature = "fault-inject")]
    fn plain_move_op() -> PlanOp {
        let mut op = keep_move_op();
        op.keep_source = false;
        op.label = "移动".into();
        op
    }

    /// N7：核对完成前不覆盖 run.lock 里的上一次 run_id。
    #[test]
    fn n7_new_run_id_is_written_only_after_reverify() {
        let dir = tempfile::tempdir().unwrap();
        let (db, fsys, id) = needs_verify_after_user_edit(dir.path());
        db.execute(
            "UPDATE exec_ops SET needs_verify=1, needs_verify_run='stale-run' WHERE exec_id=?1",
            params![id],
        )
        .unwrap();
        let data = dir.path().join("data");
        fs::create_dir_all(&data).unwrap();
        let prior = b"status=running\nrun=stale-run\npid=8\nstart=8\n";
        fs::write(data.join("run.lock"), prior).unwrap();
        let held = crate::runlock::acquire_run_lock(&data).unwrap();
        let crate::runlock::RunAcquire::Held(lock) = held else { panic!("lock"); };
        assert_eq!(fs::read(data.join("run.lock")).unwrap(), prior);
        assert!(!String::from_utf8_lossy(&fs::read(data.join("run.lock")).unwrap()).contains(lock.run_id()));
        drop(lock);
        let started = begin_run(&db, &fsys, &data).unwrap();
        let RunStart::Started { lock, .. } = started else { panic!("expected the lock"); };
        let text = fs::read_to_string(data.join("run.lock")).unwrap();
        assert!(text.starts_with("status=running\n"), "{text}");
        assert!(text.contains(&format!("run={}\n", lock.run_id())), "{text}");
        assert!(!text.contains("stale-run"), "{text}");
        assert_eq!(journal_state(&db, &id), "ROLLBACK_PARTIAL");
        lock.release_clean(&db).unwrap();
    }

    /// R1 / REC-32 的干净退出一半：没带 run_id 的标记在写成 clean 之前被清掉，之后的崩溃不再核对。
    #[test]
    fn r1_clean_exit_clears_unstamped_mark_before_a_later_crash() {
        let dir = tempfile::tempdir().unwrap();
        let (db, fsys, id) = needs_verify_after_user_edit(dir.path());
        let data = dir.path().join("data");
        let started = begin_run(&db, &fsys, &data).unwrap();
        let RunStart::Started { lock, abnormal_exit, .. } = started else { panic!("expected the lock"); };
        assert!(!abnormal_exit);
        lock.release_clean(&db).unwrap();
        let nv: i64 = db.query_row("SELECT needs_verify FROM exec_ops WHERE exec_id=?1", params![id], |r| r.get(0)).unwrap();
        assert_eq!(nv, 0);
        assert!(fs::read_to_string(data.join("run.lock")).unwrap().starts_with("status=clean\n"));
        fs::write(data.join("run.lock"), b"status=running\nrun=later-crash\npid=1\nstart=1\n").unwrap();
        let again = begin_run(&db, &fsys, &data).unwrap();
        let RunStart::Started { lock, .. } = again else { panic!("expected the next start"); };
        assert_eq!(journal_state(&db, &id), "ROLLED_BACK");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"user-edit");
        lock.release_clean(&db).unwrap();
    }

    /// REC-32：只复核崩溃那次 run_id 的标记。干净退出不清别的运行留下的标记。
    #[test]
    fn rec_32_reverify_only_the_crashed_run_id() {
        let dir = tempfile::tempdir().unwrap();
        let (db, fsys, id) = needs_verify_after_user_edit(dir.path());
        db.execute(
            "UPDATE exec_ops SET needs_verify=1, needs_verify_run='stale-run' WHERE exec_id=?1",
            params![id],
        )
        .unwrap();
        let data = dir.path().join("data");
        let started = begin_run(&db, &fsys, &data).unwrap();
        let RunStart::Started { lock, .. } = started else { panic!("expected the lock"); };
        let this_run = lock.run_id().to_string();
        db.execute(
            "UPDATE exec_ops SET needs_verify=1, needs_verify_run=?2 WHERE exec_id=?1",
            params![id, this_run],
        )
        .unwrap();
        lock.release_clean(&db).unwrap();
        let nv: i64 = db.query_row("SELECT needs_verify FROM exec_ops WHERE exec_id=?1", params![id], |r| r.get(0)).unwrap();
        assert_eq!(nv, 0, "这次运行的标记要在写成 clean 之前清掉");
        assert!(fs::read_to_string(data.join("run.lock")).unwrap().starts_with("status=clean\n"));

        db.execute(
            "UPDATE exec_ops SET needs_verify=1, needs_verify_run='stale-run', rollback_state='done' WHERE exec_id=?1",
            params![id],
        )
        .unwrap();
        db.execute("UPDATE exec_journal SET state='ROLLED_BACK' WHERE exec_id=?1", params![id]).unwrap();
        let later = begin_run(&db, &fsys, &data).unwrap();
        let RunStart::Started { lock, abnormal_exit, .. } = later else { panic!("expected the lock"); };
        assert!(!abnormal_exit);
        lock.release_clean(&db).unwrap();
        let kept: String = db
            .query_row("SELECT needs_verify_run FROM exec_ops WHERE exec_id=?1", params![id], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, "stale-run");
        fs::write(data.join("run.lock"), b"status=running\nrun=other-run\npid=2\nstart=2\n").unwrap();
        let other = begin_run(&db, &fsys, &data).unwrap();
        let RunStart::Started { lock, abnormal_exit, .. } = other else { panic!("expected the lock"); };
        assert!(abnormal_exit);
        assert_eq!(journal_state(&db, &id), "ROLLED_BACK");
        let still: i64 = db.query_row("SELECT needs_verify FROM exec_ops WHERE exec_id=?1", params![id], |r| r.get(0)).unwrap();
        assert_eq!(still, 1);
        drop(lock);

        fs::write(data.join("run.lock"), b"status=running\nrun=stale-run\npid=3\nstart=3\n").unwrap();
        let crashed = begin_run(&db, &fsys, &data).unwrap();
        let RunStart::Started { lock, .. } = crashed else { panic!("expected the lock"); };
        assert_eq!(journal_state(&db, &id), "ROLLBACK_PARTIAL");
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"user-edit");
        lock.release_clean(&db).unwrap();
    }

    /// RB-23 / R2：保留旧文件的移动，原路径被改过也不是冲突。两种选择都只删新路径。
    #[test]
    fn rb_23_keep_source_edit_is_not_a_conflict() {
        for choice in [RollbackChoice::ProceedBackupCurrent, RollbackChoice::SkipConflicts] {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("old.ts"), b"body").unwrap();
            let db = open_db(&dir.path().join("t.sqlite")).unwrap();
            let fsys = LocalFs::new(dir.path());
            let p = plan(vec![keep_move_op()]);
            execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
            fs::write(dir.path().join("old.ts"), b"user-edit").unwrap();
            let conflicts = rollback_conflicts(&fsys, &db, &p.id, &p).unwrap();
            assert!(conflicts.iter().all(|c| c.path != "old.ts"), "{conflicts:?}");
            let back = rollback(&db, &fsys, &p.id, choice, FaultPlan::default()).unwrap();
            assert_eq!(back.state, "ROLLED_BACK", "{choice:?} {:?}", back.files);
            assert_eq!(fs::read(dir.path().join("old.ts")).unwrap(), b"user-edit");
            assert!(!dir.path().join("new.ts").exists());
            assert!(!dir.path().join("bak").join(&p.id).join("conflict-current").join("old.ts").exists());
            let again = rollback(&db, &fsys, &p.id, choice, FaultPlan::default()).unwrap_err();
            assert_eq!(again.code, ErrorCode::InvalidState);
            assert_eq!(fs::read(dir.path().join("old.ts")).unwrap(), b"user-edit");
            assert!(!dir.path().join("new.ts").exists());
        }
    }

    #[cfg(feature = "fault-inject")]
    #[test]
    fn r3_attr_failure_column_survives_retry() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        fs::write(dir.path().join("b.txt"), b"old").unwrap();
        let db = open_db(&dir.path().join("t.sqlite")).unwrap();
        let fsys = LocalFs::new(dir.path());
        let p = plan(vec![ov(1, "a.txt", b"old", b"NEW", None), ov(2, "b.txt", b"old", b"NEW", None)]);
        execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
        crate::vfs::fail_set_meta_once();
        let back = rollback(
            &db,
            &fsys,
            &p.id,
            RollbackChoice::ProceedBackupCurrent,
            crate::fault::parse_spec("rollback:2").unwrap(),
        )
        .unwrap();
        assert_eq!(back.state, "ROLLBACK_PARTIAL", "{:?}", back.files);
        let attr: i64 = db
            .query_row(
                "SELECT COALESCE(SUM(attr_restore_failed),0) FROM exec_ops WHERE exec_id=?1",
                params![p.id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(attr >= 1, "第一次回滚应记下属性恢复失败");
        let again = rollback(&db, &fsys, &p.id, RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(again.state, "ROLLED_BACK", "{:?}", again.files);
        let attr_after: i64 = db
            .query_row(
                "SELECT COALESCE(SUM(attr_restore_failed),0) FROM exec_ops WHERE exec_id=?1",
                params![p.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(attr_after, attr, "重试不能清掉已经记下的属性恢复失败");
    }

    #[cfg(feature = "fault-inject")]
    #[test]
    fn r4_rolled_back_move_never_keeps_both_copies() {
        for n in 1..=4 {
            for choice in [RollbackChoice::SkipConflicts, RollbackChoice::ProceedBackupCurrent] {
                let dir = tempfile::tempdir().unwrap();
                fs::write(dir.path().join("old.ts"), b"body").unwrap();
                let db = open_db(&dir.path().join("t.sqlite")).unwrap();
                let fsys = LocalFs::new(dir.path());
                let p = plan(vec![plain_move_op()]);
                execute(&db, &fsys, &p, &opts_at(dir.path()), FaultPlan::default()).unwrap();
                fs::write(dir.path().join("new.ts"), b"user-edit").unwrap();
                let spec = format!("fsync-dir:{n}");
                let first = rollback(
                    &db,
                    &fsys,
                    &p.id,
                    RollbackChoice::ProceedBackupCurrent,
                    crate::fault::parse_spec(&spec).unwrap(),
                )
                .unwrap();
                assert_move_not_both_when_rolled_back(&first.state, dir.path());
                if first.state != "ROLLED_BACK" {
                    let again = rollback(&db, &fsys, &p.id, choice, FaultPlan::default()).unwrap();
                    assert_move_not_both_when_rolled_back(&again.state, dir.path());
                }
            }
        }
    }

    #[cfg(feature = "fault-inject")]
    fn assert_move_not_both_when_rolled_back(state: &str, root: &Path) {
        if state == "ROLLED_BACK" {
            let both = root.join("old.ts").exists() && root.join("new.ts").exists();
            assert!(!both, "已回滚时 old.ts 和 new.ts 不能同时留下");
        }
    }
}
