//! 本地文件系统。不跟随符号链接；写操作拒绝路径上的符号链接；
//! 原子替换由执行引擎调用这里的「临时文件 / 读回 / 改名」原语完成。

use crate::error::{Error, ErrorCode, Result};
use crate::hashutil::{hash_file_sha256, hash_file_xxh};
use crate::pathutil::{Ignore, RelPath};
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub rel: String,
    pub kind: EntryKind,
    pub size: u64,
    pub mtime_ms: Option<i64>,
    pub mode: Option<u32>,
    pub link_target: Option<String>,
    pub readonly: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadonlyFlag {
    Mode(u32),
    #[cfg(target_os = "macos")]
    Immutable,
}

#[derive(Clone)]
pub struct LocalFs {
    root: PathBuf,
    readonly: bool,
}

impl LocalFs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), readonly: false }
    }
    pub fn read_only(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), readonly: true }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn is_readonly_fs(&self) -> bool {
        self.readonly
    }

    pub fn abs(&self, rel: &RelPath) -> Result<PathBuf> {
        let mut p = self.root.clone();
        for part in rel.as_str().split('/') {
            if part == ".." || part == "." || part.is_empty() {
                return Err(Error::new(ErrorCode::ZipSlip, "路径穿越"));
            }
            p.push(part);
        }
        Ok(long_path(&p))
    }

    pub fn walk(&self, ignore: &Ignore) -> Result<Vec<Entry>> {
        let mut out = Vec::new();
        if !self.root.exists() {
            return Err(Error::new(ErrorCode::NotFound, "目录不存在"));
        }
        self.walk_dir(&self.root, "", ignore, &mut out)?;
        Ok(out)
    }

    fn walk_dir(&self, dir: &Path, prefix: &str, ignore: &Ignore, out: &mut Vec<Entry>) -> Result<()> {
        let rd = match fs::read_dir(dir) {
            Ok(r) => r,
            Err(e) => {
                if prefix.is_empty() {
                    return Err(e.into());
                }
                return Ok(());
            }
        };
        let mut names: Vec<_> = rd.filter_map(|e| e.ok()).collect();
        names.sort_by_key(|e| e.file_name());
        for ent in names {
            let name = ent.file_name().to_string_lossy().to_string();
            let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            if ignore.matches(&rel) {
                continue;
            }
            let meta = match fs::symlink_metadata(ent.path()) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.file_type().is_symlink() {
                let target = fs::read_link(ent.path()).ok().map(|p| p.to_string_lossy().to_string());
                out.push(Entry {
                    rel,
                    kind: EntryKind::Symlink,
                    size: target.as_ref().map(|s| s.len() as u64).unwrap_or(0),
                    mtime_ms: mtime_ms(&meta),
                    mode: mode_of(&meta),
                    link_target: target,
                    readonly: true,
                });
                continue;
            }
            if meta.is_dir() {
                out.push(Entry {
                    rel: rel.clone(),
                    kind: EntryKind::Dir,
                    size: 0,
                    mtime_ms: mtime_ms(&meta),
                    mode: mode_of(&meta),
                    link_target: None,
                    readonly: false,
                });
                self.walk_dir(&ent.path(), &rel, ignore, out)?;
            } else if meta.is_file() {
                out.push(Entry {
                    rel,
                    kind: EntryKind::File,
                    size: meta.len(),
                    mtime_ms: mtime_ms(&meta),
                    mode: mode_of(&meta),
                    link_target: None,
                    readonly: is_mode_readonly(&meta),
                });
            }
        }
        Ok(())
    }

    pub fn stat(&self, rel: &RelPath) -> Result<Entry> {
        let p = self.abs(rel)?;
        let meta = fs::symlink_metadata(&p)?;
        let kind = if meta.file_type().is_symlink() {
            EntryKind::Symlink
        } else if meta.is_dir() {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        Ok(Entry {
            rel: rel.as_str().to_string(),
            kind,
            size: if kind == EntryKind::File { meta.len() } else { 0 },
            mtime_ms: mtime_ms(&meta),
            mode: mode_of(&meta),
            link_target: if kind == EntryKind::Symlink {
                fs::read_link(&p).ok().map(|x| x.to_string_lossy().to_string())
            } else {
                None
            },
            readonly: is_mode_readonly(&meta) || uchg(&p),
        })
    }

    pub fn read(&self, rel: &RelPath) -> Result<Vec<u8>> {
        self.guard_read(rel)?;
        Ok(fs::read(self.abs(rel)?)?)
    }

    pub fn hash(&self, rel: &RelPath, sha256: bool) -> Result<String> {
        self.guard_read(rel)?;
        let p = self.abs(rel)?;
        if sha256 {
            Ok(hash_file_sha256(&p)?)
        } else {
            Ok(hash_file_xxh(&p)?)
        }
    }

    fn guard_read(&self, rel: &RelPath) -> Result<()> {
        let p = self.abs(rel)?;
        let meta = fs::symlink_metadata(&p)?;
        if meta.file_type().is_symlink() {
            return Err(Error::new(ErrorCode::SymlinkRefused, format!("{rel} 是符号链接，不读取目标")));
        }
        Ok(())
    }

    pub fn symlink_in_parents(&self, rel: &RelPath) -> Result<bool> {
        let mut cur = self.root.clone();
        if fs::symlink_metadata(&cur).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
            return Ok(true);
        }
        for part in rel.as_str().split('/') {
            cur.push(part);
            if cur == self.abs(rel)? {
                break;
            }
            if fs::symlink_metadata(&cur).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                return Ok(true);
            }
        }
        // 最后一级如果是符号链接也算
        let full = self.abs(rel)?;
        if full.exists() || fs::symlink_metadata(&full).is_ok() {
            if fs::symlink_metadata(&full).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn ensure_writable_tree(&self, rel: &RelPath) -> Result<()> {
        if self.readonly {
            return Err(Error::new(ErrorCode::Permission, "升级包是只读的"));
        }
        if self.symlink_in_parents(rel)? {
            return Err(Error::new(
                ErrorCode::SymlinkInPath,
                format!("{rel} 的路径上有符号链接，拒绝写入"),
            ));
        }
        Ok(())
    }

    /// 只分配临时文件名，不创建文件。调用方先把这个名字写进事务日志，再 `create_temp_file`。
    pub fn allocate_temp(&self, rel: &RelPath) -> Result<RelPath> {
        self.ensure_writable_tree(rel)?;
        if let Some(parent) = rel.parent() {
            self.mkdir_p(&parent)?;
        }
        let name = rel.name();
        let tmp_name = crate::brand::temp_file_name(name, &uuid::Uuid::new_v4().simple().to_string());
        match rel.parent() {
            Some(p) => RelPath::parse(&format!("{p}/{tmp_name}")),
            None => RelPath::parse(&tmp_name),
        }
    }

    /// 在已经登记的路径上创建临时文件。写到一半失败时删掉这个文件。
    pub fn create_temp_file(&self, tmp_rel: &RelPath, data: &[u8]) -> Result<()> {
        self.ensure_writable_tree(tmp_rel)?;
        if let Some(parent) = tmp_rel.parent() {
            self.mkdir_p(&parent)?;
        }
        let path = self.abs(tmp_rel)?;
        let mut f = OpenOptions::new().write(true).create_new(true).open(&path)?;
        let write_result = (|| -> std::io::Result<()> {
            #[cfg(test)]
            if fail_mid_write() {
                if !data.is_empty() {
                    f.write_all(&data[..1])?;
                }
                return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "写入中途失败"));
            }
            f.write_all(data)?;
            f.sync_all()?;
            Ok(())
        })();
        if let Err(e) = write_result {
            drop(f);
            let _ = fs::remove_file(&path);
            return Err(e.into());
        }
        Ok(())
    }

    pub fn write_temp(&self, rel: &RelPath, data: &[u8]) -> Result<RelPath> {
        let tmp_rel = self.allocate_temp(rel)?;
        self.create_temp_file(&tmp_rel, data)?;
        Ok(tmp_rel)
    }

    pub fn read_temp_hash(&self, tmp: &RelPath) -> Result<String> {
        Ok(hash_file_sha256(&self.abs(tmp)?)?)
    }

    pub fn remove_path(&self, rel: &RelPath) -> Result<()> {
        self.ensure_writable_tree(rel)?;
        let p = self.abs(rel)?;
        let meta = fs::symlink_metadata(&p)?;
        if meta.file_type().is_symlink() {
            return Err(Error::new(ErrorCode::SymlinkRefused, format!("不删除符号链接 {rel}")));
        }
        if meta.is_dir() {
            fs::remove_dir(&p)?;
        } else {
            fs::remove_file(&p)?;
        }
        Ok(())
    }

    pub fn rename_over(&self, tmp: &RelPath, dest: &RelPath) -> Result<()> {
        self.ensure_writable_tree(dest)?;
        let from = self.abs(tmp)?;
        let to = self.abs(dest)?;
        if to.exists() || fs::symlink_metadata(&to).is_ok() {
            let meta = fs::symlink_metadata(&to)?;
            if meta.file_type().is_symlink() {
                let _ = fs::remove_file(&from);
                return Err(Error::new(ErrorCode::SymlinkRefused, format!("拒绝用普通文件覆盖符号链接 {dest}")));
            }
        }
        atomic_rename(&from, &to)?;
        Ok(())
    }

    pub fn rename_over_counted(
        &self,
        tmp: &RelPath,
        dest: &RelPath,
        before_dir_fsync: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        self.ensure_writable_tree(dest)?;
        let from = self.abs(tmp)?;
        let to = self.abs(dest)?;
        if to.exists() || fs::symlink_metadata(&to).is_ok() {
            let meta = fs::symlink_metadata(&to)?;
            if meta.file_type().is_symlink() {
                let _ = fs::remove_file(&from);
                return Err(Error::new(ErrorCode::SymlinkRefused, format!("拒绝用普通文件覆盖符号链接 {dest}")));
            }
        }
        atomic_rename_counted(&from, &to, before_dir_fsync)?;
        Ok(())
    }

    pub fn mkdir_p(&self, rel: &RelPath) -> Result<Vec<String>> {
        if self.readonly {
            return Err(Error::new(ErrorCode::Permission, "升级包是只读的"));
        }
        let mut created = Vec::new();
        let mut acc = String::new();
        for part in rel.as_str().split('/') {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(part);
            let rp = RelPath::parse(&acc)?;
            if self.symlink_in_parents(&rp)? {
                return Err(Error::new(ErrorCode::SymlinkInPath, format!("{acc} 的路径上有符号链接")));
            }
            let p = self.abs(&rp)?;
            if p.exists() {
                if fs::symlink_metadata(&p)?.file_type().is_symlink() {
                    return Err(Error::new(ErrorCode::SymlinkInPath, format!("{acc} 是符号链接")));
                }
                continue;
            }
            fs::create_dir(&p)?;
            created.push(acc.clone());
        }
        Ok(created)
    }

    pub fn copy_to(&self, rel: &RelPath, dest: &Path) -> Result<()> {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let src = self.abs(rel)?;
        let meta = fs::symlink_metadata(&src)?;
        if meta.file_type().is_symlink() {
            return Err(Error::new(ErrorCode::SymlinkRefused, "不备份符号链接目标内容"));
        }
        fs::copy(&src, dest)?;
        let file = File::open(dest)?;
        file.sync_all()?;
        Ok(())
    }

    /// `register` 在创建临时文件之前调用，把名字写进事务日志。失败时删掉临时文件并调用 `forget`。
    pub fn copy_in_atomic<R, G>(
        &self,
        src: &Path,
        dest: &RelPath,
        mode: Option<u32>,
        mtime_ms: Option<i64>,
        before_dir_fsync: impl FnOnce() -> Result<()>,
        register: R,
        forget: G,
    ) -> Result<()>
    where
        R: FnOnce(&RelPath) -> Result<()>,
        G: FnOnce(&RelPath),
    {
        let data = fs::read(src)?;
        let tmp = self.allocate_temp(dest)?;
        register(&tmp)?;
        let step = (|| -> Result<()> {
            self.create_temp_file(&tmp, &data)?;
            if let Some(mode) = mode {
                set_mode(&self.abs(&tmp)?, mode)?;
            }
            self.rename_over_counted(&tmp, dest, before_dir_fsync)?;
            if mtime_ms.is_some() || mode.is_some() {
                self.set_meta(dest, mode, mtime_ms)?;
            }
            Ok(())
        })();
        if let Err(e) = step {
            let _ = self.remove_path(&tmp);
            forget(&tmp);
            return Err(e);
        }
        forget(&tmp);
        Ok(())
    }

    pub fn set_meta(&self, rel: &RelPath, mode: Option<u32>, mtime_ms: Option<i64>) -> Result<()> {
        let p = self.abs(rel)?;
        if let Some(mode) = mode {
            set_mode(&p, mode)?;
        }
        if let Some(ms) = mtime_ms {
            let ft = filetime::FileTime::from_unix_time((ms / 1000) as i64, ((ms % 1000) * 1_000_000) as u32);
            filetime::set_file_mtime(&p, ft)?;
        }
        Ok(())
    }

    pub fn set_readonly(&self, rel: &RelPath, readonly: bool) -> Result<()> {
        let p = self.abs(rel)?;
        #[cfg(windows)]
        {
            // Windows：去掉或加回只读属性。和 macOS 一样，覆盖后再恢复。
            let meta = fs::symlink_metadata(&p)?;
            let mut perm = meta.permissions();
            perm.set_readonly(readonly);
            fs::set_permissions(&p, perm)?;
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        {
            if !readonly {
                let _ = clear_uchg(&p);
            }
        }
        let meta = fs::symlink_metadata(&p)?;
        let mut mode = mode_of(&meta).unwrap_or(0o644);
        if readonly {
            mode &= !0o222;
        } else {
            mode |= 0o200;
        }
        set_mode(&p, mode)?;
        #[cfg(target_os = "macos")]
        if readonly {
            let _ = set_uchg(&p);
        }
        Ok(())
    }

    pub fn list_temps(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        fn rec(dir: &Path, prefix: &str, out: &mut Vec<String>) {
            let Ok(rd) = fs::read_dir(dir) else { return };
            for ent in rd.flatten() {
                let name = ent.file_name().to_string_lossy().to_string();
                let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
                let meta = match fs::symlink_metadata(ent.path()) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if meta.is_dir() && !meta.file_type().is_symlink() {
                    rec(&ent.path(), &rel, out);
                } else if name.contains(crate::brand::TEMP_TAG) {
                    out.push(rel);
                }
            }
        }
        rec(&self.root, "", &mut out);
        Ok(out)
    }
}

