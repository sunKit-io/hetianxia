//! 进程级 `run.lock`。
//!
//! 不根据 pid 或启动时间判断上次进程是否还活着。打开数据目录里的 `run.lock`
//!（`create`，拿到锁之前不截断），用非阻塞独占锁占住它（macOS `flock`，Windows
//! `LockFileEx`，这里走 fs4）。锁在整个进程存活期间一直拿着。文件从不删除。
//!
//! - 锁拿不到：已经有一个实例。这是单实例插件之后的第二道保险。这条路径不改文件内容。
//! - 拿到锁，文件不存在、是空的，或状态是 `clean`：正常启动，不核对 `needs_verify`。
//! - 拿到锁，状态是 `running`：上次进程不是干净退出。文件里的 `run` 是那次运行的
//!   `run_id`，只核对打着这个 id 的 `needs_verify`。
//! - 两种都先写上本次新的 `run_id` 和 `running` 并 fsync。干净退出时先清掉这次运行打上的
//!   标记，再写成 `clean`、fsync，然后放开锁。
//!
//! pid 和进程启动时间只给诊断看。`run` 用来把 `needs_verify` 对上一次具体的运行。

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
    run_id: String,
    crashed_run: Option<String>,
}

impl RunLock {
    /// 上次留下的状态是 `running`，说明它没有干净退出。
    pub fn abnormal_exit(&self) -> bool {
        self.abnormal
    }

    /// 这次进程的 `run_id`。新打上的 `needs_verify` 用它。
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// 上次异常退出留下的 `run_id`。文件里没有 `run` 时是 `None`，只核对未标记的行。
    pub fn crashed_run(&self) -> Option<&str> {
        self.crashed_run.as_deref()
    }

    /// 干净退出：先清掉这次运行的 `needs_verify`，再写成 `clean` 并 fsync，然后放开锁。不删除文件。
    pub fn release_clean(mut self, db: &rusqlite::Connection) -> Result<()> {
        crate::exec::clear_needs_verify_for_run(db, &self.run_id)?;
        write_status(&self.file, STATUS_CLEAN, &self.run_id)?;
        match self.file.unlock() {
            Ok(()) => {}
            Err(err) => return Err(Error::new(ErrorCode::Io, err.to_string())),
        }
        self.released = true;
        Ok(())
    }
}

impl Drop for RunLock {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        match self.file.unlock() {
            Ok(()) => {}
            Err(err) => eprintln!("run.lock unlock: {err}"),
        }
    }
}

pub fn acquire_run_lock(data_dir: &Path) -> Result<RunAcquire> {
    fs::create_dir_all(data_dir)?;
    let path = data_dir.join("run.lock");
    // create 不会截断已有内容。截断和改写都放在拿到锁之后。
    let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)?;
    match file.try_lock_exclusive() {
        Ok(()) => {
            let prior = read_prior(&file)?;
            let abnormal = prior.status == Some(STATUS_RUNNING);
            let crashed_run = if abnormal { prior.run_id } else { None };
            let run_id = uuid::Uuid::new_v4().simple().to_string();
            write_status(&file, STATUS_RUNNING, &run_id)?;
            Ok(RunAcquire::Held(RunLock { file, abnormal, released: false, run_id, crashed_run }))
        }
        Err(err) if lock_held_by_other(&err) => Ok(RunAcquire::AnotherInstance),
        Err(err) => Err(Error::new(ErrorCode::Io, err.to_string())),
    }
}

fn lock_held_by_other(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::WouldBlock
}

struct PriorLock {
    status: Option<&'static str>,
    run_id: Option<String>,
}

fn read_prior(file: &File) -> Result<PriorLock> {
    let mut file = file;
    file.seek(SeekFrom::Start(0))?;
    let mut buf = String::new();
    file.read_to_string(&mut buf)?;
    if buf.trim().is_empty() {
        return Ok(PriorLock { status: None, run_id: None });
    }
    let mut status = None;
    let mut run_id = None;
    for line in buf.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("status=") {
            status = match value.trim() {
                STATUS_RUNNING => Some(STATUS_RUNNING),
                STATUS_CLEAN => Some(STATUS_CLEAN),
                _ => None,
            };
        } else if let Some(value) = line.strip_prefix("run=") {
            let value = value.trim();
            if !value.is_empty() {
                run_id = Some(value.to_string());
            }
        }
    }
    Ok(PriorLock { status, run_id })
}

fn write_status(file: &File, status: &str, run_id: &str) -> Result<()> {
    let body = format!(
        "status={status}\nrun={run_id}\npid={}\nstart={}\n",
        std::process::id(),
        process_start_token()
    );
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
