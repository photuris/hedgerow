//! Overseer probe for desktop notifications: the policy and notifier in
//! `hedgerow_core::notify`, the per-thread mode and unread rows in the
//! store, and the session's `Update::Received`. Copied verbatim by task 013;
//! do not edit.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use hedgerow_core::config::ConnectionConfig;
use hedgerow_core::model::{
    Channel, DeliveryStatus, DeviceInfo, Direction, NewMessage, NotifyMode, ThreadId,
};
use hedgerow_core::notify::{self, Notifier, Post, THROTTLE_WINDOW};
use hedgerow_core::radio::{FakeRadio, IncomingMessage};
use hedgerow_core::session::{ConnState, Connector, Session, Update};
use hedgerow_core::store::Store;
use hedgerow_core::text;

const NODE: &str = "aa00000000000000000000000000000000000000000000000000000000000000";
const OTHER_NODE: &str = "bb00000000000000000000000000000000000000000000000000000000000000";
const PEER: &str = "0a0b0c0d0e0f";

// ── mentions / wants / text ─────────────────────────────────────────────────

#[test]
fn probe_mentions() {
    assert!(notify::mentions("hi @[Probe] there", "Probe"));
    assert!(notify::mentions("x@[PROBE]", "probe"));
    assert!(notify::mentions("@probe, are you there", "Probe"));
    assert!(notify::mentions("@Probe", "Probe"));
    assert!(notify::mentions("ping @John Smith!", "John Smith"));
    assert!(notify::mentions("@ünï!", "Ünï"));
    assert!(notify::mentions("@Prober and @Probe.", "Probe"));

    assert!(!notify::mentions("@Prober hi", "Probe"));
    assert!(!notify::mentions("@probe_2 hi", "Probe"));
    assert!(!notify::mentions("probe without the at sign", "Probe"));
    assert!(!notify::mentions("@[Prob]", "Probe"));
    assert!(!notify::mentions("@ anything", ""));
    assert!(!notify::mentions("@[] x", "  "));
}

#[test]
fn probe_wants() {
    let dm = ThreadId::Contact(PEER.into());
    let ch = ThreadId::Channel(3);

    assert!(notify::wants(NotifyMode::Default, &dm, "plain", "Probe"));
    assert!(!notify::wants(NotifyMode::Default, &ch, "plain", "Probe"));
    assert!(notify::wants(
        NotifyMode::Default,
        &ch,
        "yo @[Probe]",
        "Probe"
    ));
    assert!(notify::wants(NotifyMode::All, &ch, "plain", "Probe"));
    assert!(notify::wants(NotifyMode::All, &dm, "plain", "Probe"));
    assert!(!notify::wants(NotifyMode::Mute, &dm, "plain", "Probe"));
    assert!(!notify::wants(
        NotifyMode::Mute,
        &ch,
        "yo @[Probe]",
        "Probe"
    ));
}

#[test]
fn probe_texts() {
    assert_eq!(notify::summary_text("Alice", 1), "Alice");
    assert_eq!(notify::summary_text("#test", 4), "#test (4 new)");
    assert_eq!(notify::body_text(None, "hello"), "hello");
    assert_eq!(notify::body_text(Some("Bob"), "hi all"), "Bob: hi all");

    let cut = notify::body_text(None, &"é".repeat(250));
    assert_eq!(cut.chars().count(), 200);
    assert!(cut.ends_with('…'));
    assert_eq!(cut.chars().filter(|c| *c == 'é').count(), 199);

    let exact = "a".repeat(200);
    assert_eq!(notify::body_text(None, &exact), exact);
    assert_eq!(notify::BODY_MAX_CHARS, 200);
    assert_eq!(THROTTLE_WINDOW, Duration::from_secs(60));
}

// ── Notifier ────────────────────────────────────────────────────────────────

/// A store holding the probe radio, `#test` in slot 3, and the peer contact.
fn world() -> Store {
    let store = Store::open_memory().unwrap();
    store.save_device(&device()).unwrap();
    store.replace_channels(NODE, &[test_channel(3)]).unwrap();
    store
}

fn dm() -> ThreadId {
    ThreadId::Contact(PEER.into())
}

