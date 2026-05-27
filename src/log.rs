use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub const ERROR: u8 = 0;
pub const INFO: u8 = 1;
pub const DEBUG: u8 = 2;

static LEVEL: AtomicU8 = AtomicU8::new(INFO);

/// Read VPNMUX_LOG once (error|info|debug, default info).
pub fn init() {
    let lvl = match std::env::var("VPNMUX_LOG").as_deref() {
        Ok("error") => ERROR,
        Ok("debug") => DEBUG,
        _ => INFO,
    };
    LEVEL.store(lvl, Ordering::Relaxed);
}

pub fn enabled(level: u8) -> bool {
    level <= LEVEL.load(Ordering::Relaxed)
}

pub fn emit(level: u8, args: std::fmt::Arguments) {
    let tag = match level {
        ERROR => "ERROR",
        INFO => "INFO",
        _ => "DEBUG",
    };
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    eprintln!("[{ts}] {tag} vpnmux: {args}");
}

#[macro_export]
macro_rules! error {
    ($($a:tt)*) => {
        if $crate::log::enabled($crate::log::ERROR) {
            $crate::log::emit($crate::log::ERROR, format_args!($($a)*))
        }
    };
}
#[macro_export]
macro_rules! info {
    ($($a:tt)*) => {
        if $crate::log::enabled($crate::log::INFO) {
            $crate::log::emit($crate::log::INFO, format_args!($($a)*))
        }
    };
}
#[macro_export]
macro_rules! debug {
    ($($a:tt)*) => {
        if $crate::log::enabled($crate::log::DEBUG) {
            $crate::log::emit($crate::log::DEBUG, format_args!($($a)*))
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn level_gating() {
        LEVEL.store(INFO, Ordering::Relaxed);
        assert!(enabled(ERROR));
        assert!(enabled(INFO));
        assert!(!enabled(DEBUG));
    }
}
