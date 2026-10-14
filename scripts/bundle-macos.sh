#!/usr/bin/env bash
# Builds dist/Retkerno.app on macOS: release binary, Info.plist, ad-hoc signature.
# The Info.plist must carry NSBluetoothAlwaysUsageDescription, or macOS kills the
# app the first time it touches CoreBluetooth.
set -euo pipefail

cd "$(dirname "$0")/.."

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "bundle-macos.sh: run this on macOS" >&2
    exit 1
fi

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
app=dist/Retkerno.app

cargo build --release --locked -p retkerno-gui

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/retkerno "$app/Contents/MacOS/retkerno"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>Retkerno</string>
    <key>CFBundleDisplayName</key>
    <string>Retkerno</string>
    <key>CFBundleIdentifier</key>
    <string>io.github.photuris.retkerno</string>
    <key>CFBundleExecutable</key>
    <string>retkerno</string>
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
    <string>Retkerno connects to your MeshCore radio over Bluetooth.</string>
</dict>
</plist>
PLIST

codesign --force --deep --sign - "$app"
codesign --verify --deep --strict "$app"
echo "built $app ($version)"
