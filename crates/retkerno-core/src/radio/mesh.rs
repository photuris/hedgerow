//! The real radio: a thin wrapper over `meshcore_rs::MeshCore`.
//!
//! Every mutating operation is a raw command frame sent with
//! `send_multi(frame, [expected, Error])`. The library's own helpers wait
//! only for the success event, so a device refusal would look like a
//! timeout; the raw path turns it into [`Error::Rejected`].

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use meshcore_rs::events::{
    BatteryInfo, ChannelMessage, Contact as McContact, ContactMessage, DeviceInfoData, SelfInfo,
};
use meshcore_rs::{EventPayload, EventType, MeshCore, MeshCoreEvent};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::task::JoinHandle;

use super::{
    ConnectTarget, DeviceSettings, IncomingMessage, MAX_CHANNEL_NAME_BYTES, RadioEvent, SentInfo,
};
use crate::error::{Error, Result};
use crate::model::{Channel, Contact, ContactKind, DeviceInfo, ThreadId};
use crate::text;

/// How long one command may take before the outcome counts as unknown.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the full contact list may take to arrive.
const CONTACTS_TIMEOUT: Duration = Duration::from_secs(30);

/// Capacity of the event channel handed to subscribers.
const EVENT_CAPACITY: usize = 256;

/// Path length the device reports for a message that arrived on a direct
/// route, where no hop count exists.
const PATH_LEN_DIRECT: u8 = 0xFF;

/// Bytes in a contact's public key.
const PUBLIC_KEY_BYTES: usize = 32;

const CMD_SEND_TEXT: u8 = 0x02;
const CMD_SEND_CHANNEL_TEXT: u8 = 0x03;
const CMD_SET_TIME: u8 = 0x06;
const CMD_SEND_ADVERT: u8 = 0x07;
const CMD_REMOVE_CONTACT: u8 = 0x0F;
const CMD_RESET_PATH: u8 = 0x0D;
const CMD_EXPORT_CONTACT: u8 = 0x11;
const CMD_IMPORT_CONTACT: u8 = 0x12;
const CMD_GET_CHANNEL: u8 = 0x1F;
const CMD_SET_CHANNEL: u8 = 0x20;

/// A connected `meshcore-rs` client plus its event forwarder.
pub struct MeshRadio {
    mc: Arc<MeshCore>,
    events: broadcast::Sender<RadioEvent>,
    forwarder: Mutex<Option<JoinHandle<()>>>,
}

impl MeshRadio {
    /// Opens the transport and starts forwarding library events.
    pub(super) async fn connect(target: &ConnectTarget) -> Result<MeshRadio> {
        let mc = match target {
            ConnectTarget::Ble(name) => MeshCore::ble_connect(name).await,
            ConnectTarget::Serial { port, baud } => MeshCore::serial(port, *baud).await,
            ConnectTarget::Tcp { host, port } => MeshCore::tcp(host, *port).await,
        }
        .map_err(radio_err)?;

        let mc = Arc::new(mc);
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        // `receiver()` rather than `event_stream()`: the stream silently
        // drops lag errors, and a lag means we may have missed a push.
        let rx = mc.dispatcher().receiver();
        let forwarder = tokio::spawn(forward(mc.clone(), rx, events.clone()));

        Ok(MeshRadio {
            mc,
            events,
            forwarder: Mutex::new(Some(forwarder)),
        })
    }

    /// Scans for MeshCore devices over BLE and returns their names.
    pub(super) async fn scan_ble(duration: Duration) -> Result<Vec<String>> {
        MeshCore::ble_discover(duration).await.map_err(radio_err)
    }

    /// Subscribes to pushes from the radio.
    pub(super) fn subscribe(&self) -> broadcast::Receiver<RadioEvent> {
        self.events.subscribe()
    }

