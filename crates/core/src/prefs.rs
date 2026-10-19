//! 偏好里和产品决议绑在一起的默认值。界面可以改，这两个数是出厂默认。

pub const BACKUP_KEEP: u32 = 10;
pub const BACKUP_MAX_AGE_DAYS: u32 = 30;
pub const BACKUP_MAX_AGE_MS: i64 = 30 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy)]
pub struct Shortcut {
    pub keys: &'static str,
    pub action: &'static str,
}

pub const SHORTCUTS: &[Shortcut] = &[
    Shortcut { keys: "⌘ ⇧ O", action: "打开原始项目" },
    Shortcut { keys: "⌘ ⇧ U", action: "打开升级包" },
    Shortcut { keys: "⌘ ↩", action: "执行升级" },
    Shortcut { keys: "⌘ Z", action: "撤销合并选择" },
    Shortcut { keys: "⌘ ⇧ Z", action: "重做" },
    Shortcut { keys: "⌘ ,", action: "偏好设置" },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_review() {
        assert_eq!(BACKUP_KEEP, 10);
        assert_eq!(BACKUP_MAX_AGE_DAYS, 30);
        assert!(BACKUP_MAX_AGE_MS > 0);
    }
}
