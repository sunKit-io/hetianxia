//! 只在 fault-inject 打开时被解析。正式包不编译这个文件，
//! 所以哨兵字符串不会出现在发布产物里。

/// 字节直接放在符号里。`touch` 做一次 volatile 读，发布优化也不能把哨兵从最终可执行文件里删掉。
#[used]
pub static HTX_FAULT_INJECT_ENABLED: [u8; 24] = *b"HTX_FAULT_INJECT_ENABLED";

pub fn touch() {
    let ptr = HTX_FAULT_INJECT_ENABLED.as_ptr();
    unsafe {
        std::ptr::read_volatile(ptr);
    }
}