/// Stores an incoming row and returns its id.
fn arrive(store: &Store, node: &str, thread: &ThreadId, text: &str, ts: u32) -> i64 {
    store
        .insert_message(node, &incoming(thread.clone(), text, ts))
        .unwrap()
        .unwrap()
}

fn secs(t0: Instant, s: u64) -> Instant {
    t0 + Duration::from_secs(s)
}

#[test]
fn probe_notifier_posts_a_direct_message_at_once() {
    let store = world();
    let mut n = Notifier::new(THROTTLE_WINDOW);
    let t0 = Instant::now();
    let id = arrive(&store, NODE, &dm(), "hello", 10);

    let posts = n.on_received(&store, Some(NODE), false, NODE, &[id], t0);
    assert_eq!(
        posts,
        vec![Post {
            node: NODE.into(),
            thread: dm(),
            id,
            count: 1,
            summary: PEER.into(),
            body: "hello".into(),
        }]
    );
    assert_eq!(n.next_due(), None);
}

#[test]
fn probe_notifier_is_silent_while_focused_and_forgets_what_arrived_then() {
    let store = world();
    let mut n = Notifier::new(THROTTLE_WINDOW);
    let t0 = Instant::now();
    let id = arrive(&store, NODE, &dm(), "seen", 10);

    assert!(
        n.on_received(&store, Some(NODE), true, NODE, &[id], t0)
            .is_empty()
    );
    // Unfocused later: nothing old pops up.
    assert!(
        n.on_due(&store, Some(NODE), false, secs(t0, 120))
            .is_empty()
    );
    let id2 = arrive(&store, NODE, &dm(), "new", 11);
    let posts = n.on_received(&store, Some(NODE), false, NODE, &[id2], secs(t0, 121));
    assert_eq!(posts.len(), 1);
    assert_eq!((posts[0].id, posts[0].count), (id2, 1));
}

#[test]
fn probe_notifier_channel_needs_a_mention_by_default() {
    let store = world();
    let mut n = Notifier::new(THROTTLE_WINDOW);
    let t0 = Instant::now();
    let ch = ThreadId::Channel(3);

    let plain = arrive(&store, NODE, &ch, "plain chatter", 10);
    assert!(
        n.on_received(&store, Some(NODE), false, NODE, &[plain], t0)
            .is_empty()
    );

    let ping = arrive(&store, NODE, &ch, "hey @[Probe]", 11);
    let posts = n.on_received(&store, Some(NODE), false, NODE, &[ping], secs(t0, 1));
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].id, ping);
    assert_eq!(posts[0].count, 1);
    assert_eq!(posts[0].summary, "#test");
    assert_eq!(posts[0].body, "Bob: hey @[Probe]");
}

#[test]
fn probe_notifier_coalesces_a_burst_into_one_summary() {
    let store = world();
    let mut n = Notifier::new(THROTTLE_WINDOW);
    let t0 = Instant::now();

    let a = arrive(&store, NODE, &dm(), "one", 10);
    assert_eq!(
        n.on_received(&store, Some(NODE), false, NODE, &[a], t0)
            .len(),
        1
    );
    let b = arrive(&store, NODE, &dm(), "two", 11);
    assert!(
        n.on_received(&store, Some(NODE), false, NODE, &[b], secs(t0, 10))
            .is_empty()
    );
    let c = arrive(&store, NODE, &dm(), "three", 12);
    assert!(
        n.on_received(&store, Some(NODE), false, NODE, &[c], secs(t0, 20))
            .is_empty()
    );
    assert_eq!(n.next_due(), Some(t0 + THROTTLE_WINDOW));

    assert!(n.on_due(&store, Some(NODE), false, secs(t0, 59)).is_empty());
    let posts = n.on_due(&store, Some(NODE), false, t0 + THROTTLE_WINDOW);
    assert_eq!(posts.len(), 1);
    assert_eq!((posts[0].id, posts[0].count), (c, 2));
    assert_eq!(posts[0].summary, format!("{PEER} (2 new)"));
    assert_eq!(posts[0].body, "three");
    assert_eq!(n.next_due(), None);

    // The summary restarted the minute.
    let d = arrive(&store, NODE, &dm(), "four", 13);
    assert!(
        n.on_received(&store, Some(NODE), false, NODE, &[d], secs(t0, 90))
            .is_empty()
    );
    assert_eq!(n.next_due(), Some(t0 + THROTTLE_WINDOW * 2));
}

