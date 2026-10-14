//! Overseer-owned black-box probe for task 001. Do not edit.
#![allow(clippy::unwrap_used)]

use retkerno_core::Error;
use retkerno_core::config::{Config, Transport};
use retkerno_core::model::{
    Channel, Contact, ContactKind, DeliveryStatus, DeviceInfo, Direction, NewMessage, ThreadId,
};
use retkerno_core::store::Store;
use retkerno_core::text;

const NODE: &str = "aa00000000000000000000000000000000000000000000000000000000000000";
const OTHER: &str = "bb00000000000000000000000000000000000000000000000000000000000000";

fn incoming(thread: ThreadId, sender: Option<&str>, body: &str, ts: u32) -> NewMessage {
    NewMessage {
        thread,
        direction: Direction::In,
        sender: sender.map(str::to_string),
        text: body.to_string(),
        sender_ts: ts,
        received_at: i64::from(ts) + 5,
        path_len: Some(1),
        snr: Some(7.25),
        status: DeliveryStatus::Received,
    }
}

fn outgoing(thread: ThreadId, body: &str, ts: u32) -> NewMessage {
    NewMessage {
        thread,
        direction: Direction::Out,
        sender: None,
        text: body.to_string(),
        sender_ts: ts,
        received_at: i64::from(ts),
        path_len: None,
        snr: None,
        status: DeliveryStatus::Pending,
    }
}

fn contact(key_start: &str, name: &str, modified: u32) -> Contact {
    let mut public_key = key_start.to_string();
    while public_key.len() < 64 {
        public_key.push('0');
    }
    Contact {
        public_key,
        name: name.to_string(),
        kind: ContactKind::Chat,
        path_len: -1,
        last_advert: 100,
        lat: 0,
        lon: 0,
        last_modified: modified,
    }
}

fn device(node: &str) -> DeviceInfo {
    DeviceInfo {
        node_id: node.to_string(),
        name: "Probe".to_string(),
        tx_power: 20,
        max_tx_power: 22,
        radio_freq: 910_525,
        radio_bw: 62_500,
        sf: 7,
        cr: 5,
        manual_add_contacts: false,
        adv_loc_policy: 0,
        multi_acks: 0,
        telemetry_base: 0,
        telemetry_loc: 0,
        telemetry_env: 0,
        lat: 0,
        lon: 0,
        firmware: Some("1.3.6".to_string()),
        fw_build: None,
        model: None,
        max_contacts: Some(100),
        max_channels: Some(20),
        battery_mv: Some(4126),
    }
}

fn unread_of(store: &Store, node: &str, thread: &ThreadId) -> u32 {
    store
        .threads(node)
        .unwrap()
        .into_iter()
        .find(|t| &t.thread == thread)
        .map(|t| t.unread)
        .unwrap_or(u32::MAX)
}

#[test]
fn probe_text_secrets() {
    assert_eq!(text::to_hex(&text::PUBLIC_CHANNEL_SECRET), "8b3387e9c5cdea6ac9e5edbaa115cd72");
    assert_eq!(text::to_hex(&text::hashtag_secret("#test").unwrap()), "9cd8fcf22a47333b591d96a2b848b73f");
    assert_eq!(
        text::to_hex(&text::hashtag_secret("#tn-middle").unwrap()),
        "37e58905dfe839bce5554d6048e52653"
    );
    assert!(text::hashtag_secret("test").is_err());
    assert!(text::hashtag_secret("#").is_err());
    assert_eq!(
        text::parse_secret(" 9CD8FCF22A47333B591D96A2B848B73F ").unwrap(),
        text::hashtag_secret("#test").unwrap()
    );
    assert!(text::parse_secret("9cd8").is_err());
}

