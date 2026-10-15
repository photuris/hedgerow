//! SQLite-backed local history: devices, contacts, channels, and messages.
//!
//! Every row is scoped by `node_id`, the 64-hex public key of the radio.
//! The store is the only copy of messages already drained from the radio's
//! queue, so it is history, not a cache: future schema versions migrate
//! forward and tables are never dropped.
//!
//! Channel conversations are keyed by the slot's occupant, not the slot
//! index: the stored thread key of a channel is the 32-hex secret of the
//! channel currently stored at that slot, or `idx:<n>` when the slot has no
//! stored channel. Every method taking a channel [`ThreadId`] resolves it
//! that way at call time.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

use crate::error::{Error, Result};
use crate::model::{
    Channel, Contact, ContactKind, DeliveryStatus, DeviceInfo, Direction, Message, NewMessage,
    ThreadId, ThreadSummary,
};
use crate::text;

/// Version of the table layout below; stored in `meta` as `schema_version`.
const SCHEMA_VERSION: u32 = 1;

/// Prefix of the fallback thread key for a slot with no stored channel.
const SLOT_KEY_PREFIX: &str = "idx:";

/// Database schema for version 1; every statement is idempotent.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS devices(node_id TEXT PRIMARY KEY, info_json TEXT NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS contacts(node_id TEXT NOT NULL, public_key TEXT NOT NULL, name TEXT NOT NULL, kind INTEGER NOT NULL, path_len INTEGER NOT NULL, last_advert INTEGER NOT NULL, lat INTEGER NOT NULL, lon INTEGER NOT NULL, last_modified INTEGER NOT NULL, PRIMARY KEY(node_id, public_key));
CREATE TABLE IF NOT EXISTS channels(node_id TEXT NOT NULL, idx INTEGER NOT NULL, name TEXT NOT NULL, secret_hex TEXT NOT NULL, PRIMARY KEY(node_id, idx));
CREATE TABLE IF NOT EXISTS messages(id INTEGER PRIMARY KEY AUTOINCREMENT, node_id TEXT NOT NULL, thread_kind TEXT NOT NULL, thread_key TEXT NOT NULL, direction TEXT NOT NULL, sender TEXT, text TEXT NOT NULL, sender_ts INTEGER NOT NULL, received_at INTEGER NOT NULL, path_len INTEGER, snr REAL, status TEXT NOT NULL, ack_tag BLOB);
CREATE UNIQUE INDEX IF NOT EXISTS messages_dedup ON messages(node_id, thread_kind, thread_key, direction, sender_ts, ifnull(sender, ''), text);
CREATE INDEX IF NOT EXISTS messages_ack ON messages(node_id, ack_tag);
CREATE TABLE IF NOT EXISTS thread_state(node_id TEXT NOT NULL, thread_kind TEXT NOT NULL, thread_key TEXT NOT NULL, last_read_id INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(node_id, thread_kind, thread_key));
";

/// Handle to the database; clones share one connection.
#[derive(Clone)]
pub struct Store {
    /// The single connection, serialised by the mutex.
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    /// Opens (creating when missing) the database file at `path`.
    ///
    /// Creates the parent directory when it does not exist and switches
    /// the file to WAL mode.
    ///
    /// # Errors
    ///
    /// Returns [`Error::SchemaTooNew`] when the file was written by a newer
    /// build, and [`Error::Io`] or [`Error::Db`] on other failures.
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        let mut conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        init_schema(&mut conn)?;

