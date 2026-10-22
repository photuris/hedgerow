# Patches to bluez-async 0.8.2

This is the crates.io source of `bluez-async` 0.8.2 (MIT OR Apache-2.0),
patched in place and wired in through `[patch.crates-io]` in the
workspace `Cargo.toml`. `btleplug` 0.12 pulls it in on Linux. Only the
changes listed here differ from the published crate.

## Applied

- `src/messagestream.rs`, `impl Drop for MessageStream`: the spawned
  task that removes the D-Bus match no longer unwraps the result. When
  the match is already gone, D-Bus answers "No match with that id
  found" and the published crate panicked in that task on every
  Bluetooth disconnect. The panic killed only the cleanup task, but it
  filled the log. The error is now logged at debug level. 0.9.0 (used
  by `btleplug` 0.13) still has the unwrap.
