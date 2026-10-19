//! zip 升级包。路径穿越拒绝整个包；三条炸弹阈值在解压前用中央目录判断；
//! 解压时按实际写出的字节计数，超过声明大小立刻中止。

use crate::error::{Error, ErrorCode, Result};
use crate::pathutil::RelPath;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use zip::ZipArchive;

pub const MAX_UNCOMPRESSED: u64 = 4 * 1024 * 1024 * 1024;
pub const MAX_RATIO: u64 = 100;
pub const MAX_ENTRIES: u64 = 200_000;

#[derive(Debug, Clone, Copy)]
pub struct EntryStat {
    pub uncompressed: u64,
    pub compressed: u64,
    pub is_dir: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bomb {
    Entries(u64),
    Total(u64),
    Ratio { uncompressed: u64, compressed: u64 },
}

/// 正好等于上限可以通过。压缩后 0 字节的条目不计算压缩比。目录也算进条目总数。
pub fn bomb_reason(entries: &[EntryStat]) -> Option<Bomb> {
    let count = entries.len() as u64;
    if count > MAX_ENTRIES {
        return Some(Bomb::Entries(count));
    }
    let total = entries.iter().fold(0u64, |acc, e| acc.saturating_add(e.uncompressed));
    if total > MAX_UNCOMPRESSED {
        return Some(Bomb::Total(total));
    }
    for e in entries {
        if e.compressed == 0 || e.is_dir {
            continue;
        }
        if e.uncompressed > e.compressed.saturating_mul(MAX_RATIO) {
            return Some(Bomb::Ratio { uncompressed: e.uncompressed, compressed: e.compressed });
        }
    }
    None
}

pub fn is_zip_slip(name: &str) -> bool {
    if name.is_empty() || name.contains('\0') || name.contains('\\') {
        return !name.is_empty();
    }
    if name.starts_with('/') || name.starts_with('~') {
        return true;
    }
    let trimmed = name.trim_end_matches('/');
    if trimmed.len() >= 2 && trimmed.as_bytes()[1] == b':' && trimmed.as_bytes()[0].is_ascii_alphabetic() {
        return true;
    }
    trimmed.split('/').any(|p| p == "..")
}

pub fn decode_entry_name(bytes: &[u8], utf8: bool) -> Result<String> {
    if utf8 {
        String::from_utf8(bytes.to_vec()).map_err(|_| Error::new(ErrorCode::ZipEncoding, "zip 文件名不是合法的 UTF-8"))
    } else {
        let (cow, _, had_errors) = encoding_rs::GBK.decode(bytes);
        if had_errors {
            Err(Error::new(ErrorCode::ZipEncoding, "zip 文件名按 GBK 解码失败"))
        } else {
            Ok(cow.into_owned())
        }
    }
}

#[derive(Debug, Clone)]
pub struct ZipReport {
    pub files: Vec<String>,
    pub skipped_symlinks: Vec<String>,
}

pub fn extract(zip_path: &Path, dest: &Path) -> Result<ZipReport> {
    let file = File::open(zip_path)?;
    let mut archive = ZipArchive::new(file).map_err(|e| Error::new(ErrorCode::Io, e.to_string()))?;
    let mut stats = Vec::new();
    let mut names = Vec::new();
    let mut slips = Vec::new();
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| Error::new(ErrorCode::Io, e.to_string()))?;
        let name = entry.name().to_string();
        stats.push(EntryStat {
            uncompressed: entry.size(),
            compressed: entry.compressed_size(),
            is_dir: entry.is_dir(),
        });
        if is_zip_slip(&name) {
            slips.push(name);
        } else {
            names.push(name);
        }
    }
    if !slips.is_empty() {
        return Err(Error::new(ErrorCode::ZipSlip, format!("升级包里有 {} 条路径穿越，已拒绝整个包", slips.len())).with_paths(slips));
    }
    if let Some(reason) = bomb_reason(&stats) {
        let message = match reason {
            Bomb::Entries(n) => format!("条目总数 {n} 超过 {MAX_ENTRIES}"),
            Bomb::Total(n) => format!("解压后总大小 {n} 字节超过 {MAX_UNCOMPRESSED}"),
            Bomb::Ratio { uncompressed, compressed } => format!("压缩比超过 {MAX_RATIO} 倍（{uncompressed}/{compressed}）"),
        };
        return Err(Error::new(ErrorCode::ZipBomb, message));
    }
    fs::create_dir_all(dest)?;
    let mut report = ZipReport { files: Vec::new(), skipped_symlinks: Vec::new() };
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| Error::new(ErrorCode::Io, e.to_string()))?;
        let name = entry.name().to_string();
        if entry.is_dir() {
            continue;
        }
        if is_symlink(entry.unix_mode()) {
            report.skipped_symlinks.push(name);
            continue;
        }
        let rel = RelPath::parse(name.trim_end_matches('/'))?;
        let declared = entry.size();
        let target = dest.join(rel.as_str());
        if let Some(parent) = target.parent() {
            reject_symlink_parent(dest, parent)?;
            fs::create_dir_all(parent)?;
        }
        let mut out = fs::OpenOptions::new().create(true).write(true).truncate(true).open(&target)?;
        copy_capped(&mut entry, declared, &mut out)?;
        out.sync_all()?;
        report.files.push(rel.as_str().to_string());
    }
    Ok(report)
}

