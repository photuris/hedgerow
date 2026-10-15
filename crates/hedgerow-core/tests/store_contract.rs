//! Store contract tests adopted from the task 001 review: WAL, rollback,
//! duplicate channel secrets, and node-scoped read markers.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hedgerow_core::{model::*, store::Store};

fn incoming(idx: u8) -> NewMessage {
    NewMessage {
        thread: ThreadId::Channel(idx),
        direction: Direction::In,
        sender: Some("x".into()),
        text: "body".into(),
        sender_ts: 1,
        received_at: 1,
        path_len: None,
        snr: None,
        status: DeliveryStatus::Received,
    }
}
fn contact(key: &str, name: &str) -> Contact {
    Contact {
        public_key: key.into(),
        name: name.into(),
        kind: ContactKind::Chat,
        path_len: -1,
        last_advert: 0,
        lat: 0,
        lon: 0,
        last_modified: 0,
    }
}
#[test]
fn wal_is_enabled() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("db");
    let s = Store::open(&p).unwrap();
    let conn = rusqlite::Connection::open(&p).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    drop(s);
}
#[test]
fn duplicate_secret_rows_resolve_to_lowest_slot() {
    let s = Store::open_memory().unwrap();
    s.replace_channels(
        "node",
        &[
            Channel {
                idx: 1,
                name: "a".into(),
                secret: [1; 16],
            },
            Channel {
                idx: 7,
                name: "b".into(),
                secret: [1; 16],
            },
        ],
    )
    .unwrap();
    let id = s.insert_message("node", &incoming(7)).unwrap().unwrap();
    assert_eq!(
        s.message("node", id).unwrap().unwrap().thread,
        ThreadId::Channel(1)
    );
    assert_eq!(
        s.messages("node", &ThreadId::Channel(1), 10).unwrap().len(),
        1
    );
    assert!(
        s.messages("node", &ThreadId::Channel(7), 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(s.unread_total("node").unwrap(), 1);
    assert_eq!(s.threads("node").unwrap().len(), 2);
    s.mark_read("node", &ThreadId::Channel(7)).unwrap();
    assert_eq!(s.unread_total("node").unwrap(), 0);
    s.replace_channels(
        "node",
        &[Channel {
            idx: 7,
            name: "b".into(),
            secret: [1; 16],
        }],
    )
    .unwrap();
    assert_eq!(
        s.message("node", id).unwrap().unwrap().thread,
        ThreadId::Channel(7)
    );
}
#[test]
fn replace_contacts_rolls_back_on_write_error() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("db");
    let s = Store::open(&p).unwrap();
    let old = vec![contact("old", "old")];
    s.replace_contacts("node", &old).unwrap();
    let conn = rusqlite::Connection::open(&p).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_contact BEFORE INSERT ON contacts WHEN NEW.name = 'bad' BEGIN SELECT RAISE(ABORT, 'reject'); END;").unwrap();
    assert!(
        s.replace_contacts("node", &[contact("ok", "ok"), contact("bad", "bad")])
            .is_err()
    );
    assert_eq!(s.contacts("node").unwrap(), old);
}
#[test]
fn replace_channels_rolls_back_on_duplicate_index() {
    let s = Store::open_memory().unwrap();
    let old = vec![Channel {
        idx: 1,
        name: "old".into(),
        secret: [1; 16],
    }];
    s.replace_channels("node", &old).unwrap();
    let dupe = Channel {
        idx: 2,
        name: "new".into(),
        secret: [2; 16],
    };
    assert!(s.replace_channels("node", &[dupe.clone(), dupe]).is_err());
    assert_eq!(s.channels("node").unwrap(), old);
}
#[test]
fn mark_all_read_includes_fallback_keys_and_is_node_scoped() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("db");
    let s = Store::open(&p).unwrap();
    let id = s.insert_message("node", &incoming(6)).unwrap().unwrap();
    let other = s.insert_message("other", &incoming(6)).unwrap().unwrap();
    s.replace_channels(
        "node",
        &[Channel {
            idx: 6,
            name: "occupant".into(),
            secret: [6; 16],
        }],
    )
    .unwrap();
    s.mark_all_read("node").unwrap();
    let conn = rusqlite::Connection::open(&p).unwrap();
    let last: i64 = conn
        .query_row(
            "SELECT last_read_id FROM thread_state WHERE node_id='node' AND thread_key='idx:6'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(last, id);
    let other_marker: i64 = conn.query_row("SELECT coalesce((SELECT last_read_id FROM thread_state WHERE node_id='other' AND thread_key='idx:6'),0)", [], |r| r.get(0)).unwrap();
    assert_eq!(other_marker, 0);
    assert!(other > id);
}
