//! 升级包来源。扫描时把目录或 zip 的真实路径记进数据库；
//! 执行时只打开这条记录，按固定块流式拷贝，不把整个文件读进内存。

use crate::error::{Error, ErrorCode, Result};
use crate::model::{ContentSource, PlanOp};
use crate::pathutil::{Ignore, RelPath};
use crate::scan::{self, FileRec, FileStatus, ScanOptions, ScanOutput};
use crate::vfs::LocalFs;
use crate::zipfs;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 流式拷贝的块大小。测试用它证明不会按整文件分配缓冲区。
pub const STREAM_CHUNK: usize = 64 * 1024;

pub fn copy_hashed(reader: &mut dyn Read, writer: &mut dyn Write, chunk: usize) -> Result<(u64, String)> {
    let chunk = chunk.max(1);
    let mut buf = vec![0u8; chunk];
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        writer.write_all(&buf[..n]).map_err(|err| Error::new(ErrorCode::Io, err.to_string()))?;
        total += n as u64;
    }
    Ok((total, hex::encode(hasher.finalize())))
}

pub fn ensure_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS package_scans (
            scan_id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            path TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS package_entries (
            scan_id TEXT NOT NULL,
            entry_name TEXT NOT NULL,
            rel TEXT NOT NULL,
            size INTEGER NOT NULL,
            sha256 TEXT NOT NULL,
            mtime_ms INTEGER,
            PRIMARY KEY (scan_id, entry_name)
        );",
    )?;
    Ok(())
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn canonical_package(path: &Path) -> Result<PathBuf> {
    fs::canonicalize(path).map_err(|err| Error::new(ErrorCode::NotFound, format!("升级包打不开：{err}")))
}

/// 扫描项目和升级包，并把升级包路径记进 `db`。返回值里没有文件内容。
pub fn scan_recorded(db: &Connection, left: &Path, right: &Path) -> Result<ScanOutput> {
    ensure_tables(db)?;
    let package = canonical_package(right)?;
    let meta = fs::symlink_metadata(&package)?;
    if meta.is_dir() {
        record_dir(db, left, &package)
    } else if is_zip_file(&package) {
        record_zip(db, left, &package)
    } else {
        Err(Error::new(ErrorCode::NotFound, "升级包需要是目录或 zip 文件"))
    }
}

fn is_zip_file(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
}

fn record_dir(db: &Connection, left: &Path, package: &Path) -> Result<ScanOutput> {
    let left_fs = LocalFs::new(left);
    let right_fs = LocalFs::new(package);
    let mut out = scan::run_scan(&left_fs, &right_fs, &Ignore::defaults(), &ScanOptions::default())?;
    let scan_id = new_scan_id();
    insert_scan(db, &scan_id, "dir", package)?;
    for file in &mut out.files {
        if !package_member(file) {
            continue;
        }
        let rel = RelPath::parse(&file.rel)?;
        let sha = right_fs.hash(&rel, true)?;
        let size = file.right_size.unwrap_or(0);
        file.content_sha256 = Some(sha.clone());
        file.entry_name = Some(file.rel.clone());
        if file.status != FileStatus::Added {
            let left_rel = file.moved_from.clone().unwrap_or_else(|| file.rel.clone());
            if let Ok(parsed) = RelPath::parse(&left_rel) {
                file.left_sha256 = left_fs.hash(&parsed, true).ok();
            }
        }
        insert_entry(db, &scan_id, &file.rel, &file.rel, size, &sha, file.right_mtime_ms)?;
    }
    out.scan_id = scan_id;
    out.package_kind = "dir".into();
    Ok(out)
}

