//! The session actor: the only code that talks to the radio.
//!
//! The GUI sends [`Command`]s through a [`Session`] handle and re-reads the
//! [`Store`] whenever the actor broadcasts [`Update::StoreChanged`]. One
//! tokio task owns the radio, the retry timers, and the ack deadlines, so
//! nothing here needs a lock.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::future::pending;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use crate::config::ConnectionConfig;
use crate::error::{Error, Result};
use crate::model::{Channel, DeliveryStatus, DeviceInfo, Direction, Message, NewMessage, ThreadId};
use crate::radio::{
    ConnectTarget, DeviceSettings, FakeRadio, MAX_CHANNEL_NAME_BYTES, Radio, RadioEvent,
};
use crate::store::Store;
use crate::text;

/// Retry delays in seconds; the last one repeats.
const RETRY_DELAYS_SECS: [u64; 5] = [5, 10, 20, 40, 60];

/// How long a real connect may take before it counts as failed.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Delay that coalesces a burst of contact pushes into one refresh.
const CONTACTS_REFRESH_DELAY: Duration = Duration::from_secs(2);

/// How often the battery is polled while connected.
const BATTERY_POLL: Duration = Duration::from_secs(120);

/// Extra wait after the radio's suggested ack timeout.
const ACK_GRACE: Duration = Duration::from_millis(5000);

/// Channel slots to read when the device does not report a count.
const DEFAULT_MAX_CHANNELS: u8 = 8;

/// Times a duplicate outgoing message is retried with a later timestamp.
const MAX_TIMESTAMP_RETRIES: u32 = 5;

/// Capacity of the update broadcast channel.
const UPDATE_CAPACITY: usize = 256;

/// Where the connection stands.
#[derive(Debug, Clone, PartialEq)]
pub enum ConnState {
    /// No radio, and none being opened.
    Disconnected,
    /// Opening the transport or syncing.
    Connecting,
    /// Ready; carries the latest device report.
    Connected(DeviceInfo),
    /// The last attempt failed; carries the reason.
    Failed(String),
}

/// What the GUI can ask the session to do.
#[derive(Debug, Clone)]
pub enum Command {
    /// Connect with the current settings.
    Connect,
    /// Drop the connection and stay down.
    Disconnect,
    /// Replace the settings, then reconnect.
    Reconfigure(ConnectionConfig),
    /// Send text to a channel or contact thread.
    SendText {
        /// Destination thread.
        thread: ThreadId,
        /// Message text.
        text: String,
    },
    /// Resend a failed outgoing message.
    Retry {
        /// Row id of the failed message.
        message_id: i64,
    },
    /// Write a channel into its slot.
    SetChannel(Channel),
    /// Empty a channel slot.
    ClearChannel(u8),
    /// Delete a contact (64-hex key).
    RemoveContact(String),
    /// Forget the route to a contact (64-hex key).
    ResetPath(String),
    /// Import a `meshcore://` contact card.
    ImportContact(String),
    /// Export a contact card; `None` exports the radio itself.
    ExportContact(Option<String>),
    /// Broadcast an advert.
    SendAdvert {
        /// Flood the mesh instead of a zero-hop advert.
        flood: bool,
    },
    /// Set the device clock to now.
    SyncClock,
    /// Write changed device settings.
    ApplySettings(DeviceSettings),
    /// Re-run the sync step without reconnecting.
    Refresh,
}

/// What the session tells the GUI.
#[derive(Debug, Clone)]
pub enum Update {
    /// The connection state changed.
    State(ConnState),
    /// The store has new data; re-read it.
    StoreChanged,
    /// A message for the user.
    Notice(String),
    /// Result of [`Command::ExportContact`].
    Exported(String),
}

/// Where the session gets its radio.
pub enum Connector {
    /// Open a real radio from the connection settings.
    Real,
    /// Use this fake radio.
    Fake(FakeRadio),
}

/// Handle to the session actor. Cheap to clone.
#[derive(Clone)]
pub struct Session {
    commands: mpsc::UnboundedSender<Command>,
    updates: broadcast::Sender<Update>,
    state: watch::Receiver<ConnState>,
}