#[test]
fn probe_notifier_does_not_post_what_was_read() {
    let store = world();
    let mut n = Notifier::new(THROTTLE_WINDOW);
    let t0 = Instant::now();

    let a = arrive(&store, NODE, &dm(), "one", 10);
    n.on_received(&store, Some(NODE), false, NODE, &[a], t0);
    let b = arrive(&store, NODE, &dm(), "two", 11);
    n.on_received(&store, Some(NODE), false, NODE, &[b], secs(t0, 10));
    store.mark_read(NODE, &dm()).unwrap();

    // A newer unread row the notifier never heard about does not revive
    // the read "two"; only the unread filter keeps "two" from posting.
    arrive(&store, NODE, &dm(), "three", 12);
    assert!(n.on_due(&store, Some(NODE), false, secs(t0, 60)).is_empty());
}

#[test]
fn probe_notifier_counts_only_unread_rows_after_a_read() {
    let store = world();
    let mut n = Notifier::new(THROTTLE_WINDOW);
    let t0 = Instant::now();

    let a = arrive(&store, NODE, &dm(), "one", 10);
    n.on_received(&store, Some(NODE), false, NODE, &[a], t0);
    let b = arrive(&store, NODE, &dm(), "two", 11);
    n.on_received(&store, Some(NODE), false, NODE, &[b], secs(t0, 10));
    store.mark_read(NODE, &dm()).unwrap();

    // Past the deadline, before any on_due: only "three" counts.
    let c = arrive(&store, NODE, &dm(), "three", 12);
    let posts = n.on_received(&store, Some(NODE), false, NODE, &[c], secs(t0, 61));
    assert_eq!(posts.len(), 1);
    assert_eq!((posts[0].id, posts[0].count), (c, 1));
    assert_eq!(posts[0].summary, PEER);
}

#[test]
fn probe_notifier_checks_the_mode_when_it_posts() {
    let store = world();
    let mut n = Notifier::new(THROTTLE_WINDOW);
    let t0 = Instant::now();

    let a = arrive(&store, NODE, &dm(), "one", 10);
    n.on_received(&store, Some(NODE), false, NODE, &[a], t0);
    let b = arrive(&store, NODE, &dm(), "two", 11);
    n.on_received(&store, Some(NODE), false, NODE, &[b], secs(t0, 10));
    store.set_notify(NODE, &dm(), NotifyMode::Mute).unwrap();
    assert!(n.on_due(&store, Some(NODE), false, secs(t0, 60)).is_empty());

    // All -> Default: queued plain channel traffic is no longer wanted.
    let ch = ThreadId::Channel(3);
    store.set_notify(NODE, &ch, NotifyMode::All).unwrap();
    let c = arrive(&store, NODE, &ch, "plain one", 20);
    assert_eq!(
        n.on_received(&store, Some(NODE), false, NODE, &[c], secs(t0, 100))
            .len(),
        1
    );
    let d = arrive(&store, NODE, &ch, "plain two", 21);
    assert!(
        n.on_received(&store, Some(NODE), false, NODE, &[d], secs(t0, 101))
            .is_empty()
    );
    store.set_notify(NODE, &ch, NotifyMode::Default).unwrap();
    assert!(
        n.on_due(&store, Some(NODE), false, secs(t0, 160))
            .is_empty()
    );
}

#[test]
fn probe_notifier_an_undone_mute_cancels_nothing() {
    let store = world();
    let mut n = Notifier::new(THROTTLE_WINDOW);
    let t0 = Instant::now();
    let other_dm = ThreadId::Contact("1a1b1c1d1e1f".into());

    let a = arrive(&store, NODE, &dm(), "one", 10);
    assert_eq!(
        n.on_received(&store, Some(NODE), false, NODE, &[a], t0)
            .len(),
        1
    );
    let b = arrive(&store, NODE, &dm(), "two", 11);
    assert!(
        n.on_received(&store, Some(NODE), false, NODE, &[b], secs(t0, 10))
            .is_empty()
    );
    store.set_notify(NODE, &dm(), NotifyMode::Mute).unwrap();

    // Another conversation evaluates while the mute is in force.
    let c = arrive(&store, NODE, &other_dm, "elsewhere", 12);
    let posts = n.on_received(&store, Some(NODE), false, NODE, &[c], secs(t0, 30));
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].id, c);
    assert_eq!(n.next_due(), Some(t0 + THROTTLE_WINDOW));

    store.set_notify(NODE, &dm(), NotifyMode::Default).unwrap();
    let posts = n.on_due(&store, Some(NODE), false, t0 + THROTTLE_WINDOW);
    assert_eq!(posts.len(), 1);
    assert_eq!((posts[0].id, posts[0].count), (b, 1));
    assert_eq!(posts[0].body, "two");
}

