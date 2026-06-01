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

**Status:** Linux-only — it drives `nft`/`mullvad`/`tailscale` directly.

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

> **Heads up:** I run this primarily on Guix. vpnmux builds and runs natively on
> Debian 12, where the DNS-backend handling is tested (see
> [DNS backends](#dns-backends)); it only leans on systemd and the
> `nft`/`mullvad`/`tailscale` binaries, so it *should* run fine on any systemd
> distro. The packaged `.deb`/`.rpm` install path and Fedora/RHEL haven't been
> heavily exercised yet, though

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

## DNS backends

vpnmux only touches DNS to fill a gap: when Mullvad disconnects it takes its
`10.64.0.1` resolver with it, and on a box with no DNS manager nothing else fills
in. So it detects how your system manages `/etc/resolv.conf` and acts *only* where
there's a real gap — on managed systems the resolver manager already keeps a
working upstream when Mullvad/Tailscale drop their own links, so vpnmux stays out
of the way. Either way, `vpnmux status` reports the backend it detected.

| Backend | Default on | What vpnmux does |
|---------|-----------|------------------|
| **systemd-resolved** (stub `127.0.0.53`) | Ubuntu, Mint, Pop!_OS, Fedora, NixOS (`services.resolved`) | detect only — resolved keeps upstream DNS; no backfill |
| **NetworkManager** (writes `resolv.conf` directly) | Debian desktop, RHEL/Rocky/Alma, Arch, Manjaro, Guix System (desktop) | detect only — NM keeps upstream DNS; no backfill |
| **static `/etc/resolv.conf`** | Debian server/minimal, Guix System (server/DHCP), hand-rolled setups | backfills the default-route resolver when Mullvad leaves, strips it when Mullvad returns |
| **resolvconf / openresolv** | NixOS (default), legacy / opt-in | backfills via `resolvconf -a vpnmux` (`-d` on the way out) |
| **netconfig** | openSUSE | detect only — netconfig keeps upstream DNS; no backfill |
| **other / unknown** | ConnMan, anything else | left alone — never overwrites a managed `resolv.conf` |

Set `VPNMUX_DNS=<ip>` to override the backfilled resolver (default: the
default-route gateway). It only applies on the backends vpnmux backfills.

Guix System has no systemd-resolved (it doesn't use systemd), so it lands on
NetworkManager (default desktop), a static `/etc/resolv.conf` (server/DHCP), or
ConnMan (handled as *other/unknown*). NixOS defaults to openresolv and only uses
systemd-resolved if you enable `services.resolved`.

## waybar

A status icon plus a click-to-switch menu that only offers the configurations
that are actually engageable right now.

`vpnmux status --json` exposes the daemon's view as machine-readable JSON
(reading only `/run/vpnmux/status` — it spawns nothing):

```json
{"generation":12,"active":["mullvad"],"available":["mullvad","tailscale"],
 "unavailable":[{"provider":"tailscale","reason":"not logged in"}]}
```

- `active` — providers currently up.
- `available` — providers engageable right now (this drives the menu).
- `unavailable` — providers you *asked for* that couldn't be engaged, with a reason.

Two scripts under [`packaging/waybar/`](packaging/waybar) wire it up:

- `vpnmux-waybar-status.sh` — maps the JSON to waybar's format (needs `jq`).
- `vpnmux-waybar-toggle.sh` — builds the available-only menu and applies the
  choice. Launcher-agnostic: set `VPNMUX_MENU` to any dmenu-compatible command
  (defaults to `fuzzel --dmenu`), e.g. `VPNMUX_MENU="wofi --dmenu"` or
  `VPNMUX_MENU="rofi -dmenu"` (the value is word-split, so the launcher binary's
  path can't contain spaces).

Put both scripts on your `PATH`, then add the module from
[`packaging/waybar/config.jsonc`](packaging/waybar/config.jsonc) and style it
with [`packaging/waybar/style.css`](packaging/waybar/style.css). The toggle sends
`SIGRTMIN+8` to waybar (`"signal": 8`) so the icon refreshes immediately.

> The toggle runs `vpnmux set … --yes`, which **bypasses the lockdown prompt**.
> Switching off Mullvad from the menu while lockdown is on will cut all
> connectivity (the killswitch doing its job) — there's no confirmation in the
> GUI path, unlike the CLI. You'll need to be in the `vpnmux` group (see
> **Sudo-less CLI**) for the menu to read status and flip providers.

## Environment

| Var | Purpose |
|-----|---------|
| `VPNMUX_LOG` | `error` / `info` (default) / `debug` |
| `VPNMUX_NFT` | absolute path to `nft` (else scans `/gnu/store`) |
| `VPNMUX_MULLVAD` / `VPNMUX_TAILSCALE` | adapter binary paths |
| `VPNMUX_DNS` | resolver to backfill on static/resolvconf backends (default: default-route gateway) |
| `VPNMUX_GROUP` | system group for sudo-less CLI (default: `vpnmux`; empty to opt out) |