impl Session {
    /// Starts the actor on the current tokio runtime.
    ///
    /// Connects right away when `cfg.auto_connect` is set. The actor stops
    /// (disconnecting the radio) once every handle has been dropped.
    pub fn spawn(store: Store, cfg: ConnectionConfig, connector: Connector) -> Session {
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (updates, _) = broadcast::channel(UPDATE_CAPACITY);
        let (state_tx, state) = watch::channel(ConnState::Disconnected);
        let actor = Actor {
            store,
            cfg,
            connector,
            command_rx,
            updates: updates.clone(),
            state_tx,
            conn: None,
            deadlines: Vec::new(),
            retry_at: None,
            retry_step: 0,
            refresh_at: None,
            battery_at: None,
        };

        tokio::spawn(actor.run());

        Session {
            commands,
            updates,
            state,
        }
    }

    /// Queues a command for the actor.
    pub fn send(&self, cmd: Command) {
        // A send error means the actor has stopped, so there is no one left
        // to act on the command.
        let _ = self.commands.send(cmd);
    }

    /// Subscribes to updates from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<Update> {
        self.updates.subscribe()
    }

    /// Returns the current connection state.
    pub fn state(&self) -> ConnState {
        self.state.borrow().clone()
    }
}

/// A radio the actor holds, with the report of this connection.
struct Conn {
    radio: Radio,
    events: broadcast::Receiver<RadioEvent>,
    /// Set once `start()` succeeded: only then is the node id verified.
    device: Option<DeviceInfo>,
}

/// The task behind a [`Session`].
struct Actor {
    store: Store,
    cfg: ConnectionConfig,
    connector: Connector,
    command_rx: mpsc::UnboundedReceiver<Command>,
    updates: broadcast::Sender<Update>,
    state_tx: watch::Sender<ConnState>,
    conn: Option<Conn>,
    /// Sent direct messages waiting for an ack: when to give up, row id.
    deadlines: Vec<(Instant, i64)>,
    retry_at: Option<Instant>,
    retry_step: usize,
    refresh_at: Option<Instant>,
    battery_at: Option<Instant>,
}

impl Actor {
    /// Runs until every [`Session`] handle is dropped.
    async fn run(mut self) {
        if self.cfg.auto_connect {
            self.connect().await;
        }

        loop {
            let timer = self.next_timer();

            tokio::select! {
                cmd = self.command_rx.recv() => match cmd {
                    Some(cmd) => self.handle(cmd).await,
                    None => {
                        self.abandon().await;

                        return;
                    }
                },
                event = next_radio_event(&mut self.conn) => {
                    self.on_radio_event(event).await;
                },
                () = sleep_until_opt(timer) => self.on_timer().await,
            }
        }
    }

    // ── Publishing ──────────────────────────────────────────────────────────

    /// Publishes a state change to the watch channel and the broadcast.
    fn set_state(&self, state: ConnState) {
        self.state_tx.send_replace(state.clone());
        self.publish(Update::State(state));
    }

    /// Broadcasts an update.
    fn publish(&self, update: Update) {
        // No receiver just means no GUI is listening yet.
        let _ = self.updates.send(update);
    }

    /// Tells the GUI to re-read the store.
    fn changed(&self) {
        self.publish(Update::StoreChanged);
    }

    /// Tells the user something.
    fn notice(&self, msg: impl Into<String>) {
        self.publish(Update::Notice(msg.into()));
    }

    /// Reports a failed action as a notice.
    fn fail(&self, action: &str, e: &Error) {
        self.notice(format!("{action} failed: {e}"));
    }

    // ── Connection lifecycle ────────────────────────────────────────────────

    /// Opens the radio and syncs; ignored while a radio is held.
    async fn connect(&mut self) {
        if self.conn.is_some() {
            return;
        }

        self.retry_at = None;
        self.set_state(ConnState::Connecting);

        if let Err(e) = self.establish().await {
            self.connect_failed(e).await;
        }
    }

    /// Connect step: open, subscribe, sync, then publish `Connected`.
    async fn establish(&mut self) -> Result<()> {
        let radio = self.open_radio().await?;
        let events = radio.subscribe();

        self.conn = Some(Conn {
            radio,
            events,
            device: None,
        });

        let info = self.sync().await?;
        let failed = self.store.fail_unfinished(&info.node_id)?;

        if failed > 0 {
            self.notice(format!("{failed} unsent messages marked failed"));
        }

        self.retry_step = 0;
        self.battery_at = Some(Instant::now() + BATTERY_POLL);
        self.set_state(ConnState::Connected(info));
        self.changed();
        self.drain().await;

        Ok(())
    }

