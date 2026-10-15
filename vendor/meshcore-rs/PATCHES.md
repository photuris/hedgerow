# Patches to meshcore-rs 0.2.0

This is the crates.io source of `meshcore-rs` 0.2.0, patched in place and
wired in through `[patch.crates-io]` in the workspace `Cargo.toml`. Only
the changes listed here differ from the published crate.

## Applied

- `src/reader.rs`, `MessageReader::handle_rx`, `PacketType::ContactUri`
  arm (and its unit test `test_handle_rx_contact_uri`): the URI is now
  `meshcore://` followed by the lowercase hex of the raw payload. The
  published crate ran `String::from_utf8_lossy` over the binary payload
  and returned mojibake. The official Python library does
  `"meshcore://" + payload.hex()`.
- `src/commands/base.rs`, `CommandHandler::send_with_timeout` and
  `CommandHandler::send_multi` (new private helper `send_and_wait`): the
  reply receiver is created before the frame is enqueued, and the enqueue
  and the wait run under one `tokio::time::timeout`. The published crate
  awaited the enqueue first and subscribed afterwards, so a reply
  dispatched in between was lost, and a full send queue blocked past the
  timeout. Public signatures, event filtering, and the timeout error
  (`Error::timeout`) are unchanged.
- `src/meshcore/ble.rs`, `MeshCore::ble_connect`, write and read tasks
  (new private helper `lose_link`, unit tests in `ble::tests`): the first
  write error, a failure to open the notification stream, or the end of
  that stream sets the client's `connected` flag to false and emits
  `EventType::Disconnected`, but only for the task that flips the flag from
  true to false under its write lock; the other emits nothing, so one lost
  link gives one event. The write task then ends (dropping the send queue,
  so later sends fail fast). `connected` is now set to true before the
  tasks are spawned, so a link that fails at once still counts as a
  transition. The published crate logged the error, broke out of the chunk
  loop only, and kept running, so the client stayed "connected" while
  every command timed out.

## Known, not patched

- `max_contacts` is at most 255: the device's contact capacity is decoded
  into a `u8` with a saturating doubling.
- `bluez-async` 0.8.2 `messagestream.rs:40` unwraps a D-Bus
  `remove_match` result and panics in a tokio worker during BLE
  disconnect. The process survives.
- `event_stream` drops lag errors, so a consumer cannot tell that it
  missed events.
