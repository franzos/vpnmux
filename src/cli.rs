use crate::paths;
use crate::provider::Availability;
use crate::state::{read_desired, read_status, write_desired, Desired};
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

pub fn status() -> Result<()> {
    if let Some(s) = read_status(paths::STATUS_FILE)? {
        println!("generation: {}", s.generation);
        println!(
            "active: {}",
            if s.active.is_empty() {
                "none".into()
            } else {
                format_set(&s.active)
            }
        );
    } else {
        println!("vpnmux: no status yet (daemon not running, or never reconciled)");
    }
    Ok(())
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
}
