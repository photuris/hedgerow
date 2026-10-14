//! In-memory radio for tests and the GUI's UI-test mode.
//!
//! Every [`FakeRadio`] method records a call string first, so tests assert
//! on what the session asked the radio to do.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::broadcast;

use super::{DeviceSettings, IncomingMessage, RadioEvent, SentInfo};
use crate::error::{Error, Result};
use crate::model::{Channel, Contact, ContactKind, DeviceInfo};
use crate::text;

/// Capacity of the fake's event channel, matching the real radio's.
const EVENT_CAPACITY: usize = 256;

/// How long after a send the fake delivers an ack when `auto_ack` is on.
const AUTO_ACK_DELAY: Duration = Duration::from_millis(1500);

/// Battery reading when the seeded device has none.
const DEFAULT_BATTERY_MV: u16 = 4000;

/// How a [`FakeState::fail_call`] failure surfaces.
#[derive(Debug, Clone)]
pub enum FakeFailure {
    /// Transport or timeout failure: [`Error::Radio`].
    Radio(String),
    /// The device refused: [`Error::Rejected`].
    Rejected(String),
}

/// A message the fake accepted for sending.
#[derive(Debug, Clone, PartialEq)]
pub enum FakeSent {
    /// A direct message.
    Text {
        /// 12-hex key prefix of the recipient.
        prefix: String,
        /// Message text.
        text: String,
        /// Timestamp the session sent with it.
        ts: u32,
        /// Ack tag the fake handed out.
        ack: [u8; 4],
    },
    /// A channel message.
    Channel {
        /// Channel slot.
        idx: u8,
        /// Message text.
        text: String,
        /// Timestamp the session sent with it.
        ts: u32,
    },
}

/// Everything a test can read or steer on the fake.
pub struct FakeState {
    /// What `start` reports; updated by `apply_settings`.
    pub device: DeviceInfo,
    /// Channel slots held by the fake radio.
    pub channels: Vec<Channel>,
    /// Contacts held by the fake radio.
    pub contacts: Vec<Contact>,
    /// Messages waiting for `next_message`.
    pub inbox: VecDeque<IncomingMessage>,
    /// Messages the session sent, oldest first.
    pub sent: Vec<FakeSent>,
    /// Settings frames `apply_settings` produced.
    pub frames: Vec<Vec<u8>>,
    /// Call records, oldest first.
    pub calls: Vec<String>,
    /// Fails the next call (any method) with [`Error::Radio`].
    pub fail_next: Option<String>,
    /// Fails the next call whose record starts with the prefix.
    pub fail_call: Option<(String, FakeFailure)>,
    /// Panics right after recording the next call whose record starts
    /// with the prefix. One-shot: taken when it fires.
    pub panic_after: Option<String>,
    /// Makes the session's connect step fail.
    pub fail_connect: bool,
    /// How long `start` sleeps (tokio time) before it records the call
    /// and answers.
    pub start_delay_ms: u64,
    /// The `timeout_ms` that `send_text` reports.
    pub ack_timeout_ms: u32,
    /// Delivers an ack 1500 ms after every direct message.
    pub auto_ack: bool,
    /// Ack tag handed to the next direct message.
    pub next_ack: u32,
}

/// The in-memory radio. Clones share one state and one event channel.
#[derive(Clone)]
pub struct FakeRadio {
    state: Arc<Mutex<FakeState>>,
    events: broadcast::Sender<RadioEvent>,
}

impl FakeRadio {
    /// Creates a fake that reports `device` and holds nothing else.
    pub fn new(device: DeviceInfo) -> FakeRadio {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let state = FakeState {
            device,
            channels: Vec::new(),
            contacts: Vec::new(),
            inbox: VecDeque::new(),
            sent: Vec::new(),
            frames: Vec::new(),
            calls: Vec::new(),
            fail_next: None,
            fail_call: None,
            panic_after: None,
            fail_connect: false,
            start_delay_ms: 0,
            ack_timeout_ms: 3000,
            auto_ack: false,
            next_ack: 1,
        };

        FakeRadio {
            state: Arc::new(Mutex::new(state)),
            events,
        }
    }

