//! 合天下核心。界面和 Tauri 命令都调用这里，不在别的地方再写一套合并或写入。

pub mod brand;
pub mod connections;
pub mod error;
pub mod exec;
pub mod fault;
pub mod hashutil;
pub mod limits;
pub mod merge;
pub mod model;
pub mod osinfo;
pub mod pathutil;
pub mod picks;
pub mod plans;
pub mod prefs;
pub mod remote;
pub mod remote_write;
pub mod runlock;
pub mod scan;
pub mod secrets;
pub mod sftp;
pub mod vfs;
pub mod zipfs;

/// 进程启动时调用。故障注入构建会在这里检查环境变量语法。
pub fn boot() {
    fault::check_env_syntax_or_exit();
}
