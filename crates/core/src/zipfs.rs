//! zip 升级包。路径穿越拒绝整个包；三条炸弹阈值在解压前用中央目录判断；
//! 解压时按实际写出的字节计数，超过声明大小立刻中止。

use crate::error::{Error, ErrorCode, Result};
use crate::pathutil::RelPath;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
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
    // 标志打开时只接受 UTF-8。标志关闭时，字节本身是合法 UTF-8 就按 UTF-8
    // （访达“压缩”经常不置标志），否则再按 GBK。两种都解不开才是 E_ZIP_ENCODING。
    if utf8 || std::str::from_utf8(bytes).is_ok() {
        std::str::from_utf8(bytes)
            .map(|s| s.to_string())
            .map_err(|_| Error::new(ErrorCode::ZipEncoding, "zip 文件名不是合法的 UTF-8"))
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

/// 解压需要的空间是声明的解压后总大小的 1.1 倍。正好等于这个数可以通过。
pub fn required_free_bytes(uncompressed_total: u64) -> u64 {
    uncompressed_total.saturating_mul(11) / 10
}

pub fn ensure_free_space(available: u64, uncompressed_total: u64) -> Result<()> {
    let need = required_free_bytes(uncompressed_total);
    if available < need {
        return Err(Error::new(
            ErrorCode::NoSpace,
            format!("解压大约需要 {need} 字节，可用 {available} 字节"),
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub struct TempExtract {
    pub dir: PathBuf,
    pub report: ZipReport,
}

/// 解压到 `temp_root/zip-<uuid>/`。失败时删掉这个目录，不留下解到一半的文件。
pub fn extract_to_temp(zip_path: &Path, temp_root: &Path) -> Result<TempExtract> {
    let dir = temp_root.join(format!("zip-{}", uuid::Uuid::new_v4()));
    match extract(zip_path, &dir) {
        Ok(report) => Ok(TempExtract { dir, report }),
        Err(e) => {
            let _ = fs::remove_dir_all(&dir);
            Err(e)
        }
    }
}

pub fn extract(zip_path: &Path, dest: &Path) -> Result<ZipReport> {
    extract_inner(zip_path, dest, None)
}

fn extract_inner(zip_path: &Path, dest: &Path, free_override: Option<u64>) -> Result<ZipReport> {
    let file = File::open(zip_path)?;
    let mut archive = ZipArchive::new(file).map_err(|e| Error::new(ErrorCode::Io, e.to_string()))?;
    let mut stats = Vec::new();
    let mut names = Vec::new();
    let mut slips = Vec::new();
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| Error::new(ErrorCode::Io, e.to_string()))?;
        let name = entry_name(zip_path, &entry)?;
        stats.push(EntryStat {
            uncompressed: entry.size(),
            compressed: entry.compressed_size(),
            is_dir: name.ends_with('/'),
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
    let total = stats.iter().fold(0u64, |acc, e| acc.saturating_add(e.uncompressed));
    let available = match free_override {
        Some(n) => n,
        None => free_bytes(dest)?,
    };
    ensure_free_space(available, total)?;
    fs::create_dir_all(dest)?;
    let mut report = ZipReport { files: Vec::new(), skipped_symlinks: Vec::new() };
    let mut written: Vec<PathBuf> = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| Error::new(ErrorCode::Io, e.to_string()))?;
        let name = entry_name(zip_path, &entry)?;
        if name.ends_with('/') {
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
        if let Err(e) = copy_capped(&mut entry, declared, &mut out) {
            drop(out);
            written.push(target);
            cleanup_written(&written);
            return Err(e);
        }
        if let Err(e) = out.sync_all() {
            drop(out);
            written.push(target);
            cleanup_written(&written);
            return Err(Error::from(e));
        }
        written.push(target);
        report.files.push(rel.as_str().to_string());
    }
    Ok(report)
}

fn entry_name<R: Read>(zip_path: &Path, entry: &zip::read::ZipFile<'_, R>) -> Result<String> {
    let utf8 = utf8_flag(zip_path, entry.header_start())?;
    decode_entry_name(entry.name_raw(), utf8)
}

fn utf8_flag(zip_path: &Path, header_start: u64) -> Result<bool> {
    let mut file = File::open(zip_path)?;
    let at = header_start.checked_add(6).ok_or_else(|| Error::new(ErrorCode::Io, "zip 头偏移溢出"))?;
    file.seek(SeekFrom::Start(at))?;
    let mut buf = [0u8; 2];
    file.read_exact(&mut buf)?;
    Ok(u16::from_le_bytes(buf) & (1 << 11) != 0)
}

fn cleanup_written(paths: &[PathBuf]) {
    for path in paths.iter().rev() {
        let _ = fs::remove_file(path);
    }
}

fn free_bytes(dest: &Path) -> Result<u64> {
    let mut probe = dest.to_path_buf();
    if !probe.exists() {
        if let Some(parent) = dest.parent() {
            probe = parent.to_path_buf();
        }
    }
    while !probe.exists() {
        if !probe.pop() {
            break;
        }
    }
    #[cfg(unix)]
    {
        let text = probe.to_string_lossy();
        let c = std::ffi::CString::new(text.as_bytes()).map_err(|_| Error::new(ErrorCode::InvalidLocation, "路径含空字节"))?;
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statvfs(c.as_ptr(), &mut stat) };
        if rc != 0 {
            return Err(Error::new(ErrorCode::Io, "读不到剩余空间"));
        }
        Ok((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
    }
    #[cfg(not(unix))]
    {
        let _ = probe;
        Ok(u64::MAX)
    }
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

    #[test]
    fn extract_decodes_gbk_when_the_utf8_flag_is_off() {
        let (gbk, _, _) = encoding_rs::GBK.encode("说明.txt");
        let gbk = gbk.into_owned();
        let placeholder = "n".repeat(gbk.len());
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("gbk.zip");
        let mut w = ZipWriter::new(File::create(&zip_path).unwrap());
        w.start_file(&placeholder, SimpleFileOptions::default()).unwrap();
        w.write_all(b"hi").unwrap();
        w.finish().unwrap();
        let mut bytes = fs::read(&zip_path).unwrap();
        let needle = placeholder.as_bytes();
        let mut hits = 0;
        let mut i = 0;
        while i + needle.len() <= bytes.len() {
            if &bytes[i..i + needle.len()] == needle {
                bytes[i..i + needle.len()].copy_from_slice(&gbk);
                hits += 1;
                i += needle.len();
            } else {
                i += 1;
            }
        }
        assert!(hits >= 2, "local and central names, got {hits}");
        fs::write(&zip_path, bytes).unwrap();
        let dest = dir.path().join("out");
        let report = extract(&zip_path, &dest).unwrap();
        assert_eq!(report.files, vec!["说明.txt".to_string()]);
        assert_eq!(fs::read(dest.join("说明.txt")).unwrap(), b"hi");
    }

    #[test]
    fn extract_keeps_utf8_names_when_the_flag_is_off() {
        let name = "说明.txt";
        let raw = name.as_bytes();
        let (as_gbk, _, gbk_errors) = encoding_rs::GBK.decode(raw);
        assert!(!gbk_errors);
        assert_ne!(as_gbk.as_ref(), name);
        assert_eq!(decode_entry_name(raw, false).unwrap(), name);

        let placeholder = "n".repeat(raw.len());
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("finder.zip");
        let mut w = ZipWriter::new(File::create(&zip_path).unwrap());
        w.start_file(&placeholder, SimpleFileOptions::default()).unwrap();
        w.write_all(b"hi").unwrap();
        w.finish().unwrap();
        let mut bytes = fs::read(&zip_path).unwrap();
        replace_name(&mut bytes, placeholder.as_bytes(), raw);
        clear_utf8_name_flag(&mut bytes);
        fs::write(&zip_path, &bytes).unwrap();
        assert!(!utf8_flag(&zip_path, 0).unwrap());
        let dest = dir.path().join("out");
        let report = extract(&zip_path, &dest).unwrap();
        assert_eq!(report.files, vec![name.to_string()]);
        assert_eq!(fs::read(dest.join(name)).unwrap(), b"hi");
    }

    fn replace_name(bytes: &mut [u8], from: &[u8], to: &[u8]) {
        assert_eq!(from.len(), to.len());
        let mut hits = 0;
        let mut i = 0;
        while i + from.len() <= bytes.len() {
            if &bytes[i..i + from.len()] == from {
                bytes[i..i + to.len()].copy_from_slice(to);
                hits += 1;
                i += from.len();
            } else {
                i += 1;
            }
        }
        assert!(hits >= 2, "local and central names, got {hits}");
    }

    fn clear_utf8_name_flag(bytes: &mut [u8]) {
        let mut i = 0;
        while i + 10 < bytes.len() {
            if bytes[i..i + 4] == [0x50, 0x4b, 0x03, 0x04] {
                let flags = u16::from_le_bytes(bytes[i + 6..i + 8].try_into().unwrap()) & !(1 << 11);
                bytes[i + 6..i + 8].copy_from_slice(&flags.to_le_bytes());
            }
            if bytes[i..i + 4] == [0x50, 0x4b, 0x01, 0x02] {
                let flags = u16::from_le_bytes(bytes[i + 8..i + 10].try_into().unwrap()) & !(1 << 11);
                bytes[i + 8..i + 10].copy_from_slice(&flags.to_le_bytes());
            }
            i += 1;
        }
    }

    #[test]
    fn extract_rejects_undecodable_names_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("bad.zip");
        let mut w = ZipWriter::new(File::create(&zip_path).unwrap());
        w.start_file("ab", SimpleFileOptions::default()).unwrap();
        w.write_all(b"x").unwrap();
        w.finish().unwrap();
        let mut bytes = fs::read(&zip_path).unwrap();
        let mut hits = 0;
        let mut i = 0;
        while i + 2 <= bytes.len() {
            if &bytes[i..i + 2] == b"ab" {
                bytes[i] = 0xFF;
                bytes[i + 1] = 0xFF;
                hits += 1;
                i += 2;
            } else {
                i += 1;
            }
        }
        assert!(hits >= 2);
        fs::write(&zip_path, bytes).unwrap();
        let dest = dir.path().join("out");
        let err = extract(&zip_path, &dest).unwrap_err();
        assert_eq!(err.code, ErrorCode::ZipEncoding);
        assert!(!dest.exists());
    }

    #[test]
    fn bomb_during_extract_removes_files_already_written() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("bomb.zip");
        let mut w = ZipWriter::new(File::create(&zip_path).unwrap());
        let opt = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        w.start_file("src/ok.txt", opt).unwrap();
        w.write_all(b"ok").unwrap();
        w.start_file("src/big.txt", opt).unwrap();
        w.write_all(b"abcdef").unwrap();
        w.finish().unwrap();
        shrink_uncompressed_size(&zip_path, b"src/big.txt", 6, 3);
        let dest = dir.path().join("out");
        let err = extract(&zip_path, &dest).unwrap_err();
        assert_eq!(err.code, ErrorCode::ZipBomb);
        assert!(!dest.join("src/ok.txt").exists());
        assert!(!dest.join("src/big.txt").exists());
    }

    #[test]
    fn temp_extract_uses_zip_uuid_dir_and_removes_it_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("ok.zip");
        let mut w = ZipWriter::new(File::create(&zip_path).unwrap());
        w.start_file("a.txt", SimpleFileOptions::default()).unwrap();
        w.write_all(b"a").unwrap();
        w.finish().unwrap();
        let got = extract_to_temp(&zip_path, dir.path()).unwrap();
        assert!(got.dir.file_name().unwrap().to_string_lossy().starts_with("zip-"));
        assert_eq!(fs::read(got.dir.join("a.txt")).unwrap(), b"a");
        let _ = fs::remove_dir_all(&got.dir);

        let bomb = dir.path().join("bomb.zip");
        let mut w = ZipWriter::new(File::create(&bomb).unwrap());
        let opt = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        w.start_file("big.txt", opt).unwrap();
        w.write_all(b"abcdef").unwrap();
        w.finish().unwrap();
        shrink_uncompressed_size(&bomb, b"big.txt", 6, 3);
        let before = fs::read_dir(dir.path()).unwrap().count();
        let err = extract_to_temp(&bomb, dir.path()).unwrap_err();
        assert_eq!(err.code, ErrorCode::ZipBomb);
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| !n.starts_with("zip-")), "{names:?}");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), before);
    }

    #[test]
    fn free_space_shortfall_stops_before_any_file_is_written() {
        assert_eq!(required_free_bytes(100), 110);
        assert!(ensure_free_space(110, 100).is_ok());
        assert_eq!(ensure_free_space(109, 100).unwrap_err().code, ErrorCode::NoSpace);
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("ok.zip");
        let mut w = ZipWriter::new(File::create(&zip_path).unwrap());
        w.start_file("a.txt", SimpleFileOptions::default()).unwrap();
        w.write_all(b"hello").unwrap();
        w.finish().unwrap();
        let dest = dir.path().join("out");
        let err = extract_inner(&zip_path, &dest, Some(1)).unwrap_err();
        assert_eq!(err.code, ErrorCode::NoSpace);
        assert!(!dest.exists());
    }

    fn shrink_uncompressed_size(path: &Path, name: &[u8], from: u32, to: u32) {
        let mut bytes = fs::read(path).unwrap();
        let mut patched = 0;
        let mut i = 0;
        while i + 30 < bytes.len() {
            if bytes[i..i + 4] == [0x50, 0x4b, 0x03, 0x04] {
                let name_len = u16::from_le_bytes(bytes[i + 26..i + 28].try_into().unwrap()) as usize;
                let start = i + 30;
                if start + name_len <= bytes.len() && &bytes[start..start + name_len] == name {
                    let un = u32::from_le_bytes(bytes[i + 22..i + 26].try_into().unwrap());
                    if un == from {
                        bytes[i + 22..i + 26].copy_from_slice(&to.to_le_bytes());
                        patched += 1;
                    }
                }
            }
            if bytes[i..i + 4] == [0x50, 0x4b, 0x01, 0x02] && i + 46 < bytes.len() {
                let name_len = u16::from_le_bytes(bytes[i + 28..i + 30].try_into().unwrap()) as usize;
                let start = i + 46;
                if start + name_len <= bytes.len() && &bytes[start..start + name_len] == name {
                    let un = u32::from_le_bytes(bytes[i + 24..i + 28].try_into().unwrap());
                    if un == from {
                        bytes[i + 24..i + 28].copy_from_slice(&to.to_le_bytes());
                        patched += 1;
                    }
                }
            }
            i += 1;
        }
        assert!(patched >= 2, "patched {patched}");
        fs::write(path, bytes).unwrap();
    }
}