    /// Sends one raw frame and waits for `expected` or an `Error` event.
    ///
    /// The command lock is held only while this one command is in flight.
    /// Lock acquisition, enqueue, and wait share one [`COMMAND_TIMEOUT`],
    /// so a stalled link cannot block the caller past it.
    async fn command(
        &self,
        operation: &str,
        frame: &[u8],
        expected: EventType,
    ) -> Result<MeshCoreEvent> {
        let event = within(operation, COMMAND_TIMEOUT, async {
            self.mc
                .commands()
                .lock()
                .await
                .send_multi(frame, &[expected, EventType::Error], COMMAND_TIMEOUT)
                .await
                .map_err(radio_err)
        })
        .await?;

        if event.event_type == EventType::Error {
            return Err(rejection(&event));
        }

        Ok(event)
    }

    /// App start, device query, and battery, merged into one report.
    ///
    /// Each library call gets its own [`COMMAND_TIMEOUT`].
    pub(super) async fn start(&self) -> Result<DeviceInfo> {
        let me = within("start", COMMAND_TIMEOUT, async {
            self.mc
                .commands()
                .lock()
                .await
                .send_appstart()
                .await
                .map_err(radio_err)
        })
        .await?;
        let query = within("start", COMMAND_TIMEOUT, async {
            self.mc
                .commands()
                .lock()
                .await
                .send_device_query()
                .await
                .map_err(radio_err)
        })
        .await?;
        let battery = self.read_battery("start").await?;

        Ok(device_from_mc(&me, &query, &battery))
    }

    /// Reads the battery voltage in millivolts.
    pub(super) async fn battery_mv(&self) -> Result<u16> {
        Ok(self.read_battery("battery").await?.battery_mv)
    }

    /// Reads the battery report under one [`COMMAND_TIMEOUT`].
    async fn read_battery(&self, operation: &str) -> Result<BatteryInfo> {
        within(operation, COMMAND_TIMEOUT, async {
            self.mc
                .commands()
                .lock()
                .await
                .get_bat()
                .await
                .map_err(radio_err)
        })
        .await
    }

    /// Reads slots `0..max` and keeps the ones with a name.
    pub(super) async fn channels(&self, max: u8) -> Result<Vec<Channel>> {
        let mut found = Vec::new();

        for idx in 0..max {
            let event = self
                .command("channels", &[CMD_GET_CHANNEL, idx], EventType::ChannelInfo)
                .await?;
            let EventPayload::ChannelInfo(info) = event.payload else {
                return Err(Error::Radio(format!("unexpected reply for channel {idx}")));
            };

            if info.name.is_empty() {
                continue;
            }

            found.push(Channel {
                idx: info.channel_idx,
                name: info.name,
                secret: info.secret,
            });
        }

        Ok(found)
    }

    /// Writes a channel into its slot.
    pub(super) async fn set_channel(&self, ch: &Channel) -> Result<()> {
        let frame = set_channel_frame(ch.idx, &ch.name, &ch.secret)?;

        self.command("set_channel", &frame, EventType::Ok)
            .await
            .map(drop)
    }

    /// Empties a slot: empty name and zero secret.
    pub(super) async fn clear_channel(&self, idx: u8) -> Result<()> {
        let frame = set_channel_frame(idx, "", &[0; 16])?;

        self.command("clear_channel", &frame, EventType::Ok)
            .await
            .map(drop)
    }

    /// Reads contacts modified after `since`.
    pub(super) async fn contacts(&self, since: u32) -> Result<Vec<Contact>> {
        let contacts = within("contacts", CONTACTS_TIMEOUT, async {
            self.mc
                .commands()
                .lock()
                .await
                .get_contacts_with_timeout(since, CONTACTS_TIMEOUT)
                .await
                .map_err(radio_err)
        })
        .await?;

        Ok(contacts.iter().map(contact_from_mc).collect())
    }

    /// Deletes a contact from the device.
    pub(super) async fn remove_contact(&self, public_key: &str) -> Result<()> {
        let frame = key_frame(CMD_REMOVE_CONTACT, public_key)?;

        self.command("remove_contact", &frame, EventType::Ok)
            .await
            .map(drop)
    }

    /// Forgets the learned route to a contact.
    pub(super) async fn reset_path(&self, public_key: &str) -> Result<()> {
        let frame = key_frame(CMD_RESET_PATH, public_key)?;

        self.command("reset_path", &frame, EventType::Ok)
            .await
            .map(drop)
    }

