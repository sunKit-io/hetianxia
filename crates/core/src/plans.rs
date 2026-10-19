//! 合并方案。整文件按路径套用；逐块按指纹套用。对不上的块标成需重新确认。
//! 还有没确认的块时不能执行升级。

use crate::error::{Error, ErrorCode, Result};
use crate::merge::{Decision, FileDiff, MergeState, Source};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedHunk {
    pub fingerprint: u64,
    pub decision: Decision,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedFile {
    pub path: String,
    pub whole: Option<Decision>,
    pub hunks: Vec<SavedHunk>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedPlan {
    pub schema_version: u32,
    pub project_key: String,
    pub name: String,
    pub files: Vec<SavedFile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplyReport {
    pub applied: u32,
    pub needs_confirm: u32,
}

pub fn apply_file(diff: &FileDiff, state: &mut MergeState, saved: &SavedFile) -> ApplyReport {
    if state.hunks.len() < diff.hunks.len() {
        state.hunks.resize(diff.hunks.len(), crate::merge::HunkState::default_take_right());
    }
    if let Some(decision) = saved.whole {
        for st in &mut state.hunks {
            st.base = decision;
            st.right_first = matches!(decision, Decision::Both { right_first: true });
            st.source = Source::Plan;
            st.needs_confirm = false;
            st.manual = None;
        }
        return ApplyReport { applied: diff.hunks.len() as u32, needs_confirm: 0 };
    }
    let by_fp: HashMap<u64, Decision> = saved.hunks.iter().map(|h| (h.fingerprint, h.decision)).collect();
    let mut applied = 0u32;
    let mut needs = 0u32;
    for (hunk, st) in diff.hunks.iter().zip(state.hunks.iter_mut()) {
        if let Some(decision) = by_fp.get(&hunk.fingerprint) {
            st.base = *decision;
            st.right_first = matches!(decision, Decision::Both { right_first: true });
            st.source = Source::Plan;
            st.needs_confirm = false;
            applied += 1;
        } else if !saved.hunks.is_empty() {
            st.needs_confirm = true;
            st.source = Source::Plan;
            needs += 1;
        }
    }
    ApplyReport { applied, needs_confirm: needs }
}

pub fn execution_gate(needs_confirm: u32) -> Result<()> {
    if needs_confirm == 0 {
        Ok(())
    } else {
        Err(Error::new(
            ErrorCode::UnconfirmedHunks,
            format!("还有 {needs_confirm} 处需要重新确认"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge::{decode, diff_texts, HunkState};

    #[test]
    fn mismatched_fingerprint_needs_confirm_and_blocks_execute() {
        let left = decode(b"alpha\nkeep\n");
        let right = decode(b"beta\nkeep\n");
        let diff = diff_texts(left, right);
        assert!(!diff.hunks.is_empty());
        let mut state = MergeState {
            hunks: diff.hunks.iter().map(|_| HunkState::default_take_right()).collect(),
        };
        let saved = SavedFile {
            path: "a.txt".into(),
            whole: None,
            hunks: vec![SavedHunk { fingerprint: 1, decision: Decision::KeepLeft }],
        };
        let report = apply_file(&diff, &mut state, &saved);
        assert!(report.needs_confirm > 0);
        assert!(execution_gate(report.needs_confirm).is_err());
        assert!(state.hunks.iter().any(|h| h.needs_confirm));
    }

    #[test]
    fn whole_file_choice_counts_as_confirmed() {
        let left = decode(b"a\n");
        let right = decode(b"b\n");
        let diff = diff_texts(left, right);
        let mut state = MergeState {
            hunks: diff.hunks.iter().map(|_| HunkState::default_take_right()).collect(),
        };
        let saved = SavedFile {
            path: "a.txt".into(),
            whole: Some(Decision::KeepLeft),
            hunks: vec![],
        };
        let report = apply_file(&diff, &mut state, &saved);
        assert_eq!(report.needs_confirm, 0);
        assert!(execution_gate(0).is_ok());
        assert!(state.hunks.iter().all(|h| h.base == Decision::KeepLeft && !h.needs_confirm));
    }
}