fn record_zip(db: &Connection, left: &Path, package: &Path) -> Result<ScanOutput> {
    let left_fs = LocalFs::new(left);
    let ignore = Ignore::defaults();
    let listed = zipfs::list_files(package)?;
    let mut right: Vec<(String, String, u64, String)> = Vec::new();
    for item in &listed {
        let rel = match zip_rel(&item.name) {
            Some(rel) => rel,
            None => continue,
        };
        if ignore.matches(rel.as_str()) {
            continue;
        }
        let (size, hash) = hash_zip_entry(package, &item.name, item.size)?;
        right.push((rel.as_str().to_string(), item.name.clone(), size, hash));
    }
    let left_entries = left_fs.walk(&ignore)?;
    let mut files = compare_zip(&left_fs, &left_entries, &right)?;
    scan::detect_moves(&mut files);
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    let scan_id = new_scan_id();
    insert_scan(db, &scan_id, "zip", package)?;
    for file in &mut files {
        if !package_member(file) {
            continue;
        }
        let Some(sha) = file.content_sha256.clone().or_else(|| file.right_hash.clone()) else {
            continue;
        };
        file.content_sha256 = Some(sha.clone());
        let entry = file.entry_name.clone().unwrap_or_else(|| file.rel.clone());
        file.entry_name = Some(entry.clone());
        if file.status != FileStatus::Added {
            let left_rel = file.moved_from.clone().unwrap_or_else(|| file.rel.clone());
            if let Ok(parsed) = RelPath::parse(&left_rel) {
                file.left_sha256 = left_fs.hash(&parsed, true).ok();
            }
        }
        let size = file.right_size.unwrap_or(0);
        insert_entry(db, &scan_id, &entry, &file.rel, size, &sha, file.right_mtime_ms)?;
    }
    Ok(ScanOutput {
        files,
        case_sensitive: cfg!(all(unix, not(target_os = "macos"))),
        scan_id,
        package_kind: "zip".into(),
    })
}

fn zip_rel(name: &str) -> Option<RelPath> {
    let trimmed = name.trim_end_matches('/');
    RelPath::parse(trimmed).ok()
}

fn hash_zip_entry(package: &Path, name: &str, declared: u64) -> Result<(u64, String)> {
    zipfs::with_entry(package, name, |reader, size| {
        let mut sink = io::sink();
        let (written, hash) = copy_hashed(reader, &mut sink, STREAM_CHUNK)?;
        if written > declared.max(size) {
            return Err(Error::new(ErrorCode::ZipBomb, "实际读出的字节超过条目声明的大小，已中止"));
        }
        Ok((written, hash))
    })
}

fn compare_zip(left_fs: &LocalFs, left: &[crate::vfs::Entry], right: &[(String, String, u64, String)]) -> Result<Vec<FileRec>> {
    use crate::vfs::EntryKind;
    use std::collections::HashMap;
    let case_sensitive = cfg!(all(unix, not(target_os = "macos")));
    let key = |rel: &str| if case_sensitive { rel.to_string() } else { rel.to_lowercase() };
    let mut lm: HashMap<String, &crate::vfs::Entry> = HashMap::new();
    for entry in left {
        if entry.kind == EntryKind::Dir {
            continue;
        }
        lm.entry(key(&entry.rel)).or_insert(entry);
    }
    let mut rm: HashMap<String, &(String, String, u64, String)> = HashMap::new();
    for item in right {
        rm.entry(key(&item.0)).or_insert(item);
    }
    let mut keys: Vec<String> = lm.keys().chain(rm.keys()).cloned().collect();
    keys.sort();
    keys.dedup();
    let mut files = Vec::new();
    for k in keys {
        match (lm.get(&k).copied(), rm.get(&k).copied()) {
            (Some(l), None) => {
                let sha = if l.kind == EntryKind::Symlink {
                    None
                } else {
                    RelPath::parse(&l.rel).ok().and_then(|rel| left_fs.hash(&rel, true).ok())
                };
                files.push(side_rec(l, FileStatus::Extra, true, sha));
            }
            (None, Some((rel, entry, size, hash))) => {
                files.push(added_rec(rel, entry, *size, hash));
            }
            (Some(l), Some((rel, entry, size, hash))) => {
                if l.kind == EntryKind::Symlink {
                    files.push(side_rec(l, FileStatus::Symlink, true, None));
                    continue;
                }
                let left_sha = RelPath::parse(&l.rel).ok().and_then(|p| left_fs.hash(&p, true).ok());
                let same = l.size == *size && left_sha.as_deref() == Some(hash.as_str());
                let status = if same { FileStatus::Same } else { FileStatus::Modified };
                files.push(FileRec {
                    rel: rel.clone(),
                    status,
                    left_size: Some(l.size),
                    right_size: Some(*size),
                    left_mtime_ms: l.mtime_ms,
                    right_mtime_ms: None,
                    hash: left_sha.clone().or(Some(hash.clone())),
                    left_hash: left_sha.clone(),
                    right_hash: Some(hash.clone()),
                    moved_from: None,
                    moved_to: None,
                    case_conflict: false,
                    message: None,
                    readonly: l.readonly,
                    link_target: None,
                    content_sha256: Some(hash.clone()),
                    entry_name: Some(entry.clone()),
                    left_sha256: left_sha,
                });
            }
            (None, None) => {}
        }
    }
    Ok(files)
}

