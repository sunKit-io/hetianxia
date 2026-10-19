//! 故障注入。正式包不包含解析器和标记常量。
//! 具体语法只在打开 fault-inject 时编译，避免阶段名进入发布产物。

use crate::error::Result;
#[cfg(feature = "fault-inject")]
use crate::error::{Error, ErrorCode};
use crate::model::OpKind;
#[cfg(feature = "fault-inject")]
use std::sync::atomic::{AtomicU32, Ordering};
#[cfg(all(test, feature = "fault-inject"))]
use std::cell::{Cell, RefCell};
#[cfg(all(test, feature = "fault-inject"))]
use std::sync::{Mutex, MutexGuard};

#[cfg(feature = "fault-inject")]
#[path = "fault_marker.rs"]
mod fault_marker;

#[cfg(feature = "fault-inject")]
pub const MARKER: &str = "UPG_FAULT_MARKER_HETIANXIA";

#[cfg(feature = "fault-inject")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Backup,
    Write,
    VerifyPre,
    VerifyPost,
    Rename,
    Rollback,
    Log,
    SftpDisconnect,
    FsyncDir,
}

#[cfg(feature = "fault-inject")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Error,
    Panic,
    Drop,
    /// 只适用于 `backup`：备份写完并 fsync 之后、校验之前，把备份文件中间一字节翻掉。
    Corrupt,
}

#[cfg(feature = "fault-inject")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    At { stage: Stage, n: u32, mode: Mode },
    LogBegin { mode: Mode },
    LogCommit { mode: Mode },
    /// `log:probe` 或 `log:probe:N`。不占用 `log:N` 计数。`n == None` 表示每次试写都失败。
    LogProbe { n: Option<u32> },
}

#[cfg(feature = "fault-inject")]
#[derive(Debug, Clone, Default)]
pub struct FaultPlan {
    pub items: Vec<Item>,
}

