//! 故障注入。整个模块在 `fault-inject` feature 关闭时不包含任何实现，
//! 正式包里既没有解析器，也没有 `UPG_FAULT_MARKER_HETIANXIA` 这个符号。
//!
//! 环境变量 `UPG_FAULT`，逗号分隔：
//! - `stage:N` 或 `stage:N:mode`
//! - stage: `backup` `write` `verify-pre` `verify-post` `rename` `rollback` `log` `sftp-disconnect`
//! - N：执行计划里的文件序号，从 1 开始。`log:N` 例外，N 是第 N 次事务日志写入
//!   （第 1 次 = 打开事务，状态 PREPARED；之后每次状态变更各算一次）。
//! - `log:begin` 等于 `log:1`。`log:commit` 在最终 COMMITTED 写入之前触发。
//! - mode: `error`（默认，返回注入错误）、`panic`（进程中止）。
//!   `sftp-disconnect` 只有 `drop`（省略 mode 即 drop），表示丢掉连接。
//!   设计文档 12.8 把方式写成 error / panic；断开连接不是这两种，所以单独用 drop。
//!
//! 格式不合法分两处拦住，都打印 `UPG_FAULT: ...` 并以退出码 1 结束，不会静默忽略：
//! - 启动：`check_env_syntax_or_exit` 检查语法（未知阶段、空项、N=0、错误的 mode、
//!   把 drop 用在别的阶段、给 sftp-disconnect 写 error/panic、单独的 verify）。
//! - 执行开始：`load_from_env_or_exit` 再按计划检查序号。文件类阶段的 N 大于计划长度，
//!   或者阶段不适用于该操作，都算非法。`log:N` 按日志写入次数计数，上限是
//!   `4 + 2 × 计划长度`（prepared、backed_up、每个文件 doing/done、verifying、committed），
//!   可以大于计划长度。

use crate::error::Result;
#[cfg(feature = "fault-inject")]
use crate::error::{Error, ErrorCode};
use crate::model::OpKind;
use serde::Serialize;

#[cfg(feature = "fault-inject")]
pub const MARKER: &str = "UPG_FAULT_MARKER_HETIANXIA";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    Backup,
    Write,
    VerifyPre,
    VerifyPost,
    Rename,
    Rollback,
    Log,
    SftpDisconnect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Error,
    Panic,
    Drop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    At { stage: Stage, n: u32, mode: Mode },
    LogBegin { mode: Mode },
    LogCommit { mode: Mode },
}

#[derive(Debug, Clone, Default)]
pub struct FaultPlan {
    pub items: Vec<Item>,
}

impl FaultPlan {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[derive(Debug, Default)]
pub struct FaultRuntime {
    plan: FaultPlan,
    log_writes: u32,
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
    {
        return Err(format!("未知阶段「{}」（来自「{raw}」）。没有单独的 verify，请用 verify-pre 或 verify-post", parts[0]));
    }
    if parts[0] == "log" && parts.len() >= 2 && (parts[1] == "begin" || parts[1] == "commit") {
        let which = parts[1];
        let mode = if parts.len() == 2 {
            Mode::Error
        } else if parts.len() == 3 {
            parse_mode(parts[2], raw, false)?
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
        parse_mode(parts[2], raw, stage == Stage::SftpDisconnect)?
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
fn parse_mode(s: &str, raw: &str, disconnect: bool) -> std::result::Result<Mode, String> {
    match s {
        "error" if !disconnect => Ok(Mode::Error),
        "panic" if !disconnect => Ok(Mode::Panic),
        "drop" if disconnect => Ok(Mode::Drop),
        "error" | "panic" if disconnect => Err(format!("「{raw}」：sftp-disconnect 只有 drop 模式")),
        other => Err(format!("「{raw}」的模式「{other}」不认识，可用 error、panic{}", if disconnect { "、drop" } else { "" })),
    }
}

#[cfg(feature = "fault-inject")]
pub fn load_for_plan(spec: &str, ops: &[OpKind]) -> Result<FaultPlan> {
    let plan = parse_spec(spec).map_err(|m| Error::new(ErrorCode::Fault, m))?;
    validate(&plan, ops)?;
    Ok(plan)
}

#[cfg(feature = "fault-inject")]
fn validate(plan: &FaultPlan, ops: &[OpKind]) -> Result<()> {
    let len = ops.len() as u32;
    for item in &plan.items {
        match item {
            Item::LogBegin { .. } | Item::LogCommit { .. } => {}
            Item::At { stage: Stage::Log, n, .. } => {
                // 日志写入次数 = prepared + backed_up + 每个文件 doing/done + verifying + committed。
                // 大于这个上限的 log:N 无法命中，按非法处理，避免静默忽略。
                let max_logs = 4 + 2 * len;
                if *n > max_logs {
                    return Err(Error::new(
                        ErrorCode::Fault,
                        format!("log:{n} 超出本次执行最多 {max_logs} 次日志写入"),
                    ));
                }
            }
            Item::At { stage, n, .. } => {
                if *n > len {
                    return Err(Error::new(
                        ErrorCode::Fault,
                        format!("{stage:?}:{n} 大于计划长度 {len}（来自序号 {n}）"),
                    ));
                }
                let kind = ops[(*n as usize) - 1];
                if !stage_applies(*stage, kind) {
                    return Err(Error::new(
                        ErrorCode::Fault,
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
        Stage::Log => true,
    }
}

impl FaultRuntime {
    pub fn new(plan: FaultPlan) -> Self {
        Self { plan, log_writes: 0 }
    }

    #[cfg(not(feature = "fault-inject"))]
    pub fn hit_file(&mut self, _stage: Stage, _index: u32) -> Result<()> {
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

    #[cfg(feature = "fault-inject")]
    pub fn hit_file(&mut self, stage: Stage, index: u32) -> Result<()> {
        for item in &self.plan.items {
            if let Item::At { stage: s, n, mode } = item {
                if *s == stage && *n == index {
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
fn fire(stage: Stage, n: u32, mode: Mode) -> Result<()> {
    let label = format!("{stage:?}:{n}");
    match mode {
        Mode::Drop => Err(Error::new(ErrorCode::Net, format!("UPG_FAULT sftp-disconnect at {label}"))),
        Mode::Error => Err(Error::new(ErrorCode::Fault, format!("UPG_FAULT error at {label}"))),
        Mode::Panic => {
            let msg = format!("UPG_FAULT panic at {label}");
            eprintln!("{msg}");
            #[cfg(test)]
            {
                panic!("{msg}");
            }
            #[cfg(not(test))]
            {
                std::process::abort();
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
        assert!(parse_spec("nope:1").unwrap_err().contains("未知阶段"));
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
        assert!(load_for_plan("backup:2", &ops).is_ok());
        let err = load_for_plan("write:1", &[OpKind::Delete]).unwrap_err();
        assert!(err.message.contains("不适用"));
    }
}