#[test]
fn probe_notifier_a_burst_muted_at_its_deadline_stays_retired() {
    let store = world();
    let mut n = Notifier::new(THROTTLE_WINDOW);
    let t0 = Instant::now();
    let other_dm = ThreadId::Contact("1a1b1c1d1e1f".into());

    let a = arrive(&store, NODE, &dm(), "one", 10);
    n.on_received(&store, Some(NODE), false, NODE, &[a], t0);
    let b = arrive(&store, NODE, &dm(), "two", 11);
    n.on_received(&store, Some(NODE), false, NODE, &[b], secs(t0, 10));
    store.set_notify(NODE, &dm(), NotifyMode::Mute).unwrap();
    assert!(n.on_due(&store, Some(NODE), false, secs(t0, 60)).is_empty());
    assert_eq!(n.next_due(), None);

    // Unmuted after the deadline: unrelated traffic does not revive "two".
    store.set_notify(NODE, &dm(), NotifyMode::Default).unwrap();
    let c = arrive(&store, NODE, &other_dm, "elsewhere", 12);
    let posts = n.on_received(&store, Some(NODE), false, NODE, &[c], secs(t0, 71));
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].id, c);
}

#[test]
fn probe_notifier_counts_only_rows_since_the_last_decision() {
    let store = world();
    let mut n = Notifier::new(THROTTLE_WINDOW);
    let t0 = Instant::now();
    let ch = ThreadId::Channel(3);

    let a = arrive(&store, NODE, &ch, "@[Probe] first", 10);
    assert_eq!(
        n.on_received(&store, Some(NODE), false, NODE, &[a], t0)
            .len(),
        1
    );
    let b = arrive(&store, NODE, &ch, "plain", 11);
    n.on_received(&store, Some(NODE), false, NODE, &[b], secs(t0, 10));
    let c = arrive(&store, NODE, &ch, "@[Probe] again", 12);
    n.on_received(&store, Some(NODE), false, NODE, &[c], secs(t0, 20));
    let posts = n.on_due(&store, Some(NODE), false, secs(t0, 60));
    assert_eq!(posts.len(), 1);
    assert_eq!((posts[0].id, posts[0].count), (c, 1));

    // "plain" was retired at that decision; switching to All later does
    // not count it.
    store.set_notify(NODE, &ch, NotifyMode::All).unwrap();
    let d = arrive(&store, NODE, &ch, "later", 13);
    assert!(
        n.on_received(&store, Some(NODE), false, NODE, &[d], secs(t0, 71))
            .is_empty()
    );
    let posts = n.on_due(&store, Some(NODE), false, secs(t0, 120));
    assert_eq!(posts.len(), 1);
    assert_eq!((posts[0].id, posts[0].count), (d, 1));
}

#[test]
fn probe_notifier_focus_keeps_the_cooldown() {
    let store = world();
    let mut n = Notifier::new(THROTTLE_WINDOW);
    let t0 = Instant::now();

    let a = arrive(&store, NODE, &dm(), "one", 10);
    assert_eq!(
        n.on_received(&store, Some(NODE), false, NODE, &[a], t0)
            .len(),
        1
    );
    let b = arrive(&store, NODE, &dm(), "two", 11);
    assert!(
        n.on_received(&store, Some(NODE), true, NODE, &[b], secs(t0, 5))
            .is_empty()
    );
    let c = arrive(&store, NODE, &dm(), "three", 12);
    assert!(
        n.on_received(&store, Some(NODE), false, NODE, &[c], secs(t0, 16))
            .is_empty()
    );
    let posts = n.on_due(&store, Some(NODE), false, t0 + THROTTLE_WINDOW);
    assert_eq!(posts.len(), 1);
    // "two" arrived while focused, so only "three" counts.
    assert_eq!((posts[0].id, posts[0].count), (c, 1));
}

