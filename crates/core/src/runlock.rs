//! 进程级 `run.lock`。
//!
//! 不根据 pid 或启动时间判断上次进程是否还活着。打开数据目录里的 `run.lock`，
//! 用非阻塞的独占文件锁占住它（macOS `flock`，Windows `LockFileEx`，这里走 fs4）。
//! 锁在整个进程存活期间一直拿着。
//!
//! - 拿到锁，而且文件本来就在（或里面已有上次运行写下的内容）：上次进程不是干净退出。
//! - 拿到锁，而且文件是这次新建的：正常启动。
//! - 锁拿不到：已经有一个实例。调用方把焦点交给已有窗口，然后退出。这条路径不读、不写、不删 `run.lock`。
//!
//! 文件里的 pid 和进程启动时间只给诊断看。干净退出时先删文件，再放开锁。

use crate::error::{Error, ErrorCode, Result};
use fs4::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub enum RunAcquire {
    Held(RunLock),
    AnotherInstance,
}

pub struct RunLock {
    file: File,
    path: PathBuf,
    abnormal: bool,
    released: bool,
}

impl RunLock {
    /// 上次进程留下了这个文件，说明它没有干净退出。
    pub fn abnormal_exit(&self) -> bool {
        self.abnormal
    }

    /// 干净退出：先删除文件，再放开锁。
    pub fn release_clean(mut self) -> Result<()> {
        fs::remove_file(&self.path)?;
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
    if path.exists() {
        return open_existing(&path);
    }
    match OpenOptions::new().read(true).write(true).create_new(true).open(&path) {
        Ok(file) => match file.try_lock_exclusive() {
            Ok(()) => {
                write_diagnostics(&file)?;
                Ok(RunAcquire::Held(RunLock { file, path, abnormal: false, released: false }))
            }
            Err(err) if lock_held_by_other(&err) => Ok(RunAcquire::AnotherInstance),
            Err(err) => Err(Error::new(ErrorCode::Io, err.to_string())),
        },
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => open_existing(&path),
        Err(err) => Err(err.into()),
    }
}

fn open_existing(path: &Path) -> Result<RunAcquire> {
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    match file.try_lock_exclusive() {
        Ok(()) => {
            // 文件在这次打开之前就已经在。不管里面有没有内容，都算上次异常退出。
            write_diagnostics(&file)?;
            Ok(RunAcquire::Held(RunLock {
                file,
                path: path.to_path_buf(),
                abnormal: true,
                released: false,
            }))
        }
        Err(err) if lock_held_by_other(&err) => Ok(RunAcquire::AnotherInstance),
        Err(err) => Err(Error::new(ErrorCode::Io, err.to_string())),
    }
}

fn lock_held_by_other(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::WouldBlock
}

fn write_diagnostics(file: &File) -> Result<()> {
    let body = format!("pid={}\nstart={}\n", std::process::id(), process_start_token());
    file.set_len(0)?;
    let mut file = file;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(body.as_bytes())?;
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
