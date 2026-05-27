use crate::coexistence::Bins;
use crate::paths;
use crate::provider::Provider;
use crate::reconcile::reconcile;
use crate::runner::{RealRunner, Runner};
use crate::state::{read_desired, write_status, Status};
use crate::types::{format_set, providers, ProviderId, ProviderSet, Result};
use std::time::Duration;

const TICK: Duration = Duration::from_secs(2);

struct Prev {
    generation: u64,
    active: ProviderSet,
    unavailable: Vec<ProviderId>,
}

pub fn run() -> Result<()> {
    let nft = paths::nft();
    let mullvad_bin = paths::mullvad();
    let tailscale_bin = paths::tailscale();
    let bins = Bins {
        nft: nft.clone(),
        tailscale: tailscale_bin.clone(),
        resolv_conf: paths::RESOLV_CONF.to_string(),
    };

    let registry = providers(&mullvad_bin, &tailscale_bin, &nft);
    let runner = RealRunner;

    crate::sys::install_shutdown_handler();
    crate::info!("daemon started, tick {}s", TICK.as_secs());
    let mut prev: Option<Prev> = None;
    loop {
        prev = tick(prev, &registry, &bins, &runner);
        if crate::sys::shutdown_requested() {
            crate::info!("shutdown signal received; exiting cleanly");
            return Ok(());
        }
        std::thread::sleep(TICK);
    }
}

/// One iteration. Carries previous-tick state to log transitions.
/// Unset desired → idle (manage nothing this tick).
fn tick(
    prev: Option<Prev>,
    providers: &[Box<dyn Provider>],
    bins: &Bins,
    r: &dyn Runner,
) -> Option<Prev> {
    let d = match read_desired(paths::DESIRED_FILE) {
        Ok(Some(d)) => d,
        Ok(None) => {
            if prev.is_some() {
                crate::info!("desired file absent — going idle (managing nothing)");
            }
            return None;
        }
        Err(e) => {
            crate::error!("read desired failed: {e}");
            return prev;
        }
    };

    let prev_active = prev.as_ref().map(|p| p.active.clone()).unwrap_or_default();
    let outcome = reconcile(&d.providers, providers, &prev_active, bins, r);

    let desired_changed = prev.as_ref().map(|p| p.generation) != Some(d.generation);
    if desired_changed {
        crate::info!(
            "desired set gen {}: providers [{}]",
            d.generation,
            format_set(&d.providers)
        );
    }

    if prev.as_ref().map(|p| &p.active) != Some(&outcome.active) {
        let prev_active_str = prev
            .as_ref()
            .map_or_else(String::new, |p| format_set(&p.active));
        crate::info!(
            "active: [{}] -> [{}]{}",
            prev_active_str,
            format_set(&outcome.active),
            if desired_changed { "" } else { " (external)" }
        );
    }

    let cur_unavail: Vec<ProviderId> = outcome.unavailable.iter().map(|(id, _)| *id).collect();
    let prev_unavail: &[ProviderId] = prev.as_ref().map_or(&[], |p| p.unavailable.as_slice());
    for (id, reason) in &outcome.unavailable {
        if !prev_unavail.contains(id) {
            crate::info!("provider {} unavailable: {}", id, reason);
        }
    }
    for id in prev_unavail {
        if !cur_unavail.contains(id) {
            crate::info!("provider {} available again", id);
        }
    }

    if let Err(e) = write_status(
        paths::STATUS_FILE,
        &Status {
            generation: d.generation,
            active: outcome.active.clone(),
            unavailable: outcome.unavailable.clone(),
        },
    ) {
        crate::error!("write status failed: {e}");
    }

    Some(Prev {
        generation: d.generation,
        active: outcome.active,
        unavailable: cur_unavail,
    })
}