    /// Gets a radio from the connector.
    async fn open_radio(&self) -> Result<Radio> {
        match &self.connector {
            Connector::Real => {
                let target = ConnectTarget::from_config(&self.cfg)?;

                tokio::time::timeout(CONNECT_TIMEOUT, Radio::connect(&target))
                    .await
                    .map_err(|_| Error::Radio("connect timed out".into()))?
            }
            Connector::Fake(fake) => {
                if fake.state().fail_connect {
                    return Err(Error::Radio("connect refused".into()));
                }

                Ok(Radio::Fake(fake.clone()))
            }
        }
    }

    /// Sync step: device report, channels, contacts into the store.
    ///
    /// Returns the fresh report. Nothing is written for a step that failed,
    /// so the store keeps its previous snapshot of it.
    async fn sync(&mut self) -> Result<DeviceInfo> {
        let conn = self.conn.as_mut().ok_or(Error::NotConnected)?;
        let info = conn.radio.start().await?;

        conn.device = Some(info.clone());
        self.store.save_device(&info)?;

        let max = info.max_channels.unwrap_or(DEFAULT_MAX_CHANNELS);
        let channels = conn.radio.channels(max).await?;

        self.store.replace_channels(&info.node_id, &channels)?;

        let contacts = conn.radio.contacts(0).await?;

        self.store.replace_contacts(&info.node_id, &contacts)?;

        Ok(info)
    }

    /// Records a failed connect and schedules a retry when appropriate.
    async fn connect_failed(&mut self, e: Error) {
        self.abandon().await;

        let msg = e.to_string();

        self.set_state(ConnState::Failed(msg.clone()));
        self.notice(format!("Connection failed: {msg}"));

        if self.cfg.auto_connect && !matches!(e, Error::NotConfigured(_)) {
            self.schedule_retry();
        }
    }

    /// Arms the retry timer with the next backoff delay.
    fn schedule_retry(&mut self) {
        let step = self.retry_step.min(RETRY_DELAYS_SECS.len() - 1);

        self.retry_step += 1;
        self.retry_at = Some(Instant::now() + Duration::from_secs(RETRY_DELAYS_SECS[step]));
    }

    /// `Disconnect`: cancel the retry, close the radio, stay down.
    async fn disconnect(&mut self) {
        self.retry_at = None;
        self.abandon().await;
        self.set_state(ConnState::Disconnected);
    }

    /// The transport dropped. `retry_anyway` reconnects even without
    /// `auto_connect`.
    async fn radio_lost(&mut self, retry_anyway: bool) {
        self.abandon().await;
        self.set_state(ConnState::Disconnected);
        self.notice("Radio disconnected");

        if retry_anyway || self.cfg.auto_connect {
            self.schedule_retry();
        }
    }

    /// Stops using the radio: disconnect it, forget every deadline and
    /// timer, and fail what its connection left unfinished.
    ///
    /// Only a connection whose `start()` succeeded has a verified node id,
    /// so only then are rows touched.
    async fn abandon(&mut self) {
        let Some(conn) = self.conn.take() else {
            return;
        };

        conn.radio.disconnect().await;
        self.deadlines.clear();
        self.refresh_at = None;
        self.battery_at = None;

        let Some(device) = conn.device else {
            return;
        };

        match self.store.fail_unfinished(&device.node_id) {
            Ok(0) => {}
            Ok(_) => self.changed(),
            Err(e) => self.fail("Marking unsent messages", &e),
        }
    }

    // ── Events and timers ───────────────────────────────────────────────────

    /// Reacts to a push from the radio.
    async fn on_radio_event(&mut self, event: std::result::Result<RadioEvent, RecvError>) {
        match event {
            Ok(RadioEvent::MessagesWaiting) => self.drain().await,
            Ok(RadioEvent::Ack(tag)) => self.on_ack(tag),
            Ok(RadioEvent::ContactsChanged) => self.request_contacts_refresh(),
            Ok(RadioEvent::Disconnected) | Err(RecvError::Closed) => self.radio_lost(false).await,
            Err(RecvError::Lagged(_)) => self.resync_after_lag().await,
        }
    }