fn side_rec(entry: &crate::vfs::Entry, status: FileStatus, left: bool, sha: Option<String>) -> FileRec {
    use crate::vfs::EntryKind;
    let symlink = entry.kind == EntryKind::Symlink || status == FileStatus::Symlink;
    FileRec {
        rel: entry.rel.clone(),
        status: if symlink { FileStatus::Symlink } else { status },
        left_size: left.then_some(entry.size),
        right_size: (!left).then_some(entry.size),
        left_mtime_ms: left.then_some(entry.mtime_ms).flatten(),
        right_mtime_ms: (!left).then_some(entry.mtime_ms).flatten(),
        hash: sha.clone(),
        left_hash: left.then(|| sha.clone()).flatten(),
        right_hash: (!left).then(|| sha.clone()).flatten(),
        moved_from: None,
        moved_to: None,
        case_conflict: false,
        message: symlink.then(|| "符号链接只提示，不处理".to_string()),
        readonly: entry.readonly || symlink,
        link_target: entry.link_target.clone(),
        content_sha256: None,
        entry_name: None,
        left_sha256: left.then_some(sha).flatten(),
    }
}

fn added_rec(rel: &str, entry: &str, size: u64, hash: &str) -> FileRec {
    FileRec {
        rel: rel.to_string(),
        status: FileStatus::Added,
        left_size: None,
        right_size: Some(size),
        left_mtime_ms: None,
        right_mtime_ms: None,
        hash: Some(hash.to_string()),
        left_hash: None,
        right_hash: Some(hash.to_string()),
        moved_from: None,
        moved_to: None,
        case_conflict: false,
        message: None,
        readonly: false,
        link_target: None,
        content_sha256: Some(hash.to_string()),
        entry_name: Some(entry.to_string()),
        left_sha256: None,
    }
}

fn package_member(file: &FileRec) -> bool {
    if file.message.as_deref() == Some("move-source") {
        return false;
    }
    matches!(file.status, FileStatus::Added | FileStatus::Modified | FileStatus::Moved)
}

fn new_scan_id() -> String {
    format!("scan-{}", uuid::Uuid::new_v4().simple())
}

fn insert_scan(db: &Connection, scan_id: &str, kind: &str, path: &Path) -> Result<()> {
    db.execute(
        "INSERT INTO package_scans (scan_id, kind, path, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![scan_id, kind, path.display().to_string(), now_ms()],
    )?;
    Ok(())
}

fn insert_entry(db: &Connection, scan_id: &str, entry: &str, rel: &str, size: u64, sha: &str, mtime_ms: Option<i64>) -> Result<()> {
    db.execute(
        "INSERT INTO package_entries (scan_id, entry_name, rel, size, sha256, mtime_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![scan_id, entry, rel, size as i64, sha, mtime_ms],
    )?;
    Ok(())
}

struct RecordedEntry {
    kind: String,
    path: PathBuf,
    rel: String,
    size: u64,
    sha256: String,
}

