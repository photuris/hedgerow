# Hedgerow: notes for agents

Read `README.md` first. This file holds what the code does not say.

## Shape

- Rust workspace, edition 2024. `hedgerow-core` holds all logic.
  `hedgerow-gui` is a view: it sends `session::Command`s and re-reads the
  `Store` when the session broadcasts `Update::StoreChanged`. The GUI never
  talks to the radio.
- `radio::Radio` is an enum: `Mesh` wraps `meshcore_rs::MeshCore`, `Fake`
  is an in-memory radio for tests and UI test mode. `meshcore-rs` has no
  public constructor for a custom transport, so the fake sits at this enum.
- Notifications: `hedgerow_core::notify::Notifier` decides every post from
  the store, with the clock passed in, so its tests control time.
  `crates/hedgerow-gui/src/notifier.rs` only feeds it events and shows
  what it returns. Keep timing and filtering logic in core.
- The session is an actor task. A connect attempt runs as a spawned task.
  The actor keeps handling commands while it is pending.
- The original build plan and its review history are in the gitignored
  overseer ledger (`.overseer/runs/2026-10-14-mvp/`), present on the
  original machine. The doc comments in `crates/hedgerow-core/src` are the
  live contract.

## Rules that bit us

- **Never touch a real radio from a test or an automated run.** Tests use
  `Connector::Fake`. Only `examples/probe.rs` talks to hardware, and it
  only reads. The owner's radio settings must not change by accident.
- **Never drive the desktop session.** No `hyprctl`, `wtype`, `ydotool`,
  cursor or focus tools. An agent's `hyprctl repl` call crashed the
  owner's Hyprland once. Test UI behavior with headless Dioxus VDOM tests
  (`crates/hedgerow-gui/src/ui_tests.rs`).
- The store is history, not a cache. Messages pulled off the radio queue
  exist nowhere else. Schema changes migrate forward. Never drop tables.
- Everything in the store is scoped by node id, the radio's 64-hex public
  key.
- A channel thread names a slot (`ThreadId::Channel(idx)`), but the store
  keys channel history by the slot's secret. A different channel in the
  same slot starts empty. A channel that moves to a new slot keeps its
  history. `Store::message` returns `None` for a row whose channel is not
  installed. Read the channel-identity doc comments in `store.rs` before
  you change channel code.
- Every mutating radio operation is a raw frame sent with
  `send_multi(&frame, &[<expected>, EventType::Error], ..)`. An `Error`
  frame is `Error::Rejected` (the device changed nothing). A timeout or
  transport failure is `Error::Radio` (the outcome is unknown). After an
  unknown channel write the session reconnects, so the slot map is read
  again.
- `meshcore-rs` is vendored with three patches (contact URI hex encoding,
  reply subscription before enqueue, BLE link-loss detection). Keep `vendor/meshcore-rs/PATCHES.md`
  current. The vendored crate is excluded from the workspace, so
  workspace clippy does not lint it.
- `std::env::set_var` only in a synchronous `main`. Tests use the `_at`
  and `open_memory` entry points.
- Dioxus 0.7: `document::Style` inserts once, so dynamic CSS goes in a
  plain `style {}` element. Create signals in the scope that uses them.
  Event handlers must read signals at event time, not values captured at
  render.

## Checks before claiming done

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

A GUI smoke run opens a real window. Do it only when a person is watching
the screen:

```sh
HEDGEROW_UI_TEST=1 HEDGEROW_EXIT_AFTER_MS=4000 cargo run -p hedgerow-gui
```
