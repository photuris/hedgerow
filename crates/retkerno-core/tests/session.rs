//! Integration tests for the session actor, driven through the fake radio.
//!
//! These cover what `session_probe.rs` does not. Time is paused, so the
//! multi-second timers run instantly.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use retkerno_core::config::ConnectionConfig;
use retkerno_core::model::{
    Channel, Contact, ContactKind, DeliveryStatus, DeviceInfo, Direction, NewMessage, ThreadId,
};
use retkerno_core::radio::{DeviceSettings, FakeFailure, FakeRadio, FakeSent, RadioEvent};
use retkerno_core::session::{Command, ConnState, Connector, Session, Update};
use retkerno_core::store::Store;
use retkerno_core::text;
use tokio::sync::broadcast::Receiver;
use tokio::time::Instant;

const NODE: &str = "aa00000000000000000000000000000000000000000000000000000000000000";
const OTHER_NODE: &str = "bb00000000000000000000000000000000000000000000000000000000000000";
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

fn peer() -> Contact {
    Contact {
        public_key: PEER.to_string(),
        name: "Peer".to_string(),
        kind: ContactKind::Chat,
        path_len: 2,
        last_advert: 1,
        lat: 0,
        lon: 0,
        last_modified: 5,
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
        st.contacts = vec![peer()];
    }

    f
}

fn cfg(auto_connect: bool) -> ConnectionConfig {
    ConnectionConfig {
        auto_connect,
        ..ConnectionConfig::default()
    }
}

