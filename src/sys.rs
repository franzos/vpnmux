use std::sync::atomic::{AtomicBool, Ordering};

// Self-declared libc symbols keep the crate dependency-free. These are the
// stable System V / glibc signatures; only used for graceful shutdown + an
// advisory file lock.
extern "C" {
    fn signal(signum: i32, handler: usize) -> usize;
    fn flock(fd: i32, operation: i32) -> i32;
}

const SIGINT: i32 = 2;
const SIGTERM: i32 = 15;
const LOCK_EX: i32 = 2;
const LOCK_UN: i32 = 8;

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_signum: i32) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

/// Install SIGTERM/SIGINT handlers that flip the shutdown flag. The daemon
/// finishes the current tick, then exits cleanly.
pub fn install_shutdown_handler() {
    let h = on_signal as extern "C" fn(i32) as usize;
    unsafe {
        signal(SIGTERM, h);
        signal(SIGINT, h);
    }
}

pub fn shutdown_requested() -> bool {
    SHUTDOWN.load(Ordering::SeqCst)
}

/// RAII advisory exclusive lock (`flock(2)` LOCK_EX) over an open file; serializes
/// concurrent `vpnmux set` so the generation read-bump-write doesn't race.
pub struct FileLock {
    file: std::fs::File,
}

impl FileLock {
    pub fn acquire(path: &str) -> crate::types::Result<FileLock> {
        use std::os::unix::io::AsRawFd;
        if let Some(dir) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        // SAFETY: fd is valid for the lifetime of `file`.
        if unsafe { flock(file.as_raw_fd(), LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(FileLock { file })
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        // SAFETY: fd is valid until `file` drops right after.
        unsafe {
            flock(self.file.as_raw_fd(), LOCK_UN);
        }
    }
}
