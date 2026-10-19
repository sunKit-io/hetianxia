use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use xxhash_rust::xxh3::xxh3_128;

pub fn xxh128_hex(data: &[u8]) -> String {
    format!("{:032x}", xxh3_128(data))
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

pub fn hash_file_xxh(path: &Path) -> io::Result<String> {
    hash_file(path, false)
}

pub fn hash_file_sha256(path: &Path) -> io::Result<String> {
    hash_file(path, true)
}

fn hash_file(path: &Path, sha: bool) -> io::Result<String> {
    let mut f = File::open(path)?;
    let mut buf = vec![0u8; 8 * 1024 * 1024];
    if sha {
        let mut h = Sha256::new();
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        Ok(hex::encode(h.finalize()))
    } else {
        // xxh3-128 的流式接口在这个 crate 里没有公开，按 8MB 块读入后一次性计算。
        // 扫描路径对超大文件改用 SHA-256 流式（见 scan）。
        let mut all = Vec::new();
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            all.extend_from_slice(&buf[..n]);
        }
        Ok(xxh128_hex(&all))
    }
}