fn mtime_ms(meta: &fs::Metadata) -> Option<i64> {
    meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64)
}

fn mode_of(meta: &fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(meta.permissions().mode())
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

fn is_mode_readonly(meta: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o222 == 0
    }
    #[cfg(not(unix))]
    {
        meta.permissions().readonly()
    }
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    {
        let mut perm = fs::metadata(path)?.permissions();
        perm.set_readonly(mode & 0o200 == 0);
        fs::set_permissions(path, perm)?;
    }
    Ok(())
}

fn uchg(path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        file_flags(path) & libc::UF_IMMUTABLE != 0
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        false
    }
}

#[cfg(target_os = "macos")]
fn file_flags(path: &Path) -> u32 {
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else { return 0 };
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::lstat(c.as_ptr(), &mut st) } != 0 {
        return 0;
    }
    st.st_flags
}

#[cfg(target_os = "macos")]
fn clear_uchg(path: &Path) -> Result<()> {
    let flags = file_flags(path) & !libc::UF_IMMUTABLE;
    chflags(path, flags)
}

#[cfg(target_os = "macos")]
fn set_uchg(path: &Path) -> Result<()> {
    let flags = file_flags(path) | libc::UF_IMMUTABLE;
    chflags(path, flags)
}

#[cfg(target_os = "macos")]
fn chflags(path: &Path, flags: u32) -> Result<()> {
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| Error::new(ErrorCode::Io, "路径含空字节"))?;
    let rc = unsafe { libc::chflags(c.as_ptr(), flags as libc::c_uint) };
    if rc != 0 {
        return Err(Error::new(ErrorCode::Io, "chflags 失败"));
    }
    Ok(())
}

