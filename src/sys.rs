use std::ffi::c_char;
use std::sync::atomic::{AtomicBool, Ordering};

// Self-declared libc symbols keep the crate dependency-free. These are the
// stable System V / glibc signatures; only used for graceful shutdown, an
// advisory file lock, and chown'ing the state dirs.
extern "C" {
    fn signal(signum: i32, handler: usize) -> usize;
    fn flock(fd: i32, operation: i32) -> i32;
    fn chown(path: *const c_char, owner: u32, group: u32) -> i32;
}

const SIGINT: i32 = 2;
const SIGTERM: i32 = 15;
const LOCK_EX: i32 = 2;
const LOCK_UN: i32 = 8;
/// `(uid_t)-1` / `(gid_t)-1` — chown's "leave this side unchanged" sentinel.
const CHOWN_KEEP: u32 = u32::MAX;

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
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        use std::os::unix::io::AsRawFd;
        if let Some(dir) = std::path::Path::new(path).parent() {
            // Best-effort: the dir is provisioned by the daemon/packaging. A CLI
            // user without write perms here just falls through to the open()
            // below, which surfaces the real EACCES.
            let _ = std::fs::create_dir_all(dir);
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o660)
            .open(path)?;
        // `.mode()` is masked by the process umask; force the exact bits so a
        // CLI under a restrictive umask doesn't lock out other group members.
        let _ = file.set_permissions(std::fs::Permissions::from_mode(0o660));
        // SAFETY: fd is valid for the lifetime of `file`.
        if unsafe { flock(file.as_raw_fd(), LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(FileLock { file })
    }
}

/// Look up a GID by name by parsing `/etc/group`. Returns `None` if the file is
/// unreadable, missing, or doesn't contain the named group.
pub fn lookup_gid(group: &str) -> Option<u32> {
    let text = std::fs::read_to_string("/etc/group").ok()?;
    for line in text.lines() {
        let mut parts = line.splitn(4, ':');
        let name = parts.next()?;
        if name != group {
            continue;
        }
        let _passwd = parts.next()?;
        let gid_str = parts.next()?;
        return gid_str.parse().ok();
    }
    None
}

/// `chown(path, -1, gid)` — change group only, leave owner alone. Mirrors what
/// `mullvad-daemon` does to its management socket.
pub fn chown_group(path: &str, gid: u32) -> std::io::Result<()> {
    let c = std::ffi::CString::new(path)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    // SAFETY: c is a valid NUL-terminated C string for the duration of the call.
    let rc = unsafe { chown(c.as_ptr(), CHOWN_KEEP, gid) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
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