#[test]
fn probe_text_parsing() {
    assert_eq!(
        text::split_channel_text("Alice: hi: there"),
        (Some("Alice".to_string()), "hi: there".to_string())
    );
    assert_eq!(text::split_channel_text("no colon"), (None, "no colon".to_string()));
    assert_eq!(text::split_channel_text(": empty"), (None, ": empty".to_string()));
    let long = format!("{}: x", "n".repeat(33));
    assert_eq!(text::split_channel_text(&long), (None, long.clone()));
    assert_eq!(text::max_text_bytes(&ThreadId::Contact("0a0b0c0d0e0f".into()), "LAGUARDO_LILYGO"), 150);
    assert_eq!(text::max_text_bytes(&ThreadId::Channel(1), "LAGUARDO_LILYGO"), 133);
    assert_eq!(text::max_text_bytes(&ThreadId::Channel(1), &"x".repeat(200)), 0);
    assert_eq!(text::parse_contact_uri("meshcore://0A0b").unwrap(), vec![0x0a, 0x0b]);
    assert_eq!(text::parse_contact_uri("MESHCORE://0a").unwrap(), vec![0x0a]);
    assert_eq!(text::parse_contact_uri(" 0a0b \n").unwrap(), vec![0x0a, 0x0b]);
    assert!(text::parse_contact_uri("meshcore://0").is_err());
    assert!(text::parse_contact_uri("meshcore://zz").is_err());
}

#[test]
fn probe_thread_id_parts() {
    let c = ThreadId::Contact("0a0b0c0d0e0f".into());
    assert_eq!((c.kind(), c.key()), ("contact", "0a0b0c0d0e0f".to_string()));
    assert_eq!(ThreadId::Channel(7).key(), "7");
    assert_eq!(ThreadId::from_parts("channel", "7"), Some(ThreadId::Channel(7)));
    assert_eq!(ThreadId::from_parts("contact", "0a0b0c0d0e0f"), Some(c));
    assert_eq!(ThreadId::from_parts("bogus", "1"), None);
}

#[test]
fn probe_config_defaults_and_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("config.toml");
    let cfg = Config::load_at(&path).unwrap();
    assert_eq!(cfg.connection.transport, Transport::Ble);
    assert_eq!(cfg.connection.baud, 115_200);
    assert_eq!(cfg.connection.tcp_port, 5000);
    assert!(cfg.connection.auto_connect);
    assert_eq!(cfg.ui.decorations, "auto");
    let mut changed = cfg.clone();
    changed.connection.transport = Transport::Serial;
    changed.connection.serial_port = "/dev/ttyACM0".into();
    changed.save_at(&path).unwrap();
    assert_eq!(Config::load_at(&path).unwrap(), changed);
    std::fs::write(&path, "[connection]\nble_device = \"MeshCore-X\"\ntransport = \"tcp\"\n").unwrap();
    let partial = Config::load_at(&path).unwrap();
    assert_eq!(partial.connection.ble_device, "MeshCore-X");
    assert_eq!(partial.connection.transport, Transport::Tcp);
    assert_eq!(partial.connection.baud, 115_200);
}

#[test]
fn probe_store_dedup_and_order() {
    let s = Store::open_memory().unwrap();
    let ch = ThreadId::Channel(1);
    let first = s.insert_message(NODE, &incoming(ch.clone(), Some("Al"), "one", 10)).unwrap();
    assert!(first.is_some());
    assert_eq!(s.insert_message(NODE, &incoming(ch.clone(), Some("Al"), "one", 10)).unwrap(), None);
    // No sender on both rows: still a duplicate (NULL sender must not defeat dedup).
    let dm = ThreadId::Contact("0a0b0c0d0e0f".into());
    assert!(s.insert_message(NODE, &incoming(dm.clone(), None, "dm", 11)).unwrap().is_some());
    assert_eq!(s.insert_message(NODE, &incoming(dm.clone(), None, "dm", 11)).unwrap(), None);
    // Same text, different timestamp: distinct.
    assert!(s.insert_message(NODE, &incoming(ch.clone(), Some("Al"), "one", 12)).unwrap().is_some());
    for i in 0..5 {
        s.insert_message(NODE, &incoming(ch.clone(), Some("Bo"), &format!("m{i}"), 100 + i)).unwrap();
    }
    let last3: Vec<String> = s.messages(NODE, &ch, 3).unwrap().into_iter().map(|m| m.text).collect();
    assert_eq!(last3, vec!["m2", "m3", "m4"]);
    assert_eq!(s.message(OTHER, first.unwrap()).unwrap(), None);
    let got = s.message(NODE, first.unwrap()).unwrap().unwrap();
    assert_eq!(got.sender.as_deref(), Some("Al"));
    assert_eq!(got.snr, Some(7.25));
    assert_eq!(got.status, DeliveryStatus::Received);
    assert!(s.messages(OTHER, &ch, 50).unwrap().is_empty());
}