pub fn copy_capped(reader: &mut impl Read, declared: u64, out: &mut impl Write) -> Result<u64> {
    let mut buf = [0u8; 8192];
    let mut written = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let n64 = n as u64;
        if written.saturating_add(n64) > declared {
            return Err(Error::new(ErrorCode::ZipBomb, "实际写出的字节超过条目声明的大小，已中止"));
        }
        out.write_all(&buf[..n])?;
        written += n64;
    }
    Ok(written)
}

fn is_symlink(mode: Option<u32>) -> bool {
    match mode {
        Some(m) => m & 0o170000 == 0o120000,
        None => false,
    }
}

fn reject_symlink_parent(root: &Path, dir: &Path) -> Result<()> {
    let mut cur = root.to_path_buf();
    if dir.starts_with(root) {
        for part in dir.strip_prefix(root).unwrap_or(dir).components() {
            cur.push(part);
            if fs::symlink_metadata(&cur).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                return Err(Error::new(ErrorCode::SymlinkInPath, format!("{} 的上级是符号链接", display(&cur))));
            }
        }
    }
    Ok(())
}

fn display(p: &Path) -> String {
    p.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    #[test]
    fn thresholds_let_the_exact_limit_pass() {
        assert!(bomb_reason(&vec![EntryStat { uncompressed: 0, compressed: 0, is_dir: true }; MAX_ENTRIES as usize]).is_none());
        let mut over = vec![EntryStat { uncompressed: 0, compressed: 0, is_dir: true }; 1];
        over.extend(std::iter::repeat(EntryStat { uncompressed: 0, compressed: 0, is_dir: true }).take(MAX_ENTRIES as usize));
        assert!(matches!(bomb_reason(&over), Some(Bomb::Entries(_))));

        assert!(bomb_reason(&[EntryStat { uncompressed: MAX_UNCOMPRESSED, compressed: MAX_UNCOMPRESSED, is_dir: false }]).is_none());
        assert!(matches!(
            bomb_reason(&[EntryStat { uncompressed: MAX_UNCOMPRESSED + 1, compressed: 1, is_dir: false }]),
            Some(Bomb::Total(_))
        ));
        assert!(bomb_reason(&[EntryStat { uncompressed: 1000, compressed: 10, is_dir: false }]).is_none());
        assert!(matches!(
            bomb_reason(&[EntryStat { uncompressed: 1001, compressed: 10, is_dir: false }]),
            Some(Bomb::Ratio { .. })
        ));
        assert!(bomb_reason(&[EntryStat { uncompressed: 5_000_000, compressed: 0, is_dir: false }]).is_none());
    }

    #[test]
    fn slip_rejects_the_whole_package_and_lists_entries() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("bad.zip");
        let mut w = ZipWriter::new(File::create(&zip_path).unwrap());
        let opt = SimpleFileOptions::default();
        w.start_file("ok.txt", opt).unwrap();
        w.write_all(b"ok").unwrap();
        w.start_file("../evil.txt", opt).unwrap();
        w.write_all(b"no").unwrap();
        w.finish().unwrap();
        let dest = dir.path().join("out");
        let err = extract(&zip_path, &dest).unwrap_err();
        assert_eq!(err.code, ErrorCode::ZipSlip);
        assert!(err.paths.iter().any(|p| p.contains("..")));
        assert!(!dest.join("ok.txt").exists());
        assert!(!dir.path().join("evil.txt").exists());
    }

    #[test]
    fn extracts_a_normal_package_and_skips_symlink_entries() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("ok.zip");
        let mut w = ZipWriter::new(File::create(&zip_path).unwrap());
        let opt = SimpleFileOptions::default().unix_permissions(0o644);
        w.start_file("src/a.txt", opt).unwrap();
        w.write_all(b"hello").unwrap();
        w.add_symlink("src/link", "a.txt", SimpleFileOptions::default()).unwrap();
        w.finish().unwrap();
        let dest = dir.path().join("out");
        let report = extract(&zip_path, &dest).unwrap();
        assert_eq!(fs::read(dest.join("src/a.txt")).unwrap(), b"hello");
        assert!(report.skipped_symlinks.iter().any(|n| n.contains("link")));
        assert!(!dest.join("src/link").exists());
    }

    #[test]
    fn actual_bytes_past_the_declared_size_abort() {
        let mut reader = Cursor::new(b"abcdef");
        let mut out = Vec::new();
        let err = copy_capped(&mut reader, 3, &mut out).unwrap_err();
        assert_eq!(err.code, ErrorCode::ZipBomb);
        assert!(out.len() <= 3);
    }

    #[test]
    fn gbk_names_round_trip_when_the_utf8_flag_is_off() {
        let (bytes, _, _) = encoding_rs::GBK.encode("说明.txt");
        let name = decode_entry_name(&bytes, false).unwrap();
        assert_eq!(name, "说明.txt");
        let bad = decode_entry_name(&[0xFF, 0xFE], true).unwrap_err();
        assert_eq!(bad.code, ErrorCode::ZipEncoding);
        assert!(is_zip_slip("/etc/passwd"));
        assert!(is_zip_slip("C:/windows/a"));
        assert!(!is_zip_slip("src/a.txt"));
    }
}
