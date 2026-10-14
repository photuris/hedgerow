//! Seeded fake radio for the GUI's UI-test mode.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::model::{Channel, Contact, ContactKind, DeviceInfo, ThreadId};
use crate::radio::{FakeRadio, IncomingMessage};
use crate::text;

/// Returns a fake radio holding a demo device, three channels, six
/// contacts, and eight queued messages, so the GUI shows unread badges.
///
/// Message timestamps are relative to the current time; the newest Public
/// message from Alice is exactly 1800 seconds old. Direct messages are
/// acknowledged 1500 ms after they are sent.
pub fn demo_radio() -> FakeRadio {
    let radio = FakeRadio::new(demo_device());
    let contacts = demo_contacts();
    let inbox = demo_inbox(&contacts, now_secs());

    {
        let mut st = radio.state();

        st.channels = demo_channels();
        st.contacts = contacts;
        st.inbox = inbox.into();
        st.auto_ack = true;
    }

    radio
}

/// The demo radio's own report.
fn demo_device() -> DeviceInfo {
    DeviceInfo {
        node_id: format!("d0{}", "11".repeat(31)),
        name: "Demo Node".into(),
        tx_power: 20,
        max_tx_power: 22,
        radio_freq: 910_525,
        radio_bw: 62_500,
        sf: 7,
        cr: 5,
        manual_add_contacts: false,
        adv_loc_policy: 0,
        multi_acks: 0,
        telemetry_base: 2,
        telemetry_loc: 0,
        telemetry_env: 0,
        lat: 0,
        lon: 0,
        firmware: Some("1.3.6".into()),
        fw_build: Some("14 Oct 2026".into()),
        model: Some("T-Demo Pager".into()),
        max_contacts: Some(255),
        max_channels: Some(20),
        battery_mv: Some(3900),
    }
}

/// Public plus two hashtag channels.
fn demo_channels() -> Vec<Channel> {
    let hashtag = |idx: u8, name: &str| Channel {
        idx,
        name: name.into(),
        secret: text::hashtag_secret(name)
            .unwrap_or_else(|_| unreachable!("demo channel names are valid hashtags")),
    };

    vec![
        Channel {
            idx: 0,
            name: "Public".into(),
            secret: text::PUBLIC_CHANNEL_SECRET,
        },
        hashtag(1, "#test"),
        hashtag(2, "#local"),
    ]
}

/// Four chat contacts, a repeater, and a room, with a mix of routes.
fn demo_contacts() -> Vec<Contact> {
    let contact = |seed: u8, name: &str, kind: ContactKind, path_len: i8| Contact {
        public_key: text::to_hex(&[seed; 32]),
        name: name.into(),
        kind,
        path_len,
        last_advert: 1_760_000_000 + u32::from(seed),
        lat: 0,
        lon: 0,
        last_modified: u32::from(seed),
    };

    vec![
        contact(0xa1, "Alice", ContactKind::Chat, 2),
        contact(0xb2, "Bob", ContactKind::Chat, 0),
        contact(0xc3, "Carol", ContactKind::Chat, -1),
        contact(0xd4, "Dave", ContactKind::Chat, 2),
        contact(0xe5, "Hilltop RPT", ContactKind::Repeater, 0),
        contact(0xf6, "Town Room", ContactKind::Room, -1),
    ]
}

/// Eight queued messages from the last two days, oldest first.
fn demo_inbox(contacts: &[Contact], now: u32) -> Vec<IncomingMessage> {
    let prefix_of = |name: &str| {
        let key = contacts
            .iter()
            .find(|c| c.name == name)
            .map_or_else(String::new, Contact::prefix);

        ThreadId::Contact(key)
    };
    let channel = |idx: u8, sender: &str, ago: u32, text: &str| IncomingMessage {
        thread: ThreadId::Channel(idx),
        sender: Some(sender.into()),
        text: text.into(),
        sender_ts: now - ago,
        path_len: Some(1),
        snr: Some(6.5),
    };
    let direct = |name: &str, ago: u32, text: &str| IncomingMessage {
        thread: prefix_of(name),
        sender: None,
        text: text.into(),
        sender_ts: now - ago,
        path_len: Some(2),
        snr: Some(4.0),
    };

    vec![
        direct("Alice", 90_000, "Hey, did you get my last message?"),
        channel(
            1,
            "Carol",
            50_000,
            "Test from the ridge, anyone hearing me?",
        ),
        channel(0, "Bob", 20_000, "Morning all, anyone on the air?"),
        channel(0, "Alice", 9_000, "Loud and clear from the south side."),
        direct("Bob", 7_000, "Can you bring the antenna on Saturday?"),
        direct("Alice", 4_000, "Lunch tomorrow?"),
        channel(1, "Dave", 3_000, "Repeater looks good from here."),
        channel(0, "Alice", 1_800, "Net starts at 8pm on 910.525."),
    ]
}

/// Current unix time in seconds.
fn now_secs() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u32::try_from(d.as_secs()).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    mod demo_radio {
        use super::*;

        #[test]
        fn should_seed_device_channels_contacts_and_eight_messages() {
            let before = now_secs();
            let radio = demo_radio();
            let after = now_secs();
            let st = radio.state();

            assert_eq!(st.device.name, "Demo Node");
            assert_eq!(st.device.node_id.len(), 64);
            assert!(st.device.node_id.starts_with("d0"));
            assert_eq!(st.channels.len(), 3);
            assert_eq!(st.contacts.len(), 6);
            assert_eq!(st.inbox.len(), 8);
            assert!(st.auto_ack);

            let on = |thread: ThreadId| st.inbox.iter().filter(|m| m.thread == thread).count();

            assert_eq!(on(ThreadId::Channel(0)), 3);
            assert_eq!(on(ThreadId::Channel(1)), 2);
            assert_eq!(
                st.inbox
                    .iter()
                    .filter(|m| matches!(m.thread, ThreadId::Contact(_)))
                    .count(),
                3
            );

            let newest_alice = st
                .inbox
                .iter()
                .filter(|m| {
                    m.thread == ThreadId::Channel(0) && m.sender.as_deref() == Some("Alice")
                })
                .map(|m| m.sender_ts)
                .max()
                .unwrap();

            assert!((before - 1800..=after - 1800).contains(&newest_alice));
        }
    }
}
