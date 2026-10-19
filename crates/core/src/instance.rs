//! 单实例用本机 TCP，不经过 zbus，也不碰钥匙串。
//! 第一个进程握住监听端口。后来的进程连上就退出，并且不写 `run.lock`。

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceRole {
    Primary,
    Secondary,
}

fn read_port(path: &Path) -> Option<u16> {
    let text = std::fs::read_to_string(path).ok()?;
    text.trim().parse().ok()
}

fn write_port(path: &Path, port: u16) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut file = std::fs::File::create(path)?;
    writeln!(file, "{port}")?;
    file.sync_all()?;
    Ok(())
}

/// `path` 是端口文件。已有进程在听就返回 [`InstanceRole::Secondary`]。
pub fn claim_instance(path: &Path) -> std::io::Result<(InstanceRole, Option<TcpListener>)> {
    if let Some(port) = read_port(path) {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Ok((InstanceRole::Secondary, None));
        }
    }
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    write_port(path, port)?;
    if let Some(published) = read_port(path) {
        if published != port {
            if TcpStream::connect(("127.0.0.1", published)).is_ok() {
                return Ok((InstanceRole::Secondary, None));
            }
            write_port(path, port)?;
        }
    }
    Ok((InstanceRole::Primary, Some(listener)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_process_is_secondary_without_zbus() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("single.port");
        let (role, listener) = claim_instance(&path).unwrap();
        assert_eq!(role, InstanceRole::Primary);
        let listener = listener.expect("主进程要握住端口");
        let (again, none) = claim_instance(&path).unwrap();
        assert_eq!(again, InstanceRole::Secondary);
        assert!(none.is_none());
        drop(listener);
        let (restarted, held) = claim_instance(&path).unwrap();
        assert_eq!(restarted, InstanceRole::Primary);
        assert!(held.is_some());
    }
}
