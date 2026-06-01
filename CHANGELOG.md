# Changelog

## [0.1.3] - 2026-06-01

### Added
- DNS backend detection, shown in `vpnmux status` and `--json`
- resolvconf/openresolv backfill on resolvconf-managed systems

### Changed
- Leaves systemd-resolved/NetworkManager DNS alone instead of erroring

## [0.1.2] - 2026-06-01

### Added
- `vpnmux status --json` — machine-readable status output
- waybar integration: status icon + available-only toggle

## [0.1.1] - 2026-05-28

### Added
- Sudo-less CLI for members of the `vpnmux` system group
- `VPNMUX_GROUP` env var to pick the group name (empty to opt out)

### Changed
- Daemon chowns `/var/lib/vpnmux` and `/run/vpnmux` to `root:vpnmux` at startup
- Friendlier permission-denied hint when running `vpnmux set` unprivileged

## [0.1.0] - 2026-05-27

### Added
- Initial release