#[test]
fn probe_notifier_follows_the_connected_node() {
    let store = world();
    let mut n = Notifier::new(THROTTLE_WINDOW);
    let t0 = Instant::now();

    // A batch for a radio that is not the connected one changes nothing.
    let other = arrive(&store, OTHER_NODE, &dm(), "elsewhere", 10);
    assert!(
        n.on_received(&store, Some(NODE), false, OTHER_NODE, &[other], t0)
            .is_empty()
    );

    let a = arrive(&store, NODE, &dm(), "one", 11);
    n.on_received(&store, Some(NODE), false, NODE, &[a], t0);
    let b = arrive(&store, NODE, &dm(), "two", 12);
    n.on_received(&store, Some(NODE), false, NODE, &[b], secs(t0, 1));
    // The stale batch did not cost the pending "two".
    assert!(
        n.on_received(&store, Some(NODE), false, OTHER_NODE, &[other], secs(t0, 2))
            .is_empty()
    );
    assert_eq!(n.next_due(), Some(t0 + THROTTLE_WINDOW));

    // Another radio connects: what was pending for the first is dropped.
    assert!(
        n.on_due(&store, Some(OTHER_NODE), false, secs(t0, 60))
            .is_empty()
    );
    assert_eq!(n.next_due(), None);

    // Disconnected: nothing posts.
    let c = arrive(&store, NODE, &dm(), "three", 13);
    assert!(
        n.on_received(&store, None, false, NODE, &[c], secs(t0, 200))
            .is_empty()
    );
}

#[test]
fn probe_notifier_drops_a_pending_row_whose_channel_was_replaced() {
    let store = world();
    let mut n = Notifier::new(THROTTLE_WINDOW);
    let t0 = Instant::now();
    let ch = ThreadId::Channel(3);
    store.set_notify(NODE, &ch, NotifyMode::All).unwrap();

    let a = arrive(&store, NODE, &ch, "x one", 10);
    assert_eq!(
        n.on_received(&store, Some(NODE), false, NODE, &[a], t0)
            .len(),
        1
    );
    let b = arrive(&store, NODE, &ch, "x two", 11);
    assert!(
        n.on_received(&store, Some(NODE), false, NODE, &[b], secs(t0, 1))
            .is_empty()
    );

    // #other replaces #test in slot 3, wants everything, and has an unread
    // row the notifier never heard about.
    store.replace_channels(NODE, &[other_channel(3)]).unwrap();
    store.set_notify(NODE, &ch, NotifyMode::All).unwrap();
    arrive(&store, NODE, &ch, "y history", 12);
    assert!(n.on_due(&store, Some(NODE), false, secs(t0, 60)).is_empty());

    // New traffic in #other posts on its own, counted alone.
    let c = arrive(&store, NODE, &ch, "y live", 13);
    let posts = n.on_received(&store, Some(NODE), false, NODE, &[c], secs(t0, 61));
    assert_eq!(posts.len(), 1);
    assert_eq!((posts[0].id, posts[0].count), (c, 1));
    assert_eq!(posts[0].summary, "#other");
}

#[test]
fn probe_notifier_follows_a_channel_that_moved() {
    let store = world();
    let mut n = Notifier::new(THROTTLE_WINDOW);
    let t0 = Instant::now();
    store
        .set_notify(NODE, &ThreadId::Channel(3), NotifyMode::All)
        .unwrap();

    let a = arrive(&store, NODE, &ThreadId::Channel(3), "x one", 10);
    n.on_received(&store, Some(NODE), false, NODE, &[a], t0);
    let b = arrive(&store, NODE, &ThreadId::Channel(3), "x two", 11);
    n.on_received(&store, Some(NODE), false, NODE, &[b], secs(t0, 1));

    store.replace_channels(NODE, &[test_channel(5)]).unwrap();
    let posts = n.on_due(&store, Some(NODE), false, t0 + THROTTLE_WINDOW);
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].thread, ThreadId::Channel(5));
    assert_eq!((posts[0].id, posts[0].count), (b, 1));
}

// ── Store ───────────────────────────────────────────────────────────────────

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

fn test_channel(idx: u8) -> Channel {
    Channel {
        idx,
        name: "#test".into(),
        secret: text::hashtag_secret("#test").unwrap(),
    }
}