    /// Imports a contact card.
    pub(super) async fn import_contact(&self, card: &[u8]) -> Result<()> {
        let frame = [&[CMD_IMPORT_CONTACT][..], card].concat();

        self.command("import_contact", &frame, EventType::Ok)
            .await
            .map(drop)
    }

    /// Exports a contact card URI; `None` exports the radio itself.
    pub(super) async fn export_contact(&self, public_key: Option<&str>) -> Result<String> {
        let frame = match public_key {
            Some(key) => key_frame(CMD_EXPORT_CONTACT, key)?,
            None => vec![CMD_EXPORT_CONTACT],
        };
        let event = self
            .command("export_contact", &frame, EventType::ContactUri)
            .await?;

        match event.payload {
            EventPayload::String(uri) => Ok(uri),
            _ => Err(Error::Radio("unexpected reply to contact export".into())),
        }
    }

    /// Sends a direct message and returns the ack tag to wait for.
    pub(super) async fn send_text(&self, prefix: &str, text: &str, ts: u32) -> Result<SentInfo> {
        let frame = dm_frame(prefix, text, ts)?;
        let event = self
            .command("send_text", &frame, EventType::MsgSent)
            .await?;

        match event.payload {
            EventPayload::MsgSent(info) => Ok(SentInfo {
                ack: info.expected_ack,
                timeout_ms: info.suggested_timeout,
            }),
            _ => Err(Error::Radio("unexpected reply to message send".into())),
        }
    }

    /// Sends bare text to a channel slot.
    pub(super) async fn send_channel_text(&self, idx: u8, text: &str, ts: u32) -> Result<()> {
        let frame = channel_text_frame(idx, text, ts);

        self.command("send_channel_text", &frame, EventType::Ok)
            .await
            .map(drop)
    }

    /// Pops the next queued message.
    pub(super) async fn next_message(&self) -> Result<Option<IncomingMessage>> {
        let event = within("next_message", COMMAND_TIMEOUT, async {
            self.mc
                .commands()
                .lock()
                .await
                .get_msg_with_timeout(COMMAND_TIMEOUT)
                .await
                .map_err(radio_err)
        })
        .await?;

        Ok(event.and_then(|e| match e.payload {
            EventPayload::ContactMessage(m) => Some(incoming_from_contact_msg(&m)),
            EventPayload::ChannelMessage(m) => Some(incoming_from_channel_msg(&m)),
            _ => None,
        }))
    }

    /// Sends a flood or zero-hop advert.
    pub(super) async fn send_advert(&self, flood: bool) -> Result<()> {
        let frame = if flood {
            vec![CMD_SEND_ADVERT, 0x01]
        } else {
            vec![CMD_SEND_ADVERT]
        };

        self.command("send_advert", &frame, EventType::Ok)
            .await
            .map(drop)
    }

    /// Sets the device clock.
    pub(super) async fn sync_clock(&self, now: u32) -> Result<()> {
        let frame = [&[CMD_SET_TIME][..], &now.to_le_bytes()].concat();

        self.command("sync_clock", &frame, EventType::Ok)
            .await
            .map(drop)
    }

    /// Validates `s`, then sends each changed settings group in order.
    pub(super) async fn apply_settings(
        &self,
        current: &DeviceInfo,
        s: &DeviceSettings,
    ) -> Result<()> {
        s.validate(current)?;

        for frame in s.frames(current) {
            self.command("apply_settings", &frame, EventType::Ok)
                .await?;
        }

        Ok(())
    }

    /// Stops the forwarder and closes the transport. Safe to call twice.
    pub(super) async fn disconnect(&self) {
        self.stop_forwarder();

        if let Err(e) = self.mc.disconnect().await {
            tracing::warn!(error = %e, "radio disconnect failed");
        }
    }

    /// Reports whether the library still considers the transport up.
    pub(super) async fn is_connected(&self) -> bool {
        self.mc.is_connected().await
    }

    /// Aborts the forwarder task if it is still running.
    fn stop_forwarder(&self) {
        let handle = self
            .forwarder
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();

        if let Some(handle) = handle {
            handle.abort();
        }
    }
}

