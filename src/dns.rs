use crate::runner::Runner;
use crate::types::Result;
use std::fs;
use std::net::IpAddr;

const PROC_ROUTE: &str = "/proc/net/route";
/// Fixed resolvconf record name; we inject a gateway resolver, not an
/// interface-scoped one, so a stable name lets us add/delete idempotently.
const RESOLVCONF_RECORD: &str = "vpnmux";
const RESOLV_MODE: u32 = 0o644;
/// Trailing marker on the nameserver line we inject, so we can remove only our
/// own backfill when Mullvad reclaims DNS — never a line the operator added.
const MARKER: &str = "# vpnmux";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsBackend {
    Static,
    SystemdResolved,
    NetworkManager,
    Resolvconf,
    Netconfig,
    OtherManaged,
}

impl DnsBackend {
    pub fn as_str(&self) -> &'static str {
        match self {
            DnsBackend::Static => "static",
            DnsBackend::SystemdResolved => "systemd-resolved",
            DnsBackend::NetworkManager => "network-manager",
            DnsBackend::Resolvconf => "resolvconf",
            DnsBackend::Netconfig => "netconfig",
            DnsBackend::OtherManaged => "other-managed",
        }
    }

    /// Inverse of `as_str`; unknown/legacy tokens default to `Static`.
    pub fn from_str(s: &str) -> DnsBackend {
        match s {
            "systemd-resolved" => DnsBackend::SystemdResolved,
            "network-manager" => DnsBackend::NetworkManager,
            "resolvconf" => DnsBackend::Resolvconf,
            "netconfig" => DnsBackend::Netconfig,
            "other-managed" => DnsBackend::OtherManaged,
            _ => DnsBackend::Static,
        }
    }
}

impl std::fmt::Display for DnsBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

const SYSTEMD_STUBS: [&str; 6] = [
    "/run/systemd/resolve/stub-resolv.conf",
    "/run/systemd/resolve/resolv.conf",
    "/var/run/systemd/resolve/stub-resolv.conf",
    "/var/run/systemd/resolve/resolv.conf",
    "/usr/lib/systemd/resolv.conf",
    "/lib/systemd/resolv.conf",
];

/// Detect which subsystem owns resolv.conf so we can backfill only where it's
/// safe (mirrors Tailscale's `resolvOwner` / Mullvad's symlink checks).
pub fn detect_backend(resolv_conf: &str) -> DnsBackend {
    if let Ok(target) = fs::read_link(resolv_conf) {
        return classify_symlink_target(&target.to_string_lossy());
    }
    let Ok(text) = fs::read_to_string(resolv_conf) else {
        return DnsBackend::Static;
    };
    if let Some(b) = classify_comment_header(&text) {
        return b;
    }
    if all_nameservers_are_localhost_resolved(&text) {
        return DnsBackend::SystemdResolved;
    }
    DnsBackend::Static
}

fn classify_symlink_target(target: &str) -> DnsBackend {
    use std::path::{Component, Path};
    let p = Path::new(target);
    if SYSTEMD_STUBS.iter().any(|s| is_anchored_stub(p, s)) {
        return DnsBackend::SystemdResolved;
    }
    let has_component = |name: &str| {
        p.components()
            .any(|c| matches!(c, Component::Normal(n) if n == name))
    };
    if has_component("resolvconf") {
        DnsBackend::Resolvconf
    } else if has_component("netconfig") {
        DnsBackend::Netconfig
    } else {
        DnsBackend::OtherManaged
    }
}

/// True when `target` is one of the systemd stubs anchored at the filesystem
/// root: the stub's components must match from the root down (absolute), or
/// after a leading run of `.`/`..` (relative). This rejects lookalikes that
/// merely *end with* the stub, e.g. `/var/lib/x/run/systemd/resolve/...`.
fn is_anchored_stub(target: &std::path::Path, stub: &str) -> bool {
    use std::path::Component;
    let stub_tail: Vec<Component> = std::path::Path::new(stub)
        .components()
        .filter(|c| !matches!(c, Component::RootDir))
        .collect();
    let target_tail: Vec<Component> = target
        .components()
        .skip_while(|c| {
            matches!(
                c,
                Component::RootDir | Component::CurDir | Component::ParentDir
            )
        })
        .collect();
    target_tail == stub_tail
}