fn other_channel(idx: u8) -> Channel {
    Channel {
        idx,
        name: "#other".into(),
        secret: text::hashtag_secret("#other").unwrap(),
    }
}

fn incoming(thread: ThreadId, text: &str, ts: u32) -> NewMessage {
    NewMessage {
        thread: thread.clone(),
        direction: Direction::In,
        sender: match thread {
            ThreadId::Channel(_) => Some("Bob".into()),
            ThreadId::Contact(_) => None,
        },
        text: text.into(),
        sender_ts: ts,
        received_at: i64::from(ts),
        path_len: None,
        snr: None,
        status: DeliveryStatus::Received,
    }
}

#[test]
fn probe_store_mode_defaults_and_round_trips() {
    let store = Store::open_memory().unwrap();
    store.replace_channels(NODE, &[test_channel(3)]).unwrap();
    let dm = ThreadId::Contact(PEER.into());

    assert_eq!(
        store.notify_mode(NODE, &ThreadId::Channel(3)).unwrap(),
        NotifyMode::Default
    );
    assert_eq!(store.notify_mode(NODE, &dm).unwrap(), NotifyMode::Default);

    store
        .set_notify(NODE, &ThreadId::Channel(3), NotifyMode::Mute)
        .unwrap();
    store.set_notify(NODE, &dm, NotifyMode::All).unwrap();
    assert_eq!(
        store.notify_mode(NODE, &ThreadId::Channel(3)).unwrap(),
        NotifyMode::Mute
    );
    assert_eq!(store.notify_mode(NODE, &dm).unwrap(), NotifyMode::All);

    // Scoped by node.
    assert_eq!(
        store
            .notify_mode(OTHER_NODE, &ThreadId::Channel(3))
            .unwrap(),
        NotifyMode::Default
    );

    // Back to default.
    store.set_notify(NODE, &dm, NotifyMode::Default).unwrap();
    assert_eq!(store.notify_mode(NODE, &dm).unwrap(), NotifyMode::Default);
}

#[test]
fn probe_store_threads_carry_the_mode() {
    let store = Store::open_memory().unwrap();
    store.replace_channels(NODE, &[test_channel(3)]).unwrap();
    let dm = ThreadId::Contact(PEER.into());
    store
        .insert_message(NODE, &incoming(dm.clone(), "hi", 10))
        .unwrap();
    store.set_notify(NODE, &dm, NotifyMode::Mute).unwrap();

    let threads = store.threads(NODE).unwrap();
    let mode_of = |t: &ThreadId| threads.iter().find(|s| &s.thread == t).unwrap().notify;
    assert_eq!(mode_of(&dm), NotifyMode::Mute);
    assert_eq!(mode_of(&ThreadId::Channel(3)), NotifyMode::Default);
}

#[test]
fn probe_store_read_markers_keep_the_mode_and_mode_keeps_markers() {
    let store = Store::open_memory().unwrap();
    let dm = ThreadId::Contact(PEER.into());
    store
        .insert_message(NODE, &incoming(dm.clone(), "one", 10))
        .unwrap();
    store
        .insert_message(NODE, &incoming(dm.clone(), "two", 11))
        .unwrap();

    store.set_notify(NODE, &dm, NotifyMode::Mute).unwrap();
    store.mark_read(NODE, &dm).unwrap();
    assert_eq!(store.notify_mode(NODE, &dm).unwrap(), NotifyMode::Mute);
    store.mark_unread(NODE, &dm).unwrap();
    assert_eq!(store.notify_mode(NODE, &dm).unwrap(), NotifyMode::Mute);
    store.mark_all_read(NODE).unwrap();
    assert_eq!(store.notify_mode(NODE, &dm).unwrap(), NotifyMode::Mute);
    assert_eq!(store.unread_total(NODE).unwrap(), 0);

    // Changing the mode leaves the read marker alone.
    store.set_notify(NODE, &dm, NotifyMode::All).unwrap();
    assert_eq!(store.unread_total(NODE).unwrap(), 0);
}