#[test]
fn probe_store_threads_and_unread() {
    let s = Store::open_memory().unwrap();
    s.replace_channels(
        NODE,
        &[
            Channel { idx: 2, name: "#b".into(), secret: [2; 16] },
            Channel { idx: 0, name: "Public".into(), secret: text::PUBLIC_CHANNEL_SECRET },
        ],
    )
    .unwrap();
    s.replace_contacts(NODE, &[contact("0a0b0c0d0e0f", "Zed", 5)]).unwrap();
    let known = ThreadId::Contact("0a0b0c0d0e0f".into());
    let unknown = ThreadId::Contact("ffeeddccbbaa".into());
    s.insert_message(NODE, &incoming(unknown.clone(), None, "early", 1)).unwrap();
    s.insert_message(NODE, &incoming(known.clone(), None, "late", 50)).unwrap();
    let threads = s.threads(NODE).unwrap();
    let ids: Vec<ThreadId> = threads.iter().map(|t| t.thread.clone()).collect();
    assert_eq!(ids, vec![ThreadId::Channel(0), ThreadId::Channel(2), known.clone(), unknown.clone()]);
    assert_eq!(threads[0].title, "Public");
    assert_eq!(threads[2].title, "Zed");
    assert_eq!(threads[3].title, "ffeeddccbbaa");
    assert_eq!(threads[2].last_text.as_deref(), Some("late"));
    assert_eq!(threads[2].last_at, Some(55));
    assert_eq!(threads[0].unread, 0);
    assert_eq!(threads[0].last_text, None);

    let ch = ThreadId::Channel(0);
    s.insert_message(NODE, &incoming(ch.clone(), Some("A"), "a", 60)).unwrap();
    s.insert_message(NODE, &incoming(ch.clone(), Some("B"), "b", 61)).unwrap();
    s.insert_message(NODE, &outgoing(ch.clone(), "mine", 62)).unwrap();
    assert_eq!(unread_of(&s, NODE, &ch), 2);
    assert_eq!(s.unread_total(NODE).unwrap(), 4);
    s.mark_read(NODE, &ch).unwrap();
    assert_eq!(unread_of(&s, NODE, &ch), 0);
    s.mark_unread(NODE, &ch).unwrap();
    assert_eq!(unread_of(&s, NODE, &ch), 1);
    // A thread with only outgoing messages has nothing to mark unread.
    let out_only = ThreadId::Channel(2);
    s.insert_message(NODE, &outgoing(out_only.clone(), "solo", 70)).unwrap();
    s.mark_unread(NODE, &out_only).unwrap();
    assert_eq!(unread_of(&s, NODE, &out_only), 0);
    s.mark_all_read(NODE).unwrap();
    assert_eq!(s.unread_total(NODE).unwrap(), 0);
    // New arrival after mark_read is unread again.
    s.insert_message(NODE, &incoming(ch.clone(), Some("C"), "c", 80)).unwrap();
    assert_eq!(unread_of(&s, NODE, &ch), 1);
    assert_eq!(s.unread_total(OTHER).unwrap(), 0);
    assert!(s.threads(OTHER).unwrap().is_empty());
}

