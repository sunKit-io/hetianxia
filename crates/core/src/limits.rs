//! 打开文件的分档。正好 50MB 仍算「50MB 以内」那一档：提示后打开，关掉语法高亮。
//! 正好 5MB 仍走正常打开。

pub const MIB: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenTier {
    /// 小于等于 5MB。
    Full,
    /// 大于 5MB，小于等于 50MB。
    PromptNoSyntax,
    /// 大于 50MB，只能整文件选择。
    WholeFile,
}

pub fn open_tier(bytes: u64) -> OpenTier {
    if bytes <= 5 * MIB {
        OpenTier::Full
    } else if bytes <= 50 * MIB {
        OpenTier::PromptNoSyntax
    } else {
        OpenTier::WholeFile
    }
}

/// 超过 10 万行不算字符级差异。正好 10 万行仍然算。
pub fn char_diff_allowed(total_lines: usize) -> bool {
    total_lines <= 100_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifty_megabytes_stays_in_the_prompt_tier() {
        assert_eq!(open_tier(5 * MIB), OpenTier::Full);
        assert_eq!(open_tier(5 * MIB + 1), OpenTier::PromptNoSyntax);
        assert_eq!(open_tier(50 * MIB), OpenTier::PromptNoSyntax);
        assert_eq!(open_tier(50 * MIB + 1), OpenTier::WholeFile);
        assert!(char_diff_allowed(100_000));
        assert!(!char_diff_allowed(100_001));
    }
}
