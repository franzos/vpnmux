#!/bin/sh
# waybar custom module: vpnmux status icon. Emits one line of waybar JSON.
# Reads `vpnmux status --json` (which only reads a file — no daemon spawning).
set -eu

json="$(vpnmux status --json 2>/dev/null || true)"

# No CLI output, or not valid JSON → neutral "unknown" state.
if [ -z "$json" ] || ! printf '%s' "$json" | jq -e . >/dev/null 2>&1; then
  printf '{"text":"vpn?","tooltip":"vpnmux: status unavailable","class":"unknown"}\n'
  exit 0
fi

printf '%s' "$json" | jq -c '
  (.active // [])      as $a
  | (.unavailable // []) as $u
  | (if   ($a | index("mullvad")) and ($a | index("tailscale")) then "both"
     elif ($a | index("mullvad"))   then "mullvad"
     elif ($a | index("tailscale")) then "tailscale"
     else "none" end) as $state
  | {
      text: $state,
      class: (if ($u | length) > 0 then "degraded" else $state end),
      tooltip: (
        "vpnmux: " + (if ($a | length) > 0 then ($a | join("+")) else "none" end)
        + (if ($u | length) > 0
           then "\n" + ($u | map("unavailable: " + .provider + " (" + .reason + ")") | join("\n"))
           else "" end)
      )
    }
'
