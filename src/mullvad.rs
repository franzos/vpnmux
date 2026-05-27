use crate::nft;
use crate::provider::{Availability, Provider, ProviderStatus};
use crate::runner::Runner;
use crate::types::{ProviderId, Result};

pub struct Mullvad {
    pub bin: String,
    pub nft: String,
}

impl Mullvad {
    /// Shared lockdown-mode parser used by both the daemon and the CLI precheck.
    /// Real output is a sentence: "Block traffic when the VPN is disconnected:
    /// off" — parse the value after the last colon; a naive contains("on")
    /// matches "disconnected" and is always true.
    pub fn lockdown_on(&self, r: &dyn Runner) -> bool {
        r.run(&self.bin, &["lockdown-mode", "get"])
            .map(|o| {
                o.stdout
                    .rsplit(':')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .eq_ignore_ascii_case("on")
            })
            .unwrap_or(false)
    }
}

impl Provider for Mullvad {
    fn id(&self) -> ProviderId {
        ProviderId::Mullvad
    }

    fn status(&self, r: &dyn Runner) -> ProviderStatus {
        let Ok(out) = r.run(&self.bin, &["status"]) else {
            return ProviderStatus {
                availability: Availability::Unavailable("mullvad daemon not reachable".into()),
                active: false,
            };
        };
        if !out.ok() {
            return ProviderStatus {
                availability: Availability::Unavailable("mullvad daemon not reachable".into()),
                active: false,
            };
        }
        let state = out.stdout.lines().next().map(str::trim).unwrap_or("");
        // "Connecting"/"Disconnecting" are transitional and not yet active.
        let active = state == "Connected";
        ProviderStatus {
            availability: Availability::Available,
            active,
        }
    }

    fn activate(&self, r: &dyn Runner) -> Result<()> {
        r.run(&self.bin, &["connect"])?;
        Ok(())
    }

    fn deactivate(&self, r: &dyn Runner) -> Result<()> {
        r.run(&self.bin, &["disconnect"])?;
        // Lockdown is user-owned: only clear the table when lockdown is OFF.
        // When ON, leaving the Blocked table is the intended killswitch.
        if !self.lockdown_on(r) {
            nft::delete_table(&self.nft, r)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::MockRunner;

    fn m() -> Mullvad {
        Mullvad {
            bin: "mullvad".into(),
            nft: "nft".into(),
        }
    }

    #[test]
    fn is_active_true_when_connected() {
        let r = MockRunner::new().on("mullvad status", 0, "Connected\n  Relay: x\n");
        assert!(m().is_active(&r));
    }

    #[test]
    fn is_active_false_when_disconnected() {
        let r = MockRunner::new().on("mullvad status", 0, "Disconnected\n");
        assert!(!m().is_active(&r));
    }

    #[test]
    fn connecting_is_not_active_but_available() {
        let r = MockRunner::new().on("mullvad status", 0, "Connecting\n  Relay: x\n");
        let s = m().status(&r);
        assert!(!s.active);
        assert_eq!(s.availability, Availability::Available);
    }

    #[test]
    fn disconnecting_is_not_active() {
        let r = MockRunner::new().on("mullvad status", 0, "Disconnecting\n");
        assert!(!m().is_active(&r));
    }

    #[test]
    fn blocked_is_not_active() {
        let r = MockRunner::new().on("mullvad status", 0, "Blocked\n");
        assert!(!m().is_active(&r));
    }

    #[test]
    fn nonzero_exit_is_unavailable_and_inactive() {
        let r = MockRunner::new().on("mullvad status", 1, "Connected\n");
        let s = m().status(&r);
        assert!(!s.active);
        assert!(matches!(s.availability, Availability::Unavailable(_)));
    }

    #[test]
    fn check_unavailable_on_rpc_error() {
        let r = MockRunner::new().on("mullvad status", 1, "Error: transport error");
        assert!(matches!(m().check(&r), Availability::Unavailable(_)));
    }

    #[test]
    fn deactivate_deletes_table_when_lockdown_off() {
        let r = MockRunner::new().on("mullvad lockdown-mode get", 0, "off\n");
        m().deactivate(&r).unwrap();
        assert!(r.called("mullvad disconnect"));
        assert!(r.called("nft delete table inet mullvad"));
    }

    #[test]
    fn deactivate_keeps_table_when_lockdown_on() {
        let r = MockRunner::new().on("mullvad lockdown-mode get", 0, "on\n");
        m().deactivate(&r).unwrap();
        assert!(r.called("mullvad disconnect"));
        assert!(!r.called("nft delete table inet mullvad"));
    }

    // Real CLI output is a sentence containing "disconnected" — guards against
    // a naive contains("on") parse that would wrongly keep the table forever.
    #[test]
    fn lockdown_off_parsed_from_real_cli_sentence() {
        let r = MockRunner::new().on(
            "mullvad lockdown-mode get",
            0,
            "Block traffic when the VPN is disconnected: off\n",
        );
        m().deactivate(&r).unwrap();
        assert!(r.called("nft delete table inet mullvad"));
    }

    #[test]
    fn lockdown_on_parsed_from_real_cli_sentence() {
        let r = MockRunner::new().on(
            "mullvad lockdown-mode get",
            0,
            "Block traffic when the VPN is disconnected: on\n",
        );
        m().deactivate(&r).unwrap();
        assert!(!r.called("nft delete table inet mullvad"));
    }
}
