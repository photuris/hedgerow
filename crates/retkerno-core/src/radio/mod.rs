//! The radio layer: one [`Radio`] enum over the real `meshcore-rs` client
//! and an in-memory fake, so the session actor and the tests share a path.

mod fake;
mod mesh;

use std::time::Duration;

use tokio::sync::broadcast;

pub use fake::{FakeFailure, FakeRadio, FakeSent, FakeState};
pub use mesh::MeshRadio;

use crate::config::{ConnectionConfig, Transport};
use crate::error::{Error, Result};
use crate::model::{Channel, Contact, DeviceInfo, ThreadId};

/// Most bytes a channel name can use; the 32nd byte is the terminator.
pub(crate) const MAX_CHANNEL_NAME_BYTES: usize = 31;

/// Longest device name the firmware accepts, in bytes.
const MAX_NAME_BYTES: usize = 31;

/// Bandwidths the radio hardware supports, in kHz x 1000.
const VALID_BANDWIDTHS: [u32; 10] = [
    7_800, 10_400, 15_600, 20_800, 31_250, 41_700, 62_500, 125_000, 250_000, 500_000,
];

/// Where to reach a real radio.
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectTarget {
    /// Bluetooth LE device name or MAC address.
    Ble(String),
    /// USB serial port.
    Serial {
        /// Port path.
        port: String,
        /// Baud rate.
        baud: u32,
    },
    /// TCP socket.
    Tcp {
        /// Host name or address.
        host: String,
        /// Port.
        port: u16,
    },
}

impl ConnectTarget {
    /// Builds the target for the configured transport.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotConfigured`] when the field the transport needs
    /// is empty.
    pub fn from_config(c: &ConnectionConfig) -> Result<ConnectTarget> {
        match c.transport {
            Transport::Ble => {
                let name = c.ble_device.trim();

                if name.is_empty() {
                    return Err(Error::NotConfigured("ble_device is empty".into()));
                }

                Ok(ConnectTarget::Ble(name.to_string()))
            }
            Transport::Serial => {
                if c.serial_port.is_empty() {
                    return Err(Error::NotConfigured("serial_port is empty".into()));
                }

                Ok(ConnectTarget::Serial {
                    port: c.serial_port.clone(),
                    baud: c.baud,
                })
            }
            Transport::Tcp => {
                if c.tcp_host.is_empty() {
                    return Err(Error::NotConfigured("tcp_host is empty".into()));
                }

                Ok(ConnectTarget::Tcp {
                    host: c.tcp_host.clone(),
                    port: c.tcp_port,
                })
            }
        }
    }
}

/// A push from the radio that the session reacts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadioEvent {
    /// The device queue holds messages; drain it.
    MessagesWaiting,
    /// A direct message was acknowledged; carries the ack tag.
    Ack([u8; 4]),
    /// The contact list may have changed.
    ContactsChanged,
    /// The transport dropped.
    Disconnected,
}

/// What the radio returns after accepting a direct message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SentInfo {
    /// Tag the future `Ack` push will carry.
    pub ack: [u8; 4],
    /// How long the radio suggests waiting for that ack, in milliseconds.
    pub timeout_ms: u32,
}

/// A message pulled from the device queue.
#[derive(Debug, Clone, PartialEq)]
pub struct IncomingMessage {
    /// Conversation it belongs to.
    pub thread: ThreadId,
    /// Parsed sender name (channel messages only).
    pub sender: Option<String>,
    /// Message text without the sender prefix.
    pub text: String,
    /// Sender clock, unix seconds.
    pub sender_ts: u32,
    /// Hops the message travelled, when known.
    pub path_len: Option<u8>,
    /// Signal-to-noise ratio, when reported.
    pub snr: Option<f32>,
}

