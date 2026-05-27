use crate::types::Result;
use std::fs;
use std::net::IpAddr;

const PROC_ROUTE: &str = "/proc/net/route";
const RESOLV_MODE: u32 = 0o644;
/// Trailing marker on the nameserver line we inject, so we can remove only our
/// own backfill when Mullvad reclaims DNS — never a line the operator added.
const MARKER: &str = "# vpnmux";

/// Resolver to inject: `VPNMUX_DNS` override → default-route gateway.
pub fn preferred_resolver() -> Option<String> {
    pick_resolver(std::env::var("VPNMUX_DNS").ok(), default_gateway())
}

/// Put `resolver` into resolv.conf when it has no nameserver — Mullvad, when it
/// disconnects, takes its `10.64.0.1` resolver with it and nothing else fills
/// the gap on a box without a DNS manager. Only acts when no nameserver is
/// present, so it never overrides systemd-resolved/NetworkManager. A `None`
/// resolver (no gateway visible yet — e.g. routes still settling after the
/// disconnect) is a no-op: the next tick retries rather than guessing.
pub fn ensure_resolver(resolv_conf: &str, resolver: Option<&str>) -> Result<()> {
    if is_symlink(resolv_conf) {
        crate::error!("dns: {resolv_conf} is a symlink (resolved/NM stub?); refusing to write");
        return Ok(());
    }
    let current = fs::read_to_string(resolv_conf).unwrap_or_default();
    if has_nameserver(&current) {
        return Ok(());
    }
    let Some(ns) = resolver else {
        crate::debug!("dns: no resolver yet (no gateway / VPNMUX_DNS); will retry");
        return Ok(());
    };
    if ns.parse::<IpAddr>().is_err() {
        crate::error!("dns: VPNMUX_DNS/gateway {ns:?} is not a valid IP; skipping");
        return Ok(());
    }
    crate::info!("dns: no resolver present; set nameserver {ns}");
    crate::fsutil::write_atomic(resolv_conf, &inject_nameserver(&current, ns), RESOLV_MODE)?;
    Ok(())
}

/// Remove the vpnmux-tagged nameserver line when Mullvad reclaims DNS, so the
/// backfilled gateway resolver doesn't linger and shadow Mullvad's.
pub fn remove_injected_resolver(resolv_conf: &str) -> Result<()> {
    if is_symlink(resolv_conf) {
        return Ok(());
    }
    let Ok(current) = fs::read_to_string(resolv_conf) else {
        return Ok(());
    };
    if !current.lines().any(is_injected_line) {
        return Ok(());
    }
    let mut cleaned = String::with_capacity(current.len());
    for line in current.lines().filter(|l| !is_injected_line(l)) {
        cleaned.push_str(line);
        cleaned.push('\n');
    }
    crate::info!("dns: mullvad active; removed backfilled vpnmux nameserver");
    crate::fsutil::write_atomic(resolv_conf, &cleaned, RESOLV_MODE)?;
    Ok(())
}

fn is_symlink(path: &str) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

fn is_injected_line(line: &str) -> bool {
    let l = line.trim();
    l.starts_with("nameserver") && l.ends_with(MARKER)
}

fn has_nameserver(resolv: &str) -> bool {
    resolv
        .lines()
        .any(|l| l.trim_start().starts_with("nameserver"))
}

/// `VPNMUX_DNS` override → default-route gateway. `None` when neither is known,
/// so we wait instead of writing a public resolver behind your back.
fn pick_resolver(env_dns: Option<String>, gateway: Option<String>) -> Option<String> {
    env_dns.filter(|s| !s.trim().is_empty()).or(gateway)
}

