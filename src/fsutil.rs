use crate::types::Result;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const O_NOFOLLOW: i32 = 0x20000;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Atomic write: create a fresh temp file in the target's directory (O_NOFOLLOW
/// plus create_new closes the predictable-name symlink race), write, then
/// rename into place. `mode` sets the final file's permission bits.
pub fn write_atomic(path: &str, contents: &str, mode: u32) -> Result<()> {
    let target = Path::new(path);
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = format!(".vpnmux.tmp.{}.{}.{}", std::process::id(), nanos, seq);
    let tmp = dir.join(name);

    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(O_NOFOLLOW)
        .mode(mode)
        .open(&tmp)?;
    // `.mode()` is masked by the process umask; force exact bits so callers get
    // what they ask for (resolv.conf must be 0644 even under UMask=0077).
    f.set_permissions(std::fs::Permissions::from_mode(mode))?;
    f.write_all(contents.as_bytes())?;
    f.sync_all()?;
    drop(f);

    if let Err(e) = std::fs::rename(&tmp, target) {
        std::fs::remove_file(&tmp).ok();
        return Err(e.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!("vpnmux-fsutil-{}-{}", tag, std::process::id()));
        p.to_string_lossy().into_owned()
    }

    // Self-declared umask(2) binding (mirrors src/sys.rs) so the test can prove
    // the mode is exact regardless of the process umask, without pulling in libc.
    extern "C" {
        fn umask(mask: u32) -> u32;
    }

    #[test]
    fn writes_contents_and_mode() {
        let path = temp_path("mode");
        write_atomic(&path, "hello\n", 0o600).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\n");
        let perm = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(perm, 0o600);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn mode_is_exact_under_restrictive_umask() {
        let path = temp_path("umask");
        // SAFETY: process-global state; restored before returning.
        let prev = unsafe { umask(0o077) };
        let res = write_atomic(&path, "nameserver 1.1.1.1\n", 0o644);
        // SAFETY: restore the umask we clobbered above.
        unsafe { umask(prev) };
        res.unwrap();
        let perm = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(perm, 0o644, "umask must not strip the requested bits");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn overwrites_existing_target_atomically() {
        let path = temp_path("overwrite");
        write_atomic(&path, "first\n", 0o644).unwrap();
        write_atomic(&path, "second\n", 0o644).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second\n");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn concurrent_writes_do_not_collide_on_temp_name() {
        let path = temp_path("concurrent");
        for _ in 0..50 {
            write_atomic(&path, "x\n", 0o600).unwrap();
        }
        std::fs::remove_file(&path).ok();
    }
}
