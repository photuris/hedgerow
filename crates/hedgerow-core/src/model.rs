//! Plain data types shared by the store, the radio layer, and the GUI.

use serde::{Deserialize, Serialize};

/// A conversation: a direct thread with one contact, or a channel slot.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ThreadId {
    /// Direct thread; holds the 12-hex lowercase public-key prefix.
    Contact(String),
    /// Channel slot index on the radio.
    Channel(u8),
}

impl ThreadId {
    /// Returns `"contact"` or `"channel"`.
    pub fn kind(&self) -> &'static str {
        match self {
            ThreadId::Contact(_) => "contact",
            ThreadId::Channel(_) => "channel",
        }
    }

    /// Returns the key prefix, or the channel index as a decimal string.
    pub fn key(&self) -> String {
        match self {
            ThreadId::Contact(prefix) => prefix.clone(),
            ThreadId::Channel(idx) => idx.to_string(),
        }
    }

    /// Rebuilds a thread from the output of [`ThreadId::kind`] and
    /// [`ThreadId::key`]; `None` for an unknown kind or a bad channel index.
    pub fn from_parts(kind: &str, key: &str) -> Option<ThreadId> {
        match kind {
            "contact" => Some(ThreadId::Contact(key.to_string())),
            "channel" => key.parse().ok().map(ThreadId::Channel),
            _ => None,
        }
    }
}

/// Role of a contact on the mesh.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContactKind {
    /// A chat client (type 1).
    Chat,
    /// A repeater (type 2).
    Repeater,
    /// A room server (type 3).
    Room,
    /// A sensor (type 4).
    Sensor,
    /// Any other type code the firmware reports.
    Other(u8),
}

impl ContactKind {
    /// Maps the firmware type code to a kind.
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => ContactKind::Chat,
            2 => ContactKind::Repeater,
            3 => ContactKind::Room,
            4 => ContactKind::Sensor,
            other => ContactKind::Other(other),
        }
    }

    /// Returns the firmware type code.
    pub fn as_u8(&self) -> u8 {
        match self {
            ContactKind::Chat => 1,
            ContactKind::Repeater => 2,
            ContactKind::Room => 3,
            ContactKind::Sensor => 4,
            ContactKind::Other(v) => *v,
        }
    }

    /// Returns a short display label.
    pub fn label(&self) -> &'static str {
        match self {
            ContactKind::Chat => "Chat",
            ContactKind::Repeater => "Repeater",
            ContactKind::Room => "Room",
            ContactKind::Sensor => "Sensor",
            ContactKind::Other(_) => "Other",
        }
    }
}

/// A contact stored on the radio.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Contact {
    /// 64-hex lowercase public key.
    pub public_key: String,
    /// Advertised name.
    pub name: String,
    /// Role on the mesh.
    pub kind: ContactKind,
    /// Hops to the contact; -1 means flood (no known route).
    pub path_len: i8,
    /// Unix seconds of the last advert heard.
    pub last_advert: u32,
    /// Latitude in micro-degrees.
    pub lat: i32,
    /// Longitude in micro-degrees.
    pub lon: i32,
    /// Unix seconds of the last change on the radio.
    pub last_modified: u32,
}

impl Contact {
    /// Returns the first 12 hex characters of the public key.
    pub fn prefix(&self) -> String {
        self.public_key.chars().take(12).collect()
    }
}

/// A channel slot on the radio.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Channel {
    /// Slot index.
    pub idx: u8,
    /// Channel name; empty means the slot is unused.
    pub name: String,
    /// 16-byte shared secret.
    pub secret: [u8; 16],
}

/// Whether a message arrived or was sent from this node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Direction {
    /// Received from the mesh.
    In,
    /// Sent by this node.
    Out,
}

/// Delivery state of a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DeliveryStatus {
    /// An incoming message.
    Received,
    /// Outgoing, not yet accepted by the radio.
    Pending,
    /// Outgoing, accepted by the radio.
    Sent,
    /// Outgoing direct message acknowledged by the recipient.
    Delivered,
    /// Outgoing and given up on.
    Failed,
}

impl DeliveryStatus {
    /// Returns the lowercase name stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            DeliveryStatus::Received => "received",
            DeliveryStatus::Pending => "pending",
            DeliveryStatus::Sent => "sent",
            DeliveryStatus::Delivered => "delivered",
            DeliveryStatus::Failed => "failed",
        }
    }

    /// Parses the output of [`DeliveryStatus::as_str`].
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "received" => Some(DeliveryStatus::Received),
            "pending" => Some(DeliveryStatus::Pending),
            "sent" => Some(DeliveryStatus::Sent),
            "delivered" => Some(DeliveryStatus::Delivered),
            "failed" => Some(DeliveryStatus::Failed),
            _ => None,
        }
    }
}