fn load_entry(db: &Connection, source: &ContentSource) -> Result<RecordedEntry> {
    let (kind, path): (String, String) = db
        .query_row(
            "SELECT kind, path FROM package_scans WHERE scan_id=?1",
            params![source.scan_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Error::new(ErrorCode::NotFound, "找不到这次扫描记录的升级包，请重新扫描"),
            other => other.into(),
        })?;
    if !source.kind.is_empty() && source.kind != kind {
        return Err(Error::new(ErrorCode::InvalidState, "计划里的升级包类型和扫描记录不一致"));
    }
    if !source.path.is_empty() && source.path != path {
        return Err(Error::new(ErrorCode::InvalidState, "计划里的升级包路径和扫描记录不一致"));
    }
    let (rel, size, sha): (String, i64, String) = db
        .query_row(
            "SELECT rel, size, sha256 FROM package_entries WHERE scan_id=?1 AND entry_name=?2",
            params![source.scan_id, source.entry],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Error::new(ErrorCode::NotFound, format!("升级包里没有 {}", source.entry)),
            other => other.into(),
        })?;
    Ok(RecordedEntry {
        kind,
        path: PathBuf::from(path),
        rel,
        size: size as u64,
        sha256: sha,
    })
}

/// 升级包里的这个文件不能当文本合并：超过 1MB、含有空字节，或读不出来。
pub fn source_unmergeable(db: &Connection, source: &ContentSource) -> Result<bool> {
    let recorded = load_entry(db, source)?;
    if recorded.size > 1024 * 1024 {
        return Ok(true);
    }
    if recorded.kind == "zip" {
        return match zipfs::with_entry(&recorded.path, &source.entry, |reader, _declared| {
            let mut buf = [0u8; 8192];
            let n = std::io::Read::read(reader, &mut buf).unwrap_or(0);
            Ok(buf[..n].contains(&0))
        }) {
            Ok(binary) => Ok(binary),
            Err(_) => Ok(true),
        };
    }
    let Ok(rel) = RelPath::parse(&source.entry) else {
        return Ok(true);
    };
    let mut path = recorded.path.clone();
    for part in rel.as_str().split('/') {
        path.push(part);
    }
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(_) => return Ok(true),
    };
    let mut buf = [0u8; 8192];
    let n = match file.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return Ok(true),
    };
    Ok(buf[..n].contains(&0))
}

