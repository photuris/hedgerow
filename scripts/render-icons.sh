#!/usr/bin/env bash
# Renders every Hedgerow icon raster from the SVG masters in assets/icon.
# Needs rsvg-convert and magick; the macOS .icns step also needs iconutil.
# Usage: scripts/render-icons.sh [--preview contact-sheet.png]
set -euo pipefail

cd "$(dirname "$0")/.."

preview=""
case "${1:-}" in
    "") ;;
    --preview) preview="${2:?--preview needs an output path}" ;;
    *) echo "usage: render-icons.sh [--preview out.png]" >&2; exit 2 ;;
esac

src=assets/icon
hicolor=packaging/linux/icons/hicolor
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# render <svg> <px> <out.png> [rsvg-convert args...]
render() {
    local svg=$1 px=$2 out=$3
    shift 3
    rsvg-convert -w "$px" -h "$px" "$@" "$svg" -o "$out"
}

# 1. Linux hicolor theme: the small glyph below 48 px, the master from 48 up.
for n in 16 24 32; do
    mkdir -p "$hicolor/${n}x${n}/apps"
    render "$src/hedgerow-small.svg" "$n" "$hicolor/${n}x${n}/apps/hedgerow.png"
done
for n in 48 64 128 256 512; do
    mkdir -p "$hicolor/${n}x${n}/apps"
    render "$src/hedgerow.svg" "$n" "$hicolor/${n}x${n}/apps/hedgerow.png"
done
mkdir -p "$hicolor/scalable/apps"
cp "$src/hedgerow.svg" "$hicolor/scalable/apps/hedgerow.svg"

# 2. Runtime window icon.
render "$src/hedgerow.svg" 256 crates/hedgerow-gui/assets/icon-256.png

# 3. Windows .ico: small glyph at 16/24/32, Fluent master at 48/64/256.
ico=()
for n in 16 24 32; do
    render "$src/hedgerow-small.svg" "$n" "$tmp/win-$n.png"
    ico+=("$tmp/win-$n.png")
done
for n in 48 64 256; do
    render "$src/hedgerow-windows.svg" "$n" "$tmp/win-$n.png"
    ico+=("$tmp/win-$n.png")
done
mkdir -p packaging/windows
magick "${ico[@]}" packaging/windows/hedgerow.ico

# 4. macOS .icns: the Big Sur plate is drawn at every size, so every size
# comes from the macOS master. iconutil only exists on macOS.
if [[ "$(uname -s)" == "Darwin" ]] && command -v iconutil >/dev/null; then
    set_dir="$tmp/Hedgerow.iconset"
    mkdir -p "$set_dir" packaging/macos
    for n in 16 32 128 256 512; do
        render "$src/hedgerow-macos.svg" "$n" "$set_dir/icon_${n}x${n}.png"
        render "$src/hedgerow-macos.svg" "$((n * 2))" "$set_dir/icon_${n}x${n}@2x.png"
    done
    iconutil -c icns "$set_dir" -o packaging/macos/Hedgerow.icns
else
    echo "skipping macOS .icns (needs iconutil on macOS)"
fi

# 5. Contact sheet: rows are Linux, small, Windows, macOS; columns are sizes.
# The white panel sits above the #1E1E2E panel. Not committed.
if [[ -n "$preview" ]]; then
    sizes=(16 32 48 128 256)
    cell=288
    rows=()
    for bg in '#FFFFFF' '#1E1E2E'; do
        for svg in hedgerow hedgerow-small hedgerow-windows hedgerow-macos; do
            tiles=()
            for n in "${sizes[@]}"; do
                png="$tmp/prev-$svg-$n.png"
                [[ -f "$png" ]] || render "$src/$svg.svg" "$n" "$png"
                tile="$tmp/tile-${bg#\#}-$svg-$n.png"
                magick -size "${cell}x${cell}" "xc:$bg" "$png" -gravity center -composite "$tile"
                tiles+=("$tile")
            done
            row="$tmp/row-${bg#\#}-$svg.png"
            magick "${tiles[@]}" +append "$row"
            rows+=("$row")
        done
    done
    magick "${rows[@]}" -append "$preview"
    echo "wrote $preview"
fi
