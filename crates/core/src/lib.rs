//! 合天下核心。界面和 Tauri 命令都调用这里，不在别的地方再写一套合并或写入。

pub mod brand;
pub mod error;
pub mod hashutil;
pub mod limits;
pub mod model;
pub mod pathutil;
pub mod prefs;

/// 进程启动时调用。故障注入构建会在执行引擎里检查 `UPG_FAULT` 的语法。
pub fn boot() {}
