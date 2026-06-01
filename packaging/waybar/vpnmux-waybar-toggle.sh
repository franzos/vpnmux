#!/usr/bin/env bash
# waybar on-click: pick a vpnmux configuration from the currently-available
# providers and apply it. Launcher-agnostic via $VPNMUX_MENU (dmenu protocol).
# Always passes --yes: a GUI click has no stdin to answer the lockdown prompt,
# so switching off Mullvad under lockdown WILL drop connectivity. (Documented.)
set -euo pipefail

MENU="${VPNMUX_MENU:-fuzzel --dmenu}"

json="$(vpnmux status --json 2>/dev/null || true)"
if [ -n "$json" ]; then
  mapfile -t available < <(printf '%s' "$json" | jq -r '.available[]? // empty')
  active="$(printf '%s' "$json" | jq -r '(.active // []) | sort | join(",")')"
else
  available=()
  active=""
fi

# Power-set of available providers as comma-joined labels; always offer "none".
# `available[]` already arrives in vpnmux's canonical (sorted) order, so each
# combo is built in that order — no re-sort needed.
labels=("none")
n=${#available[@]}
for ((mask = 1; mask < (1 << n); mask++)); do
  combo=()
  for ((i = 0; i < n; i++)); do
    if ((mask & (1 << i))); then combo+=("${available[i]}"); fi
  done
  labels+=("$(IFS=,; echo "${combo[*]}")")
done

# Render the menu; a fixed 2-char ASCII marker keeps the strip byte-exact.
menu=""
for l in "${labels[@]}"; do
  key="$l"; [ "$l" = "none" ] && key=""
  if [ "$key" = "$active" ]; then menu+="* $l"$'\n'; else menu+="  $l"$'\n'; fi
done

choice="$(printf '%s' "$menu" | $MENU || true)"
[ -z "$choice" ] && exit 0
choice="${choice#??}"   # strip the 2-char marker ("* " or "  ")

if [ "$choice" = "none" ]; then
  vpnmux set --yes
else
  # shellcheck disable=SC2086
  vpnmux set ${choice//,/ } --yes
fi

pkill -RTMIN+8 waybar 2>/dev/null || true
