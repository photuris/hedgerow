#!/usr/bin/env bash
# Builds dist/Hedgerow.app on macOS: release binary, Info.plist, ad-hoc signature.
# The Info.plist must carry NSBluetoothAlwaysUsageDescription, or macOS kills the
# app the first time it touches CoreBluetooth.
set -euo pipefail

cd "$(dirname "$0")/.."

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "bundle-macos.sh: run this on macOS" >&2
    exit 1
fi

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
app=dist/Hedgerow.app

icon=packaging/macos/Hedgerow.icns

if [[ ! -f "$icon" ]]; then
    echo "bundle-macos.sh: $icon is missing" >&2
    exit 1
fi

cargo build --release --locked -p hedgerow-gui

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/hedgerow "$app/Contents/MacOS/hedgerow"
cp "$icon" "$app/Contents/Resources/Hedgerow.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>Hedgerow</string>
    <key>CFBundleDisplayName</key>
    <string>Hedgerow</string>
    <key>CFBundleIdentifier</key>
    <string>io.github.photuris.hedgerow</string>
    <key>CFBundleExecutable</key>
    <string>hedgerow</string>
    <key>CFBundleIconFile</key>
    <string>Hedgerow</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>${version}</string>
    <key>CFBundleVersion</key>
    <string>${version}</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSBluetoothAlwaysUsageDescription</key>
    <string>Hedgerow connects to your MeshCore radio over Bluetooth.</string>
</dict>
</plist>
PLIST

codesign --force --deep --sign - "$app"
codesign --verify --deep --strict "$app"
echo "built $app ($version)"
