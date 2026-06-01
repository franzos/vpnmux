use crate::coexistence::{self, Bins};
use crate::dns::DnsBackend;
use crate::provider::{Availability, Provider};
use crate::runner::Runner;
use crate::types::{ProviderId, ProviderSet};

#[derive(Debug, PartialEq)]
pub struct Outcome {
    pub active: ProviderSet,
    /// Every provider engageable right now (probe == Available), regardless of
    /// whether it was desired. Drives the waybar available-only menu.
    pub available: ProviderSet,
    pub unavailable: Vec<(ProviderId, String)>,
    /// Providers that flipped inactive→active this tick (coexistence one-shots).
    pub newly_active: ProviderSet,
}

/// One idempotent convergence pass. Best-effort: provider errors don't abort
/// the others; the per-provider `status` probe is captured once per tick and
/// both availability and active-ness derive from it.
pub fn reconcile(
    desired: &ProviderSet,
    providers: &[Box<dyn Provider>],
    prev_active: &ProviderSet,
    bins: &Bins,
    backend: DnsBackend,
    r: &dyn Runner,
) -> Outcome {
    let mut active = ProviderSet::new();
    let mut available = ProviderSet::new();
    let mut unavailable = Vec::new();

    for p in providers {
        // Single status probe per provider per tick; both availability and
        // active-ness derive from it. We only re-probe after an action.
        let st = p.status(r);
        if st.availability == Availability::Available {
            available.insert(p.id());
        }
        let want = desired.contains(&p.id());
        let mut acted = false;
        if want {
            match st.availability {
                Availability::Available => {
                    // Activate only on the inactive→active transition.
                    if !st.active {
                        acted = true;
                        if let Err(e) = p.activate(r) {
                            crate::error!("provider {} activate failed: {e}", p.id());
                        }
                    }
                }
                Availability::Unavailable(reason) => {
                    unavailable.push((p.id(), reason));
                    continue;
                }
            }
        } else if st.active {
            // Tear down only on the active→inactive transition, not every tick.
            if let Err(e) = p.deactivate(r) {
                crate::error!("provider {} deactivate failed: {e}", p.id());
            }
            continue; // not desired → never in the active set
        } else {
            continue; // not desired, already inactive
        }
        // Use the post-action truth if we acted, else the probe we already have.
        let is_active = if acted { p.is_active(r) } else { st.active };
        if is_active {
            active.insert(p.id());
        }
    }

    let newly_active: ProviderSet = active.difference(prev_active).copied().collect();
    if let Err(e) = coexistence::apply(&active, &newly_active, bins, backend, r) {
        crate::error!("coexistence apply failed: {e}");
    }
    Outcome {
        active,
        available,
        unavailable,
        newly_active,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::MockRunner;
    use crate::types::{parse_set, providers};

    fn bins() -> Bins {
        Bins {
            nft: "nft".into(),
            tailscale: "tailscale".into(),
            resolv_conf: String::new(),
        }
    }

    fn registry() -> Vec<Box<dyn Provider>> {
        providers("mullvad", "tailscale", "nft")
    }

    const TS_RUNNING: &str = r#"{"BackendState":"Running"}"#;
    const TS_STOPPED: &str = r#"{"BackendState":"Stopped"}"#;
    const TS_NEEDS_LOGIN: &str = r#"{"BackendState":"NeedsLogin"}"#;

    #[test]
    fn both_desired_and_available_become_active_and_whitelisted() {
        let r = MockRunner::new()
            .on("mullvad status", 0, "Connected\n")
            .on("tailscale status --json", 0, TS_RUNNING)
            .on("nft list table inet mullvad", 0, "chain input {\n}");
        let out = reconcile(
            &parse_set("mullvad,tailscale").unwrap(),
            &registry(),
            &ProviderSet::new(),
            &bins(),
            DnsBackend::Static,
            &r,
        );
        assert_eq!(out.active, parse_set("mullvad,tailscale").unwrap());
        // Both already active → no redundant connect/up.
        assert!(!r.called("mullvad connect"));
        assert!(!r.called("tailscale up"));
        assert!(r.called("nft insert rule inet mullvad input iifname tailscale* accept"));
    }

    #[test]
    fn activates_only_when_inactive() {
        let r = MockRunner::new()
            .on("mullvad status", 0, "Disconnected\n")
            .on("tailscale status --json", 0, TS_STOPPED);
        let out = reconcile(
            &parse_set("mullvad,tailscale").unwrap(),
            &registry(),
            &ProviderSet::new(),
            &bins(),
            DnsBackend::Static,
            &r,
        );
        // Available but inactive → activate fires once.
        assert!(r.called("mullvad connect"));
        assert!(r.called("tailscale up"));
        // Still inactive in this mock world → not reported active.
        assert!(out.active.is_empty());
    }

    #[test]
    fn unavailable_tailscale_is_reported_and_does_not_block_mullvad() {
        let r = MockRunner::new().on("mullvad status", 0, "Connected\n").on(
            "tailscale status --json",
            0,
            TS_NEEDS_LOGIN,
        );
        let out = reconcile(
            &parse_set("mullvad,tailscale").unwrap(),
            &registry(),
            &ProviderSet::new(),
            &bins(),
            DnsBackend::Static,
            &r,
        );
        assert_eq!(out.active, parse_set("mullvad").unwrap());
        assert_eq!(out.unavailable.len(), 1);
        assert!(!r.called("tailscale up"));
        assert!(!r.called("nft insert rule inet mullvad input iifname tailscale* accept"));
    }

    #[test]
    fn empty_desired_tears_down_active_providers() {
        let r = MockRunner::new()
            .on("mullvad status", 0, "Connected\n")
            .on("mullvad lockdown-mode get", 0, "off\n")
            .on("tailscale status --json", 0, TS_RUNNING);
        let _ = reconcile(
            &ProviderSet::new(),
            &registry(),
            &parse_set("mullvad,tailscale").unwrap(),
            &bins(),
            DnsBackend::Static,
            &r,
        );
        assert!(r.called("mullvad disconnect"));
        assert!(r.called("tailscale down"));
    }

    #[test]
    fn empty_desired_skips_teardown_when_already_inactive() {
        // The fix for the every-tick teardown spam: nothing to tear down means
        // no disconnect / down / table delete, so the daemon stays quiet.
        let r = MockRunner::new()
            .on("mullvad status", 0, "Disconnected\n")
            .on("tailscale status --json", 0, TS_STOPPED);
        let out = reconcile(
            &ProviderSet::new(),
            &registry(),
            &ProviderSet::new(),
            &bins(),
            DnsBackend::Static,
            &r,
        );
        assert!(out.active.is_empty());
        assert!(!r.called("mullvad disconnect"));
        assert!(!r.called("tailscale down"));
        assert!(!r.called("nft delete table inet mullvad"));
    }

    #[test]
    fn available_includes_non_desired_providers() {
        // Tailscale is Stopped (available, just not up) and NOT desired; it must
        // still appear in `available` so the waybar menu can offer it.
        let r = MockRunner::new().on("mullvad status", 0, "Connected\n").on(
            "tailscale status --json",
            0,
            TS_STOPPED,
        );
        let out = reconcile(
            &parse_set("mullvad").unwrap(),
            &registry(),
            &ProviderSet::new(),
            &bins(),
            DnsBackend::Static,
            &r,
        );
        assert_eq!(out.available, parse_set("mullvad,tailscale").unwrap());
        assert_eq!(out.active, parse_set("mullvad").unwrap());
    }

    #[test]
    fn unavailable_provider_is_absent_from_available() {
        let r = MockRunner::new().on("mullvad status", 0, "Connected\n").on(
            "tailscale status --json",
            0,
            TS_NEEDS_LOGIN,
        );
        let out = reconcile(
            &parse_set("mullvad,tailscale").unwrap(),
            &registry(),
            &ProviderSet::new(),
            &bins(),
            DnsBackend::Static,
            &r,
        );
        assert!(out.available.contains(&ProviderId::Mullvad));
        assert!(!out.available.contains(&ProviderId::Tailscale));
        assert_eq!(out.active, parse_set("mullvad").unwrap());
    }
}
