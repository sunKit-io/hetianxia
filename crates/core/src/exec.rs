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
//! 日志写失败立刻停止，不自动回滚。回滚依据是已写入的日志加上备份目录里的 `manifest.json`。
//! 下次启动由 `pending()` 提示未完成的升级。
//!
//! 继续：对还没 done 的操作重新做变动检测（只认哈希，touch 不算改动）。
//! 回滚冲突三个选择：继续（先把当前版本另存再覆盖）、跳过冲突文件、取消。
//! 回滚单个文件失败不中断，记在 `rollback_error`，状态为 `ROLLBACK_PARTIAL`。
//!
//! 只读文件默认不覆盖，预演里以 E_PERMISSION 列出。勾选「覆盖只读文件」后
//! 先去掉只读（chmod，macOS 再 chflags nouchg），写完再恢复只读。
//! 删除开关关闭时，移动变成复制，界面文案是「移动（保留旧文件）」。
//! 直接覆盖保留升级包的 mtime；合并结果用写入时刻。

use crate::error::{Error, ErrorCode, Result};
use crate::fault::{FaultPlan, FaultRuntime, Stage};
use crate::hashutil::sha256_hex;
use crate::model::{ExecPlan, OpKind, PlanOp};
use crate::pathutil::RelPath;
use crate::vfs::{now_ms, LocalFs};
use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeHit {
    pub path: String,
    pub hash_changed: bool,
    pub detail: String,
}

