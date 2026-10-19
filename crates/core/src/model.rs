use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OpKind {
    Create,
    Overwrite,
    Move,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub size: u64,
    pub mtime_ms: Option<i64>,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanOp {
    pub index: u32,
    pub kind: OpKind,
    pub path: String,
    /// 移动的旧路径。
    pub from: Option<String>,
    /// 要写入的内容。删除没有。
    pub bytes: Option<Vec<u8>>,
    pub expect: Option<Snapshot>,
    pub mode: Option<u32>,
    /// 直接覆盖时写回升级包的修改时间；合并结果为 None，用写入时刻。
    pub source_mtime_ms: Option<i64>,
    pub merged: bool,
    /// 删除开关关闭时的移动：复制到新路径，旧文件保留。
    pub keep_source: bool,
    pub readonly: bool,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecPlan {
    pub id: String,
    pub project_key: String,
    /// 这一笔执行的项目目录。执行开始时写成规范路径，旧计划没有这一项时为空。
    #[serde(default)]
    pub project_root: String,
    pub ops: Vec<PlanOp>,
    pub summary: Summary,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub create: u32,
    pub overwrite: u32,
    pub move_count: u32,
    pub delete: u32,
    pub merged: u32,
    pub move_keep_source: u32,
}