    /// Locks the shared state. Not recorded as a call.
    pub fn state(&self) -> MutexGuard<'_, FakeState> {
        // A panic in another test thread must not hide this test's failure.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues a message and announces it with `MessagesWaiting`.
    pub fn push_incoming(&self, m: IncomingMessage) {
        self.state().inbox.push_back(m);
        self.emit(RadioEvent::MessagesWaiting);
    }

    /// Publishes an event as if the radio had pushed it.
    pub fn emit(&self, e: RadioEvent) {
        // No receiver just means nobody is connected yet.
        let _ = self.events.send(e);
    }

    /// Subscribes to events. Not recorded as a call.
    pub(super) fn subscribe(&self) -> broadcast::Receiver<RadioEvent> {
        self.events.subscribe()
    }

    /// Records `call`, panics if `panic_after` matches, then applies
    /// `fail_call` and `fail_next`.
    ///
    /// Returns the locked state when no failure is armed for this call.
    fn enter(&self, call: String) -> Result<MutexGuard<'_, FakeState>> {
        let mut st = self.state();

        st.calls.push(call.clone());

        if st
            .panic_after
            .as_ref()
            .is_some_and(|prefix| call.starts_with(prefix.as_str()))
        {
            st.panic_after = None;
            drop(st);

            panic!("fake radio panic after {call}");
        }

        if st
            .fail_call
            .as_ref()
            .is_some_and(|(prefix, _)| call.starts_with(prefix.as_str()))
            && let Some((_, failure)) = st.fail_call.take()
        {
            return Err(match failure {
                FakeFailure::Radio(m) => Error::Radio(m),
                FakeFailure::Rejected(m) => Error::Rejected(m),
            });
        }

        if let Some(msg) = st.fail_next.take() {
            return Err(Error::Radio(msg));
        }

