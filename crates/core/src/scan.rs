//! 扫描比对：大小 → 哈希，忽略规则，换行/行尾空白，移动识别。
//! 只改了 mtime、内容哈希没变的文件不算改动（由执行前的变动检测再确认一次）。

use crate::error::Result;
use crate::hashutil::{sha256_hex, xxh128_hex};
use crate::pathutil::Ignore;
use crate::vfs::{Entry, EntryKind, LocalFs};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileStatus {
    Added,
    Modified,
    Extra,
    Same,
    Moved,
    Symlink,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanOptions {
    pub ignore_crlf: bool,
    pub ignore_trailing_ws: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self { ignore_crlf: true, ignore_trailing_ws: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRec {
    pub rel: String,
    pub status: FileStatus,
    pub left_size: Option<u64>,
    pub right_size: Option<u64>,
    pub left_mtime_ms: Option<i64>,
    pub right_mtime_ms: Option<i64>,
    pub hash: Option<String>,
    pub left_hash: Option<String>,
    pub right_hash: Option<String>,
    pub moved_from: Option<String>,
    pub moved_to: Option<String>,
    pub case_conflict: bool,
    pub message: Option<String>,
    pub readonly: bool,
    pub link_target: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanOutput {
    pub files: Vec<FileRec>,
    pub case_sensitive: bool,
}

pub fn scan_local(left: &LocalFs, right: &LocalFs, ignore: &Ignore, opt: &ScanOptions) -> Result<ScanOutput> {
    let l = left.walk(ignore)?;
    let r = right.walk(ignore)?;
    Ok(compare_entries(&l, &r, left, right, opt))
}

fn key_of(rel: &str, case_sensitive: bool) -> String {
    if case_sensitive { rel.to_string() } else { rel.to_lowercase() }
}

fn compare_entries(left: &[Entry], right: &[Entry], lfs: &LocalFs, rfs: &LocalFs, opt: &ScanOptions) -> ScanOutput {
    let case_sensitive = cfg!(all(unix, not(target_os = "macos")));
    let mut lm: HashMap<String, Vec<&Entry>> = HashMap::new();
    let mut rm: HashMap<String, Vec<&Entry>> = HashMap::new();
    for e in left {
        if e.kind == EntryKind::Dir { continue; }
        lm.entry(key_of(&e.rel, case_sensitive)).or_default().push(e);
    }
    for e in right {
        if e.kind == EntryKind::Dir { continue; }
        rm.entry(key_of(&e.rel, case_sensitive)).or_default().push(e);
    }
    let mut keys: Vec<String> = lm.keys().chain(rm.keys()).cloned().collect();
    keys.sort();
    keys.dedup();
    let mut files = Vec::new();
    let mut added: Vec<FileRec> = Vec::new();
    let mut extra: Vec<FileRec> = Vec::new();
    for k in keys {
        let ls = lm.get(&k).map(|v| v.as_slice()).unwrap_or(&[]);
        let rs = rm.get(&k).map(|v| v.as_slice()).unwrap_or(&[]);
        let case_conflict = ls.len() + rs.len() > 1 && !case_sensitive
            && ls.iter().chain(rs.iter()).map(|e| e.rel.as_str()).collect::<std::collections::BTreeSet<_>>().len() > 1;
        let le = ls.first().copied();
        let re = rs.first().copied();
        match (le, re) {
            (Some(l), None) => extra.push(rec_one(l, FileStatus::Extra, true)),
            (None, Some(r)) => added.push(rec_one(r, FileStatus::Added, false)),
            (Some(l), Some(r)) => {
                if l.kind == EntryKind::Symlink || r.kind == EntryKind::Symlink {
                    let same = l.link_target == r.link_target && l.kind == r.kind;
                    files.push(FileRec {
                        rel: l.rel.clone(),
                        status: if l.kind == EntryKind::Symlink && r.kind == EntryKind::Symlink {
                            if same { FileStatus::Same } else { FileStatus::Symlink }
                        } else {
                            FileStatus::Symlink
                        },
                        left_size: Some(l.size),
                        right_size: Some(r.size),
                        left_mtime_ms: l.mtime_ms,
                        right_mtime_ms: r.mtime_ms,
                        hash: None,
                        left_hash: None,
                        right_hash: None,
                        moved_from: None,
                        moved_to: None,
                        case_conflict,
                        message: Some("符号链接只提示，不覆盖、不删除、不新建".into()),
                        readonly: true,
                        link_target: l.link_target.clone().or(r.link_target.clone()),
                    });
                    continue;
                }
                let use_sha = l.size > 64 * 1024 * 1024 || r.size > 64 * 1024 * 1024;
                let mut status = if l.size != r.size { FileStatus::Modified } else { FileStatus::Same };
                let mut lh = None;
                let mut rh = None;
                if status == FileStatus::Same {
                    let lp = crate::pathutil::RelPath::parse(&l.rel).ok();
                    let rp = crate::pathutil::RelPath::parse(&r.rel).ok();
                    if let (Some(lp), Some(rp)) = (lp, rp) {
                        lh = lfs.hash(&lp, use_sha).ok();
                        rh = rfs.hash(&rp, use_sha).ok();
                        if lh != rh {
                            status = FileStatus::Modified;
                        }
                    }
                } else {
                    let lp = crate::pathutil::RelPath::parse(&l.rel).ok();
                    let rp = crate::pathutil::RelPath::parse(&r.rel).ok();
                    if let (Some(lp), Some(rp)) = (lp, rp) {
                        lh = lfs.hash(&lp, use_sha).ok();
                        rh = rfs.hash(&rp, use_sha).ok();
                    }
                }
                if status == FileStatus::Modified && (opt.ignore_crlf || opt.ignore_trailing_ws) {
                    if let (Some(lp), Some(rp)) = (
                        crate::pathutil::RelPath::parse(&l.rel).ok(),
                        crate::pathutil::RelPath::parse(&r.rel).ok(),
                    ) {
                        if let (Ok(lb), Ok(rb)) = (lfs.read(&lp), rfs.read(&rp)) {
                            if looks_text(&lb) && looks_text(&rb) {
                                let ln = normalize(&lb, opt);
                                let rn = normalize(&rb, opt);
                                let algo = |d: &[u8]| if use_sha { sha256_hex(d) } else { xxh128_hex(d) };
                                if algo(&ln) == algo(&rn) {
                                    status = FileStatus::Same;
                                }
                            }
                        }
                    }
                }
                files.push(FileRec {
                    rel: l.rel.clone(),
                    status,
                    left_size: Some(l.size),
                    right_size: Some(r.size),
                    left_mtime_ms: l.mtime_ms,
                    right_mtime_ms: r.mtime_ms,
                    hash: lh.clone().or(rh.clone()),
                    left_hash: lh,
                    right_hash: rh,
                    moved_from: None,
                    moved_to: None,
                    case_conflict,
                    message: None,
                    readonly: l.readonly,
                    link_target: None,
                });
            }
            (None, None) => {}
        }
    }
    // 哈希还没填。移动配对在 run_scan 里填完哈希后做。
    attach_moves(&mut added, &mut extra);
    files.extend(added);
    files.extend(extra);
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    ScanOutput { files, case_sensitive }
}

fn rec_one(e: &Entry, status: FileStatus, left: bool) -> FileRec {
    if e.kind == EntryKind::Symlink {
        return FileRec {
            rel: e.rel.clone(),
            status: FileStatus::Symlink,
            left_size: left.then_some(e.size),
            right_size: (!left).then_some(e.size),
            left_mtime_ms: left.then_some(e.mtime_ms).flatten(),
            right_mtime_ms: (!left).then_some(e.mtime_ms).flatten(),
            hash: None,
            left_hash: None,
            right_hash: None,
            moved_from: None,
            moved_to: None,
            case_conflict: false,
            message: Some("符号链接只提示，不处理".into()),
            readonly: true,
            link_target: e.link_target.clone(),
        };
    }
    FileRec {
        rel: e.rel.clone(),
        status,
        left_size: left.then_some(e.size),
        right_size: (!left).then_some(e.size),
        left_mtime_ms: left.then_some(e.mtime_ms).flatten(),
        right_mtime_ms: (!left).then_some(e.mtime_ms).flatten(),
        hash: None,
        left_hash: None,
        right_hash: None,
        moved_from: None,
        moved_to: None,
        case_conflict: false,
        message: None,
        readonly: e.readonly,
        link_target: None,
    }
}

fn attach_moves(_added: &mut Vec<FileRec>, _extra: &mut Vec<FileRec>) {
    // 单侧文件的哈希要等 fill_side_hashes。配对在 detect_moves。
}

pub fn fill_side_hashes(files: &mut [FileRec], left: Option<&LocalFs>, right: Option<&LocalFs>) {
    for f in files.iter_mut() {
        if f.status == FileStatus::Extra {
            if let (Some(fs), Ok(p)) = (left, crate::pathutil::RelPath::parse(&f.rel)) {
                f.left_hash = fs.hash(&p, false).ok();
                f.hash = f.left_hash.clone();
            }
        } else if f.status == FileStatus::Added {
            if let (Some(fs), Ok(p)) = (right, crate::pathutil::RelPath::parse(&f.rel)) {
                f.right_hash = fs.hash(&p, false).ok();
                f.hash = f.right_hash.clone();
            }
        }
    }
}

/// 旧路径在目标（左侧项目）里被删、新路径在升级包（右侧）里是新增，且内容哈希相同，记成一次移动。
///
/// 配对只看内容，不看文件名像不像。进入同一组的条件是：哈希非空、大小大于 0、两侧大小相等。
/// 空文件不参与，即使空内容的哈希相同。
///
/// 同一组里，旧路径和新路径都按相对路径的字典序（字节序）排序，然后按下标配对：
/// 第 i 个旧路径对第 i 个新路径。一侧更长时只配到较短的那一侧，多出来的保持 Extra 或 Added。
/// 一对多、多对一都走这一条，不另选「最像」的路径。
pub fn detect_moves(files: &mut Vec<FileRec>) {
    let mut groups: BTreeMap<(String, u64), (Vec<usize>, Vec<usize>)> = BTreeMap::new();
    for (i, f) in files.iter().enumerate() {
        let Some(hash) = f.hash.as_ref().filter(|h| !h.is_empty()) else { continue };
        if f.status == FileStatus::Extra && f.left_size.unwrap_or(0) > 0 {
            groups.entry((hash.clone(), f.left_size.unwrap_or(0))).or_default().0.push(i);
        } else if f.status == FileStatus::Added && f.right_size.unwrap_or(0) > 0 {
            groups.entry((hash.clone(), f.right_size.unwrap_or(0))).or_default().1.push(i);
        }
    }
    let mut pairs = Vec::new();
    for (_key, (mut extras, mut addeds)) in groups {
        extras.sort_by(|&a, &b| files[a].rel.cmp(&files[b].rel).then(a.cmp(&b)));
        addeds.sort_by(|&a, &b| files[a].rel.cmp(&files[b].rel).then(a.cmp(&b)));
        let n = extras.len().min(addeds.len());
        for k in 0..n {
            pairs.push((extras[k], addeds[k]));
        }
    }
    for (e, a) in pairs {
        let from = files[e].rel.clone();
        let to = files[a].rel.clone();
        files[a].status = FileStatus::Moved;
        files[a].moved_from = Some(from.clone());
        files[a].moved_to = Some(to.clone());
        files[a].left_size = files[e].left_size;
        files[a].left_mtime_ms = files[e].left_mtime_ms;
        files[a].left_hash = files[e].left_hash.clone();
        files[e].status = FileStatus::Moved;
        files[e].moved_from = Some(from);
        files[e].moved_to = Some(to);
        // 列表里保留一条：用新路径那条，旧路径那条标记成同一移动的镜像，UI 合并显示。
        files[e].message = Some("move-source".into());
    }
}

/// 关闭「允许删除」时，移动等于复制，预演文案固定为这一句。
pub fn move_preview_label(allow_delete: bool) -> &'static str {
    if allow_delete { "移动" } else { "移动（保留旧文件）" }
}

fn looks_text(b: &[u8]) -> bool {
    !b.iter().take(8192).any(|c| *c == 0)
}

fn normalize(b: &[u8], opt: &ScanOptions) -> Vec<u8> {
    let s = String::from_utf8_lossy(b);
    let mut lines: Vec<String> = s.split('\n').map(|l| l.trim_end_matches('\r').to_string()).collect();
    if opt.ignore_trailing_ws {
        for l in &mut lines {
            *l = l.trim_end().to_string();
        }
    }
    let mut out = lines.join("\n");
    if !opt.ignore_crlf {
        out = out.replace('\n', "\r\n");
    }
    out.into_bytes()
}

pub fn run_scan(left: &LocalFs, right: &LocalFs, ignore: &Ignore, opt: &ScanOptions) -> Result<ScanOutput> {
    let mut out = scan_local(left, right, ignore, opt)?;
    fill_side_hashes(&mut out.files, Some(left), Some(right));
    detect_moves(&mut out.files);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn four_states_and_move_and_crlf() {
        let l = tempfile::tempdir().unwrap();
        let r = tempfile::tempdir().unwrap();
        fs::create_dir(l.path().join("src")).unwrap();
        fs::create_dir(r.path().join("src")).unwrap();
        fs::write(l.path().join("src/a.txt"), b"same\n").unwrap();
        fs::write(r.path().join("src/a.txt"), b"same\n").unwrap();
        fs::write(l.path().join("src/b.txt"), b"old\n").unwrap();
        fs::write(r.path().join("src/b.txt"), b"new\n").unwrap();
        fs::write(l.path().join("only-left.txt"), b"x").unwrap();
        fs::write(r.path().join("only-right.txt"), b"y").unwrap();
        fs::write(l.path().join("lib-format.ts"), b"export const n = 1\n").unwrap();
        fs::create_dir(r.path().join("utils")).unwrap();
        fs::write(r.path().join("utils/format.ts"), b"export const n = 1\n").unwrap();
        fs::write(l.path().join("crlf.txt"), b"a\r\nb\r\n").unwrap();
        fs::write(r.path().join("crlf.txt"), b"a\nb\n").unwrap();
        fs::create_dir_all(l.path().join(".git")).unwrap();
        fs::create_dir_all(r.path().join(".git")).unwrap();
        fs::write(l.path().join(".git/config"), b"x").unwrap();
        fs::write(r.path().join(".git/config"), b"y").unwrap();
        let out = run_scan(&LocalFs::new(l.path()), &LocalFs::new(r.path()), &Ignore::defaults(), &ScanOptions { ignore_crlf: true, ignore_trailing_ws: false }).unwrap();
        let st = |p: &str| out.files.iter().find(|f| f.rel == p).map(|f| f.status);
        assert_eq!(st("src/a.txt"), Some(FileStatus::Same));
        assert_eq!(st("src/b.txt"), Some(FileStatus::Modified));
        assert_eq!(st("only-left.txt"), Some(FileStatus::Extra));
        assert_eq!(st("only-right.txt"), Some(FileStatus::Added));
        assert_eq!(st("crlf.txt"), Some(FileStatus::Same));
        assert!(out.files.iter().all(|f| !f.rel.contains(".git")));
        let mv = out.files.iter().find(|f| f.rel == "utils/format.ts").unwrap();
        assert_eq!(mv.status, FileStatus::Moved);
        assert_eq!(mv.moved_from.as_deref(), Some("lib-format.ts"));
    }

    #[test]
    fn empty_file_is_not_a_move() {
        let l = tempfile::tempdir().unwrap();
        let r = tempfile::tempdir().unwrap();
        fs::write(l.path().join("a.txt"), b"").unwrap();
        fs::write(r.path().join("b.txt"), b"").unwrap();
        let out = run_scan(&LocalFs::new(l.path()), &LocalFs::new(r.path()), &Ignore::defaults(), &ScanOptions::default()).unwrap();
        assert!(out.files.iter().all(|f| f.status != FileStatus::Moved));
        assert_eq!(status_of(&out, "a.txt"), FileStatus::Extra);
        assert_eq!(status_of(&out, "b.txt"), FileStatus::Added);
    }

    #[test]
    fn rename_is_a_move_from_old_path_to_new_path() {
        let (l, r) = pair_roots();
        fs::write(l.path().join("old.ts"), b"body").unwrap();
        fs::write(r.path().join("new.ts"), b"body").unwrap();
        let out = scan_pair(&l, &r);
        let dest = file_of(&out, "new.ts");
        assert_eq!(dest.status, FileStatus::Moved);
        assert_eq!(dest.moved_from.as_deref(), Some("old.ts"));
        assert_eq!(dest.moved_to.as_deref(), Some("new.ts"));
        let src = file_of(&out, "old.ts");
        assert_eq!(src.status, FileStatus::Moved);
        assert_eq!(src.message.as_deref(), Some("move-source"));
        assert!(out.files.iter().all(|f| f.status != FileStatus::Added && f.status != FileStatus::Extra));
    }

    #[test]
    fn cross_directory_move_keeps_both_relative_paths() {
        let (l, r) = pair_roots();
        fs::create_dir(l.path().join("src")).unwrap();
        fs::write(l.path().join("src/old.ts"), b"body").unwrap();
        fs::create_dir(r.path().join("pkg")).unwrap();
        fs::write(r.path().join("pkg/new.ts"), b"body").unwrap();
        let out = scan_pair(&l, &r);
        let dest = file_of(&out, "pkg/new.ts");
        assert_eq!(dest.status, FileStatus::Moved);
        assert_eq!(dest.moved_from.as_deref(), Some("src/old.ts"));
        assert_eq!(file_of(&out, "src/old.ts").message.as_deref(), Some("move-source"));
    }

    #[test]
    fn same_content_pairs_by_sorted_path_and_leaves_the_rest() {
        let (l, r) = pair_roots();
        // 字典序：旧 a.txt、c.txt；新 b.txt、d.txt。按下标配成 a→b、c→d。
        for name in ["a.txt", "c.txt"] {
            fs::write(l.path().join(name), b"same").unwrap();
        }
        for name in ["b.txt", "d.txt"] {
            fs::write(r.path().join(name), b"same").unwrap();
        }
        let out = scan_pair(&l, &r);
        assert_eq!(file_of(&out, "b.txt").moved_from.as_deref(), Some("a.txt"));
        assert_eq!(file_of(&out, "d.txt").moved_from.as_deref(), Some("c.txt"));
        assert!(out.files.iter().all(|f| f.status == FileStatus::Moved));

        // 一对多：旧路径只有 m.txt，新路径 a.txt、z.txt。只配排序后的第一个新路径。
        let (l, r) = pair_roots();
        fs::write(l.path().join("m.txt"), b"same").unwrap();
        fs::write(r.path().join("z.txt"), b"same").unwrap();
        fs::write(r.path().join("a.txt"), b"same").unwrap();
        let out = scan_pair(&l, &r);
        assert_eq!(file_of(&out, "a.txt").moved_from.as_deref(), Some("m.txt"));
        assert_eq!(status_of(&out, "z.txt"), FileStatus::Added);

        // 多对一：旧路径 a.txt、z.txt，新路径只有 m.txt。只配排序后的第一个旧路径。
        let (l, r) = pair_roots();
        fs::write(l.path().join("z.txt"), b"same").unwrap();
        fs::write(l.path().join("a.txt"), b"same").unwrap();
        fs::write(r.path().join("m.txt"), b"same").unwrap();
        let out = scan_pair(&l, &r);
        assert_eq!(file_of(&out, "m.txt").moved_from.as_deref(), Some("a.txt"));
        assert_eq!(status_of(&out, "z.txt"), FileStatus::Extra);
    }

    #[test]
    fn empty_files_stay_unpaired_beside_a_real_move() {
        let (l, r) = pair_roots();
        fs::write(l.path().join("empty-old.txt"), b"").unwrap();
        fs::write(r.path().join("empty-new.txt"), b"").unwrap();
        fs::write(l.path().join("old.ts"), b"body").unwrap();
        fs::write(r.path().join("new.ts"), b"body").unwrap();
        let out = scan_pair(&l, &r);
        assert_eq!(status_of(&out, "empty-old.txt"), FileStatus::Extra);
        assert_eq!(status_of(&out, "empty-new.txt"), FileStatus::Added);
        assert_eq!(file_of(&out, "new.ts").moved_from.as_deref(), Some("old.ts"));
    }

    #[test]
    fn changed_content_is_not_a_move() {
        let (l, r) = pair_roots();
        fs::write(l.path().join("old.ts"), b"before").unwrap();
        fs::write(r.path().join("old.ts"), b"after").unwrap();
        let out = scan_pair(&l, &r);
        assert_eq!(status_of(&out, "old.ts"), FileStatus::Modified);
        assert!(out.files.iter().all(|f| f.status != FileStatus::Moved));

        let (l, r) = pair_roots();
        fs::write(l.path().join("old.ts"), b"before").unwrap();
        fs::write(r.path().join("new.ts"), b"after").unwrap();
        let out = scan_pair(&l, &r);
        assert_eq!(status_of(&out, "old.ts"), FileStatus::Extra);
        assert_eq!(status_of(&out, "new.ts"), FileStatus::Added);

        // 同路径内容变了，旁边同哈希的新文件也不把这条改动当成移动。
        let (l, r) = pair_roots();
        fs::write(l.path().join("old.ts"), b"before").unwrap();
        fs::write(r.path().join("old.ts"), b"after").unwrap();
        fs::write(r.path().join("new.ts"), b"before").unwrap();
        let out = scan_pair(&l, &r);
        assert_eq!(status_of(&out, "old.ts"), FileStatus::Modified);
        assert_eq!(status_of(&out, "new.ts"), FileStatus::Added);
    }

    fn pair_roots() -> (tempfile::TempDir, tempfile::TempDir) {
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap())
    }

    fn scan_pair(l: &tempfile::TempDir, r: &tempfile::TempDir) -> ScanOutput {
        run_scan(&LocalFs::new(l.path()), &LocalFs::new(r.path()), &Ignore::defaults(), &ScanOptions::default()).unwrap()
    }

    fn file_of(out: &ScanOutput, rel: &str) -> FileRec {
        out.files.iter().find(|f| f.rel == rel).unwrap_or_else(|| panic!("missing {rel}")).clone()
    }

    fn status_of(out: &ScanOutput, rel: &str) -> FileStatus {
        file_of(out, rel).status
    }
}
