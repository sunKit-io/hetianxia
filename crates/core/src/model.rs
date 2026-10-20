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

/// 升级包里的一个文件。路径不在这里：执行只按 `scan_id` 读扫描时写入数据库的那条路径。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContentSource {
    pub scan_id: String,
    /// `dir` 或 `zip`。
    pub kind: String,
    /// 目录包里的相对路径，或 zip 条目名。
    pub entry: String,
    /// 前端如果带了路径，执行会拿它和扫描记录比对，不一致就中止，并且不会打开它。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanOp {
    pub index: u32,
    pub kind: OpKind,
    pub path: String,
    /// 移动的旧路径。
    pub from: Option<String>,
    /// 引擎内部测试仍可直接带内容。界面计划不能带这个字段，见 `parse_client_plan`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    /// 要写入内容的 SHA-256。删除没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// 要写入内容的字节数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// 升级包来源。只有 scan_id 和条目名；磁盘路径在扫描记录里。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ContentSource>,
}

pub const LEGACY_BYTES_PLAN: &str = "旧格式计划带有文件内容（bytes），已不再支持。请重新扫描后再执行";

/// 界面送来的计划。带 `bytes` 数组的旧格式直接拒绝。
pub fn parse_client_plan(value: serde_json::Value) -> crate::error::Result<ExecPlan> {
    if plan_embeds_bytes(&value) {
        return Err(crate::error::Error::new(crate::error::ErrorCode::InvalidState, LEGACY_BYTES_PLAN));
    }
    serde_json::from_value(value).map_err(|err| crate::error::Error::new(crate::error::ErrorCode::Internal, err.to_string()))
}

fn plan_embeds_bytes(value: &serde_json::Value) -> bool {
    let Some(ops) = value.get("ops").and_then(|item| item.as_array()) else {
        return false;
    };
    ops.iter().any(|op| matches!(op.get("bytes"), Some(serde_json::Value::Array(_))))
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