fn fsync_parent(path: &Path) -> Result<()> {
    let Some(parent) = path.parent() else { return Ok(()) };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    let dir = File::open(parent)?;
    dir.sync_all()?;
    Ok(())
}

pub fn atomic_rename(from: &Path, to: &Path) -> Result<()> {
    atomic_rename_counted(from, to, || Ok(()))
}

pub fn atomic_rename_counted(from: &Path, to: &Path, before_dir_fsync: impl FnOnce() -> Result<()>) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let to_w: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
        let from_w: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
        extern "system" {
            fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
        }
        const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
        const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
        let ok = unsafe {
            MoveFileExW(from_w.as_ptr(), to_w.as_ptr(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)
        };
        if ok == 0 {
            return Err(Error::new(ErrorCode::Io, "MoveFileExW 失败"));
        }
        before_dir_fsync()?;
        fsync_parent(to)?;
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        fs::rename(from, to)?;
        before_dir_fsync()?;
        fsync_parent(to)?;
        Ok(())
    }
}

fn long_path(p: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let s = p.to_string_lossy().replace('/', "\\");
        if s.starts_with(r"\\?\") {
            return PathBuf::from(s);
        }
        PathBuf::from(format!(r"\\?\{s}"))
    }
    #[cfg(not(windows))]
    {
        p.to_path_buf()
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

#[cfg(test)]
thread_local! {
    static FAIL_MID_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn fail_mid_write() -> bool {
    FAIL_MID_WRITE.with(|c| c.get())
}

#[cfg(test)]
fn set_fail_mid_write(on: bool) {
    FAIL_MID_WRITE.with(|c| c.set(on));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn does_not_follow_or_overwrite_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("real.txt"), b"real").unwrap();
        symlink("real.txt", root.join("link.txt")).unwrap();
        symlink(root.join("outside-target"), root.join("out-link")).ok();
        let fs = LocalFs::new(root);
        let entries = fs.walk(&Ignore::defaults()).unwrap();
        let link = entries.iter().find(|e| e.rel == "link.txt").unwrap();
        assert_eq!(link.kind, EntryKind::Symlink);
        assert!(fs.read(&RelPath::parse("link.txt").unwrap()).is_err());
        let err = fs.write_temp(&RelPath::parse("link.txt").unwrap(), b"nope").unwrap_err();
        assert_eq!(err.code, ErrorCode::SymlinkInPath);
        assert_eq!(fs::read(root.join("real.txt")).unwrap(), b"real");
    }

    #[test]
    fn parent_symlink_blocks_write() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), dir.path().join("conf")).unwrap();
        let fs = LocalFs::new(dir.path());
        let err = fs.write_temp(&RelPath::parse("conf/app.yml").unwrap(), b"x").unwrap_err();
        assert_eq!(err.code, ErrorCode::SymlinkInPath);
        assert!(outside.path().read_dir().unwrap().next().is_none());
    }

    #[test]
    fn temp_then_rename_replaces_and_failed_hash_keeps_original() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let fs = LocalFs::new(dir.path());
        let rel = RelPath::parse("a.txt").unwrap();
        let tmp = fs.write_temp(&rel, b"new").unwrap();
        let hash = fs.read_temp_hash(&tmp).unwrap();
        assert_eq!(hash, crate::hashutil::sha256_hex(b"new"));
        // 模拟校验失败：删掉临时文件，原文件不动
        fs.remove_path(&tmp).unwrap();
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"old");
        let tmp = fs.write_temp(&rel, b"new").unwrap();
        fs.rename_over(&tmp, &rel).unwrap();
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"new");
        assert!(fs.list_temps().unwrap().is_empty());
    }

    #[test]
    fn partial_write_deletes_the_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let fs = LocalFs::new(dir.path());
        let rel = RelPath::parse("a.txt").unwrap();
        let tmp = fs.allocate_temp(&rel).unwrap();
        set_fail_mid_write(true);
        let err = fs.create_temp_file(&tmp, b"partial-bytes").unwrap_err();
        set_fail_mid_write(false);
        assert_eq!(err.code, ErrorCode::Io);
        assert!(!dir.path().join(tmp.as_str()).exists());
    }
}