/// 按扫描记录把一个文件流式写到 `dest`。哈希或大小和计划不一致就删掉 `dest` 并返回错误。
pub fn stream_plan_file(db: &Connection, op: &PlanOp, dest: &Path) -> Result<()> {
    let source = op.source.as_ref().ok_or_else(|| Error::new(ErrorCode::InvalidState, "计划缺少升级包来源，不能写入"))?;
    let expect_hash = op.content_hash.as_deref().filter(|h| !h.is_empty()).ok_or_else(|| {
        Error::new(ErrorCode::InvalidState, "计划缺少内容哈希，不能写入")
    })?;
    let expect_size = op.size.ok_or_else(|| Error::new(ErrorCode::InvalidState, "计划缺少文件大小，不能写入"))?;
    let recorded = load_entry(db, source)?;
    if recorded.rel != op.path {
        return Err(Error::new(ErrorCode::InvalidState, "计划路径和扫描记录不一致"));
    }
    if recorded.sha256 != expect_hash || recorded.size != expect_size {
        return Err(Error::new(ErrorCode::InvalidState, "计划里的哈希或大小和扫描记录不一致"));
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut out = OpenOptions::new().write(true).create_new(true).open(dest)?;
    let streamed = if recorded.kind == "zip" {
        let entry = source.entry.clone();
        let limit = recorded.size;
        zipfs::with_entry(&recorded.path, &entry, |reader, declared| {
            let (written, hashed) = copy_hashed(reader, &mut out, STREAM_CHUNK)?;
            if written > declared.max(limit) {
                return Err(Error::new(ErrorCode::ZipBomb, "实际写出的字节超过条目声明的大小，已中止"));
            }
            Ok((written, hashed))
        })
    } else {
        stream_dir_file(&recorded.path, &source.entry, &mut out)
    };
    let streamed = match streamed {
        Ok(pair) => pair,
        Err(err) => {
            drop(out);
            let _ = fs::remove_file(dest);
            return Err(err);
        }
    };
    if let Err(err) = out.sync_all() {
        drop(out);
        let _ = fs::remove_file(dest);
        return Err(err.into());
    }
    drop(out);
    let (written, hash) = streamed;
    if written != expect_size || hash != expect_hash {
        let _ = fs::remove_file(dest);
        return Err(Error::new(
            ErrorCode::VerifyFailed,
            format!("{} 的升级包内容和计划里的哈希不一致，已中止", op.path),
        ));
    }
    Ok(())
}

fn stream_dir_file(root: &Path, entry: &str, out: &mut File) -> Result<(u64, String)> {
    let rel = RelPath::parse(entry)?;
    let mut path = root.to_path_buf();
    for part in rel.as_str().split('/') {
        path.push(part);
    }
    let meta = fs::symlink_metadata(&path).map_err(|err| Error::new(ErrorCode::NotFound, err.to_string()))?;
    if meta.file_type().is_symlink() {
        return Err(Error::new(ErrorCode::SymlinkRefused, format!("{entry} 是符号链接，不读取目标")));
    }
    let mut input = File::open(&path)?;
    copy_hashed(&mut input, out, STREAM_CHUNK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{self, ExecOpts, RollbackChoice};
    use crate::fault::FaultPlan;
    use crate::hashutil::sha256_hex;
    use crate::model::{ExecPlan, OpKind, PlanOp, Summary};
    use crate::zipfs::ZipListed;
    use std::io::Cursor;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    struct Cap<R> {
        inner: R,
        cap: usize,
    }

    impl<R: Read> Read for Cap<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if buf.len() > self.cap {
                return Err(io::Error::new(io::ErrorKind::Other, "read buffer exceeds streaming cap"));
            }
            self.inner.read(buf)
        }
    }

    #[test]
    fn copy_hashed_rejects_a_buffer_bigger_than_the_chunk() {
        let data = vec![7u8; STREAM_CHUNK * 3 + 50];
        let mut reader = Cap { inner: Cursor::new(data.clone()), cap: STREAM_CHUNK };
        let mut out = Vec::new();
        let (n, hash) = copy_hashed(&mut reader, &mut out, STREAM_CHUNK).unwrap();
        assert!(data.len() > STREAM_CHUNK);
        assert_eq!(n, data.len() as u64);
        assert_eq!(out, data);
        assert_eq!(hash, sha256_hex(&data));
    }

    fn opts(dir: &Path) -> ExecOpts {
        ExecOpts {
            allow_readonly: false,
            backup_root: dir.join("bak"),
            remote: false,
            stop_after_log: None,
            corrupt_temp: false,
        }
    }

    fn write_tree(root: &Path, files: &[(&str, &[u8])]) {
        for (rel, bytes) in files {
            let path = root.join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, bytes).unwrap();
        }
    }

    fn plan_from(scan: &ScanOutput, id: &str) -> ExecPlan {
        let mut ops = Vec::new();
        let mut summary = Summary::default();
        for file in &scan.files {
            if file.message.as_deref() == Some("move-source") {
                continue;
            }
            let kind = match file.status {
                FileStatus::Added => OpKind::Create,
                FileStatus::Modified => OpKind::Overwrite,
                FileStatus::Moved => OpKind::Move,
                _ => continue,
            };
            match kind {
                OpKind::Create => summary.create += 1,
                OpKind::Overwrite => summary.overwrite += 1,
                OpKind::Move => {
                    summary.move_count += 1;
                    summary.move_keep_source += 1;
                }
                OpKind::Delete => summary.delete += 1,
            }
            let index = ops.len() as u32 + 1;
            ops.push(PlanOp {
                index,
                kind,
                path: file.rel.clone(),
                from: if kind == OpKind::Move { file.moved_from.clone() } else { None },
                bytes: None,
                expect: if kind == OpKind::Create {
                    None
                } else {
                    Some(crate::model::Snapshot {
                        size: file.left_size.unwrap_or(0),
                        mtime_ms: file.left_mtime_ms,
                        hash: file.left_sha256.clone().unwrap_or_default(),
                    })
                },
                mode: None,
                source_mtime_ms: file.right_mtime_ms,
                merged: false,
                keep_source: kind == OpKind::Move,
                readonly: file.readonly,
                label: match kind {
                    OpKind::Create => "升级包",
                    OpKind::Move => "移动（保留旧文件）",
                    _ => "覆盖",
                }
                .into(),
                content_hash: file.content_sha256.clone(),
                size: file.right_size,
                source: Some(ContentSource {
                    scan_id: scan.scan_id.clone(),
                    kind: scan.package_kind.clone(),
                    entry: file.entry_name.clone().unwrap_or_else(|| file.rel.clone()),
                    path: String::new(),
                }),
            });
        }
        ExecPlan {
            id: id.into(),
            project_key: "proj".into(),
            project_root: String::new(),
            ops,
            summary,
        }
    }

    fn journal_plan(db: &Connection, id: &str) -> String {
        db.query_row("SELECT plan_json FROM exec_journal WHERE exec_id=?1", params![id], |row| row.get(0)).unwrap()
    }

    #[test]
    fn directory_package_streams_without_putting_bytes_in_the_plan() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        let package = dir.path().join("pkg");
        let big = vec![9u8; STREAM_CHUNK * 2 + 17];
        assert!(big.len() > STREAM_CHUNK);
        write_tree(&project, &[("a.txt", b"old-a"), ("big.bin", b"old-big"), ("keep.txt", b"same")]);
        write_tree(&package, &[("a.txt", b"new-a"), ("big.bin", big.as_slice()), ("keep.txt", b"same"), ("sub/c.txt", b"created")]);
        let db = exec::open_db(&dir.path().join("t.sqlite")).unwrap();
        let scan = scan_recorded(&db, &project, &package).unwrap();
        assert_eq!(scan.package_kind, "dir");
        assert!(!scan.scan_id.is_empty());
        let encoded = serde_json::to_string(&scan).unwrap();
        assert!(!encoded.contains("\"bytes\""), "{encoded}");
        let plan = plan_from(&scan, "dir-1");
        let plan_json = serde_json::to_string(&plan).unwrap();
        assert!(!plan_json.contains("\"bytes\""), "{plan_json}");
        assert!(plan_json.contains("contentHash"));
        assert!(plan_json.contains(&scan.scan_id));
        let report = exec::execute(&db, &LocalFs::new(&project), &plan, &opts(dir.path()), FaultPlan::default()).unwrap();
        assert_eq!(report.state, "COMMITTED", "{:?}", report.files);
        assert_eq!(fs::read(project.join("a.txt")).unwrap(), b"new-a");
        assert_eq!(fs::read(project.join("big.bin")).unwrap(), big);
        assert_eq!(fs::read(project.join("sub/c.txt")).unwrap(), b"created");
        assert_eq!(fs::read(project.join("keep.txt")).unwrap(), b"same");
        let stored = journal_plan(&db, "dir-1");
        assert!(!stored.contains("\"bytes\""), "{stored}");
    }

    #[test]
    fn zip_package_streams_many_entries_and_a_large_file() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        fs::create_dir_all(project.join("nested")).unwrap();
        fs::write(project.join("a.txt"), b"old").unwrap();
        let zip_path = dir.path().join("pkg.zip");
        let big: Vec<u8> = (0..STREAM_CHUNK + 128).map(|i| (i % 251) as u8).collect();
        let mut writer = ZipWriter::new(File::create(&zip_path).unwrap());
        let opt = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("a.txt", opt).unwrap();
        writer.write_all(b"from-zip").unwrap();
        writer.start_file("nested/b.txt", opt).unwrap();
        writer.write_all(b"bee").unwrap();
        writer.start_file("nested/big.bin", opt).unwrap();
        writer.write_all(&big).unwrap();
        writer.finish().unwrap();
        let db = exec::open_db(&dir.path().join("t.sqlite")).unwrap();
        let scan = scan_recorded(&db, &project, &zip_path).unwrap();
        assert_eq!(scan.package_kind, "zip");
        let plan = plan_from(&scan, "zip-1");
        assert!(plan.ops.len() >= 3, "{:?}", plan.ops.iter().map(|op| &op.path).collect::<Vec<_>>());
        assert!(plan.ops.iter().all(|op| op.bytes.is_none()));
        let report = exec::execute(&db, &LocalFs::new(&project), &plan, &opts(dir.path()), FaultPlan::default()).unwrap();
        assert_eq!(report.state, "COMMITTED", "{:?}", report.files);
        assert_eq!(fs::read(project.join("a.txt")).unwrap(), b"from-zip");
        assert_eq!(fs::read(project.join("nested/b.txt")).unwrap(), b"bee");
        assert_eq!(fs::read(project.join("nested/big.bin")).unwrap(), big);
        assert!(!journal_plan(&db, "zip-1").contains("\"bytes\""));
    }

    #[test]
    fn hash_mismatch_stops_and_can_roll_back() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        let package = dir.path().join("pkg");
        write_tree(&project, &[("a.txt", b"old-a"), ("b.txt", b"old-b")]);
        write_tree(&package, &[("a.txt", b"new-a"), ("b.txt", b"new-b")]);
        let db = exec::open_db(&dir.path().join("t.sqlite")).unwrap();
        let scan = scan_recorded(&db, &project, &package).unwrap();
        fs::write(package.join("b.txt"), b"tampered").unwrap();
        let plan = plan_from(&scan, "bad-1");
        let report = exec::execute(&db, &LocalFs::new(&project), &plan, &opts(dir.path()), FaultPlan::default()).unwrap();
        assert_eq!(report.state, "STOPPED", "{:?}", report.files);
        assert!(report.files.iter().any(|file| file.path == "b.txt" && file.code.as_deref() == Some("E_VERIFY_FAILED")), "{:?}", report.files);
        assert_eq!(fs::read(project.join("b.txt")).unwrap(), b"old-b");
        assert_eq!(fs::read(project.join("a.txt")).unwrap(), b"new-a");
        let rolled = exec::rollback(&db, &LocalFs::new(&project), "bad-1", RollbackChoice::ProceedBackupCurrent, FaultPlan::default()).unwrap();
        assert_eq!(rolled.state, "ROLLED_BACK", "{:?}", rolled.files);
        assert_eq!(fs::read(project.join("a.txt")).unwrap(), b"old-a");
        assert_eq!(fs::read(project.join("b.txt")).unwrap(), b"old-b");
    }

    #[test]
    fn client_path_is_not_opened() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        let package = dir.path().join("pkg");
        let evil = dir.path().join("evil");
        write_tree(&project, &[("a.txt", b"old")]);
        write_tree(&package, &[("a.txt", b"new")]);
        write_tree(&evil, &[("a.txt", b"pwned")]);
        let db = exec::open_db(&dir.path().join("t.sqlite")).unwrap();
        let scan = scan_recorded(&db, &project, &package).unwrap();
        let mut plan = plan_from(&scan, "path-1");
        plan.ops[0].source.as_mut().unwrap().path = evil.display().to_string();
        let report = exec::execute(&db, &LocalFs::new(&project), &plan, &opts(dir.path()), FaultPlan::default()).unwrap();
        assert_eq!(report.state, "STOPPED", "{:?}", report.files);
        let message = report.files.iter().find_map(|file| file.error.clone()).unwrap_or_default();
        assert!(message.contains("计划里的升级包路径和扫描记录不一致"), "{message}");
        assert_eq!(fs::read(project.join("a.txt")).unwrap(), b"old");
        assert_eq!(fs::read(evil.join("a.txt")).unwrap(), b"pwned");
    }

    #[test]
    fn legacy_bytes_plan_is_rejected() {
        let raw = serde_json::json!({
            "id": "old",
            "projectKey": "p",
            "ops": [{
                "index": 1,
                "kind": "overwrite",
                "path": "a.txt",
                "from": null,
                "bytes": [97, 98, 99],
                "expect": null,
                "mode": null,
                "sourceMtimeMs": null,
                "merged": false,
                "keepSource": false,
                "readonly": false,
                "label": "覆盖"
            }],
            "summary": { "create": 0, "overwrite": 1, "moveCount": 0, "delete": 0, "merged": 0, "moveKeepSource": 0 }
        });
        let err = crate::model::parse_client_plan(raw).unwrap_err();
        assert_eq!(err.message, crate::model::LEGACY_BYTES_PLAN);
    }

    #[test]
    fn merged_inline_bytes_are_accepted_and_other_files_still_stream() {
        let raw = serde_json::json!({
            "id": "mix",
            "projectKey": "p",
            "ops": [{
                "index": 1,
                "kind": "overwrite",
                "path": "a.txt",
                "from": null,
                "bytes": [69, 68, 73, 84],
                "expect": null,
                "mode": null,
                "sourceMtimeMs": null,
                "merged": true,
                "keepSource": false,
                "readonly": false,
                "label": "合并结果"
            }],
            "summary": { "create": 0, "overwrite": 1, "moveCount": 0, "delete": 0, "merged": 1, "moveKeepSource": 0 }
        });
        let parsed = crate::model::parse_client_plan(raw).unwrap();
        assert_eq!(parsed.ops[0].bytes.as_deref(), Some(b"EDIT".as_slice()));

        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        let package = dir.path().join("pkg");
        write_tree(&project, &[("a.txt", b"old-a"), ("b.txt", b"old-b")]);
        write_tree(&package, &[("a.txt", b"pkg-a"), ("b.txt", b"pkg-b")]);
        let db = exec::open_db(&dir.path().join("t.sqlite")).unwrap();
        let scan = scan_recorded(&db, &project, &package).unwrap();
        let mut plan = plan_from(&scan, "mix-1");
        let inline = plan.ops.iter_mut().find(|op| op.path == "a.txt").unwrap();
        inline.bytes = Some(b"EDITED".to_vec());
        inline.merged = true;
        let report = exec::execute(&db, &LocalFs::new(&project), &plan, &opts(dir.path()), FaultPlan::default()).unwrap();
        assert_eq!(report.state, "COMMITTED", "{:?}", report.files);
        assert_eq!(fs::read(project.join("a.txt")).unwrap(), b"EDITED");
        assert_eq!(fs::read(project.join("b.txt")).unwrap(), b"pkg-b");
        let stored: ExecPlan = serde_json::from_str(&journal_plan(&db, "mix-1")).unwrap();
        let inline = stored.ops.iter().find(|op| op.path == "a.txt").unwrap();
        let streamed = stored.ops.iter().find(|op| op.path == "b.txt").unwrap();
        assert_eq!(inline.bytes.as_deref(), Some(b"EDITED".as_slice()));
        assert!(streamed.bytes.is_none());
        assert!(streamed.content_hash.as_deref().unwrap_or("").len() == 64);
    }

    #[test]
    fn listed_zip_names_do_not_include_directories() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("names.zip");
        let mut writer = ZipWriter::new(File::create(&zip_path).unwrap());
        writer.add_directory("nested/", SimpleFileOptions::default()).unwrap();
        writer.start_file("nested/a.txt", SimpleFileOptions::default()).unwrap();
        writer.write_all(b"a").unwrap();
        writer.start_file("b.txt", SimpleFileOptions::default()).unwrap();
        writer.write_all(b"b").unwrap();
        writer.finish().unwrap();
        let names = zipfs::list_files(&zip_path).unwrap();
        let mut got: Vec<_> = names.iter().map(|item| item.name.as_str()).collect();
        got.sort();
        assert_eq!(got, vec!["b.txt", "nested/a.txt"]);
        let _ = ZipListed { name: "b.txt".into(), size: 1 };
    }
}