/// The settings the Settings view can change.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceSettings {
    /// Device name.
    pub name: String,
    /// Transmit power in dBm.
    pub tx_power: u8,
    /// Frequency in MHz x 1000.
    pub radio_freq: u32,
    /// Bandwidth in kHz x 1000.
    pub radio_bw: u32,
    /// Spreading factor.
    pub sf: u8,
    /// Coding rate.
    pub cr: u8,
    /// Add contacts only by hand.
    pub manual_add_contacts: bool,
    /// Advert location policy (0 to 2).
    pub adv_loc_policy: u8,
    /// Extra acknowledgements (0 or 1).
    pub multi_acks: u8,
}

impl DeviceSettings {
    /// Copies the editable settings out of the device report.
    pub fn from_device(d: &DeviceInfo) -> DeviceSettings {
        DeviceSettings {
            name: d.name.clone(),
            tx_power: d.tx_power,
            radio_freq: d.radio_freq,
            radio_bw: d.radio_bw,
            sf: d.sf,
            cr: d.cr,
            manual_add_contacts: d.manual_add_contacts,
            adv_loc_policy: d.adv_loc_policy,
            multi_acks: d.multi_acks,
        }
    }

    /// Checks every value against what the firmware accepts.
    ///
    /// The name is judged after trimming, because [`Self::frames`] sends the
    /// trimmed name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] naming the first field that is out of
    /// range and why.
    pub fn validate(&self, d: &DeviceInfo) -> Result<()> {
        let name = self.name.trim();

        if name.is_empty() {
            return Err(Error::Invalid("name is empty".into()));
        }

        if name.len() > MAX_NAME_BYTES {
            return Err(Error::Invalid(format!(
                "name is {} bytes, the limit is {MAX_NAME_BYTES}",
                name.len()
            )));
        }

        if !(1..=d.max_tx_power).contains(&self.tx_power) {
            return Err(Error::Invalid(format!(
                "tx_power {} is outside 1..={}",
                self.tx_power, d.max_tx_power
            )));
        }

        if !(5..=12).contains(&self.sf) {
            return Err(Error::Invalid(format!("sf {} is outside 5..=12", self.sf)));
        }

        if !(5..=8).contains(&self.cr) {
            return Err(Error::Invalid(format!("cr {} is outside 5..=8", self.cr)));
        }

        if !VALID_BANDWIDTHS.contains(&self.radio_bw) {
            return Err(Error::Invalid(format!(
                "radio_bw {} is not a supported bandwidth",
                self.radio_bw
            )));
        }

        if !(150_000..=2_500_000).contains(&self.radio_freq) {
            return Err(Error::Invalid(format!(
                "radio_freq {} is outside 150000..=2500000",
                self.radio_freq
            )));
        }

        if self.adv_loc_policy > 2 {
            return Err(Error::Invalid(format!(
                "adv_loc_policy {} is above 2",
                self.adv_loc_policy
            )));
        }

        if self.multi_acks > 1 {
            return Err(Error::Invalid(format!(
                "multi_acks {} is above 1",
                self.multi_acks
            )));
        }

        Ok(())
    }

    /// Builds the command frames for the groups that differ from `current`.
    ///
    /// Order: name, tx power, radio (frequency, bandwidth, sf, cr), other
    /// params. The other-params frame carries `current`'s telemetry modes
    /// unchanged.
    pub fn frames(&self, current: &DeviceInfo) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        let name = self.name.trim();

        if name != current.name {
            frames.push([&[0x08][..], name.as_bytes()].concat());
        }

        if self.tx_power != current.tx_power {
            frames.push([&[0x0C][..], &u32::from(self.tx_power).to_le_bytes()].concat());
        }

        if (self.radio_freq, self.radio_bw, self.sf, self.cr)
            != (current.radio_freq, current.radio_bw, current.sf, current.cr)
        {
            frames.push(
                [
                    &[0x0B][..],
                    &self.radio_freq.to_le_bytes(),
                    &self.radio_bw.to_le_bytes(),
                    &[self.sf, self.cr],
                ]
                .concat(),
            );
        }

