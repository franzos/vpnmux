use crate::coexistence::Bins;
use crate::paths;
use crate::provider::Provider;
use crate::reconcile::reconcile;
use crate::runner::{RealRunner, Runner};
use crate::state::{read_desired, write_status, Status};
use crate::types::{format_set, providers, ProviderId, ProviderSet, Result};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

const TICK: Duration = Duration::from_secs(2);

/// Setgid + group rwx; new files inherit the dir's group so a CLI invocation
/// by any `vpnmux`-group member produces files the daemon (root) and other
/// group members can still read/overwrite. Falls back to 0o700 when no group
/// is configured or the group doesn't exist yet.
const DIR_MODE_GROUP: u32 = 0o2770;
const DIR_MODE_ROOT_ONLY: u32 = 0o0700;

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
    prepare_state_dirs();
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

/// Ensure `/var/lib/vpnmux` and `/run/vpnmux` exist with permissions matching
/// the configured group policy. Mirrors what `mullvad-daemon` does to its
/// management socket at startup (see `mullvad-management-interface/src/lib.rs`,
/// `MULLVAD_MANAGEMENT_SOCKET_GROUP`): the daemon owns the policy so packagers
/// only have to create the system group.
fn prepare_state_dirs() {
    let group = paths::group_name();
    let gid = group.as_deref().and_then(crate::sys::lookup_gid);
    let mode = if gid.is_some() {
        DIR_MODE_GROUP
    } else {
        DIR_MODE_ROOT_ONLY
    };

    for dir in [paths::STATE_DIR, paths::RUNTIME_DIR] {
        if let Err(e) = std::fs::create_dir_all(dir) {
            crate::error!("create {dir}: {e}");
            continue;
        }
        if let Some(g) = gid {
            if let Err(e) = crate::sys::chown_group(dir, g) {
                crate::error!("chown {dir} to gid {g}: {e}");
            }
        }
        // Only chmod when the mode actually differs: under
        // `RestrictSUIDSGID=yes` (set in our systemd unit), `chmod 02770`
        // would fail with EPERM, so we rely on systemd's StateDirectoryMode
        // having already set the setgid bit at unit setup time, pre-fork.
        let cur = std::fs::metadata(dir)
            .ok()
            .map(|m| m.permissions().mode() & 0o7777);
        if cur != Some(mode) {
            if let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)) {
                crate::error!("chmod {dir} to {mode:o}: {e}");
            }
        }
    }

    match (group.as_deref(), gid) {
        (Some(g), Some(_)) => {
            crate::info!("state dirs group-writable by '{g}' (mode 0{DIR_MODE_GROUP:o})")
        }
        (Some(g), None) => crate::info!(
            "group '{g}' not present; CLI will require sudo (create the group to enable)"
        ),
        (None, _) => crate::info!("VPNMUX_GROUP unset — state dirs root-only; CLI requires sudo"),
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

    let backend = crate::dns::detect_backend(paths::RESOLV_CONF);
    let prev_active = prev.as_ref().map(|p| p.active.clone()).unwrap_or_default();
    let outcome = reconcile(&d.providers, providers, &prev_active, bins, backend, r);

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
            available: outcome.available.clone(),
            unavailable: outcome.unavailable.clone(),
            dns: backend,
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