/// Scan only the leading contiguous run of comment lines; stop at the first
/// non-comment line (blank or content). Manager-written headers are a solid
/// block at the top, so a `resolvconf` mention after a blank line is incidental.
fn classify_comment_header(text: &str) -> Option<DnsBackend> {
    for line in text.lines() {
        let t = line.trim_start();
        if !t.starts_with('#') {
            break;
        }
        if t.contains("systemd-resolved") {
            return Some(DnsBackend::SystemdResolved);
        }
        if t.contains("NetworkManager") {
            return Some(DnsBackend::NetworkManager);
        }
        if t.contains("resolvconf") {
            return Some(DnsBackend::Resolvconf);
        }
    }
    None
}

fn all_nameservers_are_localhost_resolved(text: &str) -> bool {
    let mut saw = false;
    for l in text.lines() {
        if let Some(ns) = l.trim().strip_prefix("nameserver") {
            saw = true;
            if ns.trim() != "127.0.0.53" {
                return false;
            }
        }
    }
    saw
}

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
pub fn ensure_resolver(
    resolv_conf: &str,
    backend: DnsBackend,
    resolver: Option<&str>,
    r: &dyn Runner,
) -> Result<()> {
    if !matches!(backend, DnsBackend::Static | DnsBackend::Resolvconf) {
        crate::debug!("dns: managed by {backend}; no backfill needed");
        return Ok(());
    }
    // On a resolvconf box /etc/resolv.conf is the generated file, so the same
    // "no nameserver" gate that guards the static path also stops us re-running
    // `resolvconf -a vpnmux` every tick once our record is already in effect.
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
    match backend {
        DnsBackend::Static => ensure_resolver_static(resolv_conf, &current, ns),
        DnsBackend::Resolvconf => ensure_resolver_resolvconf(ns, r),
        _ => unreachable!(),
    }
}

fn ensure_resolver_static(resolv_conf: &str, current: &str, ns: &str) -> Result<()> {
    if is_symlink(resolv_conf) {
        crate::error!("dns: {resolv_conf} is a symlink (resolved/NM stub?); refusing to write");
        return Ok(());
    }
    crate::info!("dns: no resolver present; set nameserver {ns}");
    crate::fsutil::write_atomic(resolv_conf, &inject_nameserver(current, ns), RESOLV_MODE)?;
    Ok(())
}

/// Remove the vpnmux-tagged nameserver line when Mullvad reclaims DNS, so the
/// backfilled gateway resolver doesn't linger and shadow Mullvad's.
pub fn remove_injected_resolver(
    resolv_conf: &str,
    backend: DnsBackend,
    r: &dyn Runner,
) -> Result<()> {
    match backend {
        DnsBackend::Static => remove_injected_resolver_static(resolv_conf),
        DnsBackend::Resolvconf => remove_injected_resolver_resolvconf(r),
        managed => {
            crate::debug!("dns: managed by {managed}; nothing to remove");
            Ok(())
        }
    }
}

