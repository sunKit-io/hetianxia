//! 只在 fault-inject 打开时被解析。正式包不编译这个文件，
//! 所以哨兵字符串不会出现在发布产物里。

#[used]
pub static HTX_FAULT_INJECT_ENABLED: &str = "HTX_FAULT_INJECT_ENABLED";

pub fn touch() {
    let _keep = HTX_FAULT_INJECT_ENABLED;
}