#[test]
fn probe_store_mode_follows_channel_identity() {
    let store = Store::open_memory().unwrap();
    store.replace_channels(NODE, &[test_channel(3)]).unwrap();
    store
        .set_notify(NODE, &ThreadId::Channel(3), NotifyMode::Mute)
        .unwrap();

    // #test moves to slot 5; another channel takes slot 3.
    store
        .replace_channels(NODE, &[other_channel(3), test_channel(5)])
        .unwrap();
    assert_eq!(
        store.notify_mode(NODE, &ThreadId::Channel(5)).unwrap(),
        NotifyMode::Mute
    );
    assert_eq!(
        store.notify_mode(NODE, &ThreadId::Channel(3)).unwrap(),
        NotifyMode::Default
    );
}

/// The version 1 layout, as shipped before notifications.
const V1_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE IF NOT EXISTS devices(node_id TEXT PRIMARY KEY, info_json TEXT NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS contacts(node_id TEXT NOT NULL, public_key TEXT NOT NULL, name TEXT NOT NULL, kind INTEGER NOT NULL, path_len INTEGER NOT NULL, last_advert INTEGER NOT NULL, lat INTEGER NOT NULL, lon INTEGER NOT NULL, last_modified INTEGER NOT NULL, PRIMARY KEY(node_id, public_key));
CREATE TABLE IF NOT EXISTS channels(node_id TEXT NOT NULL, idx INTEGER NOT NULL, name TEXT NOT NULL, secret_hex TEXT NOT NULL, PRIMARY KEY(node_id, idx));
CREATE TABLE IF NOT EXISTS messages(id INTEGER PRIMARY KEY AUTOINCREMENT, node_id TEXT NOT NULL, thread_kind TEXT NOT NULL, thread_key TEXT NOT NULL, direction TEXT NOT NULL, sender TEXT, text TEXT NOT NULL, sender_ts INTEGER NOT NULL, received_at INTEGER NOT NULL, path_len INTEGER, snr REAL, status TEXT NOT NULL, ack_tag BLOB);
CREATE UNIQUE INDEX IF NOT EXISTS messages_dedup ON messages(node_id, thread_kind, thread_key, direction, sender_ts, ifnull(sender, ''), text);
CREATE INDEX IF NOT EXISTS messages_ack ON messages(node_id, ack_tag);
CREATE TABLE IF NOT EXISTS thread_state(node_id TEXT NOT NULL, thread_kind TEXT NOT NULL, thread_key TEXT NOT NULL, last_read_id INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(node_id, thread_kind, thread_key));
INSERT INTO meta(key, value) VALUES('schema_version', '1');
";

#[test]
fn probe_store_migrates_a_version_1_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v1.db");

    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(V1_SCHEMA).unwrap();
        conn.execute(
            "INSERT INTO messages(node_id, thread_kind, thread_key, direction, sender, text, \
             sender_ts, received_at, status) VALUES(?1, 'contact', ?2, 'in', NULL, 'old', 5, 5, \
             'received')",
            rusqlite::params![NODE, PEER],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO thread_state(node_id, thread_kind, thread_key, last_read_id) \
             VALUES(?1, 'contact', ?2, ?3)",
            rusqlite::params![NODE, PEER, id],
        )
        .unwrap();
    }

    let dm = ThreadId::Contact(PEER.into());
    {
        let store = Store::open(&path).unwrap();
        assert_eq!(store.notify_mode(NODE, &dm).unwrap(), NotifyMode::Default);
        assert_eq!(store.messages(NODE, &dm, 10).unwrap().len(), 1);
        // The read marker survived the migration.
        assert_eq!(store.unread_total(NODE).unwrap(), 0);
        store.set_notify(NODE, &dm, NotifyMode::Mute).unwrap();
    }

    let conn = rusqlite::Connection::open(&path).unwrap();
    let version: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(version, "3");
    drop(conn);

    // Reopening a version 3 file works, keeps the mode, and can archive.
    let store = Store::open(&path).unwrap();
    assert_eq!(store.notify_mode(NODE, &dm).unwrap(), NotifyMode::Mute);
    store.archive(NODE, &dm).unwrap();
    assert!(store.threads(NODE).unwrap().is_empty());
}