fn spawn(f: &FakeRadio, store: &Store, auto_connect: bool) -> Session {
    Session::spawn(store.clone(), cfg(auto_connect), Connector::Fake(f.clone()))
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

fn battery(s: &Session) -> Option<u16> {
    match s.state() {
        ConnState::Connected(d) => d.battery_mv,
        _ => None,
    }
}

fn count(f: &FakeRadio, call: &str) -> usize {
    f.state()
        .calls
        .iter()
        .filter(|c| c.as_str() == call)
        .count()
}

fn called(f: &FakeRadio, prefix: &str) -> bool {
    f.state().calls.iter().any(|c| c.starts_with(prefix))
}

fn drain(rx: &mut Receiver<Update>) -> Vec<Update> {
    let mut out = Vec::new();

    while let Ok(u) = rx.try_recv() {
        out.push(u);
    }

    out
}

fn notices(rx: &mut Receiver<Update>) -> Vec<String> {
    drain(rx)
        .into_iter()
        .filter_map(|u| match u {
            Update::Notice(n) => Some(n),
            _ => None,
        })
        .collect()
}

/// Waits until a notice containing `needle` arrives.
async fn wait_notice(rx: &mut Receiver<Update>, needle: &str) {
    for _ in 0..12_000 {
        if notices(rx).iter().any(|n| n.contains(needle)) {
            return;
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    panic!("timed out waiting for a notice containing {needle:?}");
}

fn dm_thread() -> ThreadId {
    ThreadId::Contact(PEER[..12].to_string())
}

fn status_of(store: &Store, thread: &ThreadId, text: &str) -> Option<DeliveryStatus> {
    store
        .messages(NODE, thread, 50)
        .unwrap()
        .iter()
        .find(|m| m.text == text)
        .map(|m| m.status)
}

/// Sends a DM and waits until it is `Sent`; returns its ack tag.
async fn send_dm(s: &Session, f: &FakeRadio, store: &Store, text: &str) -> [u8; 4] {
    s.send(Command::SendText {
        thread: dm_thread(),
        text: text.into(),
    });
    wait_for("dm sent", || {
        status_of(store, &dm_thread(), text) == Some(DeliveryStatus::Sent)
    })
    .await;

    let sent = f.state().sent.clone();

    sent.iter()
        .rev()
        .find_map(|m| match m {
            FakeSent::Text { text: t, ack, .. } if t == text => Some(*ack),
            _ => None,
        })
        .expect("the fake recorded the send")
}

fn failed_out_row(text: &str) -> NewMessage {
    NewMessage {
        thread: dm_thread(),
        direction: Direction::Out,
        sender: None,
        text: text.into(),
        sender_ts: 5,
        received_at: 5,
        path_len: None,
        snr: None,
        status: DeliveryStatus::Failed,
    }
}

#[tokio::test(start_paused = true)]
async fn should_reconnect_when_reconfigured() {
    let f = fake();
    let store = Store::open_memory().unwrap();
    let s = spawn(&f, &store, true);

    wait_for("connected", || connected(&s)).await;
    s.send(Command::Reconfigure(cfg(true)));
    wait_for("second start", || count(&f, "start") == 2).await;
    wait_for("connected again", || connected(&s)).await;
}

#[tokio::test(start_paused = true)]
async fn should_send_nothing_and_notice_when_import_uri_is_bad() {
    let f = fake();
    let store = Store::open_memory().unwrap();
    let s = spawn(&f, &store, true);
    let mut rx = s.subscribe();

    wait_for("connected", || connected(&s)).await;
    s.send(Command::ImportContact("meshcore://not-hex".into()));
    wait_notice(&mut rx, "Import contact failed").await;

    assert!(!called(&f, "import_contact"), "{:?}", f.state().calls);
}

#[tokio::test(start_paused = true)]
async fn should_store_imported_contact_when_card_is_valid() {
    let f = fake();
    let store = Store::open_memory().unwrap();
    let s = spawn(&f, &store, true);
    let key = "ab".repeat(32);

    wait_for("connected", || connected(&s)).await;
    s.send(Command::ImportContact(format!("meshcore://{key}")));
    wait_for("imported contact stored", || {
        store
            .contacts(NODE)
            .unwrap()
            .iter()
            .any(|c| c.name == "Imported" && c.public_key == key)
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn should_reload_contacts_when_path_is_reset() {
    let f = fake();
    let store = Store::open_memory().unwrap();
    let s = spawn(&f, &store, true);

    wait_for("connected", || connected(&s)).await;
    s.send(Command::ResetPath(PEER.into()));
    wait_for("contacts reloaded", || count(&f, "contacts:0") == 2).await;

    assert!(called(&f, &format!("reset_path:{PEER}")));
    assert_eq!(store.contacts(NODE).unwrap()[0].path_len, -1);
}

#[tokio::test(start_paused = true)]
async fn should_record_clock_sync_when_asked() {
    let f = fake();
    let store = Store::open_memory().unwrap();
    let s = spawn(&f, &store, true);

    wait_for("connected", || connected(&s)).await;
    s.send(Command::SyncClock);
    wait_for("clock synced", || count(&f, "sync_clock") == 1).await;
}

#[tokio::test(start_paused = true)]
async fn should_send_flood_advert_when_flood_is_true() {
    let f = fake();
    let store = Store::open_memory().unwrap();
    let s = spawn(&f, &store, true);

    wait_for("connected", || connected(&s)).await;
    s.send(Command::SendAdvert { flood: true });
    wait_for("advert sent", || count(&f, "send_advert:true") == 1).await;
}

#[tokio::test(start_paused = true)]
async fn should_update_battery_in_state_when_120_seconds_pass() {
    let f = fake();
    let store = Store::open_memory().unwrap();
    let s = spawn(&f, &store, true);

    wait_for("connected", || connected(&s)).await;
    assert_eq!(battery(&s), Some(4126));

    f.state().device.battery_mv = Some(3700);
    tokio::time::sleep(Duration::from_secs(121)).await;

    assert_eq!(battery(&s), Some(3700));
    assert_eq!(
        store.device(NODE).unwrap().unwrap().battery_mv,
        Some(4126),
        "the poll must not write the store"
    );
}

#[tokio::test(start_paused = true)]
async fn should_rerun_start_when_refreshed() {
    let f = fake();
    let store = Store::open_memory().unwrap();
    let s = spawn(&f, &store, true);

    wait_for("connected", || connected(&s)).await;
    s.send(Command::Refresh);
    wait_for("second start", || count(&f, "start") == 2).await;
    wait_for("second sync done", || count(&f, "contacts:0") == 2).await;
}

#[tokio::test(start_paused = true)]
async fn should_not_resend_when_retrying_a_delivered_row() {
    let f = fake();

    f.state().ack_timeout_ms = 1000;

    let store = Store::open_memory().unwrap();
    let s = spawn(&f, &store, true);

    wait_for("connected", || connected(&s)).await;

    let ack = send_dm(&s, &f, &store, "hello").await;

    f.emit(RadioEvent::Ack(ack));
    wait_for("delivered", || {
        status_of(&store, &dm_thread(), "hello") == Some(DeliveryStatus::Delivered)
    })
    .await;

    let id = store.messages(NODE, &dm_thread(), 10).unwrap()[0].id;

    s.send(Command::Retry { message_id: id });
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(f.state().sent.len(), 1);
    assert_eq!(
        status_of(&store, &dm_thread(), "hello"),
        Some(DeliveryStatus::Delivered)
    );
}

#[tokio::test(start_paused = true)]
async fn should_ignore_retry_when_failed_row_belongs_to_another_node() {
    let f = fake();
    let store = Store::open_memory().unwrap();
    let id = store
        .insert_message(OTHER_NODE, &failed_out_row("stale"))
        .unwrap()
        .unwrap();
    let s = spawn(&f, &store, true);

    wait_for("connected", || connected(&s)).await;
    s.send(Command::Retry { message_id: id });
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(!called(&f, "send_text"), "{:?}", f.state().calls);
    assert_eq!(
        store.message(OTHER_NODE, id).unwrap().unwrap().status,
        DeliveryStatus::Failed
    );
}

#[tokio::test(start_paused = true)]
async fn should_fail_sent_dm_and_record_disconnect_when_radio_disconnects() {
    let f = fake();
    let store = Store::open_memory().unwrap();
    let s = spawn(&f, &store, false);

    s.send(Command::Connect);
    wait_for("connected", || connected(&s)).await;
    send_dm(&s, &f, &store, "in flight").await;

    f.emit(RadioEvent::Disconnected);
    wait_for("dropped", || s.state() == ConnState::Disconnected).await;

    assert_eq!(
        status_of(&store, &dm_thread(), "in flight"),
        Some(DeliveryStatus::Failed)
    );
    assert!(called(&f, "disconnect"), "{:?}", f.state().calls);
}

#[tokio::test(start_paused = true)]
async fn should_fail_pending_row_and_notice_when_reconnecting() {
    let f = fake();
    let store = Store::open_memory().unwrap();
    let mut left_behind = failed_out_row("left behind");

    left_behind.status = DeliveryStatus::Pending;
    store.insert_message(NODE, &left_behind).unwrap().unwrap();

    let s = spawn(&f, &store, true);
    let mut rx = s.subscribe();

    wait_for("connected", || connected(&s)).await;

    assert_eq!(
        status_of(&store, &dm_thread(), "left behind"),
        Some(DeliveryStatus::Failed)
    );
    assert!(
        notices(&mut rx).iter().any(|n| n.contains("marked failed")),
        "no 'marked failed' notice"
    );
}

#[tokio::test(start_paused = true)]
async fn should_emit_store_changed_when_settings_are_applied() {
    let f = fake();
    let store = Store::open_memory().unwrap();
    let s = spawn(&f, &store, true);
    let mut rx = s.subscribe();

    wait_for("connected", || connected(&s)).await;
    drain(&mut rx);

    let mut settings = DeviceSettings::from_device(&device());

    settings.name = "Renamed".into();
    s.send(Command::ApplySettings(settings));
    wait_for(
        "settings applied",
        || matches!(s.state(), ConnState::Connected(d) if d.name == "Renamed"),
    )
    .await;

    assert!(
        drain(&mut rx)
            .iter()
            .any(|u| matches!(u, Update::StoreChanged)),
        "no StoreChanged after ApplySettings"
    );
}

#[tokio::test(start_paused = true)]
async fn should_keep_stored_channels_when_refresh_cannot_read_them() {
    let f = fake();
    let store = Store::open_memory().unwrap();
    let s = spawn(&f, &store, true);
    let mut rx = s.subscribe();

    wait_for("connected", || connected(&s)).await;
    assert_eq!(store.channels(NODE).unwrap().len(), 2);

    f.state().fail_call = Some(("channels:".into(), FakeFailure::Radio("link lost".into())));
    s.send(Command::Refresh);
    wait_notice(&mut rx, "Refresh failed").await;

    assert_eq!(count(&f, "start"), 2);
    assert_eq!(count(&f, "channels:20"), 2);
    assert_eq!(store.channels(NODE).unwrap().len(), 2);
    assert!(connected(&s));
}

#[tokio::test(start_paused = true)]
async fn should_keep_sent_dm_sent_and_deliver_it_when_refreshed_before_the_ack() {
    let f = fake();

    f.state().ack_timeout_ms = 1000;

    let store = Store::open_memory().unwrap();
    let s = spawn(&f, &store, true);

    wait_for("connected", || connected(&s)).await;

    let ack = send_dm(&s, &f, &store, "pending ack").await;

    s.send(Command::Refresh);
    wait_for("refreshed", || count(&f, "contacts:0") == 2).await;

    assert_eq!(
        status_of(&store, &dm_thread(), "pending ack"),
        Some(DeliveryStatus::Sent)
    );

    f.emit(RadioEvent::Ack(ack));
    wait_for("delivered", || {
        status_of(&store, &dm_thread(), "pending ack") == Some(DeliveryStatus::Delivered)
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn should_store_slot_and_file_history_under_it_when_channel_reload_fails() {
    let f = fake();
    let store = Store::open_memory().unwrap();
    let s = spawn(&f, &store, true);
    let slot = ThreadId::Channel(7);

    wait_for("connected", || connected(&s)).await;

    f.state().fail_call = Some(("channels:".into(), FakeFailure::Radio("link lost".into())));
    s.send(Command::SetChannel(Channel {
        idx: 7,
        name: "#fresh".into(),
        secret: text::hashtag_secret("#fresh").unwrap(),
    }));
    wait_for("reload attempted", || count(&f, "channels:20") == 2).await;

    assert!(store.channels(NODE).unwrap().iter().any(|c| c.idx == 7));
    assert!(connected(&s));

    s.send(Command::SendText {
        thread: slot.clone(),
        text: "first words".into(),
    });
    wait_for("channel text stored", || {
        store.messages(NODE, &slot, 10).unwrap().len() == 1
    })
    .await;

    // A later good reload must still show the history under the new channel.
    s.send(Command::Refresh);
    wait_for("refreshed", || count(&f, "contacts:0") == 2).await;

    let history = store.messages(NODE, &slot, 10).unwrap();

    assert_eq!(history.len(), 1);
    assert_eq!(history[0].text, "first words");
}

#[tokio::test(start_paused = true)]
async fn should_leave_store_and_connection_alone_when_radio_rejects_channel_write() {
    let f = fake();
    let store = Store::open_memory().unwrap();
    let s = spawn(&f, &store, true);
    let mut rx = s.subscribe();

    wait_for("connected", || connected(&s)).await;

    f.state().fail_call = Some((
        "set_channel:".into(),
        FakeFailure::Rejected("slot locked".into()),
    ));
    s.send(Command::SetChannel(Channel {
        idx: 7,
        name: "#fresh".into(),
        secret: text::hashtag_secret("#fresh").unwrap(),
    }));
    wait_notice(&mut rx, "slot locked").await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(store.channels(NODE).unwrap().len(), 2);
    assert!(connected(&s));
    assert!(!called(&f, "disconnect"), "{:?}", f.state().calls);
}

#[tokio::test(start_paused = true)]
async fn should_disconnect_and_reconnect_when_channel_write_outcome_is_unknown() {
    let f = fake();
    let store = Store::open_memory().unwrap();
    let s = spawn(&f, &store, false);

    s.send(Command::Connect);
    wait_for("connected", || connected(&s)).await;

    f.state().fail_call = Some(("set_channel:".into(), FakeFailure::Radio("timeout".into())));
    s.send(Command::SetChannel(Channel {
        idx: 7,
        name: "#fresh".into(),
        secret: text::hashtag_secret("#fresh").unwrap(),
    }));
    wait_for("second start", || count(&f, "start") == 2).await;
    wait_for("connected again", || connected(&s)).await;

    assert!(called(&f, "disconnect"), "{:?}", f.state().calls);
}

#[tokio::test(start_paused = true)]
async fn should_back_off_5_10_20_seconds_when_connect_keeps_failing() {
    let f = fake();

    f.state().fail_connect = true;

    let store = Store::open_memory().unwrap();
    let s = spawn(&f, &store, true);
    let mut rx = s.subscribe();
    let mut failed_at = Vec::new();

    while failed_at.len() < 4 {
        let update = tokio::time::timeout(Duration::from_secs(300), rx.recv())
            .await
            .expect("no further update within 300 s")
            .unwrap();

        if matches!(update, Update::State(ConnState::Failed(_))) {
            failed_at.push(Instant::now());
        }
    }

    let gaps: Vec<f64> = failed_at
        .windows(2)
        .map(|w| (w[1] - w[0]).as_secs_f64())
        .collect();

    for (gap, want) in gaps.iter().zip([5.0, 10.0, 20.0]) {
        assert!((gap - want).abs() <= 0.5, "gaps {gaps:?}, want 5/10/20");
    }
}

#[tokio::test(start_paused = true)]
async fn should_keep_whole_snapshot_when_refresh_fails_late() {
    let f = fake();
    let store = Store::open_memory().unwrap();
    let s = spawn(&f, &store, true);
    let mut rx = s.subscribe();

    wait_for("connected", || connected(&s)).await;

    let old_device = store.device(NODE).unwrap();
    let old_channels = store.channels(NODE).unwrap();
    let old_contacts = store.contacts(NODE).unwrap();

    {
        let mut st = f.state();

        st.device.name = "New name".into();
        st.channels[0].name = "New occupant".into();
        st.channels[0].secret = [44; 16];
        st.fail_call = Some((
            "contacts:".into(),
            FakeFailure::Radio("contact timeout".into()),
        ));
    }

    s.send(Command::Refresh);
    wait_notice(&mut rx, "Refresh failed").await;

    assert_eq!(
        store.channels(NODE).unwrap(),
        old_channels,
        "failed Refresh replaced channels"
    );
    assert_eq!(
        store.device(NODE).unwrap(),
        old_device,
        "failed Refresh replaced device"
    );
    assert_eq!(store.contacts(NODE).unwrap(), old_contacts);
}

#[tokio::test(start_paused = true)]
async fn should_ignore_connect_while_connecting_even_when_attempt_fails() {
    let f = fake();

    f.state().start_delay_ms = 1000;
    f.state().fail_call = Some(("start".into(), FakeFailure::Radio("sync failed".into())));

    let store = Store::open_memory().unwrap();
    let s = spawn(&f, &store, false);

    s.send(Command::Connect);
    wait_for("connecting", || s.state() == ConnState::Connecting).await;
    s.send(Command::Connect);
    tokio::time::sleep(Duration::from_millis(2200)).await;

    assert_eq!(
        count(&f, "start"),
        1,
        "Connect queued while Connecting must be ignored"
    );
    assert!(matches!(s.state(), ConnState::Failed(_)));
}

#[tokio::test(start_paused = true)]
async fn should_disconnect_at_once_and_abandon_attempt_when_disconnect_arrives_while_connecting() {
    let f = fake();

    f.state().start_delay_ms = 5000;

    let store = Store::open_memory().unwrap();
    let s = spawn(&f, &store, false);
    let mut rx = s.subscribe();

    s.send(Command::Connect);
    wait_for("connecting", || s.state() == ConnState::Connecting).await;
    s.send(Command::Disconnect);
    wait_for("disconnected", || s.state() == ConnState::Disconnected).await;

    assert_eq!(count(&f, "start"), 0, "start must still be in flight");
    assert!(!called(&f, "disconnect"), "{:?}", f.state().calls);

    wait_for("attempt abandoned", || called(&f, "disconnect")).await;

    assert_eq!(count(&f, "start"), 1);
    assert_eq!(s.state(), ConnState::Disconnected);
    assert!(
        !drain(&mut rx)
            .iter()
            .any(|u| matches!(u, Update::State(ConnState::Connected(_)))),
        "an abandoned attempt must never publish Connected"
    );
}

#[tokio::test(start_paused = true)]
async fn should_connect_with_new_config_when_reconfigured_while_connecting() {
    let f = fake();

    f.state().start_delay_ms = 5000;

    let store = Store::open_memory().unwrap();
    let s = spawn(&f, &store, false);
    let mut rx = s.subscribe();

    s.send(Command::Connect);
    wait_for("connecting", || s.state() == ConnState::Connecting).await;

    // The first attempt already opened its radio; only the replacement
    // attempt meets the refusal.
    f.state().fail_connect = true;
    s.send(Command::Reconfigure(cfg(true)));

    let mut failed_at = Vec::new();

    while failed_at.len() < 2 {
        let update = tokio::time::timeout(Duration::from_secs(60), rx.recv())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "no retry after {} failed attempt(s): the new auto_connect was ignored",
                    failed_at.len()
                )
            })
            .unwrap();

        if matches!(update, Update::State(ConnState::Failed(_))) {
            failed_at.push(Instant::now());
        }
    }

    let gap = (failed_at[1] - failed_at[0]).as_secs_f64();

    assert!((gap - 5.0).abs() <= 0.5, "retry came after {gap} s, want 5");
}

#[tokio::test(start_paused = true)]
async fn should_abandon_radio_and_fail_verified_rows_when_connect_panics_after_start() {
    let f = fake();
    let store = Store::open_memory().unwrap();
    let mut pending = failed_out_row("unfinished");

    pending.status = DeliveryStatus::Pending;
    store.insert_message(NODE, &pending).unwrap().unwrap();
    f.state().panic_after = Some("channels".into());

    let s = spawn(&f, &store, false);

    s.send(Command::Connect);
    wait_for("failed", || matches!(s.state(), ConnState::Failed(_))).await;

    assert!(called(&f, "disconnect"), "{:?}", f.state().calls);
    assert_eq!(
        status_of(&store, &dm_thread(), "unfinished"),
        Some(DeliveryStatus::Failed)
    );
}

#[tokio::test(start_paused = true)]
async fn should_abandon_radio_and_leave_rows_alone_when_connect_panics_at_start() {
    let f = fake();
    let store = Store::open_memory().unwrap();
    let mut pending = failed_out_row("unverified");

    pending.status = DeliveryStatus::Pending;
    store.insert_message(NODE, &pending).unwrap().unwrap();
    store.insert_message(OTHER_NODE, &pending).unwrap().unwrap();
    f.state().panic_after = Some("start".into());

    let s = spawn(&f, &store, false);

    s.send(Command::Connect);
    wait_for("failed", || matches!(s.state(), ConnState::Failed(_))).await;

    assert!(called(&f, "disconnect"), "{:?}", f.state().calls);

    for node in [NODE, OTHER_NODE] {
        let rows = store.messages(node, &dm_thread(), 50).unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].status,
            DeliveryStatus::Pending,
            "node {node} row changed although the node was never verified"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn should_abandon_radio_and_stay_disconnected_when_cancelled_connect_panics() {
    let f = fake();

    f.state().start_delay_ms = 5000;
    f.state().panic_after = Some("channels".into());

    let store = Store::open_memory().unwrap();
    let s = spawn(&f, &store, false);

    s.send(Command::Connect);
    wait_for("connecting", || s.state() == ConnState::Connecting).await;
    s.send(Command::Disconnect);
    wait_for("disconnected", || s.state() == ConnState::Disconnected).await;
    wait_for("attempt abandoned", || called(&f, "disconnect")).await;

    assert_eq!(s.state(), ConnState::Disconnected);
}
