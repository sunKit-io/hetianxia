//! 进程级 `run.lock`。
//!
//! 不根据 pid 或启动时间判断上次进程是否还活着。打开数据目录里的 `run.lock`
//!（`create`，拿到锁之前不截断），用非阻塞独占锁占住它（macOS `flock`，Windows
//! `LockFileEx`，这里走 fs4）。锁在整个进程存活期间一直拿着。文件从不删除。
//!
//! - 锁拿不到：已经有一个实例。这是单实例插件之后的第二道保险。这条路径不改文件内容。
//! - 拿到锁，文件不存在、是空的，或状态是 `clean`：正常启动，不核对 `needs_verify`。
//! - 拿到锁，状态是 `running`：上次进程不是干净退出，要先核对 `needs_verify`。
//! - 两种都先写上 `running` 并 fsync。干净退出时写成 `clean`、fsync，再放开锁。
//!
//! pid 和进程启动时间只给诊断看。

use crate::error::{Error, ErrorCode, Result};
use fs4::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

const STATUS_RUNNING: &str = "running";
const STATUS_CLEAN: &str = "clean";

pub enum RunAcquire {
    Held(RunLock),
    AnotherInstance,
}

pub struct RunLock {
    file: File,
    abnormal: bool,
    released: bool,
}

impl RunLock {
    /// 上次留下的状态是 `running`，说明它没有干净退出。
    pub fn abnormal_exit(&self) -> bool {
        self.abnormal
    }

    /// 干净退出：写成 `clean` 并 fsync，然后放开锁。不删除文件。
    pub fn release_clean(mut self) -> Result<()> {
        write_status(&self.file, STATUS_CLEAN)?;
        self.file.unlock()?;
        self.released = true;
        Ok(())
    }
}

impl Drop for RunLock {
    fn drop(&mut self) {
        if !self.released {
            let _ = self.file.unlock();
        }
    }
}

pub fn acquire_run_lock(data_dir: &Path) -> Result<RunAcquire> {
    fs::create_dir_all(data_dir)?;
    let path = data_dir.join("run.lock");
    // create 不会截断已有内容。截断和改写都放在拿到锁之后。
    let file = OpenOptions::new().read(true).write(true).create(true).open(&path)?;
    match file.try_lock_exclusive() {
        Ok(()) => {
            let abnormal = read_status(&file)? == Some(STATUS_RUNNING);
            write_status(&file, STATUS_RUNNING)?;
            Ok(RunAcquire::Held(RunLock { file, abnormal, released: false }))
        }
        Err(err) if lock_held_by_other(&err) => Ok(RunAcquire::AnotherInstance),
        Err(err) => Err(Error::new(ErrorCode::Io, err.to_string())),
    }
}

fn lock_held_by_other(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::WouldBlock
}

fn read_status(file: &File) -> Result<Option<&'static str>> {
    let mut file = file;
    file.seek(SeekFrom::Start(0))?;
    let mut buf = String::new();
    file.read_to_string(&mut buf)?;
    if buf.trim().is_empty() {
        return Ok(None);
    }
    for line in buf.lines() {
        let Some(value) = line.trim().strip_prefix("status=") else { continue };
        return Ok(match value.trim() {
            STATUS_RUNNING => Some(STATUS_RUNNING),
            STATUS_CLEAN => Some(STATUS_CLEAN),
            _ => None,
        });
    }
    Ok(None)
}

fn write_status(file: &File, status: &str) -> Result<()> {
    let body = format!("status={status}\npid={}\nstart={}\n", std::process::id(), process_start_token());
    let mut file = file;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(body.as_bytes())?;
    file.set_len(body.len() as u64)?;
    file.sync_all()?;
    Ok(())
}

fn process_start_token() -> String {
    #[cfg(target_os = "linux")]
    {
        return linux_start().unwrap_or_else(|| "0".into());
    }
    #[cfg(target_os = "macos")]
    {
        return macos_start().unwrap_or_else(|| "0".into());
    }
    #[cfg(windows)]
    {
        return windows_start().unwrap_or_else(|| "0".into());
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        "0".into()
    }
}

#[cfg(target_os = "linux")]
fn linux_start() -> Option<String> {
    let stat = fs::read_to_string("/proc/self/stat").ok()?;
    let rest = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // /proc/self/stat 第 22 项是启动时刻。`)` 后面从第 3 项开始，所以下标是 19。
    fields.get(19).map(|field| (*field).to_string())
}

#[cfg(target_os = "macos")]
fn macos_start() -> Option<String> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    let wrote = unsafe {
        libc::proc_pidinfo(
            std::process::id() as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if wrote != size {
        return None;
    }
    let info = unsafe { info.assume_init() };
    Some(format!("{}.{}", info.pbi_start_tvsec, info.pbi_start_tvusec))
}

#[cfg(windows)]
fn windows_start() -> Option<String> {
    #[repr(C)]
    struct FileTime {
        lo: u32,
        hi: u32,
    }
    extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn GetProcessTimes(
            process: *mut std::ffi::c_void,
            created: *mut FileTime,
            exited: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
    }
    unsafe {
        let mut created = FileTime { lo: 0, hi: 0 };
        let mut exited = FileTime { lo: 0, hi: 0 };
        let mut kernel = FileTime { lo: 0, hi: 0 };
        let mut user = FileTime { lo: 0, hi: 0 };
        if GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) == 0 {
            return None;
        }
        let ticks = ((created.hi as u64) << 32) | created.lo as u64;
        Some(ticks.to_string())
    }
}
