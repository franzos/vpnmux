use crate::paths;
use crate::provider::Availability;
use crate::state::{read_desired, read_status, write_desired, Desired, Status};
use crate::sys::FileLock;
use crate::types::{format_set, parse_set, providers, ProviderId, ProviderSet, Result};
use std::io::Write;
use std::time::{Duration, Instant};

const DESIRED_LOCK: &str = "/var/lib/vpnmux/desired.lock";

pub fn set(args: &[String]) -> Result<()> {
    let (want, yes) = parse_args(args)?;

    let nft = paths::nft();
    let mullvad_bin = paths::mullvad();
    let tailscale_bin = paths::tailscale();
    let registry = providers(&mullvad_bin, &tailscale_bin, &nft);
    let runner = crate::runner::RealRunner;

    // Availability precheck (read-only, informational).
    for p in &registry {
        if want.contains(&p.id()) {
            if let Availability::Unavailable(reason) = p.check(&runner) {
                eprintln!(
                    "vpnmux: {}: unavailable — {reason} (may be transient; will retry)",
                    p.id()
                );
            }
        }
    }

    // Lockdown precheck: leaving Mullvad while lockdown is on loses connectivity.
    if !want.contains(&ProviderId::Mullvad) && lockdown_on(&runner) && !yes {
        eprint!(
            "Mullvad lockdown is active — switching to '{}' will block ALL connectivity \
             (incl. the tailnet) until you reconnect Mullvad or disable lockdown. Continue? [y/N] ",
            if want.is_empty() {
                "none".into()
            } else {
                format_set(&want)
            }
        );
        std::io::stderr().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if !matches!(line.trim(), "y" | "Y" | "yes") {
            eprintln!("vpnmux: aborted.");
            return Ok(());
        }
    }

    // Serialize concurrent `set` so the generation read-bump-write can't race.
    let _lock = FileLock::acquire(DESIRED_LOCK).map_err(hint_on_eacces)?;
    let prev_gen = read_desired(paths::DESIRED_FILE)?.map_or(0, |d| d.generation);
    let gen = prev_gen + 1;
    write_desired(
        paths::DESIRED_FILE,
        &Desired {
            generation: gen,
            providers: want.clone(),
        },
    )?;

    // Wait for the daemon to reconcile our generation (or time out).
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        // A status parse error means "not yet reconciled" — keep polling rather
        // than aborting the whole set (writes are atomic, so this is rare).
        if let Ok(Some(s)) = read_status(paths::STATUS_FILE) {
            if s.generation >= gen {
                println!(
                    "vpnmux: active = {}",
                    if s.active.is_empty() {
                        "none".into()
                    } else {
                        format_set(&s.active)
                    }
                );
                let unmet: Vec<_> = want.iter().filter(|p| !s.active.contains(p)).collect();
                if !unmet.is_empty() {
                    eprintln!(
                        "vpnmux: not yet active: {}",
                        unmet
                            .iter()
                            .map(|p| p.as_str())
                            .collect::<Vec<_>>()
                            .join(",")
                    );
                    std::process::exit(1);
                }
                return Ok(());
            }
        }
        if Instant::now() > deadline {
            eprintln!("vpnmux: timed out waiting for daemon (is the vpnmux service running?)");
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

pub fn status(args: &[String]) -> Result<()> {
    let json = args.iter().any(|a| a == "--json");
    let s = read_status(paths::STATUS_FILE)?;
    if json {
        println!("{}", status_json(s.as_ref()));
        return Ok(());
    }
    if let Some(s) = s {
        println!("generation: {}", s.generation);
        println!(
            "active: {}",
            if s.active.is_empty() {
                "none".into()
            } else {
                format_set(&s.active)
            }
        );
        println!("dns: {}", s.dns);
    } else {
        println!("vpnmux: no status yet (daemon not running, or never reconciled)");
    }
    Ok(())
}

/// Machine-readable status; `None` yields an empty payload so waybar always gets valid JSON.
fn status_json(s: Option<&Status>) -> String {
    let Some(s) = s else {
        return "{\"generation\":0,\"active\":[],\"available\":[],\"unavailable\":[],\"dns\":\"static\"}".to_string();
    };
    let arr = |set: &ProviderSet| {
        set.iter()
            .map(|p| format!("\"{}\"", p.as_str()))
            .collect::<Vec<_>>()
            .join(",")
    };
    let unavailable = s
        .unavailable
        .iter()
        .map(|(id, reason)| {
            format!(
                "{{\"provider\":\"{}\",\"reason\":\"{}\"}}",
                id.as_str(),
                json_escape(reason)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"generation\":{},\"active\":[{}],\"available\":[{}],\"unavailable\":[{}],\"dns\":\"{}\"}}",
        s.generation,
        arr(&s.active),
        arr(&s.available),
        unavailable,
        s.dns.as_str()
    )
}

/// Parse `set` args into the desired provider set and the --yes flag. Accepts
/// both `mullvad,tailscale` (one comma-joined arg) and `mullvad tailscale`
/// (separate args), since either is natural to type.
fn parse_args(args: &[String]) -> Result<(ProviderSet, bool)> {
    let mut yes = false;
    let mut want = ProviderSet::new();
    for a in args {
        if a == "--yes" || a == "-y" {
            yes = true;
        } else {
            want.extend(parse_set(a)?);
        }
    }
    Ok((want, yes))
}

/// Translate a permission-denied error into actionable guidance — the dirs are
/// root-only unless the daemon has chowned them to the configured group.
fn hint_on_eacces(e: Box<dyn std::error::Error>) -> Box<dyn std::error::Error> {
    let msg = e.to_string();
    if msg.contains("Permission denied") || msg.contains("(os error 13)") {
        return format!(
            "{msg}\n\
             hint: add your user to the 'vpnmux' group (and re-login), \
             or run this command with sudo"
        )
        .into();
    }
    e
}

/// Shared lockdown-mode check used by the precheck; delegates to the single
/// `Mullvad::lockdown_on` parser.
fn lockdown_on(r: &dyn crate::runner::Runner) -> bool {
    let m = crate::mullvad::Mullvad {
        bin: paths::mullvad(),
        nft: paths::nft(),
    };
    m.lockdown_on(r)
}

/// Escape a string for a JSON double-quoted value (reasons come from CLI output).
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn comma_and_space_forms_are_equivalent() {
        let (comma, _) = parse_args(&args(&["mullvad,tailscale"])).unwrap();
        let (space, _) = parse_args(&args(&["mullvad", "tailscale"])).unwrap();
        assert_eq!(comma, space);
        assert!(comma.contains(&ProviderId::Mullvad));
        assert!(comma.contains(&ProviderId::Tailscale));
    }

    #[test]
    fn yes_flag_with_empty_set() {
        let (want, yes) = parse_args(&args(&["--yes"])).unwrap();
        assert!(yes);
        assert!(want.is_empty());
    }

    #[test]
    fn unknown_provider_is_an_error() {
        assert!(parse_args(&args(&["mullvad,wireguard"])).is_err());
    }

    #[test]
    fn json_escape_handles_specials() {
        assert_eq!(json_escape(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(json_escape("line1\nline2"), "line1\\nline2");
        assert_eq!(json_escape("tab\tend"), "tab\\tend");
        assert_eq!(json_escape("\u{0001}"), "\\u0001");
        assert_eq!(json_escape("a\rb"), "a\\rb");
    }

    #[test]
    fn status_json_shape() {
        let s = Status {
            generation: 12,
            active: parse_set("mullvad").unwrap(),
            available: parse_set("mullvad,tailscale").unwrap(),
            unavailable: vec![(ProviderId::Tailscale, "not logged in".into())],
            dns: crate::dns::DnsBackend::SystemdResolved,
        };
        assert_eq!(
            status_json(Some(&s)),
            "{\"generation\":12,\"active\":[\"mullvad\"],\"available\":[\"mullvad\",\"tailscale\"],\"unavailable\":[{\"provider\":\"tailscale\",\"reason\":\"not logged in\"}],\"dns\":\"systemd-resolved\"}"
        );
    }

    #[test]
    fn status_json_none_is_empty_payload() {
        assert_eq!(
            status_json(None),
            "{\"generation\":0,\"active\":[],\"available\":[],\"unavailable\":[],\"dns\":\"static\"}"
        );
    }

    #[test]
    fn status_json_escapes_reason() {
        let s = Status {
            generation: 1,
            active: ProviderSet::new(),
            available: ProviderSet::new(),
            unavailable: vec![(ProviderId::Mullvad, "weird \"quote\"".into())],
            dns: crate::dns::DnsBackend::Static,
        };
        assert_eq!(
            status_json(Some(&s)),
            "{\"generation\":1,\"active\":[],\"available\":[],\"unavailable\":[{\"provider\":\"mullvad\",\"reason\":\"weird \\\"quote\\\"\"}],\"dns\":\"static\"}"
        );
    }
}