impl Drop for MeshRadio {
    /// Stops the forwarder so a dropped radio leaks no task.
    fn drop(&mut self) {
        self.stop_forwarder();
    }
}

// ── Event forwarding ────────────────────────────────────────────────────────

/// Republishes library events as [`RadioEvent`]s until the dispatcher closes.
async fn forward(
    mc: Arc<MeshCore>,
    mut rx: broadcast::Receiver<MeshCoreEvent>,
    tx: broadcast::Sender<RadioEvent>,
) {
    loop {
        let events: Vec<RadioEvent> = match rx.recv().await {
            Ok(event) => radio_event(&event).into_iter().collect(),
            Err(RecvError::Lagged(_)) => lag_events(mc.is_connected().await),
            Err(RecvError::Closed) => return,
        };

        for event in events {
            // No receiver just means the session is between connections.
            let _ = tx.send(event);
        }
    }
}

/// Maps one library event to the push the session cares about, if any.
fn radio_event(event: &MeshCoreEvent) -> Option<RadioEvent> {
    match event.event_type {
        EventType::MessagesWaiting => Some(RadioEvent::MessagesWaiting),
        EventType::Ack => match event.payload {
            EventPayload::Ack { tag } => Some(RadioEvent::Ack(tag)),
            _ => None,
        },
        EventType::Advertisement | EventType::NewContact | EventType::PathUpdate => {
            Some(RadioEvent::ContactsChanged)
        }
        EventType::Disconnected => Some(RadioEvent::Disconnected),
        _ => None,
    }
}

/// Events to publish after the forwarder missed some: a dead transport is a
/// disconnect, otherwise resync everything we could have missed.
fn lag_events(connected: bool) -> Vec<RadioEvent> {
    if connected {
        vec![RadioEvent::MessagesWaiting, RadioEvent::ContactsChanged]
    } else {
        vec![RadioEvent::Disconnected]
    }
}

// ── Frames and errors ───────────────────────────────────────────────────────

/// Runs `work` under one deadline; expiry is an unknown outcome.
async fn within<T>(
    operation: &str,
    limit: Duration,
    work: impl Future<Output = Result<T>>,
) -> Result<T> {
    tokio::time::timeout(limit, work)
        .await
        .unwrap_or_else(|_| Err(Error::Radio(format!("timed out: {operation}"))))
}

/// Wraps a library error: the outcome of the command is unknown.
fn radio_err(e: meshcore_rs::Error) -> Error {
    Error::Radio(e.to_string())
}

/// Turns a device `Error` event into [`Error::Rejected`].
fn rejection(event: &MeshCoreEvent) -> Error {
    match &event.payload {
        EventPayload::String(msg) => Error::Rejected(msg.clone()),
        _ => Error::Rejected("unknown error".into()),
    }
}

/// Decodes a 64-hex public key into its 32 bytes.
fn public_key_bytes(public_key: &str) -> Result<Vec<u8>> {
    let bytes = text::from_hex(public_key)?;

    if bytes.len() != PUBLIC_KEY_BYTES {
        return Err(Error::Invalid(format!(
            "public key is {} bytes, need {PUBLIC_KEY_BYTES}",
            bytes.len()
        )));
    }

    Ok(bytes)
}

/// Builds `[cmd] ++ public_key_32`.
fn key_frame(cmd: u8, public_key: &str) -> Result<Vec<u8>> {
    Ok([&[cmd][..], &public_key_bytes(public_key)?].concat())
}

/// Builds the direct-message frame for the contact with the 12-hex prefix.
fn dm_frame(prefix: &str, text: &str, ts: u32) -> Result<Vec<u8>> {
    let prefix = text::from_hex(prefix)?;

    if prefix.len() != 6 {
        return Err(Error::Invalid(format!(
            "key prefix is {} bytes, need 6",
            prefix.len()
        )));
    }

    Ok([
        &[CMD_SEND_TEXT, 0x00, 0x00][..],
        &ts.to_le_bytes(),
        &prefix,
        text.as_bytes(),
    ]
    .concat())
}