    /// We missed pushes: a dead transport is a disconnect, otherwise
    /// assume both messages and contacts changed.
    async fn resync_after_lag(&mut self) {
        let Some(conn) = &self.conn else {
            return;
        };

        if !conn.radio.is_connected().await {
            self.radio_lost(false).await;

            return;
        }

        self.drain().await;
        self.request_contacts_refresh();
    }

    /// Schedules one contact refresh unless one is already pending.
    fn request_contacts_refresh(&mut self) {
        if self.refresh_at.is_none() {
            self.refresh_at = Some(Instant::now() + CONTACTS_REFRESH_DELAY);
        }
    }

    /// An ack arrived: mark the matching message delivered.
    fn on_ack(&mut self, tag: [u8; 4]) {
        let Ok(node) = self.node() else {
            return;
        };

        let delivered = self.store.message_by_ack(&node, tag).and_then(|found| {
            found
                .map(|id| {
                    self.store
                        .set_status(id, DeliveryStatus::Delivered, None)
                        .map(|()| id)
                })
                .transpose()
        });

        match delivered {
            Ok(Some(id)) => {
                self.deadlines.retain(|(_, d)| *d != id);
                self.changed();
            }
            Ok(None) => {}
            Err(e) => self.fail("Recording delivery", &e),
        }
    }

    /// Earliest instant at which `on_timer` has something to do.
    fn next_timer(&self) -> Option<Instant> {
        [self.retry_at, self.refresh_at, self.battery_at]
            .into_iter()
            .flatten()
            .chain(self.deadlines.iter().map(|(at, _)| *at))
            .min()
    }

    /// Runs everything whose time has come.
    async fn on_timer(&mut self) {
        let now = Instant::now();

        if self.retry_at.is_some_and(|at| at <= now) {
            self.retry_at = None;
            self.connect().await;
        }

        if self.refresh_at.is_some_and(|at| at <= now) {
            self.refresh_at = None;
            self.refresh_contacts().await;
        }

        if self.battery_at.is_some_and(|at| at <= now) {
            self.poll_battery().await;
        }

        self.expire_deadlines(now);
    }

    /// Fetches contacts changed since the newest one we hold.
    async fn refresh_contacts(&mut self) {
        let result = async {
            let node = self.node()?;
            let since = self.store.max_contact_modified(&node)?;
            let contacts = self.conn()?.radio.contacts(since).await?;

            self.store.upsert_contacts(&node, &contacts)
        }
        .await;

        match result {
            Ok(()) => self.changed(),
            Err(e) => self.fail("Contact refresh", &e),
        }
    }

    /// Reads the battery into the published device report.
    async fn poll_battery(&mut self) {
        self.battery_at = Some(Instant::now() + BATTERY_POLL);

        let Ok(conn) = self.conn() else {
            return;
        };

        let reading = conn.radio.battery_mv().await;

        match (reading, self.conn.as_mut().and_then(|c| c.device.as_mut())) {
            (Ok(mv), Some(device)) => {
                device.battery_mv = Some(mv);

                let state = ConnState::Connected(device.clone());

                self.set_state(state);
            }
            (Err(e), _) => tracing::warn!(error = %e, "battery poll failed"),
            (Ok(_), None) => {}
        }
    }

    /// Fails direct messages whose ack never came.
    fn expire_deadlines(&mut self, now: Instant) {
        let due: Vec<i64> = self
            .deadlines
            .iter()
            .filter(|(at, _)| *at <= now)
            .map(|(_, id)| *id)
            .collect();

        self.deadlines.retain(|(at, _)| *at > now);

        for id in due {
            if let Err(e) = self.fail_if_still_sent(id) {
                self.fail("Recording ack timeout", &e);
            }
        }
    }

    /// Marks one row failed unless its ack arrived in the meantime.
    fn fail_if_still_sent(&self, id: i64) -> Result<()> {
        let node = self.node()?;

        if let Some(m) = self.store.message(&node, id)?
            && m.status == DeliveryStatus::Sent
        {
            self.store.set_status(id, DeliveryStatus::Failed, None)?;
            self.changed();
        }

        Ok(())
    }