        if (
            self.manual_add_contacts,
            self.adv_loc_policy,
            self.multi_acks,
        ) != (
            current.manual_add_contacts,
            current.adv_loc_policy,
            current.multi_acks,
        ) {
            let telemetry = (current.telemetry_base & 3)
                | ((current.telemetry_loc & 3) << 2)
                | ((current.telemetry_env & 3) << 4);

            frames.push(vec![
                0x26,
                u8::from(self.manual_add_contacts),
                telemetry,
                self.adv_loc_policy,
                self.multi_acks,
            ]);
        }

        frames
    }
}

/// A radio the session can drive: the real client or the in-memory fake.
pub enum Radio {
    /// A real `meshcore-rs` client.
    Mesh(MeshRadio),
    /// The in-memory fake used by tests and the GUI's UI-test mode.
    Fake(FakeRadio),
}

impl Radio {
    /// Connects to a real radio.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Radio`] when the transport cannot be opened.
    pub async fn connect(target: &ConnectTarget) -> Result<Radio> {
        MeshRadio::connect(target).await.map(Radio::Mesh)
    }

    /// Lists nearby BLE device names after scanning for `secs` seconds.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Radio`] when the Bluetooth adapter fails.
    pub async fn scan_ble(secs: u64) -> Result<Vec<String>> {
        MeshRadio::scan_ble(Duration::from_secs(secs)).await
    }

    /// Lists serial port names without opening any of them.
    pub fn serial_ports() -> Vec<String> {
        tokio_serial::available_ports()
            .map(|ports| ports.into_iter().map(|p| p.port_name).collect())
            .unwrap_or_default()
    }

    /// Subscribes to pushes from the radio.
    pub fn subscribe(&self) -> broadcast::Receiver<RadioEvent> {
        match self {
            Radio::Mesh(r) => r.subscribe(),
            Radio::Fake(r) => r.subscribe(),
        }
    }

    /// Starts the app session and reads the device report and battery.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Radio`] when any of the three queries fails.
    pub async fn start(&self) -> Result<DeviceInfo> {
        match self {
            Radio::Mesh(r) => r.start().await,
            Radio::Fake(r) => r.start().await,
        }
    }

    /// Reads the battery voltage in millivolts.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Radio`] when the query fails.
    pub async fn battery_mv(&self) -> Result<u16> {
        match self {
            Radio::Mesh(r) => r.battery_mv().await,
            Radio::Fake(r) => r.battery_mv(),
        }
    }

    /// Reads slots `0..max` and returns the ones with a name.
    ///
    /// # Errors
    ///
    /// Returns the first slot read error; never a partial list.
    pub async fn channels(&self, max: u8) -> Result<Vec<Channel>> {
        match self {
            Radio::Mesh(r) => r.channels(max).await,
            Radio::Fake(r) => r.channels(max),
        }
    }

    /// Writes a channel into its slot.
    ///
    /// # Errors
    ///
    /// [`Error::Rejected`] when the device refuses, [`Error::Radio`] when
    /// the outcome is unknown.
    pub async fn set_channel(&self, ch: &Channel) -> Result<()> {
        match self {
            Radio::Mesh(r) => r.set_channel(ch).await,
            Radio::Fake(r) => r.set_channel(ch),
        }
    }

    /// Empties a channel slot (name `""`, zero secret).
    ///
    /// # Errors
    ///
    /// Same as [`Radio::set_channel`].
    pub async fn clear_channel(&self, idx: u8) -> Result<()> {
        match self {
            Radio::Mesh(r) => r.clear_channel(idx).await,
            Radio::Fake(r) => r.clear_channel(idx),
        }
    }

    /// Reads contacts modified after `since` (everything when `0`).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Radio`] on timeout (30 s) or transport failure.
    pub async fn contacts(&self, since: u32) -> Result<Vec<Contact>> {
        match self {
            Radio::Mesh(r) => r.contacts(since).await,
            Radio::Fake(r) => r.contacts(since),
        }
    }

    /// Deletes a contact from the device.
    ///
    /// # Errors
    ///
    /// Same as [`Radio::set_channel`].
    pub async fn remove_contact(&self, public_key: &str) -> Result<()> {
        match self {
            Radio::Mesh(r) => r.remove_contact(public_key).await,
            Radio::Fake(r) => r.remove_contact(public_key),
        }
    }