/// Prepend our tagged nameserver, keeping any existing lines (e.g. a search
/// domain). The trailing marker lets us remove only our own line later.
fn inject_nameserver(current: &str, ns: &str) -> String {
    let mut out = format!("nameserver {ns} {MARKER}\n");
    for line in current.lines() {
        if !line.trim().is_empty() {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn default_gateway() -> Option<String> {
    default_gateway_from(&fs::read_to_string(PROC_ROUTE).ok()?)
}

/// Parse the default route's gateway from /proc/net/route. The Gateway column
/// is a little-endian hex u32; the lowest-metric default route wins.
fn default_gateway_from(table: &str) -> Option<String> {
    let mut best: Option<(u64, String)> = None;
    for line in table.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 11 || f[1] != "00000000" || f[2] == "00000000" {
            continue;
        }
        let Ok(v) = u32::from_str_radix(f[2], 16) else {
            continue;
        };
        let ip = format!(
            "{}.{}.{}.{}",
            v & 0xff,
            (v >> 8) & 0xff,
            (v >> 16) & 0xff,
            (v >> 24) & 0xff
        );
        let metric: u64 = f[6].parse().unwrap_or(u64::MAX);
        if best.as_ref().is_none_or(|(m, _)| metric < *m) {
            best = Some((metric, ip));
        }
    }
    best.map(|(_, ip)| ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTE: &str =
        "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT
wlp1s0\t00000000\t010A0A0A\t0003\t0\t0\t600\t00000000\t0\t0\t0
wlp1s0\t000A0A0A\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0";

    #[test]
    fn parses_default_gateway_little_endian() {
        assert_eq!(default_gateway_from(ROUTE), Some("10.10.10.1".to_string()));
    }

    #[test]
    fn lowest_metric_default_route_wins() {
        let table = "h\na\t00000000\t0101010A\t0003\t0\t0\t1000\t0\t0\t0\t0\n\
                     b\t00000000\t010A0A0A\t0003\t0\t0\t600\t0\t0\t0\t0";
        assert_eq!(default_gateway_from(table), Some("10.10.10.1".to_string()));
    }

    #[test]
    fn no_default_route_is_none() {
        let table = "h\nwlp1s0\t000A0A0A\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0";
        assert_eq!(default_gateway_from(table), None);
    }

    #[test]
    fn has_nameserver_detects_presence() {
        assert!(has_nameserver("search foo\nnameserver 10.0.0.1\n"));
        assert!(!has_nameserver("search foo\n"));
        assert!(!has_nameserver(""));
    }

    #[test]
    fn pick_resolver_precedence() {
        assert_eq!(
            pick_resolver(Some("9.9.9.9".into()), Some("10.0.0.1".into())),
            Some("9.9.9.9".into())
        );
        assert_eq!(
            pick_resolver(None, Some("10.0.0.1".into())),
            Some("10.0.0.1".into())
        );
        assert_eq!(pick_resolver(Some("  ".into()), None), None);
        assert_eq!(pick_resolver(None, None), None);
    }

    #[test]
    fn inject_prepends_tagged_nameserver_and_keeps_search() {
        let out = inject_nameserver("search example.ts.net\n", "10.10.10.1");
        assert_eq!(
            out,
            "nameserver 10.10.10.1 # vpnmux\nsearch example.ts.net\n"
        );
    }

    #[test]
    fn ensure_resolver_writes_only_when_missing() {
        let path = std::env::temp_dir().join(format!("vpnmux-resolv-{}", std::process::id()));
        let p = path.to_str().unwrap();
        fs::write(p, "search example.ts.net\n").unwrap();

        ensure_resolver(p, Some("10.10.10.1")).unwrap();
        assert_eq!(
            fs::read_to_string(p).unwrap(),
            "nameserver 10.10.10.1 # vpnmux\nsearch example.ts.net\n"
        );

        // Idempotent: a nameserver now exists, so a second pass leaves it alone.
        fs::write(p, "nameserver 9.9.9.9\n").unwrap();
        ensure_resolver(p, Some("10.10.10.1")).unwrap();
        assert_eq!(fs::read_to_string(p).unwrap(), "nameserver 9.9.9.9\n");

        // No resolver available yet → no write; wait for a later tick.
        fs::write(p, "search only\n").unwrap();
        ensure_resolver(p, None).unwrap();
        assert_eq!(fs::read_to_string(p).unwrap(), "search only\n");

        fs::remove_file(p).ok();
    }

    #[test]
    fn ensure_resolver_skips_invalid_ip() {
        let path = std::env::temp_dir().join(format!("vpnmux-resolv-bad-{}", std::process::id()));
        let p = path.to_str().unwrap();
        fs::write(p, "search only\n").unwrap();
        ensure_resolver(p, Some("not-an-ip")).unwrap();
        assert_eq!(fs::read_to_string(p).unwrap(), "search only\n");
        fs::remove_file(p).ok();
    }

    #[test]
    fn ensure_resolver_refuses_symlink() {
        let dir = std::env::temp_dir();
        let target = dir.join(format!("vpnmux-resolv-tgt-{}", std::process::id()));
        let link = dir.join(format!("vpnmux-resolv-link-{}", std::process::id()));
        fs::write(&target, "search only\n").unwrap();
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(&target, &link).unwrap();
        ensure_resolver(link.to_str().unwrap(), Some("10.10.10.1")).unwrap();
        // Target untouched — we refused to follow the link.
        assert_eq!(fs::read_to_string(&target).unwrap(), "search only\n");
        fs::remove_file(&link).ok();
        fs::remove_file(&target).ok();
    }

    #[test]
    fn remove_injected_strips_only_tagged_line() {
        let path = std::env::temp_dir().join(format!("vpnmux-resolv-rm-{}", std::process::id()));
        let p = path.to_str().unwrap();
        fs::write(p, "nameserver 10.10.10.1 # vpnmux\nsearch foo\n").unwrap();
        remove_injected_resolver(p).unwrap();
        assert_eq!(fs::read_to_string(p).unwrap(), "search foo\n");

        // A foreign nameserver is left alone.
        fs::write(p, "nameserver 9.9.9.9\n").unwrap();
        remove_injected_resolver(p).unwrap();
        assert_eq!(fs::read_to_string(p).unwrap(), "nameserver 9.9.9.9\n");

        fs::remove_file(p).ok();
    }
}