/// Builds the channel-message frame; the radio prepends our name.
fn channel_text_frame(idx: u8, text: &str, ts: u32) -> Vec<u8> {
    [
        &[CMD_SEND_CHANNEL_TEXT, 0x00, idx][..],
        &ts.to_le_bytes(),
        text.as_bytes(),
    ]
    .concat()
}

/// Builds the set-channel frame: name zero-padded to 32 bytes, then secret.
fn set_channel_frame(idx: u8, name: &str, secret: &[u8; 16]) -> Result<Vec<u8>> {
    if name.len() > MAX_CHANNEL_NAME_BYTES {
        return Err(Error::Invalid(format!(
            "channel name is {} bytes, the limit is {MAX_CHANNEL_NAME_BYTES}",
            name.len()
        )));
    }

    let mut padded = [0u8; 32];

    padded[..name.len()].copy_from_slice(name.as_bytes());

    Ok([&[CMD_SET_CHANNEL, idx][..], &padded, secret].concat())
}

// ── Library to model conversions ────────────────────────────────────────────

/// Converts a library contact to ours.
fn contact_from_mc(c: &McContact) -> Contact {
    Contact {
        public_key: text::to_hex(&c.public_key),
        name: c.adv_name.clone(),
        kind: ContactKind::from_u8(c.contact_type),
        path_len: c.path_len,
        last_advert: c.last_advert,
        lat: c.adv_lat,
        lon: c.adv_lon,
        last_modified: c.last_modification_timestamp,
    }
}

/// Maps the device's hop count: `0xFF` means a direct route, so no count.
fn hops(path_len: u8) -> Option<u8> {
    (path_len != PATH_LEN_DIRECT).then_some(path_len)
}

/// Converts a queued direct message; the sender is the key prefix.
fn incoming_from_contact_msg(m: &ContactMessage) -> IncomingMessage {
    IncomingMessage {
        thread: ThreadId::Contact(text::to_hex(&m.sender_prefix)),
        sender: None,
        text: m.text.clone(),
        sender_ts: m.sender_timestamp,
        path_len: hops(m.path_len),
        snr: m.snr,
    }
}

/// Converts a queued channel message, splitting `"<sender>: <text>"`.
fn incoming_from_channel_msg(m: &ChannelMessage) -> IncomingMessage {
    let (sender, body) = text::split_channel_text(&m.text);

    IncomingMessage {
        thread: ThreadId::Channel(m.channel_idx),
        sender,
        text: body,
        sender_ts: m.sender_timestamp,
        path_len: hops(m.path_len),
        snr: m.snr,
    }
}