    // ── Commands ────────────────────────────────────────────────────────────

    /// Dispatches one command.
    async fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Connect => self.connect().await,
            Command::Disconnect => self.disconnect().await,
            Command::Reconfigure(cfg) => {
                self.cfg = cfg;
                self.disconnect().await;
                self.connect().await;
            }
            _ if self.conn.is_none() => self.notice("Not connected"),
            Command::SendText { thread, text } => self.send_text(thread, text).await,
            Command::Retry { message_id } => self.retry(message_id).await,
            Command::SetChannel(ch) => self.write_channel(ch.idx, Some(ch)).await,
            Command::ClearChannel(idx) => self.write_channel(idx, None).await,
            Command::RemoveContact(key) => {
                let result = self.remove_contact(&key).await;

                self.report("Remove contact", result);
            }
            Command::ResetPath(key) => {
                let result = self.reset_path(&key).await;

                self.report("Reset path", result);
            }
            Command::ImportContact(uri) => {
                let result = self.import_contact(&uri).await;

                self.report("Import contact", result);
            }
            Command::ExportContact(key) => {
                let result = self.export_contact(key.as_deref()).await;

                self.report("Export contact", result);
            }
            Command::SendAdvert { flood } => {
                let result = self.send_advert(flood).await;

                self.report("Send advert", result);
            }
            Command::SyncClock => {
                let result = self.sync_clock().await;

                self.report("Sync clock", result);
            }
            Command::ApplySettings(s) => {
                let result = self.apply_settings(&s).await;

                self.report("Apply settings", result);
            }
            Command::Refresh => self.refresh().await,
        }
    }

    /// Turns a command error into a notice; the connection stays up.
    fn report(&self, action: &str, result: Result<()>) {
        if let Err(e) = result {
            self.fail(action, &e);
        }
    }

    /// The held connection.
    fn conn(&self) -> Result<&Conn> {
        self.conn.as_ref().ok_or(Error::NotConnected)
    }

    /// The device report of the held connection.
    fn device(&self) -> Result<&DeviceInfo> {
        self.conn()?.device.as_ref().ok_or(Error::NotConnected)
    }

    /// The verified node id of the held connection.
    fn node(&self) -> Result<String> {
        Ok(self.device()?.node_id.clone())
    }

    /// Pops every queued message into the store.
    ///
    /// An error stops the drain with a notice; rows inserted before it are
    /// still announced.
    async fn drain(&mut self) {
        let mut inserted = false;

        if let Err(e) = self.drain_queue(&mut inserted).await {
            self.fail("Reading messages", &e);
        }

        if inserted {
            self.changed();
        }
    }

    /// Stores queued messages until the queue is empty.
    async fn drain_queue(&self, inserted: &mut bool) -> Result<()> {
        let node = self.node()?;
        let radio = &self.conn()?.radio;

        while let Some(m) = radio.next_message().await? {
            let row = NewMessage {
                thread: m.thread,
                direction: Direction::In,
                sender: m.sender,
                text: m.text,
                sender_ts: m.sender_ts,
                received_at: unix_now(),
                path_len: m.path_len,
                snr: m.snr,
                status: DeliveryStatus::Received,
            };

            *inserted |= self.store.insert_message(&node, &row)?.is_some();
        }

        Ok(())
    }

    /// `SendText`: store a pending row, then send it.
    async fn send_text(&mut self, thread: ThreadId, text: String) {
        if text.trim().is_empty() {
            return;
        }

        let Ok(device) = self.device() else {
            return;
        };

        let (node, name) = (device.node_id.clone(), device.name.clone());
        let max = text::max_text_bytes(&thread, &name);

        if text.len() > max {
            self.notice(format!("Message too long ({}/{max} bytes)", text.len()));

            return;
        }

        let sender = match thread {
            ThreadId::Channel(_) => Some(name),
            ThreadId::Contact(_) => None,
        };
        let now = unix_now();
        let mut row = NewMessage {
            thread: thread.clone(),
            direction: Direction::Out,
            sender,
            text: text.clone(),
            sender_ts: clamp_ts(now),
            received_at: now,
            path_len: None,
            snr: None,
            status: DeliveryStatus::Pending,
        };

        match self.insert_outgoing(&node, &mut row) {
            Ok(Some(id)) => self.deliver(id, thread, &text, row.sender_ts).await,
            Ok(None) => self.notice("Could not store message: duplicate"),
            Err(e) => self.fail("Storing message", &e),
        }
    }

    /// Inserts `row`, moving its timestamp forward while it collides with
    /// an identical message sent in the same second.
    fn insert_outgoing(&self, node: &str, row: &mut NewMessage) -> Result<Option<i64>> {
        for _ in 0..=MAX_TIMESTAMP_RETRIES {
            if let Some(id) = self.store.insert_message(node, row)? {
                self.changed();

                return Ok(Some(id));
            }

            row.sender_ts += 1;
        }

        Ok(None)
    }

    /// `Retry`: resend a failed outgoing row with its own timestamp.
    async fn retry(&mut self, id: i64) {
        let found = self
            .node()
            .and_then(|node| self.store.message(&node, id))
            .map(|row| row.filter(is_retryable));

        let m = match found {
            Ok(Some(m)) => m,
            Ok(None) => return,
            Err(e) => return self.fail("Retry", &e),
        };

        if let Err(e) = self.store.set_status(id, DeliveryStatus::Pending, None) {
            return self.fail("Retry", &e);
        }

        self.changed();
        self.deliver(id, m.thread, &m.text, m.sender_ts).await;
    }

    /// Hands a stored row to the radio and records the outcome.
    async fn deliver(&mut self, id: i64, thread: ThreadId, text: &str, ts: u32) {
        let Ok(conn) = self.conn() else {
            return;
        };

        let sent = match &thread {
            ThreadId::Contact(prefix) => conn.radio.send_text(prefix, text, ts).await.map(Some),
            ThreadId::Channel(idx) => conn
                .radio
                .send_channel_text(*idx, text, ts)
                .await
                .map(|()| None),
        };

        let recorded = match sent {
            Ok(Some(info)) => {
                self.deadlines.push((
                    Instant::now() + Duration::from_millis(u64::from(info.timeout_ms)) + ACK_GRACE,
                    id,
                ));
                self.store
                    .set_status(id, DeliveryStatus::Sent, Some(info.ack))
            }
            Ok(None) => self.store.set_status(id, DeliveryStatus::Sent, None),
            Err(e) => {
                self.fail("Send", &e);
                self.store.set_status(id, DeliveryStatus::Failed, None)
            }
        };

        match recorded {
            Ok(()) => self.changed(),
            Err(e) => self.fail("Recording send", &e),
        }
    }

    /// `SetChannel` / `ClearChannel`.
    ///
    /// Channel slots decide where messages go, so after a successful write
    /// the slot is stored first and the full list reloaded second. A
    /// refusal changes nothing. Any other error leaves the slot's content
    /// unknown, so the radio is dropped and the reconnect sync reloads all.
    async fn write_channel(&mut self, idx: u8, ch: Option<Channel>) {
        let (action, done) = match &ch {
            Some(_) => ("Save channel", "Channel saved"),
            None => ("Remove channel", "Channel removed"),
        };

        if let Some(c) = &ch
            && !(1..=MAX_CHANNEL_NAME_BYTES).contains(&c.name.len())
        {
            self.notice(format!(
                "Channel name must be 1 to {MAX_CHANNEL_NAME_BYTES} bytes"
            ));

            return;
        }

        let Ok(conn) = self.conn() else {
            return;
        };

        let written = match &ch {
            Some(c) => conn.radio.set_channel(c).await,
            None => conn.radio.clear_channel(idx).await,
        };

        match written {
            Ok(()) => match self.store_channel_change(idx, ch) {
                Ok(()) => {
                    self.reload_channels().await;
                    self.changed();
                    self.notice(done);
                }
                Err(e) => self.fail(action, &e),
            },
            Err(e @ Error::Rejected(_)) => self.fail(action, &e),
            Err(e) => {
                self.fail(action, &e);
                self.radio_lost(true).await;
            }
        }
    }

    /// Applies one slot change to the stored channels.
    fn store_channel_change(&self, idx: u8, ch: Option<Channel>) -> Result<()> {
        let node = self.node()?;
        let mut slots = self.store.channels(&node)?;

        slots.retain(|c| c.idx != idx);
        slots.extend(ch);
        slots.sort_by_key(|c| c.idx);

        self.store.replace_channels(&node, &slots)
    }

    /// Replaces stored channels with the radio's list; a failure only
    /// adds a notice, so the local slot change stays.
    async fn reload_channels(&self) {
        let result = async {
            let device = self.device()?;
            let max = device.max_channels.unwrap_or(DEFAULT_MAX_CHANNELS);
            let channels = self.conn()?.radio.channels(max).await?;

            self.store.replace_channels(&device.node_id, &channels)
        }
        .await;

        if let Err(e) = result {
            self.fail("Channel reload", &e);
        }
    }

    /// `RemoveContact`: radio first, then the store.
    async fn remove_contact(&self, key: &str) -> Result<()> {
        let node = self.node()?;

        self.conn()?.radio.remove_contact(key).await?;
        self.store.remove_contact(&node, key)?;
        self.changed();

        Ok(())
    }

    /// `ResetPath`: radio first, then reload contacts to see the new path.
    async fn reset_path(&self, key: &str) -> Result<()> {
        self.conn()?.radio.reset_path(key).await?;
        self.reload_contacts().await
    }

    /// `ImportContact`: parse locally, import, reload contacts.
    async fn import_contact(&self, uri: &str) -> Result<()> {
        let card = text::parse_contact_uri(uri)?;

        self.conn()?.radio.import_contact(&card).await?;
        self.reload_contacts().await?;
        self.notice("Contact imported");

        Ok(())
    }

    /// Replaces stored contacts with the radio's full list.
    async fn reload_contacts(&self) -> Result<()> {
        let node = self.node()?;
        let contacts = self.conn()?.radio.contacts(0).await?;

        self.store.replace_contacts(&node, &contacts)?;
        self.changed();

        Ok(())
    }

    /// `ExportContact`: publish the card URI.
    async fn export_contact(&self, key: Option<&str>) -> Result<()> {
        let uri = self.conn()?.radio.export_contact(key).await?;

        self.publish(Update::Exported(uri));

        Ok(())
    }

    /// `SendAdvert`.
    async fn send_advert(&self, flood: bool) -> Result<()> {
        self.conn()?.radio.send_advert(flood).await?;
        self.notice("Advert sent");

        Ok(())
    }

    /// `SyncClock`.
    async fn sync_clock(&self) -> Result<()> {
        self.conn()?.radio.sync_clock(clamp_ts(unix_now())).await?;
        self.notice("Clock synced");

        Ok(())
    }

    /// `ApplySettings`: validate, write, then re-read the device.
    async fn apply_settings(&mut self, s: &DeviceSettings) -> Result<()> {
        let current = self.device()?.clone();

        s.validate(&current)?;

        let conn = self.conn.as_mut().ok_or(Error::NotConnected)?;

        conn.radio.apply_settings(&current, s).await?;

        let info = conn.radio.start().await?;

        conn.device = Some(info.clone());
        self.store.save_device(&info)?;
        self.set_state(ConnState::Connected(info));
        self.changed();
        self.notice("Settings applied");

        Ok(())
    }

    /// `Refresh`: the sync step again, keeping the old snapshot on failure
    /// and every ack deadline.
    async fn refresh(&mut self) {
        match self.sync().await {
            Ok(info) => {
                self.set_state(ConnState::Connected(info));
                self.changed();
            }
            Err(e) => self.fail("Refresh", &e),
        }
    }
}

/// Waits for the next push from the held radio; never resolves without one.
async fn next_radio_event(conn: &mut Option<Conn>) -> std::result::Result<RadioEvent, RecvError> {
    match conn {
        Some(conn) => conn.events.recv().await,
        None => pending().await,
    }
}

/// Sleeps until `at`, or forever when there is nothing to wait for.
async fn sleep_until_opt(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => pending().await,
    }
}

/// Whether a stored row may be resent: ours, outgoing, and failed.
fn is_retryable(m: &Message) -> bool {
    m.direction == Direction::Out && m.status == DeliveryStatus::Failed
}

/// Current unix time in seconds.
fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Narrows unix seconds to the radio's 32-bit clock.
fn clamp_ts(secs: i64) -> u32 {
    u32::try_from(secs).unwrap_or(u32::MAX)
}