        Ok(Store::from_connection(conn))
    }

    /// Opens a private in-memory database, for tests and UI-test mode.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Db`] when SQLite fails to initialise.
    pub fn open_memory() -> Result<Store> {
        let mut conn = Connection::open_in_memory()?;
        init_schema(&mut conn)?;

        Ok(Store::from_connection(conn))
    }

    /// Wraps an initialised connection.
    fn from_connection(conn: Connection) -> Store {
        Store {
            conn: Arc::new(Mutex::new(conn)),
        }
    }

    /// Locks the connection; a poisoned lock is recovered because every
    /// multi-statement write runs in a transaction.
    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    // ── Devices ─────────────────────────────────────────────────────────────

    /// Stores the device record and makes it the most recent node.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Db`] on SQLite failure.
    pub fn save_device(&self, info: &DeviceInfo) -> Result<()> {
        let json = serde_json::to_string(info)
            .map_err(|e| Error::Invalid(format!("device record: {e}")))?;
        let mut conn = self.lock();
        let tx = conn.transaction()?;

        tx.execute(
            "INSERT INTO devices(node_id, info_json, updated_at) VALUES(?1, ?2, ?3) \
             ON CONFLICT(node_id) DO UPDATE SET info_json = excluded.info_json, \
             updated_at = excluded.updated_at",
            params![info.node_id, json, now_secs()],
        )?;
        tx.execute(
            "INSERT INTO meta(key, value) VALUES('last_node', ?1) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![info.node_id],
        )?;
        tx.commit()?;

        Ok(())
    }

    /// Returns the stored device record for `node`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Db`] on SQLite failure and [`Error::Invalid`] when
    /// the stored record cannot be decoded.
    pub fn device(&self, node: &str) -> Result<Option<DeviceInfo>> {
        let json: Option<String> = self
            .lock()
            .query_row(
                "SELECT info_json FROM devices WHERE node_id = ?1",
                params![node],
                |r| r.get(0),
            )
            .optional()?;

        json.map(|j| {
            serde_json::from_str(&j).map_err(|e| Error::Invalid(format!("device record: {e}")))
        })
        .transpose()
    }

    /// Returns the node id of the most recently saved device.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Db`] on SQLite failure.
    pub fn last_node(&self) -> Result<Option<String>> {
        let value: Option<Option<String>> = self
            .lock()
            .query_row("SELECT value FROM meta WHERE key = 'last_node'", [], |r| {
                r.get(0)
            })
            .optional()?;

        Ok(value.flatten())
    }

    // ── Contacts ────────────────────────────────────────────────────────────

    /// Replaces all contacts of `node` in one transaction.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Db`] on SQLite failure; nothing changes then.
    pub fn replace_contacts(&self, node: &str, contacts: &[Contact]) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;

        tx.execute("DELETE FROM contacts WHERE node_id = ?1", params![node])?;
        write_contacts(&tx, node, contacts)?;
        tx.commit()?;

        Ok(())
    }

    /// Inserts or updates contacts of `node` by public key.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Db`] on SQLite failure; nothing changes then.
    pub fn upsert_contacts(&self, node: &str, contacts: &[Contact]) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;

        write_contacts(&tx, node, contacts)?;
        tx.commit()?;

        Ok(())
    }

    /// Deletes one contact of `node`; a missing contact is not an error.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Db`] on SQLite failure.
    pub fn remove_contact(&self, node: &str, public_key: &str) -> Result<()> {
        self.lock().execute(
            "DELETE FROM contacts WHERE node_id = ?1 AND public_key = ?2",
            params![node, public_key],
        )?;

        Ok(())
    }

    /// Lists the contacts of `node`, ordered by name ignoring case.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Db`] on SQLite failure.
    pub fn contacts(&self, node: &str) -> Result<Vec<Contact>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT public_key, name, kind, path_len, last_advert, lat, lon, last_modified \
             FROM contacts WHERE node_id = ?1 ORDER BY name COLLATE NOCASE, public_key",
        )?;
        let rows = stmt.query_map(params![node], |r| {
            Ok(Contact {
                public_key: r.get(0)?,
                name: r.get(1)?,
                kind: ContactKind::from_u8(r.get(2)?),
                path_len: r.get(3)?,
                last_advert: r.get(4)?,
                lat: r.get(5)?,
                lon: r.get(6)?,
                last_modified: r.get(7)?,
            })
        })?;

        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Returns the newest `last_modified` among the contacts of `node`, or 0.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Db`] on SQLite failure.
    pub fn max_contact_modified(&self, node: &str) -> Result<u32> {
        let max: Option<u32> = self.lock().query_row(
            "SELECT max(last_modified) FROM contacts WHERE node_id = ?1",
            params![node],
            |r| r.get(0),
        )?;

        Ok(max.unwrap_or(0))
    }

    // ── Channels ────────────────────────────────────────────────────────────

    /// Replaces all channels of `node` in one transaction.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Db`] on SQLite failure; nothing changes then.
    pub fn replace_channels(&self, node: &str, channels: &[Channel]) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;

        tx.execute("DELETE FROM channels WHERE node_id = ?1", params![node])?;

        {
            let mut stmt = tx.prepare(
                "INSERT INTO channels(node_id, idx, name, secret_hex) VALUES(?1, ?2, ?3, ?4)",
            )?;

            for ch in channels {
                stmt.execute(params![node, ch.idx, ch.name, text::to_hex(&ch.secret)])?;
            }
        }

        tx.commit()?;

        Ok(())
    }

    /// Lists the channels of `node`, ordered by slot index.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Db`] on SQLite failure and [`Error::Invalid`] when a
    /// stored secret is malformed.
    pub fn channels(&self, node: &str) -> Result<Vec<Channel>> {
        let conn = self.lock();

        stored_channels(&conn, node)?
            .into_iter()
            .map(|(idx, name, secret_hex)| {
                Ok(Channel {
                    idx,
                    name,
                    secret: text::parse_secret(&secret_hex)?,
                })
            })
            .collect()
    }

    // ── Messages ────────────────────────────────────────────────────────────

    /// Stores a message; `None` means an identical message already exists.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Db`] on SQLite failure.
    pub fn insert_message(&self, node: &str, m: &NewMessage) -> Result<Option<i64>> {
        let conn = self.lock();
        let (kind, key) = thread_key(&conn, node, &m.thread)?;
        let inserted = conn.execute(
            "INSERT INTO messages(node_id, thread_kind, thread_key, direction, sender, text, \
             sender_ts, received_at, path_len, snr, status) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) ON CONFLICT DO NOTHING",
            params![
                node,
                kind,
                key,
                direction_str(m.direction),
                m.sender,
                m.text,
                m.sender_ts,
                m.received_at,
                m.path_len,
                m.snr.map(f64::from),
                m.status.as_str(),
            ],
        )?;

        if inserted == 0 {
            return Ok(None);
        }

        Ok(Some(conn.last_insert_rowid()))
    }

    /// Returns the newest `limit` messages of `thread`, oldest first.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Db`] on SQLite failure.
    pub fn messages(&self, node: &str, thread: &ThreadId, limit: u32) -> Result<Vec<Message>> {
        let conn = self.lock();
        let (kind, key) = thread_key(&conn, node, thread)?;

        if row_thread(&conn, node, kind, &key)?.as_ref() != Some(thread) {
            return Ok(Vec::new());
        }

        let mut stmt = conn.prepare(&format!(
            "SELECT {MESSAGE_COLS} FROM messages \
             WHERE node_id = ?1 AND thread_kind = ?2 AND thread_key = ?3 \
             ORDER BY id DESC LIMIT ?4"
        ))?;
        let rows = stmt.query_map(params![node, kind, key, limit], |r| {
            message_from_row(r, thread.clone())
        })?;
        let mut messages: Vec<Message> = rows.collect::<rusqlite::Result<_>>()?;

        messages.reverse();

        Ok(messages)
    }

    /// Returns one message of `node`; `None` for another node's message or
    /// a channel row that is not addressable.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Db`] on SQLite failure.
    pub fn message(&self, node: &str, id: i64) -> Result<Option<Message>> {
        let conn = self.lock();
        let found = conn
            .query_row(
                &format!(
                    "SELECT {MESSAGE_COLS}, thread_kind, thread_key FROM messages \
                     WHERE node_id = ?1 AND id = ?2"
                ),
                params![node, id],
                |r| {
                    let kind: String = r.get(9)?;
                    let key: String = r.get(10)?;

                    Ok((message_from_row(r, ThreadId::Channel(0))?, kind, key))
                },
            )
            .optional()?;
        let Some((mut message, kind, key)) = found else {
            return Ok(None);
        };
        let Some(thread) = row_thread(&conn, node, &kind, &key)? else {
            return Ok(None);
        };

        message.thread = thread;

        Ok(Some(message))
    }

    /// Finds the addressable messages of `node` whose text or sender contains
    /// `query`, newest first, at most `limit` of them, each with its resolved
    /// `thread`.
    ///
    /// The query is trimmed; an empty one finds nothing. The match is a SQL
    /// `LIKE` with `%`, `_`, and `\` in the query taken literally, so it
    /// ignores case for ASCII letters only. A channel row counts only when
    /// its key is the current key of some slot; contact rows always count.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Db`] on SQLite failure.
    pub fn search(&self, node: &str, query: &str, limit: u32) -> Result<Vec<Message>> {
        let query = query.trim();

        if query.is_empty() {
            return Ok(Vec::new());
        }

        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        let pattern = format!("%{}%", escape_like(query));
        let conn = self.lock();
        // Addressability is decided per row by `row_thread` below.
        let mut stmt = conn.prepare(&format!(
            "SELECT {MESSAGE_COLS}, thread_kind, thread_key FROM messages \
             WHERE node_id = ?1 AND (text LIKE ?2 ESCAPE '\\' OR sender LIKE ?2 ESCAPE '\\') \
             ORDER BY id DESC"
        ))?;
        let mut rows = stmt.query(params![node, pattern])?;
        let mut found = Vec::new();

        while found.len() < limit
            && let Some(r) = rows.next()?
        {
            let kind: String = r.get(9)?;
            let key: String = r.get(10)?;
            let Some(thread) = row_thread(&conn, node, &kind, &key)? else {
                continue;
            };

            found.push(message_from_row(r, thread)?);
        }

        Ok(found)
    }

    /// Sets a message's delivery status; `ack` of `None` keeps the ack tag.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Db`] on SQLite failure.
    pub fn set_status(&self, id: i64, status: DeliveryStatus, ack: Option<[u8; 4]>) -> Result<()> {
        self.lock().execute(
            "UPDATE messages SET status = ?1, ack_tag = coalesce(?2, ack_tag) WHERE id = ?3",
            params![status.as_str(), ack.map(|a| a.to_vec()), id],
        )?;

        Ok(())
    }

    /// Finds the newest message of `node` carrying the ack tag.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Db`] on SQLite failure.
    pub fn message_by_ack(&self, node: &str, ack: [u8; 4]) -> Result<Option<i64>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT id FROM messages WHERE node_id = ?1 AND ack_tag = ?2 \
                 ORDER BY id DESC LIMIT 1",
                params![node, ack.to_vec()],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Fails outgoing messages that can no longer finish; returns the count.
    ///
    /// Outgoing rows still `Pending`, and outgoing contact rows still
    /// `Sent` (their ack never came), become `Failed`. Sent channel rows
    /// stay `Sent`: channel text has no acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Db`] on SQLite failure.
    pub fn fail_unfinished(&self, node: &str) -> Result<u32> {
        let changed = self.lock().execute(
            "UPDATE messages SET status = ?1 WHERE node_id = ?2 AND direction = 'out' \
             AND (status = ?3 OR (status = ?4 AND thread_kind = 'contact'))",
            params![
                DeliveryStatus::Failed.as_str(),
                node,
                DeliveryStatus::Pending.as_str(),
                DeliveryStatus::Sent.as_str(),
            ],
        )?;

        Ok(u32::try_from(changed).unwrap_or(u32::MAX))
    }

    // ── Threads and read state ──────────────────────────────────────────────

    /// Lists conversations: every stored channel by slot, then contact
    /// threads with messages, newest first.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Db`] on SQLite failure.
    pub fn threads(&self, node: &str) -> Result<Vec<ThreadSummary>> {
        let conn = self.lock();
        let mut threads = Vec::new();

        for (idx, name, secret_hex) in stored_channels(&conn, node)? {
            threads.push(summarize(
                &conn,
                node,
                ThreadId::Channel(idx),
                "channel",
                &secret_hex,
                name,
            )?);
        }

        let mut stmt = conn.prepare(
            "SELECT DISTINCT thread_key FROM messages WHERE node_id = ?1 AND thread_kind = 'contact'",
        )?;
        let keys: Vec<String> = stmt
            .query_map(params![node], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut contact_threads = Vec::new();

        for key in keys {
            let title = contact_name(&conn, node, &key)?.unwrap_or_else(|| key.clone());

            contact_threads.push(summarize(
                &conn,
                node,
                ThreadId::Contact(key.clone()),
                "contact",
                &key,
                title,
            )?);
        }

        contact_threads.sort_by(|a, b| {
            b.last_at
                .cmp(&a.last_at)
                .then_with(|| a.thread.key().cmp(&b.thread.key()))
        });
        threads.extend(contact_threads);

        Ok(threads)
    }

    /// Marks every message of `thread` read.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Db`] on SQLite failure.
    pub fn mark_read(&self, node: &str, thread: &ThreadId) -> Result<()> {
        let conn = self.lock();
        let (kind, key) = thread_key(&conn, node, thread)?;

        conn.execute(
            "INSERT INTO thread_state(node_id, thread_kind, thread_key, last_read_id) \
             VALUES(?1, ?2, ?3, coalesce((SELECT max(id) FROM messages \
             WHERE node_id = ?1 AND thread_kind = ?2 AND thread_key = ?3), 0)) \
             ON CONFLICT(node_id, thread_kind, thread_key) \
             DO UPDATE SET last_read_id = excluded.last_read_id",
            params![node, kind, key],
        )?;

        Ok(())
    }

    /// Makes exactly the newest incoming message of `thread` unread; does
    /// nothing when the thread has no incoming message.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Db`] on SQLite failure.
    pub fn mark_unread(&self, node: &str, thread: &ThreadId) -> Result<()> {
        let conn = self.lock();
        let (kind, key) = thread_key(&conn, node, thread)?;
        let newest_in: Option<i64> = conn.query_row(
            "SELECT max(id) FROM messages \
             WHERE node_id = ?1 AND thread_kind = ?2 AND thread_key = ?3 AND direction = 'in'",
            params![node, kind, key],
            |r| r.get(0),
        )?;
        let Some(newest_in) = newest_in else {
            return Ok(());
        };

        conn.execute(
            "INSERT INTO thread_state(node_id, thread_kind, thread_key, last_read_id) \
             VALUES(?1, ?2, ?3, ?4) \
             ON CONFLICT(node_id, thread_kind, thread_key) \
             DO UPDATE SET last_read_id = excluded.last_read_id",
            params![node, kind, key, newest_in - 1],
        )?;

        Ok(())
    }

    /// Marks every conversation key with messages read, hidden ones included,
    /// so a channel re-added later shows no old unread messages.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Db`] on SQLite failure.
    pub fn mark_all_read(&self, node: &str) -> Result<()> {
        self.lock().execute(
            "INSERT INTO thread_state(node_id, thread_kind, thread_key, last_read_id) \
             SELECT node_id, thread_kind, thread_key, max(id) FROM messages \
             WHERE node_id = ?1 GROUP BY node_id, thread_kind, thread_key \
             ON CONFLICT(node_id, thread_kind, thread_key) \
             DO UPDATE SET last_read_id = excluded.last_read_id",
            params![node],
        )?;

        Ok(())
    }

    /// Counts unread incoming messages in contact threads and stored channels.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Db`] on SQLite failure.
    pub fn unread_total(&self, node: &str) -> Result<u32> {
        Ok(self.lock().query_row(
            "SELECT count(*) FROM messages m WHERE m.node_id = ?1 AND m.direction = 'in' \
             AND m.id > coalesce((SELECT s.last_read_id FROM thread_state s \
             WHERE s.node_id = m.node_id AND s.thread_kind = m.thread_kind \
             AND s.thread_key = m.thread_key), 0) \
             AND (m.thread_kind = 'contact' OR m.thread_key IN \
             (SELECT secret_hex FROM channels WHERE node_id = m.node_id))",
            params![node],
            |r| r.get(0),
        )?)
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Column list matching [`message_from_row`].
const MESSAGE_COLS: &str =
    "id, direction, sender, text, sender_ts, received_at, path_len, snr, status";

/// Creates the schema in a fresh database, or checks the version of an
/// existing one.
fn init_schema(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    tx.execute_batch("CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);")?;

    let found: Option<String> = tx
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();

    match found {
        None => {
            tx.execute_batch(SCHEMA)?;
            tx.execute(
                "INSERT INTO meta(key, value) VALUES('schema_version', ?1)",
                params![SCHEMA_VERSION.to_string()],
            )?;
        }
        Some(v) => {
            let found: u32 = v
                .parse()
                .map_err(|_| Error::Invalid(format!("schema_version {v:?} is not a number")))?;

            if found > SCHEMA_VERSION {
                return Err(Error::SchemaTooNew {
                    found,
                    supported: SCHEMA_VERSION,
                });
            }

            if found != SCHEMA_VERSION {
                return Err(Error::Invalid(format!("unknown schema_version {found}")));
            }
        }
    }

    tx.commit()?;

    Ok(())
}

/// Returns the current Unix time in seconds.
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Writes contacts with upsert semantics inside an open transaction.
fn write_contacts(conn: &Connection, node: &str, contacts: &[Contact]) -> Result<()> {
    let mut stmt = conn.prepare(
        "INSERT INTO contacts(node_id, public_key, name, kind, path_len, last_advert, lat, lon, \
         last_modified) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
         ON CONFLICT(node_id, public_key) DO UPDATE SET name = excluded.name, \
         kind = excluded.kind, path_len = excluded.path_len, last_advert = excluded.last_advert, \
         lat = excluded.lat, lon = excluded.lon, last_modified = excluded.last_modified",
    )?;

    for c in contacts {
        stmt.execute(params![
            node,
            c.public_key,
            c.name,
            c.kind.as_u8(),
            c.path_len,
            c.last_advert,
            c.lat,
            c.lon,
            c.last_modified,
        ])?;
    }

    Ok(())
}

/// Reads `(idx, name, secret_hex)` of every stored channel, by slot.
fn stored_channels(conn: &Connection, node: &str) -> Result<Vec<(u8, String, String)>> {
    let mut stmt =
        conn.prepare("SELECT idx, name, secret_hex FROM channels WHERE node_id = ?1 ORDER BY idx")?;
    let rows = stmt.query_map(params![node], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;

    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Resolves a thread to its stored `(thread_kind, thread_key)`.
///
/// A channel's key is the secret of the channel stored at its slot, or
/// `idx:<n>` when the slot is empty.
fn thread_key(conn: &Connection, node: &str, thread: &ThreadId) -> Result<(&'static str, String)> {
    let ThreadId::Channel(idx) = thread else {
        return Ok((thread.kind(), thread.key()));
    };
    let secret: Option<String> = conn
        .query_row(
            "SELECT secret_hex FROM channels WHERE node_id = ?1 AND idx = ?2",
            params![node, idx],
            |r| r.get(0),
        )
        .optional()?;

    Ok((
        thread.kind(),
        secret.unwrap_or_else(|| format!("{SLOT_KEY_PREFIX}{idx}")),
    ))
}

/// Maps a stored row key back to the thread it is addressable as, or `None`
/// for a channel key that no slot currently uses.
fn row_thread(conn: &Connection, node: &str, kind: &str, key: &str) -> Result<Option<ThreadId>> {
    if kind != "channel" {
        return Ok(Some(ThreadId::Contact(key.to_string())));
    }

    if let Some(slot) = key.strip_prefix(SLOT_KEY_PREFIX) {
        let Ok(idx) = slot.parse::<u8>() else {
            return Ok(None);
        };
        let occupied: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM channels WHERE node_id = ?1 AND idx = ?2)",
            params![node, idx],
            |r| r.get(0),
        )?;

        return Ok((!occupied).then_some(ThreadId::Channel(idx)));
    }

    let lowest: Option<u8> = conn.query_row(
        "SELECT min(idx) FROM channels WHERE node_id = ?1 AND secret_hex = ?2",
        params![node, key],
        |r| r.get(0),
    )?;

    Ok(lowest.map(ThreadId::Channel))
}

/// Escapes `%`, `_`, and `\` so a `LIKE ... ESCAPE '\'` pattern matches
/// `text` literally.
fn escape_like(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());

    for c in text.chars() {
        if matches!(c, '%' | '_' | '\\') {
            escaped.push('\\');
        }

        escaped.push(c);
    }

    escaped
}

/// Builds a [`Message`] from [`MESSAGE_COLS`] and an already-known thread.
fn message_from_row(r: &Row<'_>, thread: ThreadId) -> rusqlite::Result<Message> {
    let direction: String = r.get(1)?;
    let status: String = r.get(8)?;
    let snr: Option<f64> = r.get(7)?;

    Ok(Message {
        id: r.get(0)?,
        thread,
        direction: parse_direction(&direction).ok_or_else(|| bad_value(1, &direction))?,
        sender: r.get(2)?,
        text: r.get(3)?,
        sender_ts: r.get(4)?,
        received_at: r.get(5)?,
        path_len: r.get(6)?,
        snr: snr.map(|v| v as f32),
        status: DeliveryStatus::parse(&status).ok_or_else(|| bad_value(8, &status))?,
    })
}

/// Builds the error for an unrecognised enum string in column `col`.
fn bad_value(col: usize, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        col,
        Type::Text,
        format!("unrecognised value {value:?}").into(),
    )
}

