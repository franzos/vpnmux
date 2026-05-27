use std::fs;
use std::path::Path;

pub const DESIRED_FILE: &str = "/var/lib/vpnmux/desired";
pub const STATUS_FILE: &str = "/run/vpnmux/status";
pub const RESOLV_CONF: &str = "/etc/resolv.conf";

/// Where to look for binaries, in order: the Guix system profile (a root
/// shepherd service has a minimal `$PATH`), then the FHS locations Debian and
/// other distros use. A daemon under systemd or shepherd can't rely on `$PATH`.
const BIN_DIRS: &[&str] = &[
    "/run/current-system/profile/bin",
    "/usr/sbin",
    "/usr/bin",
    "/sbin",
    "/bin",
];

/// First `BIN_DIRS` entry that holds `name`, else the bare name so a populated
/// `$PATH` (e.g. an interactive shell, or systemd's default PATH) still works.
fn find_bin(name: &str) -> String {
    for dir in BIN_DIRS {
        let cand = format!("{dir}/{name}");
        if Path::new(&cand).exists() {
            return cand;
        }
    }
    name.to_string()
}

/// Resolve an external binary; the `VPNMUX_*` override wins.
fn resolve(var: &str, name: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| find_bin(name))
}

pub fn mullvad() -> String {
    resolve("VPNMUX_MULLVAD", "mullvad")
}

pub fn tailscale() -> String {
    resolve("VPNMUX_TAILSCALE", "tailscale")
}

/// Resolve `nft` like the other binaries: the `VPNMUX_NFT` override, then the
/// stable profile/FHS locations (incl. an `sbin` for nft), then `$PATH`. The
/// Guix `/gnu/store` scan is only a best-effort last resort — picking a store
/// path by string sort is a root-code-exec risk if the store is plantable, so
/// it runs only when no stable path exists.
pub fn nft() -> String {
    if let Ok(p) = std::env::var("VPNMUX_NFT") {
        return p;
    }
    if let Some(p) = find_bin_in("nft", NFT_DIRS) {
        return p;
    }
    if let Some(p) = nft_from_store() {
        return p;
    }
    "nft".to_string()
}

/// `nft` lives in `sbin` on most distros; include those alongside `BIN_DIRS`.
const NFT_DIRS: &[&str] = &[
    "/run/current-system/profile/sbin",
    "/run/current-system/profile/bin",
    "/usr/sbin",
    "/sbin",
    "/usr/bin",
    "/bin",
];

fn find_bin_in(name: &str, dirs: &[&str]) -> Option<String> {
    for dir in dirs {
        let cand = format!("{dir}/{name}");
        if is_regular_executable(&cand) {
            return Some(cand);
        }
    }
    None
}

/// Best-effort store fallback: take the lexically newest nftables build whose
/// `sbin/nft` is a real file (not a planted symlink/dir). Documented as
/// best-effort — the stable paths above are always preferred.
fn nft_from_store() -> Option<String> {
    let mut candidates: Vec<String> = Vec::new();
    for entry in fs::read_dir("/gnu/store").ok()?.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.contains("-nftables-") {
            let cand = path.join("sbin/nft");
            if let Some(s) = cand.to_str() {
                if is_regular_executable(s) {
                    candidates.push(s.to_string());
                }
            }
        }
    }
    candidates.sort();
    candidates.pop()
}

/// A real file (following symlinks) — guards against a planted directory or
/// dangling symlink being chosen as `nft`.
fn is_regular_executable(path: &str) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_wins() {
        std::env::set_var("VPNMUX_TEST_OVERRIDE", "/custom/path/to/bin");
        assert_eq!(
            resolve("VPNMUX_TEST_OVERRIDE", "bin"),
            "/custom/path/to/bin"
        );
        std::env::remove_var("VPNMUX_TEST_OVERRIDE");
    }

    #[test]
    fn unknown_bin_falls_back_to_bare_name() {
        std::env::remove_var("VPNMUX_TEST_MISSING");
        assert_eq!(
            resolve("VPNMUX_TEST_MISSING", "vpnmux-no-such-bin-xyz"),
            "vpnmux-no-such-bin-xyz"
        );
    }
}