#[cfg(feature = "fault-inject")]
impl FaultPlan {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// 正式包没有故障项。阶段名不进入这个类型。
#[cfg(not(feature = "fault-inject"))]
#[derive(Debug, Clone, Default)]
pub struct FaultPlan;

#[cfg(not(feature = "fault-inject"))]
impl FaultPlan {
    pub fn is_empty(&self) -> bool {
        true
    }
}

#[derive(Debug, Default)]
pub struct FaultRuntime {
    #[cfg_attr(not(feature = "fault-inject"), allow(dead_code))]
    plan: FaultPlan,
    log_writes: u32,
    #[cfg_attr(not(feature = "fault-inject"), allow(dead_code))]
    dir_fsyncs: u32,
}

#[cfg(not(feature = "fault-inject"))]
pub fn load_for_plan(_spec: &str, _ops: &[OpKind]) -> Result<FaultPlan> {
    Ok(FaultPlan::default())
}

#[cfg(not(feature = "fault-inject"))]
pub fn load_from_env_or_exit(_ops: &[OpKind]) -> FaultPlan {
    FaultPlan::default()
}

/// 正式包里这个函数是空的。故障注入构建在进程启动时调用，语法错误直接退出。
pub fn check_env_syntax_or_exit() {
    #[cfg(feature = "fault-inject")]
    {
        fault_marker::touch();
        let Ok(spec) = std::env::var("UPG_FAULT") else { return };
        if spec.trim().is_empty() {
            return;
        }
        if let Err(e) = parse_spec(spec.trim()) {
            eprintln!("UPG_FAULT: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(feature = "fault-inject")]
pub fn load_from_env_or_exit(ops: &[OpKind]) -> FaultPlan {
    match std::env::var("UPG_FAULT") {
        Err(_) => FaultPlan::default(),
        Ok(s) if s.trim().is_empty() => FaultPlan::default(),
        Ok(s) => match load_for_plan(s.trim(), ops) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("UPG_FAULT: {} ({})", e.message, e.code_str());
                std::process::exit(1);
            }
        },
    }
}

#[cfg(not(feature = "fault-inject"))]
pub fn load_for_plan_len(_spec: &str, _plan_len: usize) -> std::result::Result<FaultPlan, String> {
    Ok(FaultPlan::default())
}

#[cfg(feature = "fault-inject")]
pub fn parse_spec(spec: &str) -> std::result::Result<FaultPlan, String> {
    let mut items = Vec::new();
    if spec.trim().is_empty() {
        return Ok(FaultPlan { items });
    }
    for raw in spec.split(',') {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("空的故障项".into());
        }
        items.push(parse_item(raw)?);
    }
    Ok(FaultPlan { items })
}

#[cfg(feature = "fault-inject")]
fn parse_item(raw: &str) -> std::result::Result<Item, String> {
    let parts: Vec<&str> = raw.split(':').collect();
    if parts.is_empty() || parts.iter().any(|p| p.is_empty()) {
        return Err(format!("无法解析「{raw}」"));
    }
    if parts[0] != "log" && parts[0] != "backup" && parts[0] != "write"
        && parts[0] != "verify-pre" && parts[0] != "verify-post"
        && parts[0] != "rename" && parts[0] != "rollback"
        && parts[0] != "sftp-disconnect"
        && parts[0] != "fsync-dir"
    {
        return Err(format!("未知阶段「{}」（来自「{raw}」）。没有单独的 verify，请用 verify-pre 或 verify-post", parts[0]));
    }
    if parts[0] == "log" && parts.len() >= 2 && parts[1] == "probe" {
        if parts.len() == 2 {
            return Ok(Item::LogProbe { n: None });
        }
        if parts.len() == 3 && parts[2].chars().all(|c| c.is_ascii_digit()) {
            let n: u32 = parts[2].parse().map_err(|_| format!("「{raw}」：log:probe 的序号不是正整数"))?;
            if n == 0 {
                return Err(format!("「{raw}」的序号不能为 0（从 1 开始）"));
            }
            return Ok(Item::LogProbe { n: Some(n) });
        }
        return Err(format!("「{raw}」：log:probe 不接受模式"));
    }
    if parts[0] == "fsync-dir" {
        if parts.len() != 2 && parts.len() != 3 {
            return Err(format!("无法解析「{raw}」，应为 fsync-dir:N 或 fsync-dir:N:mode"));
        }
        let n: u32 = parts[1].parse().map_err(|_| format!("「{raw}」的序号不是正整数"))?;
        if n == 0 {
            return Err(format!("「{raw}」的序号不能为 0（从 1 开始）"));
        }
        let mode = if parts.len() == 2 {
            Mode::Error
        } else {
            parse_mode(parts[2], raw, Stage::FsyncDir)?
        };
        if mode != Mode::Error && mode != Mode::Panic {
            return Err(format!("「{raw}」：fsync-dir 只有 error 和 panic"));
        }
        return Ok(Item::At { stage: Stage::FsyncDir, n, mode });
    }
    if parts[0] == "log" && parts.len() >= 2 && (parts[1] == "begin" || parts[1] == "commit") {
        let which = parts[1];
        let mode = if parts.len() == 2 {
            Mode::Error
        } else if parts.len() == 3 {
            parse_mode(parts[2], raw, Stage::Log)?
        } else {
            return Err(format!("无法解析「{raw}」"));
        };
        return Ok(if which == "begin" {
            Item::LogBegin { mode }
        } else {
            Item::LogCommit { mode }
        });
    }
    if parts.len() != 2 && parts.len() != 3 {
        return Err(format!("无法解析「{raw}」，应为 stage:N 或 stage:N:mode"));
    }
    let stage = match parts[0] {
        "backup" => Stage::Backup,
        "write" => Stage::Write,
        "verify-pre" => Stage::VerifyPre,
        "verify-post" => Stage::VerifyPost,
        "rename" => Stage::Rename,
        "rollback" => Stage::Rollback,
        "log" => Stage::Log,
        "sftp-disconnect" => Stage::SftpDisconnect,
        other => return Err(format!("未知阶段「{other}」（来自「{raw}」）")),
    };
    let n: u32 = parts[1].parse().map_err(|_| format!("「{raw}」的序号不是正整数"))?;
    if n == 0 {
        return Err(format!("「{raw}」的序号不能为 0（从 1 开始）"));
    }
    let mode = if parts.len() == 2 {
        if stage == Stage::SftpDisconnect { Mode::Drop } else { Mode::Error }
    } else {
        parse_mode(parts[2], raw, stage)?
    };
    if stage == Stage::SftpDisconnect && mode != Mode::Drop {
        return Err(format!("「{raw}」：sftp-disconnect 只有 drop 模式"));
    }
    if stage != Stage::SftpDisconnect && mode == Mode::Drop {
        return Err(format!("「{raw}」：drop 只适用于 sftp-disconnect"));
    }
    Ok(Item::At { stage, n, mode })
}

#[cfg(feature = "fault-inject")]
fn parse_mode(s: &str, raw: &str, stage: Stage) -> std::result::Result<Mode, String> {
    let disconnect = stage == Stage::SftpDisconnect;
    match s {
        "error" if !disconnect => Ok(Mode::Error),
        "panic" if !disconnect => Ok(Mode::Panic),
        "drop" if disconnect => Ok(Mode::Drop),
        "corrupt" if stage == Stage::Backup => Ok(Mode::Corrupt),
        "corrupt" => Err(format!("「{raw}」：corrupt 只适用于 backup")),
        "error" | "panic" if disconnect => Err(format!("「{raw}」：sftp-disconnect 只有 drop 模式")),
        other => Err(format!(
            "「{raw}」的模式「{other}」不认识，可用 error、panic{}",
            if disconnect { "、drop" } else if stage == Stage::Backup { "、corrupt" } else { "" }
        )),
    }
}

#[cfg(feature = "fault-inject")]
pub fn load_for_plan(spec: &str, ops: &[OpKind]) -> Result<FaultPlan> {
    let plan = parse_spec(spec).map_err(|m| Error::new(ErrorCode::Internal, m))?;
    validate(&plan, ops)?;
    Ok(plan)
}

#[cfg(feature = "fault-inject")]
fn validate(plan: &FaultPlan, ops: &[OpKind]) -> Result<()> {
    let len = ops.len() as u32;
    for item in &plan.items {
        match item {
            Item::LogBegin { .. } | Item::LogCommit { .. } | Item::LogProbe { .. } => {}
            Item::At { stage: Stage::FsyncDir, .. } => {}
            Item::At { stage: Stage::Log, n, .. } => {
                // 日志写入次数 = prepared + backed_up + 每个文件 doing/done + verifying + committed。
                // 大于这个上限的 log:N 无法命中，按非法处理，避免静默忽略。
                let max_logs = 4 + 2 * len;
                if *n > max_logs {
                    return Err(Error::new(
                        ErrorCode::Internal,
                        format!("log:{n} 超出本次执行最多 {max_logs} 次日志写入"),
                    ));
                }
            }
            Item::At { stage, n, .. } => {
                if *n > len {
                    return Err(Error::new(
                        ErrorCode::Internal,
                        format!("{stage:?}:{n} 大于计划长度 {len}（来自序号 {n}）"),
                    ));
                }
                let kind = ops[(*n as usize) - 1];
                if !stage_applies(*stage, kind) {
                    return Err(Error::new(
                        ErrorCode::Internal,
                        format!("{:?} 不适用于第 {n} 个操作（{:?}）", stage, kind),
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg(feature = "fault-inject")]
fn stage_applies(stage: Stage, kind: OpKind) -> bool {
    match stage {
        Stage::Backup => matches!(kind, OpKind::Overwrite | OpKind::Delete | OpKind::Move),
        Stage::Write | Stage::VerifyPre | Stage::Rename => {
            matches!(kind, OpKind::Create | OpKind::Overwrite | OpKind::Move)
        }
        Stage::VerifyPost | Stage::Rollback => true,
        Stage::SftpDisconnect => matches!(kind, OpKind::Create | OpKind::Overwrite | OpKind::Move),
        Stage::Log | Stage::FsyncDir => true,
    }
}

impl FaultRuntime {
    pub fn new(plan: FaultPlan) -> Self {
        Self { plan, log_writes: 0, dir_fsyncs: 0 }
    }

    #[cfg(not(feature = "fault-inject"))]
    pub fn hit_file(&mut self, _index: u32) -> Result<()> {
        Ok(())
    }

    #[cfg(not(feature = "fault-inject"))]
    pub fn hit_log(&mut self) -> Result<()> {
        self.log_writes += 1;
        Ok(())
    }

    #[cfg(not(feature = "fault-inject"))]
    pub fn hit_commit(&self) -> Result<()> {
        Ok(())
    }

    /// 正式包里什么都不做。故障注入构建在真正 fsync 父目录之前调用。
    #[cfg(not(feature = "fault-inject"))]
    pub fn hit_fsync_dir(&mut self) -> Result<()> {
        Ok(())
    }

    /// 正式包恒为 false。故障注入构建在备份落盘之后、校验之前决定要不要改坏备份。
    #[cfg(not(feature = "fault-inject"))]
    pub fn backup_should_corrupt(&self, _index: u32) -> bool {
        false
    }

    #[cfg(feature = "fault-inject")]
    pub fn backup_should_corrupt(&self, index: u32) -> bool {
        self.plan.items.iter().any(|item| {
            matches!(item, Item::At { stage: Stage::Backup, n, mode: Mode::Corrupt } if *n == index)
        })
    }

    #[cfg(feature = "fault-inject")]
    pub fn hit_file(&mut self, stage: Stage, index: u32) -> Result<()> {
        for item in &self.plan.items {
            if let Item::At { stage: s, n, mode } = item {
                if *s == stage && *n == index && *mode != Mode::Corrupt {
                    return fire(stage, index, *mode);
                }
            }
        }
        Ok(())
    }

    #[cfg(feature = "fault-inject")]
    pub fn hit_log(&mut self) -> Result<()> {
        self.log_writes += 1;
        let n = self.log_writes;
        for item in &self.plan.items {
            match item {
                Item::At { stage: Stage::Log, n: want, mode } if *want == n => {
                    return fire(Stage::Log, n, *mode);
                }
                Item::LogBegin { mode } if n == 1 => {
                    return fire(Stage::Log, 1, *mode);
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// 本次运行或本次继续里的下一次父目录 fsync。`panic` 在 fsync 之前中止。
    #[cfg(feature = "fault-inject")]
    pub fn hit_fsync_dir(&mut self) -> Result<()> {
        self.dir_fsyncs += 1;
        let n = self.dir_fsyncs;
        for item in &self.plan.items {
            if let Item::At { stage: Stage::FsyncDir, n: want, mode } = item {
                if *want == n {
                    return fire(Stage::FsyncDir, n, *mode);
                }
            }
        }
        Ok(())
    }

    #[cfg(feature = "fault-inject")]
    pub fn hit_commit(&self) -> Result<()> {
        for item in &self.plan.items {
            if let Item::LogCommit { mode } = item {
                return fire(Stage::Log, 0, *mode);
            }
        }
        Ok(())
    }
}

#[cfg(feature = "fault-inject")]
fn real_fault_code(stage: Stage) -> ErrorCode {
    match stage {
        Stage::VerifyPre | Stage::VerifyPost => ErrorCode::VerifyFailed,
        // backup:N:error 模拟读写失败。backup:N:corrupt 不走这里，校验失败时报 E_VERIFY_FAILED。
        Stage::Backup | Stage::Write | Stage::Rename | Stage::FsyncDir => ErrorCode::Io,
        Stage::Log => ErrorCode::LogWrite,
        // 单个文件的回滚失败报真实读写码。事务结果才是 ROLLBACK_PARTIAL。
        Stage::Rollback => ErrorCode::Io,
        Stage::SftpDisconnect => ErrorCode::Net,
    }
}

/// 进程内第几次 LOG_FAILED 试写。不计入 `hit_log` 的 `log:N`。
#[cfg(feature = "fault-inject")]
static PROBE_COUNT: AtomicU32 = AtomicU32::new(0);

#[cfg(all(test, feature = "fault-inject"))]
static PROBE_MU: Mutex<()> = Mutex::new(());

#[cfg(all(test, feature = "fault-inject"))]
thread_local! {
    static PROBE_HELD: Cell<bool> = const { Cell::new(false) };
    static PLAN_OVERRIDE: RefCell<Option<FaultPlan>> = const { RefCell::new(None) };
}

/// 挡住其他测试里的试写，并把进程内计数清零。本线程里的试写不会再次加锁。
#[cfg(all(test, feature = "fault-inject"))]
pub struct ProbeSection {
    _guard: MutexGuard<'static, ()>,
}

#[cfg(all(test, feature = "fault-inject"))]
impl Drop for ProbeSection {
    fn drop(&mut self) {
        PROBE_HELD.with(|held| held.set(false));
    }
}

#[cfg(all(test, feature = "fault-inject"))]
pub fn probe_section() -> ProbeSection {
    let guard = PROBE_MU.lock().unwrap_or_else(|err| err.into_inner());
    PROBE_HELD.with(|held| held.set(true));
    PROBE_COUNT.store(0, Ordering::SeqCst);
    ProbeSection { _guard: guard }
}

#[cfg(all(test, feature = "fault-inject"))]
pub fn reset_probe_count() {
    PROBE_COUNT.store(0, Ordering::SeqCst);
}

#[cfg(all(test, feature = "fault-inject"))]
pub struct PlanOverride;

#[cfg(all(test, feature = "fault-inject"))]
impl Drop for PlanOverride {
    fn drop(&mut self) {
        PLAN_OVERRIDE.with(|slot| *slot.borrow_mut() = None);
    }
}

#[cfg(all(test, feature = "fault-inject"))]
pub fn override_plan(plan: FaultPlan) -> PlanOverride {
    PLAN_OVERRIDE.with(|slot| *slot.borrow_mut() = Some(plan));
    PlanOverride
}

#[cfg(feature = "fault-inject")]
pub fn active_plan() -> FaultPlan {
    #[cfg(test)]
    if let Some(plan) = PLAN_OVERRIDE.with(|slot| slot.borrow().clone()) {
        return plan;
    }
    match std::env::var("UPG_FAULT") {
        Ok(spec) if !spec.trim().is_empty() => parse_spec(spec.trim()).unwrap_or_default(),
        _ => FaultPlan::default(),
    }
}

/// 只让 LOG_FAILED 的试写失败。其他日志和数据库写入不看这一项。
#[cfg(feature = "fault-inject")]
pub fn check_probe(plan: &FaultPlan) -> Result<()> {
    #[cfg(test)]
    let _guard = if PROBE_HELD.with(|held| held.get()) {
        None
    } else {
        Some(PROBE_MU.lock().unwrap_or_else(|err| err.into_inner()))
    };
    let n = PROBE_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
    for item in &plan.items {
        if let Item::LogProbe { n: want } = item {
            let hit = match want {
                None => true,
                Some(k) => *k == n,
            };
            if hit {
                return Err(Error::new(ErrorCode::LogWrite, format!("试写日志失败（log:probe {n}）")));
            }
        }
    }
    Ok(())
}

#[cfg(feature = "fault-inject")]
fn fire(stage: Stage, n: u32, mode: Mode) -> Result<()> {
    let label = format!("{stage:?}:{n}");
    match mode {
        Mode::Drop => Err(Error::new(ErrorCode::Net, format!("injected disconnect at {label}"))),
        Mode::Corrupt => Ok(()),
        Mode::Error => Err(Error::new(real_fault_code(stage), format!("injected {label}"))),
        // 立刻中止进程。不返回错误码，不写日志，不做清理。下次启动走崩溃恢复。
        Mode::Panic => std::process::abort(),
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "fault-inject")]
    use super::*;

    #[test]
    #[cfg(not(feature = "fault-inject"))]
    fn feature_off_has_no_marker_constant() {
        // 这个测试只在未开 feature 时编译。正式构建同样不包含解析代码。
        assert!(option_env!("CARGO_FEATURE_FAULT_INJECT").is_none());
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn parses_the_documented_example() {
        let spec = "backup:3,write:10:panic,verify-pre:5";
        let plan = parse_spec(spec).unwrap();
        assert_eq!(plan.items.len(), 3);
        assert!(matches!(plan.items[1], Item::At { stage: Stage::Write, n: 10, mode: Mode::Panic }));
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn rejects_plain_verify_zero_and_bad_mode() {
        assert!(parse_spec("verify:1").unwrap_err().contains("verify"));
        assert!(parse_spec("backup:0").unwrap_err().contains("0"));
        assert!(parse_spec("write:1:drop").unwrap_err().contains("drop"));
        assert!(parse_spec("sftp-disconnect:1:panic").unwrap_err().contains("drop"));
        assert!(parse_spec("sftp-disconnect:1:error").unwrap_err().contains("drop"));
        assert!(parse_spec("write:2:corrupt").unwrap_err().contains("corrupt"));
        assert!(parse_spec("nope:1").unwrap_err().contains("未知阶段"));
        let corrupt = parse_spec("backup:1:corrupt").unwrap();
        assert!(matches!(corrupt.items[0], Item::At { stage: Stage::Backup, n: 1, mode: Mode::Corrupt }));
        assert!(matches!(parse_spec("sftp-disconnect:2").unwrap().items[0], Item::At { mode: Mode::Drop, .. }));
    }

    #[test]
    #[cfg(feature = "fault-inject")]
    fn log_begin_and_commit_and_range() {
        let spec = parse_spec("log:begin,log:commit,log:begin:panic").unwrap();
        assert!(matches!(spec.items[0], Item::LogBegin { mode: Mode::Error }));
        assert!(matches!(spec.items[1], Item::LogCommit { .. }));
        let ops = vec![OpKind::Create, OpKind::Overwrite];
        let err = load_for_plan("backup:3", &ops).unwrap_err();
        assert!(err.message.contains("大于计划长度"));
        assert_ne!(err.code_str(), "E_FAULT");
        assert_eq!(err.code, ErrorCode::Internal);
        assert!(load_for_plan("backup:2", &ops).is_ok());
        assert!(parse_spec("log:probe").unwrap().items.iter().any(|item| matches!(item, Item::LogProbe { n: None })));
        assert!(matches!(parse_spec("log:probe:2").unwrap().items[0], Item::LogProbe { n: Some(2) }));
        assert!(parse_spec("log:probe:panic").unwrap_err().contains("不接受模式"));
        assert!(parse_spec("log:probe:error").unwrap_err().contains("不接受模式"));
        assert!(parse_spec("log:probe:1:panic").unwrap_err().contains("不接受模式"));
        assert!(parse_spec("log:probe:0").unwrap_err().contains("0"));
        assert!(load_for_plan("log:probe:1", &ops).is_ok());
        assert!(matches!(
            parse_spec("fsync-dir:2").unwrap().items[0],
            Item::At { stage: Stage::FsyncDir, n: 2, mode: Mode::Error }
        ));
        assert!(matches!(
            parse_spec("fsync-dir:4:error").unwrap().items[0],
            Item::At { stage: Stage::FsyncDir, n: 4, mode: Mode::Error }
        ));
        assert!(matches!(
            parse_spec("fsync-dir:4:panic").unwrap().items[0],
            Item::At { stage: Stage::FsyncDir, n: 4, mode: Mode::Panic }
        ));
        assert!(parse_spec("fsync-dir:2:corrupt").unwrap_err().contains("fsync-dir"));
        assert!(parse_spec("fsync-dir:2:drop").unwrap_err().contains("fsync-dir") || parse_spec("fsync-dir:2:drop").unwrap_err().contains("drop"));
        assert!(parse_spec("fsync-dir:panic").is_err());
        assert!(parse_spec("fsync-dir:0").unwrap_err().contains("0"));
        assert!(load_for_plan("fsync-dir:8", &ops).is_ok());
        let err = load_for_plan("write:1", &[OpKind::Delete]).unwrap_err();
        assert!(err.message.contains("不适用"));
        assert_ne!(err.code_str(), "E_FAULT");
    }
}