/// Returns the database spelling of a direction.
fn direction_str(d: Direction) -> &'static str {
    match d {
        Direction::In => "in",
        Direction::Out => "out",
    }
}

/// Parses the output of [`direction_str`].
fn parse_direction(s: &str) -> Option<Direction> {
    match s {
        "in" => Some(Direction::In),
        "out" => Some(Direction::Out),
        _ => None,
    }
}

/// Finds the name of the contact whose public key starts with `prefix`.
fn contact_name(conn: &Connection, node: &str, prefix: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT name FROM contacts WHERE node_id = ?1 \
             AND substr(public_key, 1, length(?2)) = ?2 ORDER BY public_key LIMIT 1",
            params![node, prefix],
            |r| r.get(0),
        )
        .optional()?)
}

/// Builds the list row for one conversation key.
fn summarize(
    conn: &Connection,
    node: &str,
    thread: ThreadId,
    kind: &str,
    key: &str,
    title: String,
) -> Result<ThreadSummary> {
    let last: Option<(String, i64)> = conn
        .query_row(
            "SELECT text, received_at FROM messages \
             WHERE node_id = ?1 AND thread_kind = ?2 AND thread_key = ?3 \
             ORDER BY id DESC LIMIT 1",
            params![node, kind, key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let unread: u32 = conn.query_row(
        "SELECT count(*) FROM messages WHERE node_id = ?1 AND thread_kind = ?2 \
         AND thread_key = ?3 AND direction = 'in' AND id > coalesce((SELECT last_read_id \
         FROM thread_state WHERE node_id = ?1 AND thread_kind = ?2 AND thread_key = ?3), 0)",
        params![node, kind, key],
        |r| r.get(0),
    )?;
    let (last_text, last_at) = match last {
        Some((text, at)) => (Some(text), Some(at)),
        None => (None, None),
    };

    Ok(ThreadSummary {
        thread,
        title,
        last_text,
        last_at,
        unread,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NODE: &str = "aa00000000000000000000000000000000000000000000000000000000000000";

    fn channel_message(idx: u8, body: &str) -> NewMessage {
        NewMessage {
            thread: ThreadId::Channel(idx),
            direction: Direction::In,
            sender: Some("Al".into()),
            text: body.into(),
            sender_ts: 10,
            received_at: 15,
            path_len: None,
            snr: None,
            status: DeliveryStatus::Received,
        }
    }

    mod search {
        use super::*;

        /// An incoming message on `thread` from `sender` with a unique clock.
        fn said(thread: ThreadId, sender: Option<&str>, text: &str, ts: u32) -> NewMessage {
            NewMessage {
                thread,
                direction: Direction::In,
                sender: sender.map(str::to_string),
                text: text.into(),
                sender_ts: ts,
                received_at: i64::from(ts),
                path_len: None,
                snr: None,
                status: DeliveryStatus::Received,
            }
        }

        /// A store with the stored channel `#a` in slot 0.
        fn store_with_channel() -> Store {
            let s = Store::open_memory().unwrap();

            s.replace_channels(
                NODE,
                &[Channel {
                    idx: 0,
                    name: "#a".into(),
                    secret: [1; 16],
                }],
            )
            .unwrap();

            s
        }

        /// The texts of the hits for `query`, in order.
        fn texts(s: &Store, query: &str) -> Vec<String> {
            s.search(NODE, query, 100)
                .unwrap()
                .into_iter()
                .map(|m| m.text)
                .collect()
        }

        #[test]
        fn should_match_the_text() {
            let s = store_with_channel();

            s.insert_message(NODE, &said(ThreadId::Channel(0), None, "net at eight", 1))
                .unwrap();
            s.insert_message(NODE, &said(ThreadId::Channel(0), None, "lunch", 2))
                .unwrap();

            assert_eq!(texts(&s, "at eig"), ["net at eight"]);
        }

        #[test]
        fn should_match_the_sender() {
            let s = store_with_channel();

            s.insert_message(NODE, &said(ThreadId::Channel(0), Some("Carol"), "hi", 1))
                .unwrap();
            s.insert_message(NODE, &said(ThreadId::Channel(0), Some("Bob"), "yo", 2))
                .unwrap();

            assert_eq!(texts(&s, "aro"), ["hi"]);
        }

        #[test]
        fn should_ignore_ascii_case() {
            let s = store_with_channel();

            s.insert_message(
                NODE,
                &said(ThreadId::Channel(0), Some("Dave"), "Antenna", 1),
            )
            .unwrap();

            assert_eq!(texts(&s, "ANTENNA"), ["Antenna"]);
            assert_eq!(texts(&s, "dAvE"), ["Antenna"]);
        }

        #[test]
        fn should_treat_percent_and_underscore_literally() {
            let s = store_with_channel();

            for (ts, body) in [(1, "50% off"), (2, "a_b"), (3, "axb"), (4, "plain")] {
                s.insert_message(NODE, &said(ThreadId::Channel(0), None, body, ts))
                    .unwrap();
            }

            assert_eq!(texts(&s, "%"), ["50% off"]);
            assert_eq!(texts(&s, "_"), ["a_b"]);
            assert_eq!(texts(&s, "a_b"), ["a_b"]);
        }

        #[test]
        fn should_treat_a_backslash_literally() {
            let s = store_with_channel();

            for (ts, body) in [(1, r"C:\temp"), (2, "C:temp")] {
                s.insert_message(NODE, &said(ThreadId::Channel(0), None, body, ts))
                    .unwrap();
            }

            assert_eq!(texts(&s, r"\"), [r"C:\temp"]);
        }

        #[test]
        fn should_exclude_another_nodes_rows() {
            let s = store_with_channel();
            let other = "bb00000000000000000000000000000000000000000000000000000000000000";

            s.insert_message(NODE, &said(ThreadId::Channel(0), None, "shared word", 1))
                .unwrap();
            s.replace_channels(
                other,
                &[Channel {
                    idx: 0,
                    name: "#a".into(),
                    secret: [1; 16],
                }],
            )
            .unwrap();
            s.insert_message(other, &said(ThreadId::Channel(0), None, "shared word", 2))
                .unwrap();

            // The other radio holds the same channel, so only the node
            // filter keeps its row out of NODE's results.

            assert_eq!(s.search(NODE, "shared", 10).unwrap().len(), 1);
            assert_eq!(s.search(other, "shared", 10).unwrap().len(), 1);
        }

        #[test]
        fn should_exclude_a_removed_channels_rows() {
            let s = store_with_channel();

            s.insert_message(NODE, &said(ThreadId::Channel(0), None, "old news", 1))
                .unwrap();
            s.replace_channels(NODE, &[]).unwrap();

            // Slot 0 is now empty, so the old channel's rows have no thread.
            assert!(texts(&s, "old news").is_empty());
        }

        #[test]
        fn should_exclude_slot_rows_hidden_by_a_new_channel() {
            let s = Store::open_memory().unwrap();

            s.insert_message(NODE, &said(ThreadId::Channel(6), None, "slot six", 1))
                .unwrap();

            assert_eq!(texts(&s, "slot six"), ["slot six"]);

            s.replace_channels(
                NODE,
                &[Channel {
                    idx: 6,
                    name: "#new".into(),
                    secret: [6; 16],
                }],
            )
            .unwrap();

            assert!(texts(&s, "slot six").is_empty());
        }

        #[test]
        fn should_return_newest_first_and_stop_at_the_limit() {
            let s = store_with_channel();

            for ts in 1..=5 {
                s.insert_message(
                    NODE,
                    &said(ThreadId::Channel(0), None, &format!("hit {ts}"), ts),
                )
                .unwrap();
            }

            let hits = s.search(NODE, "hit", 3).unwrap();
            let texts: Vec<&str> = hits.iter().map(|m| m.text.as_str()).collect();

            assert_eq!(texts, ["hit 5", "hit 4", "hit 3"]);
        }

        #[test]
        fn should_resolve_the_thread_of_channel_and_contact_hits() {
            let s = store_with_channel();
            let contact = ThreadId::Contact("a1a1a1a1a1a1".into());

            s.insert_message(NODE, &said(ThreadId::Channel(0), None, "needle one", 1))
                .unwrap();
            s.insert_message(NODE, &said(contact.clone(), None, "needle two", 2))
                .unwrap();

            let threads: Vec<ThreadId> = s
                .search(NODE, "needle", 10)
                .unwrap()
                .into_iter()
                .map(|m| m.thread)
                .collect();

            assert_eq!(threads, [contact, ThreadId::Channel(0)]);
        }

        #[test]
        fn should_find_nothing_for_a_blank_query() {
            let s = store_with_channel();

            s.insert_message(NODE, &said(ThreadId::Channel(0), None, "anything", 1))
                .unwrap();

            assert!(texts(&s, "").is_empty());
            assert!(texts(&s, "  \t").is_empty());
        }

        #[test]
        fn should_trim_the_query() {
            let s = store_with_channel();

            s.insert_message(NODE, &said(ThreadId::Channel(0), None, "anything", 1))
                .unwrap();

            assert_eq!(texts(&s, "  thing "), ["anything"]);
        }
    }

    mod open {
        use super::*;

        #[test]
        fn should_write_schema_version_one_on_a_fresh_file() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("fresh.db");

            drop(Store::open(&path).unwrap());

            let conn = Connection::open(&path).unwrap();
            let version: String = conn
                .query_row(
                    "SELECT value FROM meta WHERE key = 'schema_version'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(version, "1");
        }

        #[test]
        fn should_create_a_missing_parent_directory() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("a").join("b").join("x.db");

            let opened = Store::open(&path);

            assert!(opened.is_ok(), "open failed: {:?}", opened.err());
            assert!(path.is_file());
        }
    }

    mod channel_identity {
        use super::*;

        #[test]
        fn should_hide_slot_history_once_a_channel_is_stored_and_restore_it_on_removal() {
            let s = Store::open_memory().unwrap();
            let slot = ThreadId::Channel(6);
            let id = s
                .insert_message(NODE, &channel_message(6, "before"))
                .unwrap()
                .unwrap();

            assert_eq!(s.messages(NODE, &slot, 10).unwrap().len(), 1);
            assert_eq!(s.message(NODE, id).unwrap().unwrap().thread, slot);

            s.replace_channels(
                NODE,
                &[Channel {
                    idx: 6,
                    name: "#new".into(),
                    secret: [6; 16],
                }],
            )
            .unwrap();

            assert!(s.messages(NODE, &slot, 10).unwrap().is_empty());
            assert_eq!(s.message(NODE, id).unwrap(), None);

            s.replace_channels(NODE, &[]).unwrap();

            assert_eq!(s.messages(NODE, &slot, 10).unwrap().len(), 1);
            assert_eq!(s.message(NODE, id).unwrap().unwrap().thread, slot);
        }
    }
}