#[test]
fn probe_store_status_and_ack() {
    let s = Store::open_memory().unwrap();
    let dm = ThreadId::Contact("0a0b0c0d0e0f".into());
    let a = s.insert_message(NODE, &outgoing(dm.clone(), "x", 1)).unwrap().unwrap();
    let b = s.insert_message(NODE, &outgoing(dm.clone(), "y", 2)).unwrap().unwrap();
    s.set_status(a, DeliveryStatus::Sent, Some([1, 2, 3, 4])).unwrap();
    s.set_status(b, DeliveryStatus::Sent, Some([1, 2, 3, 4])).unwrap();
    assert_eq!(s.message_by_ack(NODE, [1, 2, 3, 4]).unwrap(), Some(b));
    assert_eq!(s.message_by_ack(OTHER, [1, 2, 3, 4]).unwrap(), None);
    s.set_status(b, DeliveryStatus::Delivered, None).unwrap();
    assert_eq!(s.message(NODE, b).unwrap().unwrap().status, DeliveryStatus::Delivered);
    // None leaves the ack tag in place.
    assert_eq!(s.message_by_ack(NODE, [1, 2, 3, 4]).unwrap(), Some(b));
}

#[test]
fn probe_store_contacts_channels_device() {
    let s = Store::open_memory().unwrap();
    s.replace_contacts(NODE, &[contact("01", "bravo", 5), contact("02", "Alpha", 9), contact("03", "charlie", 7)])
        .unwrap();
    let names: Vec<String> = s.contacts(NODE).unwrap().into_iter().map(|c| c.name).collect();
    assert_eq!(names, vec!["Alpha", "bravo", "charlie"]);
    assert_eq!(s.max_contact_modified(NODE).unwrap(), 9);
    assert_eq!(s.max_contact_modified(OTHER).unwrap(), 0);
    s.upsert_contacts(NODE, &[contact("01", "Bravo2", 12), contact("04", "delta", 3)]).unwrap();
    let names: Vec<String> = s.contacts(NODE).unwrap().into_iter().map(|c| c.name).collect();
    assert_eq!(names, vec!["Alpha", "Bravo2", "charlie", "delta"]);
    s.remove_contact(NODE, &contact("03", "", 0).public_key).unwrap();
    s.replace_contacts(NODE, &[contact("05", "echo", 1)]).unwrap();
    let names: Vec<String> = s.contacts(NODE).unwrap().into_iter().map(|c| c.name).collect();
    assert_eq!(names, vec!["echo"]);
    assert!(s.contacts(OTHER).unwrap().is_empty());
    assert_eq!(s.contacts(NODE).unwrap()[0].prefix(), "050000000000");

    s.replace_channels(NODE, &[Channel { idx: 4, name: "#d".into(), secret: [4; 16] }]).unwrap();
    s.replace_channels(NODE, &[Channel { idx: 1, name: "#a".into(), secret: [1; 16] }]).unwrap();
    let chans = s.channels(NODE).unwrap();
    assert_eq!(chans, vec![Channel { idx: 1, name: "#a".into(), secret: [1; 16] }]);

    assert_eq!(s.last_node().unwrap(), None);
    s.save_device(&device(NODE)).unwrap();
    s.save_device(&device(OTHER)).unwrap();
    assert_eq!(s.last_node().unwrap().as_deref(), Some(OTHER));
    assert_eq!(s.device(NODE).unwrap(), Some(device(NODE)));
    assert_eq!(s.device("cc").unwrap(), None);
}