pub fn open_db(path: &Path) -> Result<Connection> {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
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
    Ok(conn)
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

pub fn execute(db: &Connection, fsys: &LocalFs, plan: &ExecPlan, opts: &ExecOpts, fault: FaultPlan) -> Result<ExecReport> {
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
    fs::create_dir_all(backup.join("files"))?;
    let mut files = Vec::new();
    log_write(db, &mut fault, &plan.id, "prepared", || {
        db.execute(
            "INSERT INTO exec_journal (exec_id, state, project_key, plan_json, backup_root, started_at) VALUES (?1,?2,?3,?4,?5,?6)",
            params![plan.id, "PREPARED", plan.project_key, serde_json::to_string(plan).unwrap_or_default(), backup.display().to_string(), now_ms()],
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
        if needs_backup(op) {
            if let Err(e) = fault.hit_file(Stage::Backup, op.index) {
                return finish_stopped(db, plan, &backup, files, op, &real_code(e));
            }
            let rel = RelPath::parse(op.from.as_deref().unwrap_or(&op.path))?;
            if fsys.stat(&rel).is_ok() {
                let dest = backup.join("files").join(rel.as_str());
                if let Err(e) = fsys.copy_to(&rel, &dest) {
                    return finish_stopped(db, plan, &backup, files, op, &real_code(e));
                }
                if fault.backup_should_corrupt(op.index) {
                    if let Err(e) = flip_middle(&dest) {
                        return finish_stopped(db, plan, &backup, files, op, &real_code(e));
                    }
                }
                let expect = fsys.hash(&rel, true).unwrap_or_default();
                let got = sha256_hex(&fs::read(&dest).unwrap_or_default());
                if got != expect {
                    let err = Error::new(ErrorCode::VerifyFailed, format!("{} 的备份和原文件哈希不一致，原文件未改", rel.as_str()));
                    return finish_stopped(db, plan, &backup, files, op, &err);
                }
                let entry = json!({
                    "execId": plan.id,
                    "projectKey": plan.project_key,
                    "index": op.index,
                    "path": rel.as_str(),
                    "backup": format!("files/{}", rel.as_str()),
                });
                // 每备份一个文件就追加一行。中途崩溃时，前面已经 fsync 的行仍然可读。
                if let Err(e) = append_manifest(&backup, &entry) {
                    return finish_stopped(db, plan, &backup, files, op, &real_code(e));
                }
            }
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
        let res = apply_one(fsys, op, opts, &mut fault, &mut recorded_temps);
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
    for op in &plan.ops {
        if !will_apply(op, opts.allow_readonly) {
            continue;
        }
        if let Some(bytes) = &op.bytes {
            if let Err(e) = fault.hit_file(Stage::VerifyPost, op.index) {
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

fn apply_one(fsys: &LocalFs, op: &PlanOp, opts: &ExecOpts, fault: &mut FaultRuntime, temps: &mut Vec<RelPath>) -> Result<()> {
    if opts.remote {
        if let Err(e) = fault.hit_file(Stage::SftpDisconnect, op.index) {
            return Err(real_code(e));
        }
    }
    match op.kind {
        OpKind::Delete => {
            fault.hit_file(Stage::Rename, op.index).map_err(real_code)?;
            let rel = RelPath::parse(&op.path)?;
            let restore = op.readonly && opts.allow_readonly;
            if restore {
                fsys.set_readonly(&rel, false)?;
            }
            let removed = fsys.remove_path(&rel);
            if removed.is_err() && restore {
                fsys.set_readonly(&rel, true)?;
            }
            removed
        }
        OpKind::Create | OpKind::Overwrite | OpKind::Move => {
            let bytes = op.bytes.clone().unwrap_or_default();
            let dest = RelPath::parse(&op.path)?;
            let restore = op.readonly && opts.allow_readonly && op.kind == OpKind::Overwrite;
            if restore {
                fsys.set_readonly(&dest, false)?;
            }
            let wrote = write_one(fsys, op, opts, fault, temps, &dest, &bytes);
            if wrote.is_err() && restore {
                fsys.set_readonly(&dest, true)?;
            }
            wrote
        }
    }
}

fn write_one(fsys: &LocalFs, op: &PlanOp, opts: &ExecOpts, fault: &mut FaultRuntime, temps: &mut Vec<RelPath>, dest: &RelPath, bytes: &[u8]) -> Result<()> {
    fault.hit_file(Stage::Write, op.index).map_err(real_code)?;
    let tmp = fsys.write_temp(dest, bytes)?;
    temps.push(tmp.clone());
    #[cfg(any(test, feature = "fault-inject"))]
    if opts.corrupt_temp {
        let p = fsys.abs(&tmp)?;
        let mut data = fs::read(&p)?;
        if data.is_empty() { data.push(0); } else { data[0] ^= 0xff; }
        fs::write(&p, &data)?;
    }
    if let Err(e) = fault.hit_file(Stage::VerifyPre, op.index) {
        let _ = fsys.remove_path(&tmp);
        return Err(real_code(e));
    }
    let got = fsys.read_temp_hash(&tmp)?;
    let expect = sha256_hex(bytes);
    if got != expect {
        fsys.remove_path(&tmp)?;
        return Err(Error::new(ErrorCode::VerifyFailed, format!("{} 临时文件校验失败，原文件未改", op.path)));
    }
    if let Some(mode) = op.mode {
        if let Err(e) = fsys.set_meta(&tmp, Some(mode), None) {
            let _ = fsys.remove_path(&tmp);
            return Err(e);
        }
    }
    if let Err(e) = fault.hit_file(Stage::Rename, op.index) {
        let _ = fsys.remove_path(&tmp);
        return Err(real_code(e));
    }
    if let Err(e) = fsys.rename_over(&tmp, dest) {
        let _ = fsys.remove_path(&tmp);
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
    if op.kind == OpKind::Move && !op.keep_source {
        if let Some(from) = &op.from {
            let src = RelPath::parse(from)?;
            fsys.remove_path(&src)?;
        }
    }
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
fn append_manifest(backup: &Path, entry: &serde_json::Value) -> Result<()> {
    let path = backup.join("manifest.json");
    let mut f = fs::OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(f, "{entry}")?;
    f.sync_all()?;
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

fn verify_restored(fsys: &LocalFs, op: &PlanOp) -> Result<()> {
    match op.kind {
        OpKind::Create => {
            let rel = RelPath::parse(&op.path)?;
            if fsys.stat(&rel).is_ok() {
                return Err(Error::new(ErrorCode::Io, format!("{} 回滚后仍然存在", op.path)));
            }
            let _ = remove_empty_parents(fsys, &rel);
            Ok(())
        }
        OpKind::Overwrite | OpKind::Delete => {
            let Some(expect) = &op.expect else { return Ok(()) };
            let rel = RelPath::parse(&op.path)?;
            let hash = fsys.hash(&rel, true)?;
            if hash != expect.hash {
                return Err(Error::new(ErrorCode::Io, format!("{} 回滚后内容和升级前不一致", op.path)));
            }
            Ok(())
        }
        OpKind::Move if op.keep_source => {
            let rel = RelPath::parse(&op.path)?;
            if fsys.stat(&rel).is_ok() {
                return Err(Error::new(ErrorCode::Io, format!("{} 回滚后仍然存在", op.path)));
            }
            Ok(())
        }
        OpKind::Move => Ok(()),
    }
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
    let states = load_op_states(db, exec_id)?;
    let mut fault = FaultRuntime::new(fault);
    let mut files = Vec::new();
    let mut any_fail = false;
    // 只动已经写过（done）或写到一半（doing）的操作。未执行的 Create 不能删目标。
    for op in plan.ops.iter().rev() {
        let state = states.get(&op.index).map(|s| s.as_str()).unwrap_or("pending");
        if state != "done" && state != "doing" {
            continue;
        }
        if conflicts.iter().any(|c| c.path == op.path) && choice == RollbackChoice::SkipConflicts {
            files.push(fr(op, "skipped", None, Some("跳过冲突文件")));
            continue;
        }
        if conflicts.iter().any(|c| c.path == op.path) && choice == RollbackChoice::ProceedBackupCurrent {
            let rel = RelPath::parse(&op.path)?;
            if fsys.stat(&rel).is_ok() {
                let dest = backup.join("conflict-current").join(&op.path);
                if let Err(e) = fsys.copy_to(&rel, &dest) {
                    any_fail = true;
                    db.execute(
                        "UPDATE exec_ops SET rollback_state='failed', rollback_error=?3 WHERE exec_id=?1 AND op_index=?2",
                        params![exec_id, op.index, e.message],
                    )?;
                    files.push(fr(op, "rollback-failed", Some(e.code_str()), Some(&e.message)));
                    continue;
                }
            }
        }
        if let Err(e) = fault.hit_file(Stage::Rollback, op.index) {
            let e = real_code(e);
            any_fail = true;
            db.execute(
                "UPDATE exec_ops SET rollback_state='failed', rollback_error=?3 WHERE exec_id=?1 AND op_index=?2",
                params![exec_id, op.index, e.message],
            )?;
            files.push(fr(op, "rollback-failed", Some(e.code_str()), Some(&e.message)));
            continue;
        }
        match restore_one(fsys, &backup, op).and_then(|_| verify_restored(fsys, op)) {
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
        }
    }
    let state = if any_fail { "ROLLBACK_PARTIAL" } else { "ROLLED_BACK" };
    let history_result = if any_fail { "E_ROLLBACK_PARTIAL" } else { "已回滚" };
    db.execute("UPDATE exec_journal SET state=?2, finished_at=?3 WHERE exec_id=?1", params![exec_id, state, now_ms()])?;
    db.execute("UPDATE history SET result=?2 WHERE exec_id=?1", params![exec_id, history_result])?;
    Ok(base_report(exec_id, state, files, &backup, false, history_result, false))
}

fn load_op_states(db: &Connection, exec_id: &str) -> Result<HashMap<u32, String>> {
    let mut stmt = db.prepare("SELECT op_index, state FROM exec_ops WHERE exec_id=?1")?;
    let rows = stmt.query_map(params![exec_id], |r| Ok((r.get::<_, i64>(0)? as u32, r.get::<_, String>(1)?)))?;
    let mut map = HashMap::new();
    for row in rows {
        let (index, state) = row?;
        map.insert(index, state);
    }
    Ok(map)
}

fn restore_one(fsys: &LocalFs, backup: &Path, op: &PlanOp) -> Result<()> {
    match op.kind {
        OpKind::Create => {
            let rel = RelPath::parse(&op.path)?;
            if fsys.stat(&rel).is_ok() {
                fsys.remove_path(&rel)?;
            }
            Ok(())
        }
        OpKind::Overwrite | OpKind::Delete => {
            let rel = RelPath::parse(&op.path)?;
            let src = backup.join("files").join(rel.as_str());
            if !src.is_file() {
                return Err(Error::new(
                    ErrorCode::NotFound,
                    format!("备份里没有 {}，这个文件没有回滚", rel.as_str()),
                ));
            }
            let mode = op.mode;
            let mtime = op.expect.as_ref().and_then(|s| s.mtime_ms);
            fsys.copy_in_atomic(&src, &rel, mode, mtime)?;
            Ok(())
        }
        OpKind::Move => {
            let to = RelPath::parse(&op.path)?;
            if op.keep_source {
                if fsys.stat(&to).is_ok() {
                    fsys.remove_path(&to)?;
                }
            } else if let Some(from) = &op.from {
                let src_rel = RelPath::parse(from)?;
                if fsys.stat(&to).is_ok() {
                    // 移回
                    let data = fsys.read(&to)?;
                    let tmp = fsys.write_temp(&src_rel, &data)?;
                    fsys.rename_over(&tmp, &src_rel)?;
                    fsys.remove_path(&to)?;
                } else {
                    let src = backup.join("files").join(from);
                    if src.exists() {
                        fsys.copy_in_atomic(&src, &src_rel, op.mode, None)?;
                    }
                }
            }
            Ok(())
        }
    }
}

pub fn rollback_conflicts(fsys: &LocalFs, db: &Connection, exec_id: &str, plan: &ExecPlan) -> Result<Vec<ConflictFile>> {
    let states = load_op_states(db, exec_id)?;
    let mut out = Vec::new();
    for op in &plan.ops {
        let state = states.get(&op.index).map(|s| s.as_str()).unwrap_or("pending");
        if state != "done" && state != "doing" {
            continue;
        }
        if op.bytes.is_none() { continue; }
        let rel = RelPath::parse(&op.path)?;
        let Ok(hash) = fsys.hash(&rel, true) else { continue };
        let after: Option<String> = db
            .query_row(
                "SELECT after_hash FROM exec_ops WHERE exec_id=?1 AND op_index=?2",
                params![exec_id, op.index],
                |r| r.get(0),
            )
            .ok();
        let wrote = sha256_hex(op.bytes.as_deref().unwrap_or_default());
        let expect = after.unwrap_or(wrote);
        if hash != expect {
            out.push(ConflictFile {
                path: op.path.clone(),
                detail: "升级后又被改过".into(),
            });
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ResumeChoice {
    /// 什么都不改，下次启动再提示。
    Cancel,
    /// 已写文件被外部改过时：不碰这些文件，从失败处接着写，并让以后的回滚把它们当冲突。
    KeepExternal,
    /// 进入已有的三选一回滚流程。这里不改文件。
    Rollback,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ResumeOutcome {
    Unchanged,
    Continued(ExecReport),
    RollbackPrompt(Vec<ConflictFile>),
}

/// 用原来的事务和原来的备份继续。不新开事务，这样回滚仍覆盖前后两次写过的文件。
pub fn resume(db: &Connection, fsys: &LocalFs, exec_id: &str, choice: ResumeChoice, opts: &ExecOpts, fault: FaultPlan) -> Result<ResumeOutcome> {
    if choice == ResumeChoice::Cancel {
        return Ok(ResumeOutcome::Unchanged);
    }
    let (plan_json, backup): (String, String) = {
        let mut stmt = db.prepare("SELECT plan_json, backup_root FROM exec_journal WHERE exec_id=?1")?;
        stmt.query_row(params![exec_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
    };
    let plan: ExecPlan = serde_json::from_str(&plan_json).map_err(|e| Error::new(ErrorCode::Internal, e.to_string()))?;
    if plan.id != exec_id {
        return Err(Error::new(ErrorCode::Internal, "事务 id 和计划不一致"));
    }
    let backup = PathBuf::from(backup);
    let states = load_op_states(db, exec_id)?;
    let mut external = Vec::new();
    for op in &plan.ops {
        if states.get(&op.index).map(|s| s.as_str()) != Some("done") {
            continue;
        }
        let Some(bytes) = &op.bytes else { continue };
        let rel = RelPath::parse(&op.path)?;
        let Ok(hash) = fsys.hash(&rel, true) else { continue };
        let after: Option<String> = db
            .query_row(
                "SELECT after_hash FROM exec_ops WHERE exec_id=?1 AND op_index=?2",
                params![exec_id, op.index],
                |r| r.get(0),
            )
            .ok();
        let expect = after.unwrap_or_else(|| sha256_hex(bytes));
        if hash != expect {
            external.push(ConflictFile { path: op.path.clone(), detail: "升级后被改过".into() });
        }
    }
    if !external.is_empty() && choice == ResumeChoice::Rollback {
        return Ok(ResumeOutcome::RollbackPrompt(external));
    }
    if !external.is_empty() && choice == ResumeChoice::KeepExternal {
        for op in &plan.ops {
            if external.iter().any(|c| c.path == op.path) {
                db.execute(
                    "UPDATE exec_ops SET code='E_CHANGED_SINCE_SCAN', error='升级后被改过', rollback_state='conflict' WHERE exec_id=?1 AND op_index=?2",
                    params![exec_id, op.index],
                )?;
            }
        }
    }
    if choice == ResumeChoice::Rollback {
        let conflicts = rollback_conflicts(fsys, db, exec_id, &plan)?;
        return Ok(ResumeOutcome::RollbackPrompt(conflicts));
    }
    let report = continue_same_transaction(db, fsys, &plan, &backup, opts, fault)?;
    Ok(ResumeOutcome::Continued(report))
}

fn continue_same_transaction(db: &Connection, fsys: &LocalFs, plan: &ExecPlan, backup: &Path, opts: &ExecOpts, fault: FaultPlan) -> Result<ExecReport> {
    let mut fault = FaultRuntime::new(fault);
    let states = load_op_states(db, &plan.id)?;
    let remaining: Vec<PlanOp> = plan
        .ops
        .iter()
        .filter(|op| {
            let st = states.get(&op.index).map(|s| s.as_str()).unwrap_or("pending");
            st != "done" && st != "skipped"
        })
        .cloned()
        .collect();
    let _hits = detect_changes(fsys, &remaining)?;
    let mut files = Vec::new();
    for op in &plan.ops {
        let st = states.get(&op.index).map(|s| s.as_str()).unwrap_or("pending");
        if st == "done" {
            files.push(fr(op, "done", None, None));
        } else if st == "skipped" {
            files.push(fr(op, "skipped", Some("E_PERMISSION"), Some("跳过（只读）")));
        }
    }
    let mut temps = Vec::new();
    for op in &plan.ops {
        let st = states.get(&op.index).map(|s| s.as_str()).unwrap_or("pending");
        if st == "done" || st == "skipped" {
            continue;
        }
        if !will_apply(op, opts.allow_readonly) {
            set_op(db, &plan.id, op.index, "skipped", Some("只读文件未勾选覆盖"), Some("E_PERMISSION"))?;
            files.push(fr(op, "skipped", Some("E_PERMISSION"), Some("跳过（只读）")));
            continue;
        }
        let doing = format!("doing:{}", op.index);
        log_write(db, &mut fault, &plan.id, &doing, || set_op(db, &plan.id, op.index, "doing", None, None))?;
        match apply_one(fsys, op, opts, &mut fault, &mut temps) {
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
            }
            Err(e) if e.code == ErrorCode::LogWrite => return Err(e),
            Err(e) => return finish_stopped(db, plan, backup, files, op, &real_code(e)),
        }
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
            if let Err(e) = fault.hit_file(Stage::VerifyPost, op.index) {
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
        let mut fp = FaultPlan::default();
        fp.items.push(crate::fault::Item::At { stage: Stage::Log, n: 4, mode: crate::fault::Mode::Error });
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
        assert_eq!(skipped.state, "ROLLED_BACK");
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
        let ResumeOutcome::RollbackPrompt(files) = prompted else { panic!("expected rollback prompt"); };
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "a.txt");
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
        assert_eq!(skipped.state, "ROLLED_BACK");
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
}
