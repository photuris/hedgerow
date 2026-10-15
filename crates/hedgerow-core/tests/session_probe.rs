//! Overseer-owned black-box probe for task 002. Do not edit.
#![allow(clippy::unwrap_used, clippy::cloned_ref_to_slice_refs)]

use std::time::Duration;

use hedgerow_core::config::{ConnectionConfig, Transport};
use hedgerow_core::model::{
    Channel, Contact, ContactKind, DeliveryStatus, DeviceInfo, Direction, ThreadId,
};
use hedgerow_core::radio::{ConnectTarget, DeviceSettings, FakeRadio, IncomingMessage, RadioEvent};
use hedgerow_core::session::{Command, ConnState, Connector, Session, Update};
use hedgerow_core::store::Store;
use hedgerow_core::text;

const NODE: &str = "aa00000000000000000000000000000000000000000000000000000000000000";
const PEER: &str = "0a0b0c0d0e0f0000000000000000000000000000000000000000000000000000";

fn device() -> DeviceInfo {
    DeviceInfo {
        node_id: NODE.to_string(),
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
        telemetry_base: 1,
        telemetry_loc: 2,
        telemetry_env: 3,
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

fn peer(modified: u32) -> Contact {
    Contact {
        public_key: PEER.to_string(),
        name: "Peer".to_string(),
        kind: ContactKind::Chat,
        path_len: 2,
        last_advert: 1,
        lat: 0,
        lon: 0,
        last_modified: modified,
    }
}

fn cfg(auto_connect: bool) -> ConnectionConfig {
    ConnectionConfig {
        auto_connect,
        ..ConnectionConfig::default()
    }
}

fn fake() -> FakeRadio {
    let f = FakeRadio::new(device());
    {
        let mut st = f.state();
        st.channels = vec![
            Channel {
                idx: 0,
                name: "Public".into(),
                secret: text::PUBLIC_CHANNEL_SECRET,
            },
            Channel {
                idx: 3,
                name: "#test".into(),
                secret: text::hashtag_secret("#test").unwrap(),
            },
        ];
        st.contacts = vec![peer(5)];
    }
    f
}

async fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    for _ in 0..12_000 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for: {what}");
}

fn connected(s: &Session) -> bool {
    matches!(s.state(), ConnState::Connected(_))
}

fn notices(rx: &mut tokio::sync::broadcast::Receiver<Update>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(u) = rx.try_recv() {
        if let Update::Notice(n) = u {
            out.push(n);
        }
    }
    out
}

#[test]
fn probe_settings_frames() {
    let d = device();
    let same = DeviceSettings::from_device(&d);
    assert!(same.frames(&d).is_empty());

    let mut s = same.clone();
    s.name = "New".into();
    s.tx_power = 10;
    s.radio_freq = 915_000;
    s.radio_bw = 250_000;
    s.sf = 10;
    s.cr = 8;
    s.manual_add_contacts = true;
    s.adv_loc_policy = 1;
    s.multi_acks = 1;
    let frames = s.frames(&d);
    let mut radio = vec![0x0B];
    radio.extend_from_slice(&915_000u32.to_le_bytes());
    radio.extend_from_slice(&250_000u32.to_le_bytes());
    radio.extend_from_slice(&[10, 8]);
    // telemetry byte keeps the device's current modes: 1 | 2<<2 | 3<<4 = 0x39
    assert_eq!(
        frames,
        vec![
            vec![0x08, b'N', b'e', b'w'],
            vec![0x0C, 10, 0, 0, 0],
            radio,
            vec![0x26, 1, 0x39, 1, 1]
        ]
    );

    let mut only_sf = same.clone();
    only_sf.sf = 9;
    let f = only_sf.frames(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0][0], 0x0B);
    assert_eq!(&f[0][1..5], &910_525u32.to_le_bytes());

    assert!(same.validate(&d).is_ok());
    let mut bad = same.clone();
    bad.tx_power = 23;
    assert!(bad.validate(&d).is_err());
    let mut bad = same.clone();
    bad.name = "x".repeat(32);
    assert!(bad.validate(&d).is_err());
    let mut bad = same.clone();
    bad.name = "   ".into();
    assert!(bad.validate(&d).is_err());
    let mut bad = same.clone();
    bad.sf = 13;
    assert!(bad.validate(&d).is_err());
    let mut bad = same.clone();
    bad.cr = 4;
    assert!(bad.validate(&d).is_err());
    let mut bad = same;
    bad.radio_bw = 60_000;
    assert!(bad.validate(&d).is_err());
}