fn remove_injected_resolver_static(resolv_conf: &str) -> Result<()> {
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

fn ensure_resolver_resolvconf(ns: &str, r: &dyn Runner) -> Result<()> {
    if resolvconf_is_shim() {
        crate::debug!("dns: resolvconf is a resolvectl shim (systemd-resolved); no backfill");
        return Ok(());
    }
    crate::info!("dns: resolvconf backfill nameserver {ns}");
    resolvconf_add(RESOLVCONF_RECORD, ns, r)
}

/// Issue `resolvconf -a <record>` with a `nameserver <ip>` body. No filesystem
/// probing — the shim guard lives in the caller.
fn resolvconf_add(record: &str, ip: &str, r: &dyn Runner) -> Result<()> {
    r.run_stdin("resolvconf", &["-a", record], &format!("nameserver {ip}\n"))?;
    Ok(())
}

fn remove_injected_resolver_resolvconf(r: &dyn Runner) -> Result<()> {
    if resolvconf_is_shim() {
        return Ok(());
    }
    crate::info!("dns: mullvad active; removed resolvconf vpnmux record");
    resolvconf_del(RESOLVCONF_RECORD, r)
}

/// Issue `resolvconf -d <record> -f`. The `-f` makes the remove idempotent by
/// suppressing "record not found". No filesystem probing here.
fn resolvconf_del(record: &str, r: &dyn Runner) -> Result<()> {
    r.run("resolvconf", &["-d", record, "-f"])?;
    Ok(())
}

/// systemd ships a `resolvconf` symlink to `resolvectl`; treat that as
/// systemd-resolved (the CLI semantics differ) and skip our backfill.
fn resolvconf_is_shim() -> bool {
    let path = find_binary("resolvconf");
    let target = path
        .as_deref()
        .and_then(|p| fs::read_link(p).ok())
        .map(|t| t.to_string_lossy().into_owned());
    resolvconf_is_resolvectl_shim(path.as_deref(), target.as_deref())
}

fn resolvconf_is_resolvectl_shim(path_lookup: Option<&str>, link_target: Option<&str>) -> bool {
    path_lookup.is_some()
        && link_target.is_some_and(|t| {
            std::path::Path::new(t)
                .file_name()
                .is_some_and(|n| n == "resolvectl")
        })
}

fn find_binary(name: &str) -> Option<String> {
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':').filter(|d| !d.is_empty()) {
            let cand = std::path::Path::new(dir).join(name);
            if cand.exists() {
                return Some(cand.to_string_lossy().into_owned());
            }
        }
    }
    for cand in ["/usr/bin", "/sbin", "/usr/sbin"] {
        let p = std::path::Path::new(cand).join(name);
        if p.exists() {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    None
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
    use crate::runner::MockRunner;

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
        let r = MockRunner::new();
        fs::write(p, "search example.ts.net\n").unwrap();

        ensure_resolver(p, DnsBackend::Static, Some("10.10.10.1"), &r).unwrap();
        assert_eq!(
            fs::read_to_string(p).unwrap(),
            "nameserver 10.10.10.1 # vpnmux\nsearch example.ts.net\n"
        );

        // Idempotent: a nameserver now exists, so a second pass leaves it alone.
        fs::write(p, "nameserver 9.9.9.9\n").unwrap();
        ensure_resolver(p, DnsBackend::Static, Some("10.10.10.1"), &r).unwrap();
        assert_eq!(fs::read_to_string(p).unwrap(), "nameserver 9.9.9.9\n");

        // No resolver available yet → no write; wait for a later tick.
        fs::write(p, "search only\n").unwrap();
        ensure_resolver(p, DnsBackend::Static, None, &r).unwrap();
        assert_eq!(fs::read_to_string(p).unwrap(), "search only\n");

        fs::remove_file(p).ok();
    }

    #[test]
    fn ensure_resolver_skips_invalid_ip() {
        let path = std::env::temp_dir().join(format!("vpnmux-resolv-bad-{}", std::process::id()));
        let p = path.to_str().unwrap();
        let r = MockRunner::new();
        fs::write(p, "search only\n").unwrap();
        ensure_resolver(p, DnsBackend::Static, Some("not-an-ip"), &r).unwrap();
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
        let r = MockRunner::new();
        ensure_resolver(
            link.to_str().unwrap(),
            DnsBackend::Static,
            Some("10.10.10.1"),
            &r,
        )
        .unwrap();
        // Target untouched — we refused to follow the link.
        assert_eq!(fs::read_to_string(&target).unwrap(), "search only\n");
        fs::remove_file(&link).ok();
        fs::remove_file(&target).ok();
    }

    #[test]
    fn remove_injected_strips_only_tagged_line() {
        let path = std::env::temp_dir().join(format!("vpnmux-resolv-rm-{}", std::process::id()));
        let p = path.to_str().unwrap();
        let r = MockRunner::new();
        fs::write(p, "nameserver 10.10.10.1 # vpnmux\nsearch foo\n").unwrap();
        remove_injected_resolver(p, DnsBackend::Static, &r).unwrap();
        assert_eq!(fs::read_to_string(p).unwrap(), "search foo\n");

        // A foreign nameserver is left alone.
        fs::write(p, "nameserver 9.9.9.9\n").unwrap();
        remove_injected_resolver(p, DnsBackend::Static, &r).unwrap();
        assert_eq!(fs::read_to_string(p).unwrap(), "nameserver 9.9.9.9\n");

        fs::remove_file(p).ok();
    }

    fn link_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("vpnmux-be-{}-{}", tag, std::process::id()))
    }

    fn symlink_to(tag: &str, target: &str) -> std::path::PathBuf {
        let link = link_path(tag);
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(target, &link).unwrap();
        link
    }

    #[test]
    fn detect_systemd_stub_symlink() {
        let l = symlink_to("stub", "/run/systemd/resolve/stub-resolv.conf");
        assert_eq!(
            detect_backend(l.to_str().unwrap()),
            DnsBackend::SystemdResolved
        );
        fs::remove_file(&l).ok();
    }

    #[test]
    fn detect_resolvconf_symlink() {
        let l = symlink_to("rc", "/run/resolvconf/resolv.conf");
        assert_eq!(detect_backend(l.to_str().unwrap()), DnsBackend::Resolvconf);
        fs::remove_file(&l).ok();
    }

    #[test]
    fn detect_netconfig_symlink() {
        let l = symlink_to("nc", "/run/netconfig/resolv.conf");
        assert_eq!(detect_backend(l.to_str().unwrap()), DnsBackend::Netconfig);
        fs::remove_file(&l).ok();
    }

    #[test]
    fn detect_unknown_symlink_is_other_managed() {
        let l = symlink_to("other", "/some/where/else.conf");
        assert_eq!(
            detect_backend(l.to_str().unwrap()),
            DnsBackend::OtherManaged
        );
        fs::remove_file(&l).ok();
    }

    fn write_resolv(tag: &str, body: &str) -> std::path::PathBuf {
        let p = link_path(tag);
        fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn detect_networkmanager_header() {
        let p = write_resolv("nm", "# Generated by NetworkManager\nnameserver 1.1.1.1\n");
        assert_eq!(
            detect_backend(p.to_str().unwrap()),
            DnsBackend::NetworkManager
        );
        fs::remove_file(&p).ok();
    }

    #[test]
    fn detect_resolvconf_header() {
        let p = write_resolv(
            "rch",
            "# Dynamic resolv.conf file generated by resolvconf\nnameserver 1.1.1.1\n",
        );
        assert_eq!(detect_backend(p.to_str().unwrap()), DnsBackend::Resolvconf);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn detect_static_when_resolvconf_mentioned_after_blank_line() {
        // The leading comment block ends at the blank line, so the `resolvconf`
        // mention below it is incidental and must not flip to Resolvconf.
        let p = write_resolv(
            "blank",
            "# plain static file\n\n# resolvconf mentioned in passing\nnameserver 1.1.1.1\n",
        );
        assert_eq!(detect_backend(p.to_str().unwrap()), DnsBackend::Static);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn classify_comment_header_stops_at_blank_line() {
        assert_eq!(
            classify_comment_header("# header\n\n# resolvconf\nnameserver 1.1.1.1\n"),
            None
        );
        // A contiguous block still classifies.
        assert_eq!(
            classify_comment_header("# generated by resolvconf\nnameserver 1.1.1.1\n"),
            Some(DnsBackend::Resolvconf)
        );
    }

    #[test]
    fn classify_symlink_components_match_real_and_reject_lookalikes() {
        // Real systemd stub → SystemdResolved.
        assert_eq!(
            classify_symlink_target("/run/systemd/resolve/stub-resolv.conf"),
            DnsBackend::SystemdResolved
        );
        // Relative stub target still classifies correctly.
        assert_eq!(
            classify_symlink_target("../run/systemd/resolve/stub-resolv.conf"),
            DnsBackend::SystemdResolved
        );
        // Crafted lookalike with extra leading components is NOT the stub.
        assert_eq!(
            classify_symlink_target("/var/lib/x/run/systemd/resolve/stub-resolv.conf"),
            DnsBackend::OtherManaged
        );
        // A path component literally named `resolvconf` matches; a substring
        // inside a single component does not.
        assert_eq!(
            classify_symlink_target("/run/resolvconf/resolv.conf"),
            DnsBackend::Resolvconf
        );
        assert_eq!(
            classify_symlink_target("/var/lib/myresolvconfdir/resolv.conf"),
            DnsBackend::OtherManaged
        );
        assert_eq!(
            classify_symlink_target("/run/netconfig/resolv.conf"),
            DnsBackend::Netconfig
        );
    }

    #[test]
    fn detect_all_localhost_resolved_is_systemd() {
        let p = write_resolv("loop", "nameserver 127.0.0.53\noptions edns0\n");
        assert_eq!(
            detect_backend(p.to_str().unwrap()),
            DnsBackend::SystemdResolved
        );
        fs::remove_file(&p).ok();
    }

    #[test]
    fn detect_plain_nameserver_is_static() {
        let p = write_resolv("plain", "nameserver 10.0.0.1\n");
        assert_eq!(detect_backend(p.to_str().unwrap()), DnsBackend::Static);
        fs::remove_file(&p).ok();
    }

    // Exercise the command path directly, bypassing the real-filesystem shim
    // guard so the assertion holds on hosts where `resolvconf` is a resolvectl
    // symlink (systemd-resolved Ubuntu/Fedora) as well as where it's absent.
    #[test]
    fn resolvconf_add_invokes_resolvconf_a() {
        let r = MockRunner::new();
        resolvconf_add(RESOLVCONF_RECORD, "10.10.10.1", &r).unwrap();
        assert!(r.called("resolvconf -a vpnmux"));
    }

    #[test]
    fn ensure_resolver_resolvconf_gates_on_existing_nameserver() {
        let path = link_path("rc-gate");
        let p = path.to_str().unwrap();

        // resolv.conf already has a nameserver (our prior record, regenerated, or
        // another interface's) → no gap → must NOT re-run `resolvconf -a vpnmux`.
        let r = MockRunner::new();
        fs::write(p, "nameserver 10.64.0.1 # vpnmux\nsearch foo\n").unwrap();
        ensure_resolver(p, DnsBackend::Resolvconf, Some("10.10.10.1"), &r).unwrap();
        assert!(!r.called("resolvconf -a vpnmux"));

        // No nameserver → gap → backfill (unless resolvconf is a resolvectl shim
        // on this host, in which case the guard correctly skips).
        let r = MockRunner::new();
        fs::write(p, "search foo\n").unwrap();
        ensure_resolver(p, DnsBackend::Resolvconf, Some("10.10.10.1"), &r).unwrap();
        assert_eq!(r.called("resolvconf -a vpnmux"), !resolvconf_is_shim());

        fs::remove_file(p).ok();
    }

    #[test]
    fn resolvconf_del_invokes_resolvconf_d() {
        let r = MockRunner::new();
        resolvconf_del(RESOLVCONF_RECORD, &r).unwrap();
        assert!(r.called("resolvconf -d vpnmux -f"));
    }

    #[test]
    fn managed_backend_is_noop() {
        let r = MockRunner::new();
        ensure_resolver("", DnsBackend::SystemdResolved, Some("10.10.10.1"), &r).unwrap();
        remove_injected_resolver("", DnsBackend::NetworkManager, &r).unwrap();
        assert!(r.calls.borrow().is_empty());
    }

    #[test]
    fn resolvectl_shim_is_detected() {
        assert!(resolvconf_is_resolvectl_shim(
            Some("/usr/bin/resolvconf"),
            Some("/usr/bin/resolvectl")
        ));
        assert!(!resolvconf_is_resolvectl_shim(
            Some("/sbin/resolvconf"),
            Some("/sbin/resolvconf")
        ));
        // Not a symlink at all → not a shim.
        assert!(!resolvconf_is_resolvectl_shim(
            Some("/sbin/resolvconf"),
            None
        ));
    }
}
