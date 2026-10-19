//! 差异与合并。`render` 是纯函数：预演、写入、预览用同一份结果。
//!
//! 整块：用右边 = R，保留左边 = L，都保留 = L+R 或 R+L。
//! 按行：左栏只决定保不保留，右栏只决定采不采纳。没被点到的行跟随整块选择。
//! 选中行「都保留」时，对不上的另一侧保持原来的选择。
//! 对齐的行对按对内顺序穿插，避免新行堆到块尾。
//! 手动编辑按块记录；点工具条只丢掉这一块的手动编辑。

use crate::hashutil::xxh128_hex;
use serde::{Deserialize, Serialize};
use similar::{Algorithm, DiffTag, TextDiff};
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::mpsc;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TextDoc {
    pub lines: Vec<String>,
    pub newline: String,
    pub ends_with_newline: bool,
    pub encoding: String,
    pub bom: bool,
    pub binary: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InlineSpan {
    pub left: Range<u32>,
    pub right: Range<u32>,
    pub tag: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hunk {
    pub id: u32,
    pub left: Range<u32>,
    pub right: Range<u32>,
    pub fingerprint: u64,
    pub inline: Vec<InlineSpan>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    pub left: TextDoc,
    pub right: TextDoc,
    pub hunks: Vec<Hunk>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Decision {
    TakeRight,
    KeepLeft,
    Both { right_first: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    Default,
    User,
    Plan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum LineRef {
    Left(u32),
    Right(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LineDecision {
    Keep,
    Drop,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HunkState {
    pub base: Decision,
    pub right_first: bool,
    pub line_overrides: BTreeMap<String, LineDecision>,
    pub source: Source,
    pub manual: Option<Vec<String>>,
    pub needs_confirm: bool,
}

impl HunkState {
    pub fn default_take_right() -> Self {
        Self {
            base: Decision::TakeRight,
            right_first: false,
            line_overrides: BTreeMap::new(),
            source: Source::Default,
            manual: None,
            needs_confirm: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeState {
    pub hunks: Vec<HunkState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rendered {
    pub lines: Vec<String>,
    pub hunk_ranges: Vec<Range<u32>>,
    pub text: String,
}

pub fn decode(bytes: &[u8]) -> TextDoc {
    if bytes.is_empty() {
        return TextDoc {
            lines: vec![],
            newline: "\n".into(),
            ends_with_newline: false,
            encoding: "UTF-8".into(),
            bom: false,
            binary: false,
        };
    }
    let binary = bytes.iter().take(8192).any(|b| *b == 0);
    if binary {
        return TextDoc {
            lines: vec![],
            newline: "\n".into(),
            ends_with_newline: false,
            encoding: "binary".into(),
            bom: false,
            binary: true,
        };
    }
    let (encoding, bom, text) = decode_text(bytes);
    let newline = if text.contains("\r\n") {
        "\r\n"
    } else if text.contains('\r') {
        "\r"
    } else {
        "\n"
    };
    let norm = text.replace("\r\n", "\n").replace('\r', "\n");
    let ends = norm.ends_with('\n');
    let mut lines: Vec<String> = norm.split('\n').map(|s| s.to_string()).collect();
    if ends && lines.last().map(|s| s.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    if !ends && lines.is_empty() {
        lines.push(String::new());
    }
    TextDoc { lines, newline: newline.into(), ends_with_newline: ends, encoding, bom, binary: false }
}

fn decode_text(bytes: &[u8]) -> (String, bool, String) {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        let (s, _, _) = encoding_rs::UTF_8.decode(&bytes[3..]);
        return ("UTF-8".into(), true, s.into_owned());
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let (s, _, _) = encoding_rs::UTF_16LE.decode(&bytes[2..]);
        return ("UTF-16LE".into(), true, s.into_owned());
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let (s, _, _) = encoding_rs::UTF_16BE.decode(&bytes[2..]);
        return ("UTF-16BE".into(), true, s.into_owned());
    }
    if std::str::from_utf8(bytes).is_ok() {
        return ("UTF-8".into(), false, String::from_utf8_lossy(bytes).into_owned());
    }
    let mut det = chardetng::EncodingDetector::new();
    det.feed(bytes, true);
    let enc = det.guess(None, true);
    let (s, _, _) = enc.decode(bytes);
    (enc.name().to_string(), false, s.into_owned())
}

pub fn encode(doc: &TextDoc, lines: &[String]) -> Vec<u8> {
    if doc.binary {
        return Vec::new();
    }
    let mut body = lines.join(&doc.newline);
    if doc.ends_with_newline && !lines.is_empty() {
        body.push_str(&doc.newline);
    }
    let enc = encoding_rs::Encoding::for_label(doc.encoding.as_bytes()).unwrap_or(encoding_rs::UTF_8);
    let (cow, _, _) = enc.encode(&body);
    let mut out = Vec::new();
    if doc.bom {
        if doc.encoding.eq_ignore_ascii_case("UTF-8") {
            out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        } else if doc.encoding.eq_ignore_ascii_case("UTF-16LE") {
            out.extend_from_slice(&[0xFF, 0xFE]);
        }
    }
    out.extend_from_slice(&cow);
    out
}

pub fn diff_texts(left: TextDoc, right: TextDoc) -> FileDiff {
    if left.binary || right.binary {
        return FileDiff { left, right, hunks: vec![] };
    }
    if left.lines.is_empty() && !right.lines.is_empty() {
        let hunks = chunk_new_file(&right.lines);
        let hunks = fingerprint_hunks(&left.lines, &right.lines, hunks);
        return FileDiff { left, right, hunks };
    }
    let lt = left.lines.join("\n");
    let rt = right.lines.join("\n");
    let ops = diff_ops_with_timeout(&lt, &rt);
    let mut hunks = Vec::new();
    let mut id = 0u32;
    for op in ops {
        if op.tag == DiffTag::Equal {
            continue;
        }
        let left_r = op.old;
        let right_r = op.new;
        let inline = if left.lines.len() + right.lines.len() <= 100_000 && (right_r.end - right_r.start) <= 40 {
            inline_for(&left.lines, &right.lines, left_r.start, left_r.end, right_r.start, right_r.end)
        } else {
            vec![]
        };
        hunks.push(Hunk {
            id,
            left: left_r.start as u32..left_r.end as u32,
            right: right_r.start as u32..right_r.end as u32,
            fingerprint: 0,
            inline,
        });
        id += 1;
    }
    let hunks = fingerprint_hunks(&left.lines, &right.lines, hunks);
    FileDiff { left, right, hunks }
}

struct OwnedOp {
    tag: DiffTag,
    old: std::ops::Range<usize>,
    new: std::ops::Range<usize>,
}

fn collect_ops(lt: &str, rt: &str, algo: Algorithm) -> Vec<OwnedOp> {
    let diff = TextDiff::configure().algorithm(algo).diff_lines(lt, rt);
    diff.ops()
        .iter()
        .map(|op| OwnedOp { tag: op.tag(), old: op.old_range(), new: op.new_range() })
        .collect()
}

/// Patience 超过 3 秒就退回按行的 Myers。小文件几乎总是走 Patience。
fn diff_ops_with_timeout(lt: &str, rt: &str) -> Vec<OwnedOp> {
    let lt_owned = lt.to_string();
    let rt_owned = rt.to_string();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(collect_ops(&lt_owned, &rt_owned, Algorithm::Patience));
    });
    match rx.recv_timeout(Duration::from_secs(3)) {
        Ok(ops) => ops,
        Err(_) => collect_ops(lt, rt, Algorithm::Myers),
    }
}

fn chunk_new_file(lines: &[String]) -> Vec<Hunk> {
    let mut hunks = Vec::new();
    let mut i = 0u32;
    let mut id = 0u32;
    let n = lines.len() as u32;
    while i < n {
        let blank = lines[i as usize].trim().is_empty();
        let start = i;
        i += 1;
        while i < n && lines[i as usize].trim().is_empty() == blank {
            i += 1;
        }
        hunks.push(Hunk { id, left: start..start, right: start..i, fingerprint: 0, inline: vec![] });
        id += 1;
    }
    hunks
}

fn fingerprint_hunks(left: &[String], right: &[String], mut hunks: Vec<Hunk>) -> Vec<Hunk> {
    for h in &mut hunks {
        h.fingerprint = fingerprint(left, right, h.left.start, h.left.end, h.right.start, h.right.end);
    }
    hunks
}

pub fn fingerprint(left: &[String], right: &[String], ls: u32, le: u32, rs: u32, re: u32) -> u64 {
    let mut buf = String::new();
    let lctx0 = ls.saturating_sub(3);
    for i in lctx0..ls {
        buf.push_str(left.get(i as usize).map(|s| s.as_str()).unwrap_or(""));
        buf.push('\n');
    }
    buf.push_str("<<\n");
    for i in ls..le {
        buf.push_str(left.get(i as usize).map(|s| s.as_str()).unwrap_or(""));
        buf.push('\n');
    }
    buf.push_str(">>\n");
    for i in rs..re {
        buf.push_str(right.get(i as usize).map(|s| s.as_str()).unwrap_or(""));
        buf.push('\n');
    }
    let after_l = (le + 3).min(left.len() as u32);
    for i in le..after_l {
        buf.push_str(&left[i as usize]);
        buf.push('\n');
    }
    let hex = xxh128_hex(buf.as_bytes());
    u64::from_str_radix(&hex[..16], 16).unwrap_or(0)
}

fn inline_for(left: &[String], right: &[String], ls: usize, le: usize, rs: usize, re: usize) -> Vec<InlineSpan> {
    let n = (le - ls).min(re - rs);
    let mut spans = Vec::new();
    for i in 0..n {
        let l = &left[ls + i];
        let r = &right[rs + i];
        if l == r { continue; }
        let d = TextDiff::configure().algorithm(Algorithm::Myers).diff_chars(l, r);
        for op in d.ops() {
            if op.tag() == DiffTag::Equal { continue; }
            spans.push(InlineSpan {
                left: (ls as u32 + i as u32)..(ls as u32 + i as u32),
                right: (rs as u32 + i as u32)..(rs as u32 + i as u32),
                tag: format!("{:?}:{}-{}:{}-{}", op.tag(), op.old_range().start, op.old_range().end, op.new_range().start, op.new_range().end),
            });
        }
    }
    spans
}

pub fn default_state(diff: &FileDiff) -> MergeState {
    MergeState { hunks: diff.hunks.iter().map(|_| HunkState::default_take_right()).collect() }
}

#[derive(Debug, Clone, Copy)]
enum Align {
    Pair(u32, u32),
    LeftOnly(u32),
    RightOnly(u32),
}

fn align(h: &Hunk) -> Vec<Align> {
    let ln = h.left.end.saturating_sub(h.left.start);
    let rn = h.right.end.saturating_sub(h.right.start);
    let n = ln.min(rn);
    let mut v = Vec::new();
    for i in 0..n {
        v.push(Align::Pair(h.left.start + i, h.right.start + i));
    }
    for i in n..ln {
        v.push(Align::LeftOnly(h.left.start + i));
    }
    for i in n..rn {
        v.push(Align::RightOnly(h.right.start + i));
    }
    v
}

fn key_of(r: LineRef) -> String {
    match r {
        LineRef::Left(n) => format!("L{n}"),
        LineRef::Right(n) => format!("R{n}"),
    }
}

fn kept(st: &HunkState, side_left: bool, idx: u32) -> bool {
    let key = if side_left { format!("L{idx}") } else { format!("R{idx}") };
    if let Some(d) = st.line_overrides.get(&key) {
        return *d == LineDecision::Keep;
    }
    match st.base {
        Decision::TakeRight => !side_left,
        Decision::KeepLeft => side_left,
        Decision::Both { .. } => true,
    }
}

pub fn render(diff: &FileDiff, st: &MergeState) -> Rendered {
    let mut lines = Vec::new();
    let mut ranges = Vec::new();
    let mut li = 0u32;
    for (i, h) in diff.hunks.iter().enumerate() {
        while li < h.left.start && (li as usize) < diff.left.lines.len() {
            lines.push(diff.left.lines[li as usize].clone());
            li += 1;
        }
        let start = lines.len() as u32;
        let hs = st.hunks.get(i).cloned().unwrap_or_else(HunkState::default_take_right);
        if let Some(manual) = &hs.manual {
            lines.extend(manual.clone());
        } else {
            lines.extend(render_hunk(diff, h, &hs));
        }
        ranges.push(start..lines.len() as u32);
        li = h.left.end;
    }
    while (li as usize) < diff.left.lines.len() {
        lines.push(diff.left.lines[li as usize].clone());
        li += 1;
    }
    let nl = if diff.left.lines.is_empty() { diff.right.newline.clone() } else { diff.left.newline.clone() };
    let ends = if diff.left.lines.is_empty() { diff.right.ends_with_newline } else { diff.left.ends_with_newline };
    let mut text = lines.join(&nl);
    if ends && !lines.is_empty() {
        text.push_str(&nl);
    }
    Rendered { lines, hunk_ranges: ranges, text }
}

fn render_hunk(diff: &FileDiff, h: &Hunk, st: &HunkState) -> Vec<String> {
    let right_first = match st.base {
        Decision::Both { right_first } => right_first,
        _ => st.right_first,
    };
    let mut out = Vec::new();
    for item in align(h) {
        match item {
            Align::Pair(l, r) => {
                let emit_l = || diff.left.lines.get(l as usize).cloned().unwrap_or_default();
                let emit_r = || diff.right.lines.get(r as usize).cloned().unwrap_or_default();
                if right_first {
                    if kept(st, false, r) { out.push(emit_r()); }
                    if kept(st, true, l) { out.push(emit_l()); }
                } else {
                    if kept(st, true, l) { out.push(emit_l()); }
                    if kept(st, false, r) { out.push(emit_r()); }
                }
            }
            Align::LeftOnly(l) => {
                if kept(st, true, l) {
                    out.push(diff.left.lines.get(l as usize).cloned().unwrap_or_default());
                }
            }
            Align::RightOnly(r) => {
                if kept(st, false, r) {
                    out.push(diff.right.lines.get(r as usize).cloned().unwrap_or_default());
                }
            }
        }
    }
    out
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SelAction {
    UseRight,
    KeepLeft,
    KeepBoth,
}

pub fn apply_selection(st: &mut HunkState, h: &Hunk, action: SelAction, selected: &[LineRef]) {
    let pairs = align(h);
    let mut left_for = BTreeMap::new();
    let mut right_for = BTreeMap::new();
    for item in &pairs {
        if let Align::Pair(l, r) = item {
            left_for.insert(*r, *l);
            right_for.insert(*l, *r);
        }
    }
    for line in selected {
        match (action, line) {
            (SelAction::UseRight, LineRef::Right(r)) => {
                st.line_overrides.insert(key_of(LineRef::Right(*r)), LineDecision::Keep);
                if let Some(l) = left_for.get(r) {
                    st.line_overrides.insert(key_of(LineRef::Left(*l)), LineDecision::Drop);
                }
            }
            (SelAction::UseRight, LineRef::Left(l)) => {
                st.line_overrides.insert(key_of(LineRef::Left(*l)), LineDecision::Drop);
                if let Some(r) = right_for.get(l) {
                    st.line_overrides.insert(key_of(LineRef::Right(*r)), LineDecision::Keep);
                }
            }
            (SelAction::KeepLeft, LineRef::Left(l)) => {
                st.line_overrides.insert(key_of(LineRef::Left(*l)), LineDecision::Keep);
                if let Some(r) = right_for.get(l) {
                    st.line_overrides.insert(key_of(LineRef::Right(*r)), LineDecision::Drop);
                }
            }
            (SelAction::KeepLeft, LineRef::Right(r)) => {
                st.line_overrides.insert(key_of(LineRef::Right(*r)), LineDecision::Drop);
                if let Some(l) = left_for.get(r) {
                    st.line_overrides.insert(key_of(LineRef::Left(*l)), LineDecision::Keep);
                }
            }
            (SelAction::KeepBoth, line) => {
                // 选中的这一行留下。能对上的另一侧一起留下；对不上的另一侧保持原来的选择。
                st.line_overrides.insert(key_of(*line), LineDecision::Keep);
                match line {
                    LineRef::Right(r) => {
                        if let Some(l) = left_for.get(r) {
                            st.line_overrides.insert(key_of(LineRef::Left(*l)), LineDecision::Keep);
                        }
                    }
                    LineRef::Left(l) => {
                        if let Some(r) = right_for.get(l) {
                            st.line_overrides.insert(key_of(LineRef::Right(*r)), LineDecision::Keep);
                        }
                    }
                }
            }
        }
    }
    st.source = Source::User;
}

pub fn bulk_counts(st: &MergeState) -> (u32, u32) {
    let manual = st.hunks.iter().filter(|h| h.manual.is_some()).count() as u32;
    let re = st.hunks.iter().filter(|h| h.needs_confirm).count() as u32;
    (manual, re)
}

pub fn unconfirmed(st: &MergeState) -> usize {
    st.hunks.iter().filter(|h| h.needs_confirm).count()
}

pub fn set_all(st: &mut MergeState, decision: Decision, ack: bool) -> Result<(), (u32, u32)> {
    let counts = bulk_counts(st);
    if (counts.0 > 0 || counts.1 > 0) && !ack {
        return Err(counts);
    }
    for h in &mut st.hunks {
        if ack {
            h.manual = None;
            h.needs_confirm = false;
        }
        h.base = decision;
        h.right_first = matches!(decision, Decision::Both { right_first: true });
        h.line_overrides.clear();
        h.source = Source::User;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(s: &str) -> TextDoc {
        decode(s.as_bytes())
    }

    #[test]
    fn all_right_equals_right_and_left() {
        let d = diff_texts(doc("a\nb\nc\n"), doc("a\nB\nc\n"));
        let mut st = default_state(&d);
        let r = render(&d, &st);
        assert_eq!(r.text, "a\nB\nc\n");
        for h in &mut st.hunks {
            h.base = Decision::KeepLeft;
        }
        assert_eq!(render(&d, &st).text, "a\nb\nc\n");
    }

    #[test]
    fn both_orders_and_interleave() {
        let d = diff_texts(doc("a\nx1\nx2\nx3\nb\n"), doc("a\ny1\ny2\ny3\nb\n"));
        let mut st = default_state(&d);
        st.hunks[0].base = Decision::Both { right_first: false };
        let text = render(&d, &st).text;
        assert_eq!(text, "a\nx1\ny1\nx2\ny2\nx3\ny3\nb\n");
        st.hunks[0].base = Decision::Both { right_first: true };
        assert_eq!(render(&d, &st).text, "a\ny1\nx1\ny2\nx2\ny3\nx3\nb\n");
    }

    #[test]
    fn selection_keep_both_one_sided_keeps_other() {
        let d = diff_texts(doc("a\nl1\nl2\nb\n"), doc("a\nr1\nr2\nr3\nb\n"));
        let mut st = default_state(&d);
        // 整块用右边：左不保留，右采纳。只在右栏选中第一行「都保留」。
        // r1 对应 l1，两者都留；r3 没有对应左行，左栏未选中的保持不保留。
        apply_selection(&mut st.hunks[0], &d.hunks[0], SelAction::KeepBoth, &[LineRef::Right(1)]);
        let text = render(&d, &st).text;
        assert!(text.contains("l1\nr1") || text.contains("l1\n") && text.contains("r1\n"), "{text}");
        assert!(text.contains("r2"));
        assert!(text.contains("r3"));
        assert!(!text.contains("l2"), "{text}");
    }

    #[test]
    fn manual_is_per_hunk() {
        let d = diff_texts(doc("a\nL\nb\nM\nc\n"), doc("a\nR\nb\nN\nc\n"));
        let mut st = default_state(&d);
        assert!(st.hunks.len() >= 2, "{}", st.hunks.len());
        st.hunks[0].manual = Some(vec!["EDIT".into()]);
        st.hunks[1].manual = Some(vec!["KEEP".into()]);
        st.hunks[0].manual = None;
        st.hunks[0].base = Decision::TakeRight;
        let text = render(&d, &st).text;
        assert!(text.contains("KEEP"), "{text}");
        assert!(!text.contains("EDIT"), "{text}");
    }

    #[test]
    fn new_file_partial() {
        let d = diff_texts(doc(""), doc("one\n\ntwo\n"));
        assert!(d.hunks.len() >= 2);
        let mut st = default_state(&d);
        st.hunks[0].base = Decision::KeepLeft; // 新增文件左边是空的，等于不采纳
        let text = render(&d, &st).text;
        assert!(!text.contains("one") || text.contains("two"));
        assert!(text.contains("two"), "{text}");
    }

    #[test]
    fn encoding_roundtrip_utf8_bom_and_crlf() {
        let raw = b"\xEF\xBB\xBFa\r\nb\r\n";
        let doc = decode(raw);
        assert!(doc.bom);
        assert_eq!(doc.newline, "\r\n");
        let back = encode(&doc, &doc.lines);
        assert_eq!(back, raw);
    }

    #[test]
    fn fingerprint_changes_when_context_changes() {
        let a = fingerprint(&["a".into(), "b".into(), "c".into(), "X".into()], &["a".into(), "b".into(), "c".into(), "Y".into()], 3, 4, 3, 4);
        let b = fingerprint(&["a".into(), "B".into(), "c".into(), "X".into()], &["a".into(), "b".into(), "c".into(), "Y".into()], 3, 4, 3, 4);
        assert_ne!(a, b);
    }

    #[test]
    fn whole_file_requires_ack_and_confirms() {
        let d = diff_texts(doc("a\n"), doc("b\n"));
        let mut st = default_state(&d);
        st.hunks[0].manual = Some(vec!["m".into()]);
        st.hunks[0].needs_confirm = true;
        assert!(set_all(&mut st, Decision::TakeRight, false).is_err());
        set_all(&mut st, Decision::TakeRight, true).unwrap();
        assert!(st.hunks[0].manual.is_none());
        assert!(!st.hunks[0].needs_confirm);
    }
}

#[cfg(test)]
mod prop {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn take_right_is_right(lines in proptest::collection::vec("[a-z]{1,8}", 1..12)) {
            let left = decode(b"a\nb\n");
            let right_text = lines.join("\n") + "\n";
            let right = decode(right_text.as_bytes());
            let d = diff_texts(left, right.clone());
            let st = default_state(&d);
            let rendered = render(&d, &st).text;
            let expect = encode_like(&right);
            prop_assert_eq!(rendered, expect);
        }
    }

    fn encode_like(doc: &TextDoc) -> String {
        let mut t = doc.lines.join(&doc.newline);
        if doc.ends_with_newline && !doc.lines.is_empty() {
            t.push_str(&doc.newline);
        }
        t
    }
}
