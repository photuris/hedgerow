#!/usr/bin/env bash
# Captures docs/screenshot.png: Hedgerow in UI-test mode (demo radio, in-memory store) on a
# private Xvfb display and session bus, so no real radio, data, or desktop session is touched.
# Renders at scale 2 for a sharp 2400x1600 image of the 1200x800 window, in the current
# Omarchy theme. Needs Xvfb (xorg-server-xvfb) and magick; oxipng is used when present.
# Usage: scripts/screenshot.sh [start-view] [out.png]   (default: thread:channel:0)
set -euo pipefail

cd "$(dirname "$0")/.."

view="${1:-thread:channel:0}"
out="${2:-docs/screenshot.png}"
tmp=$(mktemp -d)
trap 'kill "${xvfb:-}" 2>/dev/null || true; rm -rf "$tmp"' EXIT

if [[ -z "${HEDGEROW_BIN:-}" ]]; then
    cargo build -p hedgerow-gui
    HEDGEROW_BIN=target/debug/hedgerow
fi

Xvfb -displayfd 3 -screen 0 2400x1600x24 -nolisten tcp 3>"$tmp/display" 2>/dev/null &
xvfb=$!

until [[ -s "$tmp/display" ]]; do sleep 0.1; done

display=":$(cat "$tmp/display")"

env -u WAYLAND_DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE \
    DISPLAY="$display" GDK_BACKEND=x11 GDK_SCALE=2 XDG_CURRENT_DESKTOP=Hyprland \
    WEBKIT_DISABLE_DMABUF_RENDERER=1 \
    HEDGEROW_UI_TEST=1 HEDGEROW_EXIT_AFTER_MS=9000 HEDGEROW_START_VIEW="$view" \
    dbus-run-session -- "$HEDGEROW_BIN" >"$tmp/log" 2>&1 &
app=$!

# The demo radio's messages land a moment after connect.
sleep 6
DISPLAY="$display" magick import -window root "$out"

if ! wait "$app"; then
    cat "$tmp/log" >&2
    exit 1
fi

if command -v oxipng >/dev/null; then
    oxipng -o 4 --strip safe -q "$out"
fi

echo "wrote $out"