/// A message about to be stored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewMessage {
    /// Conversation the message belongs to.
    pub thread: ThreadId,
    /// Incoming or outgoing.
    pub direction: Direction,
    /// Channel messages: parsed sender (in) or our name (out); DMs: `None`.
    pub sender: Option<String>,
    /// Message body.
    pub text: String,
    /// Sender clock, unix seconds.
    pub sender_ts: u32,
    /// Local clock, unix seconds.
    pub received_at: i64,
    /// Hops the message travelled, when known.
    pub path_len: Option<u8>,
    /// Signal-to-noise ratio in dB, when known.
    pub snr: Option<f32>,
    /// Delivery state.
    pub status: DeliveryStatus,
}

/// A stored message: a [`NewMessage`] plus its row id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// Row id, increasing in insertion order.
    pub id: i64,
    /// Conversation the message belongs to.
    pub thread: ThreadId,
    /// Incoming or outgoing.
    pub direction: Direction,
    /// Channel messages: parsed sender (in) or our name (out); DMs: `None`.
    pub sender: Option<String>,
    /// Message body.
    pub text: String,
    /// Sender clock, unix seconds.
    pub sender_ts: u32,
    /// Local clock, unix seconds.
    pub received_at: i64,
    /// Hops the message travelled, when known.
    pub path_len: Option<u8>,
    /// Signal-to-noise ratio in dB, when known.
    pub snr: Option<f32>,
    /// Delivery state.
    pub status: DeliveryStatus,
}

/// One row of the conversation list.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ThreadSummary {
    /// The conversation.
    pub thread: ThreadId,
    /// Channel name, contact name, or the raw key when the contact is unknown.
    pub title: String,
    /// Body of the newest message.
    pub last_text: Option<String>,
    /// `received_at` of the newest message.
    pub last_at: Option<i64>,
    /// Incoming messages after the read marker.
    pub unread: u32,
}

/// What the radio reports about itself.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// 64-hex lowercase public key of the radio.
    pub node_id: String,
    /// Device name.
    pub name: String,
    /// Transmit power in dBm.
    pub tx_power: u8,
    /// Highest transmit power the hardware allows.
    pub max_tx_power: u8,
    /// Radio frequency in MHz x 1000.
    pub radio_freq: u32,
    /// Radio bandwidth in kHz x 1000.
    pub radio_bw: u32,
    /// LoRa spreading factor.
    pub sf: u8,
    /// LoRa coding rate.
    pub cr: u8,
    /// Whether contacts are added only by hand.
    pub manual_add_contacts: bool,
    /// Advert location policy code.
    pub adv_loc_policy: u8,
    /// Extra acknowledgement count.
    pub multi_acks: u8,
    /// Base telemetry mode (2 bits).
    pub telemetry_base: u8,
    /// Location telemetry mode (2 bits).
    pub telemetry_loc: u8,
    /// Environment telemetry mode (2 bits).
    pub telemetry_env: u8,
    /// Latitude in micro-degrees.
    pub lat: i32,
    /// Longitude in micro-degrees.
    pub lon: i32,
    /// Firmware version string.
    pub firmware: Option<String>,
    /// Firmware build date.
    pub fw_build: Option<String>,
    /// Hardware model.
    pub model: Option<String>,
    /// Contact capacity as reported (saturates at 255 in meshcore-rs 0.2.0).
    pub max_contacts: Option<u32>,
    /// Channel slot count.
    pub max_channels: Option<u8>,
    /// Battery voltage in millivolts.
    pub battery_mv: Option<u16>,
}

#[cfg(test)]
mod tests {
    use super::*;

    mod contact_kind {
        use super::*;

        #[test]
        fn should_round_trip_known_codes_and_other() {
            for v in [1, 2, 3, 4, 0, 9, 255] {
                assert_eq!(ContactKind::from_u8(v).as_u8(), v);
            }

            assert_eq!(ContactKind::from_u8(2), ContactKind::Repeater);
            assert_eq!(ContactKind::from_u8(9), ContactKind::Other(9));
        }
    }

    mod delivery_status {
        use super::*;

        #[test]
        fn should_round_trip_through_as_str_and_parse() {
            for s in [
                DeliveryStatus::Received,
                DeliveryStatus::Pending,
                DeliveryStatus::Sent,
                DeliveryStatus::Delivered,
                DeliveryStatus::Failed,
            ] {
                assert_eq!(DeliveryStatus::parse(s.as_str()), Some(s));
            }

            assert_eq!(DeliveryStatus::parse("bogus"), None);
        }
    }
}