    /// Forgets the learned route to a contact, so the next send floods.
    ///
    /// # Errors
    ///
    /// Same as [`Radio::set_channel`].
    pub async fn reset_path(&self, public_key: &str) -> Result<()> {
        match self {
            Radio::Mesh(r) => r.reset_path(public_key).await,
            Radio::Fake(r) => r.reset_path(public_key),
        }
    }

    /// Imports a contact card.
    ///
    /// # Errors
    ///
    /// Same as [`Radio::set_channel`].
    pub async fn import_contact(&self, card: &[u8]) -> Result<()> {
        match self {
            Radio::Mesh(r) => r.import_contact(card).await,
            Radio::Fake(r) => r.import_contact(card),
        }
    }

    /// Exports a contact card URI; `None` exports the radio itself.
    ///
    /// # Errors
    ///
    /// Same as [`Radio::set_channel`].
    pub async fn export_contact(&self, public_key: Option<&str>) -> Result<String> {
        match self {
            Radio::Mesh(r) => r.export_contact(public_key).await,
            Radio::Fake(r) => r.export_contact(public_key),
        }
    }

    /// Sends a direct message to the contact with the 12-hex `prefix`.
    ///
    /// # Errors
    ///
    /// Same as [`Radio::set_channel`].
    pub async fn send_text(&self, prefix: &str, text: &str, ts: u32) -> Result<SentInfo> {
        match self {
            Radio::Mesh(r) => r.send_text(prefix, text, ts).await,
            Radio::Fake(r) => r.send_text(prefix, text, ts),
        }
    }

    /// Sends bare text to a channel slot.
    ///
    /// # Errors
    ///
    /// Same as [`Radio::set_channel`].
    pub async fn send_channel_text(&self, idx: u8, text: &str, ts: u32) -> Result<()> {
        match self {
            Radio::Mesh(r) => r.send_channel_text(idx, text, ts).await,
            Radio::Fake(r) => r.send_channel_text(idx, text, ts),
        }
    }

    /// Pops the next queued message, `None` when the queue is empty.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Radio`] on timeout or transport failure.
    pub async fn next_message(&self) -> Result<Option<IncomingMessage>> {
        match self {
            Radio::Mesh(r) => r.next_message().await,
            Radio::Fake(r) => r.next_message(),
        }
    }

    /// Broadcasts an advert (`flood`) or a zero-hop advert.
    ///
    /// # Errors
    ///
    /// Same as [`Radio::set_channel`].
    pub async fn send_advert(&self, flood: bool) -> Result<()> {
        match self {
            Radio::Mesh(r) => r.send_advert(flood).await,
            Radio::Fake(r) => r.send_advert(flood),
        }
    }

    /// Sets the device clock to `now` (unix seconds).
    ///
    /// # Errors
    ///
    /// Same as [`Radio::set_channel`].
    pub async fn sync_clock(&self, now: u32) -> Result<()> {
        match self {
            Radio::Mesh(r) => r.sync_clock(now).await,
            Radio::Fake(r) => r.sync_clock(now),
        }
    }

    /// Validates `s` against `current`, then sends the frames in order.
    ///
    /// # Errors
    ///
    /// [`Error::Invalid`] before anything is sent; otherwise the first
    /// frame error, as for [`Radio::set_channel`].
    pub async fn apply_settings(&self, current: &DeviceInfo, s: &DeviceSettings) -> Result<()> {
        match self {
            Radio::Mesh(r) => r.apply_settings(current, s).await,
            Radio::Fake(r) => r.apply_settings(current, s),
        }
    }

    /// Stops the event forwarder and closes the transport. Idempotent.
    pub async fn disconnect(&self) {
        match self {
            Radio::Mesh(r) => r.disconnect().await,
            Radio::Fake(r) => r.disconnect(),
        }
    }

    /// Reports whether the transport is still up.
    pub async fn is_connected(&self) -> bool {
        match self {
            Radio::Mesh(r) => r.is_connected().await,
            Radio::Fake(_) => true,
        }
    }
}