#[test]
fn probe_store_persists_and_guards_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("retkerno.db");
    {
        let s = Store::open(&path).unwrap();
        s.insert_message(NODE, &incoming(ThreadId::Channel(0), Some("A"), "kept", 1)).unwrap();
    }
    {
        let s = Store::open(&path).unwrap();
        assert_eq!(s.messages(NODE, &ThreadId::Channel(0), 10).unwrap().len(), 1);
    }
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute("UPDATE meta SET value = '99' WHERE key = 'schema_version'", []).unwrap();
    }
    match Store::open(&path) {
        Err(Error::SchemaTooNew { found: 99, supported: 1 }) => {}
        Err(other) => panic!("expected SchemaTooNew, got {other}"),
        Ok(_) => panic!("expected SchemaTooNew, got a store"),
    }
}

#[test]
fn probe_channel_identity_follows_occupant() {
    let s = Store::open_memory().unwrap();
    let a = Channel { idx: 1, name: "#a".into(), secret: [0xa; 16] };
    let b = Channel { idx: 1, name: "#b".into(), secret: [0xb; 16] };
    let ch = ThreadId::Channel(1);
    s.replace_channels(NODE, &[a.clone()]).unwrap();
    let for_a = s.insert_message(NODE, &incoming(ch.clone(), Some("X"), "for a", 10)).unwrap().unwrap();
    assert_eq!(s.message(NODE, for_a).unwrap().unwrap().thread, ch);
    assert_eq!(s.unread_total(NODE).unwrap(), 1);

    // A different channel in the same slot starts empty.
    s.replace_channels(NODE, &[b.clone()]).unwrap();
    assert!(s.messages(NODE, &ch, 10).unwrap().is_empty());
    assert_eq!(s.unread_total(NODE).unwrap(), 0);
    // A's row is not addressable while A is not installed.
    assert_eq!(s.message(NODE, for_a).unwrap(), None);
    let t = s.threads(NODE).unwrap();
    assert_eq!(t.len(), 1);
    assert_eq!((t[0].title.as_str(), t[0].unread, t[0].last_text.clone()), ("#b", 0, None));
    s.insert_message(NODE, &incoming(ch.clone(), Some("Y"), "for b", 11)).unwrap();

    // The old channel re-added in another slot brings its history along.
    s.replace_channels(NODE, &[b.clone(), Channel { idx: 3, ..a.clone() }]).unwrap();
    let texts = |idx: u8| -> Vec<String> {
        s.messages(NODE, &ThreadId::Channel(idx), 10).unwrap().into_iter().map(|m| m.text).collect()
    };
    assert_eq!(texts(1), vec!["for b"]);
    assert_eq!(texts(3), vec!["for a"]);
    assert_eq!(s.unread_total(NODE).unwrap(), 2);
    assert_eq!(s.message(NODE, for_a).unwrap().unwrap().thread, ThreadId::Channel(3));
    assert_eq!(s.messages(NODE, &ThreadId::Channel(3), 10).unwrap()[0].thread, ThreadId::Channel(3));

    // Read state follows the channel too, and a removed channel is not counted.
    s.mark_read(NODE, &ThreadId::Channel(3)).unwrap();
    s.replace_channels(NODE, &[b.clone()]).unwrap();
    assert_eq!(s.unread_total(NODE).unwrap(), 1);
    s.replace_channels(NODE, &[b, Channel { idx: 4, ..a }]).unwrap();
    assert_eq!(texts(4), vec!["for a"]);
    assert_eq!(s.unread_total(NODE).unwrap(), 1);
}

