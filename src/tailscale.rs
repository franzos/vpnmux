use crate::provider::{Availability, Provider, ProviderStatus};
use crate::runner::Runner;
use crate::types::{ProviderId, Result};

pub struct Tailscale {
    pub bin: String,
}

impl Provider for Tailscale {
    fn id(&self) -> ProviderId {
        ProviderId::Tailscale
    }

    fn status(&self, r: &dyn Runner) -> ProviderStatus {
        let Ok(out) = r.run(&self.bin, &["status", "--json"]) else {
            return ProviderStatus {
                availability: Availability::Unavailable("tailscale CLI failed".into()),
                active: false,
            };
        };
        match backend_state(&out.stdout) {
            Some("Running") => ProviderStatus {
                availability: Availability::Available,
                active: true,
            },
            Some("Stopped" | "Starting") => ProviderStatus {
                availability: Availability::Available,
                active: false,
            },
            Some("NeedsLogin" | "NeedsMachineAuth") => ProviderStatus {
                availability: Availability::Unavailable(
                    "not logged in (run: tailscale login)".into(),
                ),
                active: false,
            },
            Some("NoState") | None => ProviderStatus {
                availability: Availability::Unavailable("tailscaled not running".into()),
                active: false,
            },
            Some(_) => ProviderStatus {
                availability: Availability::Unavailable("tailscale in unknown state".into()),
                active: false,
            },
        }
    }

    fn activate(&self, r: &dyn Runner) -> Result<()> {
        r.run(&self.bin, &["up"])?;
        Ok(())
    }

    fn deactivate(&self, r: &dyn Runner) -> Result<()> {
        r.run(&self.bin, &["down"])?;
        Ok(())
    }
}

/// Scan `tailscale status --json` for the documented `BackendState` value
/// without pulling in a JSON parser: find the key, then the next quoted string.
fn backend_state(json: &str) -> Option<&str> {
    let after = &json[json.find("\"BackendState\"")? + "\"BackendState\"".len()..];
    let colon = after.find(':')?;
    let rest = &after[colon + 1..];
    let start = rest.find('"')? + 1;
    let end = rest[start..].find('"')? + start;
    Some(&rest[start..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::MockRunner;

    fn t() -> Tailscale {
        Tailscale {
            bin: "tailscale".into(),
        }
    }

    const RUNNING: &str = r#"{"Version":"1.98.3","BackendState":"Running","Self":{}}"#;
    const STOPPED: &str = r#"{"BackendState":"Stopped","Self":{}}"#;
    const NEEDS_LOGIN: &str = r#"{"BackendState":"NeedsLogin"}"#;
    const NO_STATE: &str = r#"{"BackendState":"NoState"}"#;

    #[test]
    fn parses_backend_state() {
        assert_eq!(backend_state(RUNNING), Some("Running"));
        assert_eq!(backend_state(STOPPED), Some("Stopped"));
        assert_eq!(backend_state("{}"), None);
    }

    #[test]
    fn check_unavailable_when_needs_login() {
        let r = MockRunner::new().on("tailscale status --json", 0, NEEDS_LOGIN);
        assert!(matches!(t().check(&r), Availability::Unavailable(_)));
    }

    #[test]
    fn check_unavailable_when_no_state() {
        let r = MockRunner::new().on("tailscale status --json", 0, NO_STATE);
        assert!(matches!(t().check(&r), Availability::Unavailable(_)));
    }

    #[test]
    fn check_available_when_running() {
        let r = MockRunner::new().on("tailscale status --json", 0, RUNNING);
        assert_eq!(t().check(&r), Availability::Available);
    }

    #[test]
    fn is_active_true_when_running() {
        let r = MockRunner::new().on("tailscale status --json", 0, RUNNING);
        assert!(t().is_active(&r));
    }

    #[test]
    fn stopped_is_available_but_inactive() {
        let r = MockRunner::new().on("tailscale status --json", 0, STOPPED);
        let s = t().status(&r);
        assert_eq!(s.availability, Availability::Available);
        assert!(!s.active);
    }
}
