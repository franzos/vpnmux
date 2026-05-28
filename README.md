# vpnmux

<p align="center">
  <img src="assets/logo.svg" alt="vpnmux" width="480">
</p>
<p align="center">
  Keeps Mullvad and Tailscale from fighting at the netfilter/DNS layer.
</p>

Arbitrates a set of network providers — currently Mullvad and Tailscale — so a
chosen combination coexists cleanly. The hard part isn't running each one; it's
keeping them from fighting at the netfilter/DNS layer (Mullvad's killswitch
drops Tailscale, and both daemons claw at `/etc/resolv.conf`). vpnmux keeps that
truce continuously.

It's an operator/control-loop: a root **daemon** reconciles the system to a
desired set of providers every couple of seconds; the **CLI** just writes the
desired state and reads back status. Single writer, idempotent, std-only Rust
(no external crates).

**Status:** works, validated as a shell prototype and reimplemented in Rust
(43 unit tests). Linux-only — it drives `nft`/`mullvad`/`tailscale` directly.

## Install

| Method | Command |
|--------|---------|
| Debian/Ubuntu | Download [`.deb`](https://github.com/franzos/vpnmux/releases) — `sudo dpkg -i vpnmux_*_amd64.deb` |
| Fedora/RHEL | Download [`.rpm`](https://github.com/franzos/vpnmux/releases) — `sudo rpm -i vpnmux-*.x86_64.rpm` |
| Binary | Grab a tarball from [Releases](https://github.com/franzos/vpnmux/releases) (x86_64, aarch64) |
| From source | `cargo build --release` → `target/release/vpnmux` |

The `.deb`/`.rpm` ship a systemd unit (`vpnmux.service`, disabled by default).
Enable it once installed:

```bash
sudo systemctl enable --now vpnmux
```

## Run (manual)

Run the daemon as root — it drives `nft`/`mullvad`/`tailscale` and reconciles
every ~2s. Keep it in a terminal; add `VPNMUX_LOG=debug` for the full
command-by-command trace (default is a quiet, diff-based change-log):

```bash
sudo target/release/vpnmux daemon
```

Switch state in another shell. If your user is in the `vpnmux` group (see
**Sudo-less CLI** below) the `sudo` is optional — the CLI only needs write
access to `/var/lib/vpnmux/desired`, which the daemon picks up:

```bash
vpnmux set mullvad tailscale   # both, Tailscale via the tunnel
vpnmux set mullvad             # Mullvad only
vpnmux set tailscale           # Tailscale only
vpnmux set                     # none
vpnmux status
```

### Sudo-less CLI

The daemon mirrors `mullvad-daemon`'s pattern: at startup it chowns
`/var/lib/vpnmux` and `/run/vpnmux` to `root:vpnmux` (mode `02770`, setgid)
when a `vpnmux` system group exists, so members of that group can drive
`vpnmux set`/`status` without `sudo`. To enable:

```bash
sudo groupadd --system vpnmux
sudo usermod -aG vpnmux "$USER"
sudo systemctl restart vpnmux
# log out & back in (or `newgrp vpnmux`) for the group to take effect
```

Override the group name with `VPNMUX_GROUP=othergroup` in the unit's
`Environment=`, or set it empty to opt out and keep the dirs root-only.

> Anyone in the `vpnmux` group can flip providers, including disabling Mullvad
> while lockdown is on (the `[y/N]` prompt still applies). Same trust model as
> the `mullvad` group on systems that use one.

Switching to `none`/`tailscale` while Mullvad lockdown is on warns and prompts
first — it would cut all connectivity (that's the killswitch doing its job).

## States

| State | Mullvad | Tailscale | DNS |
|-------|---------|-----------|-----|
| `none` | off | off | system |
| `mullvad` | connected | off | Mullvad (`10.64.0.1`) |
| `tailscale` | off | up | MagicDNS |
| both | connected | up, via the tunnel | Mullvad (MagicDNS off) |

The daemon never imposes a default: with no desired state set it stays idle and
touches nothing.

## Environment

| Var | Purpose |
|-----|---------|
| `VPNMUX_LOG` | `error` / `info` (default) / `debug` |
| `VPNMUX_NFT` | absolute path to `nft` (else scans `/gnu/store`) |
| `VPNMUX_MULLVAD` / `VPNMUX_TAILSCALE` | adapter binary paths |
