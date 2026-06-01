use crate::dns::DnsBackend;
use crate::nft;
use crate::runner::Runner;
use crate::types::{ProviderId, ProviderSet, Result};

pub struct Bins {
    pub nft: String,
    pub tailscale: String,
    /// resolv.conf to repair in tailscale-only mode; empty disables (tests).
    pub resolv_conf: String,
}

/// Apply coexistence invariants for the ACTUALLY-active set. `newly_active`
/// names the providers that flipped on this tick, so one-shot actions
/// (accept-dns, stale-resolver removal) run on the transition rather than every
/// tick. The nft whitelist is re-asserted every tick because Mullvad rebuilds
/// its table on each state change.
pub fn apply(
    active: &ProviderSet,
    newly_active: &ProviderSet,
    bins: &Bins,
    backend: DnsBackend,
    r: &dyn Runner,
) -> Result<()> {
    let mullvad = active.contains(&ProviderId::Mullvad);
    let tailscale = active.contains(&ProviderId::Tailscale);

    // Never let Tailscale own /etc/resolv.conf — MagicDNS can't resolve public
    // names on every tailnet and fights Mullvad's resolver. Setting it once on
    // the activation transition is enough (Mullvad never re-enables it).
    if tailscale && newly_active.contains(&ProviderId::Tailscale) {
        set_accept_dns(&bins.tailscale, false, r)?;
    }
    if mullvad && tailscale {
        nft::ensure_whitelist(&bins.nft, r)?;
    }
    if !bins.resolv_conf.is_empty() {
        if mullvad {
            // Mullvad owns DNS again; drop any resolver we backfilled earlier.
            if newly_active.contains(&ProviderId::Mullvad) {
                crate::dns::remove_injected_resolver(&bins.resolv_conf, backend, r)?;
            }
        } else {
            // Mullvad gone (tailscale-only or none): backfill a resolver so
            // name resolution keeps working.
            crate::dns::ensure_resolver(
                &bins.resolv_conf,
                backend,
                crate::dns::preferred_resolver().as_deref(),
                r,
            )?;
        }
    }
    Ok(())
}

fn set_accept_dns(tailscale: &str, accept: bool, r: &dyn Runner) -> Result<()> {
    let flag = if accept {
        "--accept-dns=true"
    } else {
        "--accept-dns=false"
    };
    r.run(tailscale, &["set", flag])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::MockRunner;
    use crate::types::parse_set;

    fn bins() -> Bins {
        Bins {
            nft: "nft".into(),
            tailscale: "tailscale".into(),
            resolv_conf: String::new(),
        }
    }

    #[test]
    fn both_active_whitelists_and_disables_magicdns() {
        let r = MockRunner::new().on("nft list table inet mullvad", 0, "chain input {\n}");
        let active = parse_set("mullvad,tailscale").unwrap();
        apply(&active, &active, &bins(), DnsBackend::Static, &r).unwrap();
        assert!(r.called("nft insert rule inet mullvad input iifname tailscale* accept"));
        assert!(r.called("tailscale set --accept-dns=false"));
    }

    #[test]
    fn tailscale_only_disables_magicdns_on_transition() {
        let r = MockRunner::new();
        let active = parse_set("tailscale").unwrap();
        apply(&active, &active, &bins(), DnsBackend::Static, &r).unwrap();
        assert!(r.called("tailscale set --accept-dns=false"));
        assert!(!r.called("tailscale set --accept-dns=true"));
        assert!(!r.called("nft list table inet mullvad"));
    }

    #[test]
    fn steady_state_does_not_respawn_accept_dns() {
        // Already-active tailscale (not in newly_active): accept-dns is a no-op.
        let r = MockRunner::new();
        let active = parse_set("tailscale").unwrap();
        apply(
            &active,
            &ProviderSet::new(),
            &bins(),
            DnsBackend::Static,
            &r,
        )
        .unwrap();
        assert!(!r.called("tailscale set --accept-dns=false"));
    }

    #[test]
    fn mullvad_only_does_nothing() {
        let r = MockRunner::new();
        let active = parse_set("mullvad").unwrap();
        apply(&active, &active, &bins(), DnsBackend::Static, &r).unwrap();
        assert!(r.calls.borrow().is_empty());
    }
}