#[test]
fn probe_connect_target() {
    let mut c = ConnectionConfig::default();
    assert!(ConnectTarget::from_config(&c).is_err());
    c.ble_device = "50:78:7D:2B:29:4D".into();
    assert_eq!(
        ConnectTarget::from_config(&c).unwrap(),
        ConnectTarget::Ble("50:78:7D:2B:29:4D".into())
    );
    c.transport = Transport::Serial;
    assert!(ConnectTarget::from_config(&c).is_err());
    c.serial_port = "/dev/ttyACM0".into();
    assert_eq!(
        ConnectTarget::from_config(&c).unwrap(),
        ConnectTarget::Serial {
            port: "/dev/ttyACM0".into(),
            baud: 115_200
        }
    );
    c.transport = Transport::Tcp;
    c.tcp_host = "radio.local".into();
    assert_eq!(
        ConnectTarget::from_config(&c).unwrap(),
        ConnectTarget::Tcp {
            host: "radio.local".into(),
            port: 5000
        }
    );
}

#[tokio::test(start_paused = true)]
async fn probe_connect_sync_and_drain() {
    let f = fake();
    f.push_incoming(IncomingMessage {
        thread: ThreadId::Channel(3),
        sender: Some("Al".into()),
        text: "queued".into(),
        sender_ts: 100,
        path_len: Some(1),
        snr: Some(5.5),
    });
    let store = Store::open_memory().unwrap();
    let s = Session::spawn(store.clone(), cfg(true), Connector::Fake(f.clone()));
    wait_for("connected", || connected(&s)).await;
    wait_for("drained", || {
        store
            .messages(NODE, &ThreadId::Channel(3), 10)
            .unwrap()
            .len()
            == 1
    })
    .await;
    assert_eq!(store.last_node().unwrap().as_deref(), Some(NODE));
    assert_eq!(store.channels(NODE).unwrap().len(), 2);
    assert_eq!(store.contacts(NODE).unwrap(), vec![peer(5)]);
    let m = &store.messages(NODE, &ThreadId::Channel(3), 10).unwrap()[0];
    assert_eq!(
        (m.direction, m.status),
        (Direction::In, DeliveryStatus::Received)
    );
    assert_eq!(m.sender.as_deref(), Some("Al"));
    let calls = f.state().calls.clone();
    assert_eq!(
        &calls[..3],
        &[
            "start".to_string(),
            "channels:20".to_string(),
            "contacts:0".to_string()
        ]
    );

    // A push while connected is drained too.
    f.push_incoming(IncomingMessage {
        thread: ThreadId::Contact(PEER[..12].to_string()),
        sender: None,
        text: "live".into(),
        sender_ts: 200,
        path_len: Some(0),
        snr: None,
    });
    wait_for("live dm stored", || {
        store
            .messages(NODE, &ThreadId::Contact(PEER[..12].to_string()), 10)
            .unwrap()
            .len()
            == 1
    })
    .await;
    assert_eq!(store.unread_total(NODE).unwrap(), 2);
}