#[test]
fn probe_store_unread_incoming() {
    let store = Store::open_memory().unwrap();
    store.replace_channels(NODE, &[test_channel(3)]).unwrap();
    let a = store
        .insert_message(NODE, &incoming(dm(), "one", 10))
        .unwrap()
        .unwrap();
    let mut out = incoming(dm(), "mine", 11);
    out.direction = Direction::Out;
    out.status = DeliveryStatus::Sent;
    store.insert_message(NODE, &out).unwrap().unwrap();
    let b = store
        .insert_message(NODE, &incoming(dm(), "two", 12))
        .unwrap()
        .unwrap();

    let ids = |s: &Store| -> Vec<i64> {
        s.unread_incoming(NODE, &dm())
            .unwrap()
            .iter()
            .map(|m| m.id)
            .collect()
    };
    assert_eq!(ids(&store), vec![a, b]);
    store.mark_unread(NODE, &dm()).unwrap();
    assert_eq!(ids(&store), vec![b]);
    store.mark_read(NODE, &dm()).unwrap();
    assert!(ids(&store).is_empty());

    // A channel slot answers for the channel installed there now.
    let ch = ThreadId::Channel(3);
    let x = store
        .insert_message(NODE, &incoming(ch.clone(), "x", 20))
        .unwrap()
        .unwrap();
    assert_eq!(
        store
            .unread_incoming(NODE, &ch)
            .unwrap()
            .iter()
            .map(|m| m.id)
            .collect::<Vec<_>>(),
        vec![x]
    );
    store.replace_channels(NODE, &[other_channel(3)]).unwrap();
    assert!(store.unread_incoming(NODE, &ch).unwrap().is_empty());
}

// ── Session ─────────────────────────────────────────────────────────────────

fn fake() -> FakeRadio {
    let f = FakeRadio::new(device());
    f.state().channels = vec![test_channel(3)];
    f
}

fn cfg() -> ConnectionConfig {
    ConnectionConfig {
        auto_connect: true,
        ..ConnectionConfig::default()
    }
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

fn push(f: &FakeRadio, thread: ThreadId, text: &str, ts: u32) {
    f.push_incoming(IncomingMessage {
        sender: match thread {
            ThreadId::Channel(_) => Some("Bob".into()),
            ThreadId::Contact(_) => None,
        },
        thread,
        text: text.into(),
        sender_ts: ts,
        path_len: Some(1),
        snr: None,
    });
}

/// Collects updates until one `Received` arrives; returns it and whether a
/// `StoreChanged` came before it.
async fn next_received(
    rx: &mut tokio::sync::broadcast::Receiver<Update>,
) -> (String, Vec<i64>, bool) {
    let mut changed = false;

    loop {
        let got = tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .expect("no Received update")
            .unwrap();

        match got {
            Update::StoreChanged => changed = true,
            Update::Received { node, ids } => return (node, ids, changed),
            _ => {}
        }
    }
}

#[tokio::test(start_paused = true)]
async fn probe_session_announces_new_incoming_rows() {
    let f = fake();
    let store = Store::open_memory().unwrap();
    let s = Session::spawn(store.clone(), cfg(), Connector::Fake(f.clone()));
    let mut rx = s.subscribe();
    wait_for("connected", || matches!(s.state(), ConnState::Connected(_))).await;
    // Let the connect-time sync and drain finish, then discard their
    // updates, so a StoreChanged seen below belongs to the new drain.
    tokio::time::sleep(Duration::from_secs(1)).await;
    while rx.try_recv().is_ok() {}

    push(&f, ThreadId::Contact(PEER.into()), "hello", 100);
    let (node, ids, changed) = next_received(&mut rx).await;
    assert_eq!(node, NODE);
    assert!(changed, "StoreChanged must precede Received");
    assert_eq!(ids.len(), 1);
    let m = store.message(NODE, ids[0]).unwrap().unwrap();
    assert_eq!(m.text, "hello");

    // A duplicate inserts nothing and announces nothing; the next new row
    // is announced on its own.
    push(&f, ThreadId::Contact(PEER.into()), "hello", 100);
    tokio::time::sleep(Duration::from_secs(1)).await;
    while let Ok(u) = rx.try_recv() {
        assert!(
            !matches!(u, Update::Received { .. }),
            "a duplicate must not be announced: {u:?}"
        );
    }
    push(&f, ThreadId::Channel(3), "fresh", 101);
    let (_, ids, changed) = next_received(&mut rx).await;
    assert!(changed, "StoreChanged must precede Received");
    assert_eq!(ids.len(), 1);
    assert_eq!(store.message(NODE, ids[0]).unwrap().unwrap().text, "fresh");
}