        Ok(st)
    }

    /// Returns the seeded device report after `start_delay_ms`.
    pub(super) async fn start(&self) -> Result<DeviceInfo> {
        let delay = self.state().start_delay_ms;

        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }

        Ok(self.enter("start".into())?.device.clone())
    }

    /// Returns the device's battery reading.
    pub(super) fn battery_mv(&self) -> Result<u16> {
        let st = self.enter("battery".into())?;

        Ok(st.device.battery_mv.unwrap_or(DEFAULT_BATTERY_MV))
    }

    /// Returns the slots below `max`, sorted by index.
    pub(super) fn channels(&self, max: u8) -> Result<Vec<Channel>> {
        let st = self.enter(format!("channels:{max}"))?;
        let mut found: Vec<Channel> = st
            .channels
            .iter()
            .filter(|c| c.idx < max)
            .cloned()
            .collect();

        found.sort_by_key(|c| c.idx);

        Ok(found)
    }

    /// Replaces or inserts the channel at its index.
    pub(super) fn set_channel(&self, ch: &Channel) -> Result<()> {
        let mut st = self.enter(format!("set_channel:{}", ch.idx))?;

        match st.channels.iter_mut().find(|c| c.idx == ch.idx) {
            Some(slot) => *slot = ch.clone(),
            None => st.channels.push(ch.clone()),
        }

        Ok(())
    }

    /// Removes the channel at `idx`.
    pub(super) fn clear_channel(&self, idx: u8) -> Result<()> {
        let mut st = self.enter(format!("clear_channel:{idx}"))?;

        st.channels.retain(|c| c.idx != idx);

        Ok(())
    }

    /// Returns every contact, or those modified after `since`.
    pub(super) fn contacts(&self, since: u32) -> Result<Vec<Contact>> {
        let st = self.enter(format!("contacts:{since}"))?;

        Ok(st
            .contacts
            .iter()
            .filter(|c| since == 0 || c.last_modified > since)
            .cloned()
            .collect())
    }

    /// Removes a contact by key.
    pub(super) fn remove_contact(&self, public_key: &str) -> Result<()> {
        let mut st = self.enter(format!("remove_contact:{public_key}"))?;

        st.contacts.retain(|c| c.public_key != public_key);

        Ok(())
    }

    /// Marks the contact's route as unknown (flood).
    pub(super) fn reset_path(&self, public_key: &str) -> Result<()> {
        let mut st = self.enter(format!("reset_path:{public_key}"))?;

        for c in st
            .contacts
            .iter_mut()
            .filter(|c| c.public_key == public_key)
        {
            c.path_len = -1;
        }

        Ok(())
    }

    /// Upserts a contact named `Imported` keyed by the card's first 32 bytes.
    pub(super) fn import_contact(&self, card: &[u8]) -> Result<()> {
        let mut st = self.enter("import_contact".into())?;

        let Some(key_bytes) = card.get(..32) else {
            return Err(Error::Invalid(format!(
                "contact card is {} bytes, need at least 32",
                card.len()
            )));
        };

        let public_key = text::to_hex(key_bytes);
        let last_modified = st
            .contacts
            .iter()
            .map(|c| c.last_modified)
            .max()
            .unwrap_or(0)
            + 1;
        let imported = Contact {
            public_key: public_key.clone(),
            name: "Imported".into(),
            kind: ContactKind::Chat,
            path_len: -1,
            last_advert: 0,
            lat: 0,
            lon: 0,
            last_modified,
        };

        match st.contacts.iter_mut().find(|c| c.public_key == public_key) {
            Some(existing) => *existing = imported,
            None => st.contacts.push(imported),
        }

        Ok(())
    }

    /// Returns `meshcore://<key>` for the radio itself or a known contact.
    pub(super) fn export_contact(&self, public_key: Option<&str>) -> Result<String> {
        let st = self.enter("export_contact".into())?;

        let Some(key) = public_key else {
            return Ok(format!("meshcore://{}", st.device.node_id));
        };

        if !st.contacts.iter().any(|c| c.public_key == key) {
            return Err(Error::Invalid(format!("unknown contact {key}")));
        }

        Ok(format!("meshcore://{key}"))
    }

    /// Records a direct message and hands out the next ack tag.
    pub(super) fn send_text(&self, prefix: &str, text: &str, ts: u32) -> Result<SentInfo> {
        let mut st = self.enter(format!("send_text:{prefix}"))?;
        let ack = st.next_ack.to_le_bytes();

        st.next_ack += 1;
        st.sent.push(FakeSent::Text {
            prefix: prefix.to_string(),
            text: text.to_string(),
            ts,
            ack,
        });

        if st.auto_ack {
            let events = self.events.clone();

            tokio::spawn(async move {
                tokio::time::sleep(AUTO_ACK_DELAY).await;
                // No receiver just means the session already disconnected.
                let _ = events.send(RadioEvent::Ack(ack));
            });
        }

        Ok(SentInfo {
            ack,
            timeout_ms: st.ack_timeout_ms,
        })
    }

    /// Records a channel message.
    pub(super) fn send_channel_text(&self, idx: u8, text: &str, ts: u32) -> Result<()> {
        let mut st = self.enter(format!("send_channel_text:{idx}"))?;

        st.sent.push(FakeSent::Channel {
            idx,
            text: text.to_string(),
            ts,
        });

        Ok(())
    }

    /// Pops the oldest queued message.
    pub(super) fn next_message(&self) -> Result<Option<IncomingMessage>> {
        Ok(self.enter("next_message".into())?.inbox.pop_front())
    }

    /// Records an advert request.
    pub(super) fn send_advert(&self, flood: bool) -> Result<()> {
        self.enter(format!("send_advert:{flood}")).map(drop)
    }

    /// Records a clock sync.
    pub(super) fn sync_clock(&self, _now: u32) -> Result<()> {
        self.enter("sync_clock".into()).map(drop)
    }

    /// Validates `s`, stores its frames, and writes its values into `device`.
    pub(super) fn apply_settings(&self, current: &DeviceInfo, s: &DeviceSettings) -> Result<()> {
        let mut st = self.enter("apply_settings".into())?;

        s.validate(current)?;
        st.frames.extend(s.frames(current));

        let d = &mut st.device;

        d.name = s.name.trim().to_string();
        d.tx_power = s.tx_power;
        d.radio_freq = s.radio_freq;
        d.radio_bw = s.radio_bw;
        d.sf = s.sf;
        d.cr = s.cr;
        d.manual_add_contacts = s.manual_add_contacts;
        d.adv_loc_policy = s.adv_loc_policy;
        d.multi_acks = s.multi_acks;

        Ok(())
    }

    /// Records the disconnect. Never fails and never consumes a failure.
    pub(super) fn disconnect(&self) {
        self.state().calls.push("disconnect".into());
    }
}