/// Merges the three start-up replies into one device report.
fn device_from_mc(me: &SelfInfo, query: &DeviceInfoData, battery: &BatteryInfo) -> DeviceInfo {
    DeviceInfo {
        node_id: text::to_hex(&me.public_key),
        name: me.name.clone(),
        tx_power: me.tx_power,
        max_tx_power: me.max_tx_power,
        radio_freq: me.radio_freq,
        radio_bw: me.radio_bw,
        sf: me.sf,
        cr: me.cr,
        manual_add_contacts: me.manual_add_contacts,
        adv_loc_policy: me.adv_loc_policy,
        multi_acks: me.multi_acks,
        telemetry_base: me.telemetry_mode_base,
        telemetry_loc: me.telemetry_mode_loc,
        telemetry_env: me.telemetry_mode_env,
        lat: me.adv_lat,
        lon: me.adv_lon,
        firmware: query.version.clone(),
        fw_build: query.fw_build.clone(),
        model: query.model.clone(),
        max_contacts: query.max_contacts.map(u32::from),
        max_channels: query.max_channels,
        battery_mv: Some(battery.battery_mv),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "0a0b0c0d0e0f0000000000000000000000000000000000000000000000000001";

    fn mc_key() -> [u8; 32] {
        let mut key = [0u8; 32];

        key[0] = 0x0a;
        key[1] = 0x0b;
        key[2] = 0x0c;
        key[3] = 0x0d;
        key[4] = 0x0e;
        key[5] = 0x0f;
        key[31] = 0x01;

        key
    }

    mod contact_from_mc {
        use super::*;

        #[test]
        fn should_map_every_field_when_converting_a_contact() {
            let mc = McContact {
                public_key: mc_key(),
                contact_type: 2,
                flags: 0,
                path_len: -1,
                out_path: vec![],
                adv_name: "Hilltop".into(),
                last_advert: 11,
                adv_lat: 35_000_000,
                adv_lon: -86_000_000,
                last_modification_timestamp: 22,
            };

            assert_eq!(
                contact_from_mc(&mc),
                Contact {
                    public_key: KEY.into(),
                    name: "Hilltop".into(),
                    kind: ContactKind::Repeater,
                    path_len: -1,
                    last_advert: 11,
                    lat: 35_000_000,
                    lon: -86_000_000,
                    last_modified: 22,
                }
            );
        }
    }

    mod incoming_from_contact_msg {
        use super::*;

        fn msg(path_len: u8) -> ContactMessage {
            ContactMessage {
                sender_prefix: [0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f],
                path_len,
                txt_type: 0,
                sender_timestamp: 1234,
                text: "hi: there".into(),
                snr: Some(4.5),
                signature: None,
            }
        }

        #[test]
        fn should_use_prefix_hex_thread_and_no_sender_when_message_is_a_dm() {
            assert_eq!(
                incoming_from_contact_msg(&msg(2)),
                IncomingMessage {
                    thread: ThreadId::Contact("0a0b0c0d0e0f".into()),
                    sender: None,
                    text: "hi: there".into(),
                    sender_ts: 1234,
                    path_len: Some(2),
                    snr: Some(4.5),
                }
            );
        }

        #[test]
        fn should_report_no_hop_count_when_route_is_direct() {
            assert_eq!(incoming_from_contact_msg(&msg(0xFF)).path_len, None);
        }
    }

    mod incoming_from_channel_msg {
        use super::*;

        fn msg(text: &str) -> ChannelMessage {
            ChannelMessage {
                channel_idx: 3,
                path_len: 1,
                txt_type: 0,
                sender_timestamp: 99,
                text: text.into(),
                snr: None,
            }
        }

        #[test]
        fn should_split_sender_from_text_when_prefix_is_present() {
            assert_eq!(
                incoming_from_channel_msg(&msg("Al: see you: soon")),
                IncomingMessage {
                    thread: ThreadId::Channel(3),
                    sender: Some("Al".into()),
                    text: "see you: soon".into(),
                    sender_ts: 99,
                    path_len: Some(1),
                    snr: None,
                }
            );
        }
    }

    mod device_from_mc {
        use super::*;

        #[test]
        fn should_merge_self_info_query_and_battery() {
            let me = SelfInfo {
                public_key: mc_key(),
                name: "Node".into(),
                tx_power: 20,
                max_tx_power: 22,
                radio_freq: 910_525,
                radio_bw: 62_500,
                sf: 7,
                cr: 5,
                manual_add_contacts: true,
                adv_loc_policy: 1,
                multi_acks: 1,
                telemetry_mode_base: 1,
                telemetry_mode_loc: 2,
                telemetry_mode_env: 3,
                adv_lat: 5,
                adv_lon: -6,
                ..SelfInfo::default()
            };
            let query = DeviceInfoData {
                max_contacts: Some(255),
                max_channels: Some(20),
                fw_build: Some("14 Oct 2026".into()),
                model: Some("T-Pager".into()),
                version: Some("1.3.6".into()),
                ..DeviceInfoData::default()
            };
            let battery = BatteryInfo {
                battery_mv: 4126,
                used_kb: None,
                total_kb: None,
            };

            assert_eq!(
                device_from_mc(&me, &query, &battery),
                DeviceInfo {
                    node_id: KEY.into(),
                    name: "Node".into(),
                    tx_power: 20,
                    max_tx_power: 22,
                    radio_freq: 910_525,
                    radio_bw: 62_500,
                    sf: 7,
                    cr: 5,
                    manual_add_contacts: true,
                    adv_loc_policy: 1,
                    multi_acks: 1,
                    telemetry_base: 1,
                    telemetry_loc: 2,
                    telemetry_env: 3,
                    lat: 5,
                    lon: -6,
                    firmware: Some("1.3.6".into()),
                    fw_build: Some("14 Oct 2026".into()),
                    model: Some("T-Pager".into()),
                    max_contacts: Some(255),
                    max_channels: Some(20),
                    battery_mv: Some(4126),
                }
            );
        }
    }

    mod radio_event {
        use super::*;

        fn event(t: EventType, payload: EventPayload) -> MeshCoreEvent {
            MeshCoreEvent::new(t, payload)
        }

        #[test]
        fn should_map_pushes_the_session_reacts_to() {
            let ack = event(EventType::Ack, EventPayload::Ack { tag: [1, 2, 3, 4] });

            assert_eq!(radio_event(&ack), Some(RadioEvent::Ack([1, 2, 3, 4])));

            for t in [
                EventType::Advertisement,
                EventType::NewContact,
                EventType::PathUpdate,
            ] {
                assert_eq!(
                    radio_event(&event(t, EventPayload::None)),
                    Some(RadioEvent::ContactsChanged)
                );
            }

            assert_eq!(
                radio_event(&event(EventType::MessagesWaiting, EventPayload::None)),
                Some(RadioEvent::MessagesWaiting)
            );
            assert_eq!(
                radio_event(&event(EventType::Disconnected, EventPayload::None)),
                Some(RadioEvent::Disconnected)
            );
        }

        #[test]
        fn should_ignore_everything_else() {
            assert_eq!(
                radio_event(&event(EventType::Battery, EventPayload::None)),
                None
            );
        }
    }

    mod lag_events {
        use super::*;

        #[test]
        fn should_resync_when_transport_is_up_and_disconnect_when_down() {
            assert_eq!(
                lag_events(true),
                vec![RadioEvent::MessagesWaiting, RadioEvent::ContactsChanged]
            );
            assert_eq!(lag_events(false), vec![RadioEvent::Disconnected]);
        }
    }

    mod rejection {
        use super::*;

        #[test]
        fn should_carry_the_device_message_when_error_event_has_one() {
            let err = rejection(&MeshCoreEvent::error("table full"));

            assert!(matches!(err, Error::Rejected(m) if m == "table full"));
        }
    }

    mod frames {
        use super::*;

        #[test]
        fn should_build_dm_frame_with_prefix_and_little_endian_timestamp() {
            let frame = dm_frame("0a0b0c0d0e0f", "yo", 0x0102_0304).unwrap();

            assert_eq!(
                frame,
                vec![
                    0x02, 0x00, 0x00, 0x04, 0x03, 0x02, 0x01, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
                    b'y', b'o'
                ]
            );
        }

        #[test]
        fn should_reject_dm_prefix_that_is_not_six_bytes() {
            assert!(matches!(dm_frame("0a0b", "x", 1), Err(Error::Invalid(_))));
        }

        #[test]
        fn should_build_channel_text_frame_without_sender_name() {
            assert_eq!(
                channel_text_frame(7, "hi", 1),
                vec![0x03, 0x00, 7, 1, 0, 0, 0, b'h', b'i']
            );
        }

        #[test]
        fn should_pad_channel_name_to_32_bytes_then_append_secret() {
            let frame = set_channel_frame(4, "#a", &[9; 16]).unwrap();

            assert_eq!(frame.len(), 2 + 32 + 16);
            assert_eq!(&frame[..5], &[0x20, 4, b'#', b'a', 0]);
            assert_eq!(&frame[34..], &[9; 16]);
        }

        #[test]
        fn should_reject_channel_name_over_31_bytes() {
            assert!(matches!(
                set_channel_frame(0, &"x".repeat(32), &[0; 16]),
                Err(Error::Invalid(_))
            ));
        }

        #[test]
        fn should_build_key_frame_from_64_hex_chars() {
            let frame = key_frame(0x0F, KEY).unwrap();

            assert_eq!(
                (frame[0], frame.len(), frame.last().copied()),
                (0x0F, 33, Some(0x01))
            );
        }
    }
}