#[tokio::test(start_paused = true)]
async fn probe_dm_ack_and_timeout() {
    let f = fake();
    f.state().ack_timeout_ms = 1000;
    let store = Store::open_memory().unwrap();
    let s = Session::spawn(store.clone(), cfg(true), Connector::Fake(f.clone()));
    wait_for("connected", || connected(&s)).await;
    let dm = ThreadId::Contact(PEER[..12].to_string());

    s.send(Command::SendText {
        thread: dm.clone(),
        text: "hello".into(),
    });
    wait_for("sent", || {
        store
            .messages(NODE, &dm, 10)
            .unwrap()
            .iter()
            .any(|m| m.status == DeliveryStatus::Sent)
    })
    .await;
    let ack = match &f.state().sent[0] {
        hedgerow_core::radio::FakeSent::Text {
            prefix, text, ack, ..
        } => {
            assert_eq!((prefix.as_str(), text.as_str()), (&PEER[..12], "hello"));
            *ack
        }
        other => panic!("expected a text send, got {other:?}"),
    };
    f.emit(RadioEvent::Ack(ack));
    wait_for("delivered", || {
        store.messages(NODE, &dm, 10).unwrap()[0].status == DeliveryStatus::Delivered
    })
    .await;

    // No ack: Failed after timeout_ms + 5 s.
    s.send(Command::SendText {
        thread: dm.clone(),
        text: "lost".into(),
    });
    wait_for("second sent", || {
        store
            .messages(NODE, &dm, 10)
            .unwrap()
            .iter()
            .any(|m| m.text == "lost" && m.status == DeliveryStatus::Sent)
    })
    .await;
    let sent_at = tokio::time::Instant::now();
    wait_for("failed", || {
        store
            .messages(NODE, &dm, 10)
            .unwrap()
            .iter()
            .any(|m| m.text == "lost" && m.status == DeliveryStatus::Failed)
    })
    .await;
    assert!(
        sent_at.elapsed() >= Duration::from_millis(5_900),
        "failed too early: {:?}",
        sent_at.elapsed()
    );

    // Retry resends the failed row.
    let lost = store
        .messages(NODE, &dm, 10)
        .unwrap()
        .into_iter()
        .find(|m| m.text == "lost")
        .unwrap();
    s.send(Command::Retry {
        message_id: lost.id,
    });
    wait_for("retried", || f.state().sent.len() == 3).await;
    wait_for("retry sent", || {
        store.message(NODE, lost.id).unwrap().unwrap().status == DeliveryStatus::Sent
    })
    .await;

    // Same text twice in the same second: both rows kept.
    s.send(Command::SendText {
        thread: dm.clone(),
        text: "dup".into(),
    });
    s.send(Command::SendText {
        thread: dm.clone(),
        text: "dup".into(),
    });
    wait_for("both dups", || {
        store
            .messages(NODE, &dm, 20)
            .unwrap()
            .iter()
            .filter(|m| m.text == "dup")
            .count()
            == 2
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn probe_channel_send_and_limits() {
    let f = fake();
    let store = Store::open_memory().unwrap();
    let s = Session::spawn(store.clone(), cfg(true), Connector::Fake(f.clone()));
    let mut rx = s.subscribe();
    wait_for("connected", || connected(&s)).await;
    let ch = ThreadId::Channel(3);
    s.send(Command::SendText {
        thread: ch.clone(),
        text: "hi all".into(),
    });
    wait_for("channel sent", || {
        store
            .messages(NODE, &ch, 10)
            .unwrap()
            .iter()
            .any(|m| m.status == DeliveryStatus::Sent)
    })
    .await;
    let m = &store.messages(NODE, &ch, 10).unwrap()[0];
    assert_eq!(
        (m.direction, m.sender.as_deref()),
        (Direction::Out, Some("Probe"))
    );

    // "Probe" is 5 bytes: limit 150 - 5 - 2 = 143.
    s.send(Command::SendText {
        thread: ch.clone(),
        text: "x".repeat(144),
    });
    s.send(Command::SendText {
        thread: ch.clone(),
        text: "   ".into(),
    });
    s.send(Command::SendText {
        thread: ch.clone(),
        text: "y".repeat(143),
    });
    wait_for("max-length sent", || {
        store.messages(NODE, &ch, 10).unwrap().len() == 2
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(store.messages(NODE, &ch, 10).unwrap().len(), 2);
    assert!(notices(&mut rx).iter().any(|n| n.contains("too long")));

    // Radio error marks the row Failed.
    f.state().fail_next = Some("boom".into());
    s.send(Command::SendText {
        thread: ch.clone(),
        text: "err".into(),
    });
    wait_for("failed row", || {
        store
            .messages(NODE, &ch, 10)
            .unwrap()
            .iter()
            .any(|m| m.text == "err" && m.status == DeliveryStatus::Failed)
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn probe_not_connected_and_reconnect() {
    let f = fake();
    f.state().fail_connect = true;
    let store = Store::open_memory().unwrap();
    let s = Session::spawn(store.clone(), cfg(true), Connector::Fake(f.clone()));
    let mut rx = s.subscribe();
    wait_for("failed", || matches!(s.state(), ConnState::Failed(_))).await;
    s.send(Command::SendText {
        thread: ThreadId::Channel(0),
        text: "nope".into(),
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        notices(&mut rx)
            .iter()
            .any(|n| n.to_lowercase().contains("not connected"))
    );
    assert!(
        store
            .messages(NODE, &ThreadId::Channel(0), 10)
            .unwrap()
            .is_empty()
    );
    f.state().fail_connect = false;
    wait_for("auto reconnect", || connected(&s)).await;

    f.emit(RadioEvent::Disconnected);
    wait_for("dropped", || !connected(&s)).await;
    wait_for("reconnected again", || connected(&s)).await;

    s.send(Command::Disconnect);
    wait_for("disconnected", || s.state() == ConnState::Disconnected).await;
    tokio::time::sleep(Duration::from_secs(120)).await;
    assert_eq!(s.state(), ConnState::Disconnected);
}

#[tokio::test(start_paused = true)]
async fn probe_no_auto_connect() {
    let f = fake();
    let store = Store::open_memory().unwrap();
    let s = Session::spawn(store.clone(), cfg(false), Connector::Fake(f.clone()));
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(s.state(), ConnState::Disconnected);
    assert!(f.state().calls.is_empty());
    s.send(Command::Connect);
    wait_for("connected on demand", || connected(&s)).await;
}

#[tokio::test(start_paused = true)]
async fn probe_contacts_refresh_coalesced() {
    let f = fake();
    let store = Store::open_memory().unwrap();
    let s = Session::spawn(store.clone(), cfg(true), Connector::Fake(f.clone()));
    wait_for("connected", || connected(&s)).await;
    let mut newer = peer(9);
    newer.public_key = format!("0b{}", &PEER[2..]);
    newer.name = "Newer".into();
    f.state().contacts.push(newer);
    f.emit(RadioEvent::ContactsChanged);
    f.emit(RadioEvent::ContactsChanged);
    f.emit(RadioEvent::ContactsChanged);
    wait_for("new contact stored", || {
        store.contacts(NODE).unwrap().len() == 2
    })
    .await;
    tokio::time::sleep(Duration::from_secs(10)).await;
    let calls = f.state().calls.clone();
    let refreshes: Vec<&String> = calls
        .iter()
        .filter(|c| c.starts_with("contacts:"))
        .collect();
    assert_eq!(refreshes, vec!["contacts:0", "contacts:5"]);
}

#[tokio::test(start_paused = true)]
async fn probe_channel_contact_and_settings_commands() {
    let f = fake();
    let store = Store::open_memory().unwrap();
    let s = Session::spawn(store.clone(), cfg(true), Connector::Fake(f.clone()));
    let mut rx = s.subscribe();
    wait_for("connected", || connected(&s)).await;

    let secret = text::hashtag_secret("#new").unwrap();
    s.send(Command::SetChannel(Channel {
        idx: 5,
        name: "#new".into(),
        secret,
    }));
    wait_for("channel stored", || {
        store.channels(NODE).unwrap().iter().any(|c| c.idx == 5)
    })
    .await;
    s.send(Command::ClearChannel(3));
    wait_for("channel cleared", || {
        !store.channels(NODE).unwrap().iter().any(|c| c.idx == 3)
    })
    .await;

    s.send(Command::RemoveContact(PEER.into()));
    wait_for("contact removed", || {
        store.contacts(NODE).unwrap().is_empty()
    })
    .await;

    s.send(Command::ExportContact(None));
    let mut exported = None;
    for _ in 0..500 {
        if let Ok(Update::Exported(uri)) = rx.try_recv() {
            exported = Some(uri);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(exported.unwrap().starts_with("meshcore://"));

    let mut settings = DeviceSettings::from_device(&device());
    settings.name = "Renamed".into();
    s.send(Command::ApplySettings(settings));
    wait_for(
        "settings applied",
        || matches!(s.state(), ConnState::Connected(d) if d.name == "Renamed"),
    )
    .await;
    assert_eq!(
        f.state().frames,
        vec![vec![0x08, b'R', b'e', b'n', b'a', b'm', b'e', b'd']]
    );
    assert_eq!(store.device(NODE).unwrap().unwrap().name, "Renamed");

    let mut invalid = DeviceSettings::from_device(&device());
    invalid.sf = 99;
    s.send(Command::ApplySettings(invalid));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(f.state().frames.len(), 1);
}