#[test]
fn probe_fail_unfinished() {
    let s = Store::open_memory().unwrap();
    let dm = ThreadId::Contact("0a0b0c0d0e0f".into());
    let ch = ThreadId::Channel(0);
    let pending_dm = s.insert_message(NODE, &outgoing(dm.clone(), "pending", 1)).unwrap().unwrap();
    let sent_dm = s.insert_message(NODE, &outgoing(dm.clone(), "sent", 2)).unwrap().unwrap();
    s.set_status(sent_dm, DeliveryStatus::Sent, Some([9; 4])).unwrap();
    let sent_ch = s.insert_message(NODE, &outgoing(ch.clone(), "chan", 3)).unwrap().unwrap();
    s.set_status(sent_ch, DeliveryStatus::Sent, None).unwrap();
    let pending_ch = s.insert_message(NODE, &outgoing(ch.clone(), "chan pending", 4)).unwrap().unwrap();
    let done = s.insert_message(NODE, &outgoing(dm.clone(), "done", 5)).unwrap().unwrap();
    s.set_status(done, DeliveryStatus::Delivered, None).unwrap();
    let other = s.insert_message(OTHER, &outgoing(dm.clone(), "other node", 6)).unwrap().unwrap();
    let inc = s.insert_message(NODE, &incoming(dm.clone(), None, "in", 7)).unwrap().unwrap();

    assert_eq!(s.fail_unfinished(NODE).unwrap(), 3);
    let status = |node: &str, id: i64| s.message(node, id).unwrap().unwrap().status;
    assert_eq!(status(NODE, pending_dm), DeliveryStatus::Failed);
    assert_eq!(status(NODE, sent_dm), DeliveryStatus::Failed);
    assert_eq!(status(NODE, sent_ch), DeliveryStatus::Sent);
    assert_eq!(status(NODE, pending_ch), DeliveryStatus::Failed);
    assert_eq!(status(NODE, done), DeliveryStatus::Delivered);
    assert_eq!(status(OTHER, other), DeliveryStatus::Pending);
    assert_eq!(status(NODE, inc), DeliveryStatus::Received);
    assert_eq!(s.fail_unfinished(NODE).unwrap(), 0);
}

#[test]
fn probe_channel_keys_dedup_and_mark_all() {
    let s = Store::open_memory().unwrap();
    let a = Channel { idx: 1, name: "#a".into(), secret: [0xa; 16] };
    let b = Channel { idx: 2, name: "#b".into(), secret: [0xb; 16] };
    s.replace_channels(NODE, &[a.clone(), b.clone()]).unwrap();
    // Identical payloads under different channel keys are distinct rows.
    assert!(s.insert_message(NODE, &incoming(ThreadId::Channel(1), Some("Z"), "same", 5)).unwrap().is_some());
    assert!(s.insert_message(NODE, &incoming(ThreadId::Channel(2), Some("Z"), "same", 5)).unwrap().is_some());
    // Unconfigured slots get distinct fallback keys.
    assert!(s.insert_message(NODE, &incoming(ThreadId::Channel(7), Some("Z"), "same", 5)).unwrap().is_some());
    assert!(s.insert_message(NODE, &incoming(ThreadId::Channel(8), Some("Z"), "same", 5)).unwrap().is_some());
    // The same message for A after A moves slots is still a duplicate.
    s.replace_channels(NODE, &[b.clone(), Channel { idx: 4, ..a.clone() }]).unwrap();
    assert_eq!(s.insert_message(NODE, &incoming(ThreadId::Channel(4), Some("Z"), "same", 5)).unwrap(), None);
    assert_eq!(s.unread_total(NODE).unwrap(), 2);
    // Fallback histories are not counted; mark_read on a fallback slot touches only it.
    s.mark_read(NODE, &ThreadId::Channel(7)).unwrap();
    assert_eq!(s.unread_total(NODE).unwrap(), 2);
    // mark_all_read covers hidden keys: remove A, mark all, re-add A -> nothing unread.
    s.replace_channels(NODE, &[b.clone()]).unwrap();
    assert_eq!(s.unread_total(NODE).unwrap(), 1);
    s.mark_all_read(NODE).unwrap();
    s.replace_channels(NODE, &[b, Channel { idx: 5, ..a }]).unwrap();
    assert_eq!(s.unread_total(NODE).unwrap(), 0);
    let five = s.threads(NODE).unwrap().into_iter().find(|t| t.thread == ThreadId::Channel(5)).unwrap();
    assert_eq!((five.unread, five.last_text.as_deref()), (0, Some("same")));
}
