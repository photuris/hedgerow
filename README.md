# Hedgerow

A desktop chat client for [MeshCore](https://meshcore.co.uk) companion
radios. It's written in Rust, with a Dioxus 0.7 desktop GUI. A hedgerow is
a living mesh strung between fields.

This is a first pass. It covers chat in channels and direct threads,
unread tracking, contacts, channels, and device settings. GPS, maps, room
servers, repeater administration, and telemetry are not in it yet.

## Features

- Connects over Bluetooth LE, USB serial, or TCP (WiFi companion firmware).
  You pick the transport in Settings > Connection.
- Shows channels and direct-message threads in a sidebar with unread
  badges. The window title shows the total unread count.
- Chat view with day separators, sender names, delivery state for direct
  messages (pending, sent, delivered, failed), and Retry for failed sends.
- Marks a thread read when you open it. Right-click a thread to mark it
  read or unread, or use "Mark all read".
- Keeps every message in a local SQLite database, so history survives
  restarts and works offline.
- Searches messages. Type two or more characters in the box at the top of
  the sidebar (or press Ctrl+F, Cmd+F on macOS) to list matching messages,
  newest first, up to 200. The match covers message text and sender names,
  and ignores ASCII case. Click a result to open its thread at that
  message. Escape clears the box and brings back the view you were in.
- The sidebar resizes and collapses. Drag the divider between the sidebar
  and the main area to set its width (180 to 480 px, saved in
  `config.toml` as `ui.sidebar_width`). Press Ctrl+B (Cmd+B on macOS) or
  use the panel button to hide or show it. The collapsed state is not
  saved.
- Contacts: search, filter by kind, message, share as a `meshcore://`
  URI, import a URI, reset a path, remove, and send adverts.
- Channels: add hashtag channels (the key comes from the name), add
  private channels with a generated or pasted key, and remove channels.
- Device settings: name, TX power, frequency, bandwidth, spreading factor,
  coding rate, manual contact adding, advert location, and multiple acks.
  Every change goes through a confirmation dialog that lists it.
- Follows the Omarchy theme when it is present, else the system light or
  dark scheme.

## Layout

| Path | What it is |
|---|---|
| `crates/hedgerow-core` | All logic: config, SQLite store, radio wrapper (real and fake), session actor, demo fixtures. |
| `crates/hedgerow-gui` | The `hedgerow` binary: a Dioxus desktop view over the core. |
| `vendor/meshcore-rs` | [`meshcore-rs`](https://github.com/andrewdavidmackenzie/meshcore-rs) 0.2.0 (MIT) with three patches. `PATCHES.md` lists them. |

## Build and run

```sh
cargo build --workspace
cargo test --workspace
cargo run -p hedgerow-gui        # the app
```

On Linux the GUI needs WebKitGTK 4.1, a Wayland or X11 session, and BlueZ
for Bluetooth. No `dx` CLI is needed.

### macOS app bundle

On a Mac with Xcode or the Command Line Tools installed, run:

```sh
./scripts/bundle-macos.sh
```

The script builds `dist/Hedgerow.app` and signs it ad hoc. It runs on the
Mac that built it. Another Mac needs a Developer ID signature and
notarization. The `Info.plist` holds the Bluetooth usage text. macOS asks
for Bluetooth permission the first time the app connects.

macOS hides Bluetooth addresses from apps. Use the radio's advertised
name, for example `MeshCore-MYNODE`, or click Scan in Settings >
Connection.

## First run

1. Pair the radio with the operating system first, if it uses Bluetooth.
2. Start `hedgerow`.
3. Open Settings > Connection.
4. Select the transport. For Bluetooth, type the device name or address,
   or click Scan and choose the radio.
5. Click "Save and connect".

The app connects again on its next start when "Connect automatically on
start" is on. On macOS, use the Bluetooth device name, because macOS does
not expose Bluetooth addresses.

Only one client can use a radio's Bluetooth link at a time. Disconnect
the phone app before you connect Hedgerow.

## Data

Data lives in `$HEDGEROW_DATA_DIR`, default `~/.local/share/hedgerow`
(the platform data directory on macOS and Windows):

- `config.toml`: the connection settings and UI options.
- `hedgerow.db`: the message history, contacts, and channels, scoped by
  the radio's public key.

The radio holds incoming messages in a queue until a client fetches them.
Hedgerow fetches them and stores them in `hedgerow.db`. After that, the
phone app does not get them. Back up `hedgerow.db` if the history matters
to you.

```toml
# config.toml
[connection]
transport = "ble"          # "ble", "serial", or "tcp"
ble_device = "AA:BB:CC:DD:EE:FF"   # or the advertised name, e.g. "MeshCore-MYNODE"
serial_port = ""
baud = 115200
tcp_host = ""
tcp_port = 5000
auto_connect = true

[ui]
decorations = "auto"       # "auto", "on", or "off"
```

## UI test mode

`HEDGEROW_UI_TEST=1 hedgerow` runs on an in-memory store with a fake
radio full of demo data. It never reads or writes the config, and it never
touches Bluetooth or serial ports. These variables help with automated
checks:

| Variable | Effect |
|---|---|
| `HEDGEROW_EXIT_AFTER_MS=<n>` | Exit with code 0 after n milliseconds. |
| `HEDGEROW_DUMP_TEXT=<path>` | Before that exit, write the page text to `<path>` and the main view's text to `<path>.main`. |
| `HEDGEROW_START_VIEW=<view>` | Open `thread:channel:<idx>`, `thread:contact:<prefix>`, `contacts`, or `settings:<connection\|device\|channels>`. |

## Hardware check

`examples/probe.rs` connects, reads the device report, the channel names,
the contact count, and the radio's own contact URI, then prints JSON. It
changes nothing on the radio.

```sh
cargo run -p hedgerow-core --example probe -- --ble AA:BB:CC:DD:EE:FF
```

## Known limitations

- Connecting can take up to 30 seconds over Bluetooth. A Disconnect during
  that time takes effect at once in the UI. The app closes the link when
  the attempt finishes.
- `meshcore-rs` reports a contact capacity of at most 255. The device
  panel shows `≥255` for that value.
- On Linux, `bluez-async` can log a panic from a background task when a
  Bluetooth link closes. The app keeps running.
- The GUI reads the store on the UI thread. Long database writes can
  stall the UI for a moment.
- Direct messages are not resent automatically. Use Retry on a failed
  message.
- Some radios reboot when a program opens their USB serial port. The
  LilyGo T-LoRa Pager does this. Use Bluetooth for those radios.

## Not in this first pass

GPS and maps, room-server login, repeater administration, telemetry, path
discovery and trace, private-key export or import, desktop notifications,
message search, Linux packaging, and a notarized macOS build.
