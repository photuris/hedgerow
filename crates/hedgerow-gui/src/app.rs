//! Root component and the state every view shares.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dioxus::html::Modifiers;
use dioxus::prelude::*;
use hedgerow_core::config::Config;
use hedgerow_core::model::{Channel, ThreadId};
use hedgerow_core::radio::FakeRadio;
use hedgerow_core::session::{Command, ConnState, Connector, Guard, Session, Update};
use hedgerow_core::store::Store;
use hedgerow_core::text;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::components::{
    Chat, Contacts, ExportDialog, Icon, SearchResults, Settings, Sidebar, Toast,
};
use crate::theme::{self, Theme, Watcher};

/// How long a toast stays up.
const TOAST_SECS: u64 = 4;

/// How often relative times are recomputed.
const CLOCK_TICK_SECS: u64 = 30;

/// How long the text dump may take after the exit timer fired.
const DUMP_TIMEOUT: Duration = Duration::from_secs(10);

/// Narrowest sidebar the divider allows, in pixels.
const SIDEBAR_MIN: u32 = 180;

/// Widest sidebar the divider allows, in pixels.
const SIDEBAR_MAX: u32 = 480;

/// Fewest characters of a search that show results.
pub const MIN_SEARCH_CHARS: usize = 2;

/// The test hooks, read from the environment once in `main`.
#[derive(Clone, Default)]
pub struct Hooks {
    /// `HEDGEROW_EXIT_AFTER_MS`: exit after this many milliseconds.
    pub exit_after_ms: Option<u64>,
    /// `HEDGEROW_DUMP_TEXT`: write the page text here before exiting.
    pub dump_text: Option<PathBuf>,
    /// `HEDGEROW_START_VIEW`: the initial view, see [`View::parse_start`].
    pub start_view: Option<String>,
}

/// Everything `main` hands to the root component.
#[derive(Clone)]
pub struct Boot {
    /// The message store.
    pub store: Store,
    /// The loaded (or default) configuration.
    pub config: Config,
    /// The demo radio in UI-test mode; `None` connects a real radio.
    pub fake: Option<FakeRadio>,
    /// The test hooks.
    pub hooks: Hooks,
}

/// A tab of the settings view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsTab {
    /// Transport and target.
    Connection,
    /// Device name, radio, and power.
    Device,
    /// Channel slots.
    Channels,
}

/// What the main region shows.
#[derive(Clone, Debug, PartialEq)]
pub enum View {
    /// Nothing selected.
    None,
    /// One conversation.
    Thread(ThreadId),
    /// The contact list.
    Contacts,
    /// The settings.
    Settings(SettingsTab),
    /// Message search results for this query, as typed.
    Search(String),
}

impl View {
    /// Parses `HEDGEROW_START_VIEW`: `contacts`,
    /// `settings:connection|device|channels`, `thread:channel:<idx>`, or
    /// `thread:contact:<prefix>`. Anything else is `None`.
    pub fn parse_start(s: &str) -> Option<View> {
        match s {
            "contacts" => return Some(View::Contacts),
            "settings:connection" => return Some(View::Settings(SettingsTab::Connection)),
            "settings:device" => return Some(View::Settings(SettingsTab::Device)),
            "settings:channels" => return Some(View::Settings(SettingsTab::Channels)),
            _ => {}
        }

        let (kind, key) = s.strip_prefix("thread:")?.split_once(':')?;

        if key.is_empty() {
            return None;
        }

        ThreadId::from_parts(kind, key).map(View::Thread)
    }
}

/// Keeps a sidebar width within the limits the divider allows.
pub fn clamp_sidebar(width: u32) -> u32 {
    width.clamp(SIDEBAR_MIN, SIDEBAR_MAX)
}

/// The sidebar width after the divider moved from `start_x` to `x` while the
/// sidebar was `start_width` wide.
fn dragged_width(start_width: u32, start_x: f64, x: f64) -> u32 {
    // A float-to-int `as` saturates, so a far-off pointer cannot overflow.
    clamp_sidebar((f64::from(start_width) + x - start_x).round() as u32)
}

/// `config` with the sidebar `width`, or `None` when it already has it.
fn with_sidebar_width(config: &Config, width: u32) -> Option<Config> {
    if config.ui.sidebar_width == width {
        return None;
    }

    let mut updated = config.clone();

    updated.ui.sidebar_width = width;

    Some(updated)
}

/// What a global key chord asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shortcut {
    /// Show or hide the sidebar.
    ToggleSidebar,
    /// Open the sidebar and focus the search box.
    FocusSearch,
}

/// The shortcut for `key` pressed with `modifiers`: Ctrl+B and Ctrl+F, or
/// Cmd+B and Cmd+F on macOS. Shift and Alt turn a chord into another one.
fn shortcut(key: &Key, modifiers: Modifiers) -> Option<Shortcut> {
    let command = modifiers.contains(Modifiers::CONTROL)
        || (cfg!(target_os = "macos") && modifiers.contains(Modifiers::META));

    if !command || modifiers.intersects(Modifiers::SHIFT | Modifiers::ALT) {
        return None;
    }

    let Key::Character(letter) = key else {
        return None;
    };

    match letter.to_ascii_lowercase().as_str() {
        "b" => Some(Shortcut::ToggleSidebar),
        "f" => Some(Shortcut::FocusSearch),
        _ => None,
    }
}

/// Keeps Ctrl+B from reaching the app-level shortcut handler. Mounted where
/// text is typed: a keyboard event carries no target element, so the fields
/// cannot be told apart from their pane. Ctrl+F is left alone, because it
/// works from anywhere.
pub(crate) fn keep_typing(evt: &KeyboardEvent) {
    if shortcut(&evt.key(), evt.modifiers()) == Some(Shortcut::ToggleSidebar) {
        evt.stop_propagation();
    }
}

/// The window title: `Hedgerow`, or `(N) Hedgerow` with unread messages.
pub fn window_title(unread: u32) -> String {
    if unread == 0 {
        "Hedgerow".to_string()
    } else {
        format!("({unread}) Hedgerow")
    }
}

/// The node whose data the views show: the connected device, else the one
/// seen last.
pub fn node_for(state: &ConnState, store: &Store) -> Option<String> {
    if let ConnState::Connected(device) = state {
        return Some(device.node_id.clone());
    }

    store.last_node().unwrap_or_else(|e| {
        tracing::warn!("read last node: {e}");
        None
    })
}

/// The node id of the radio `state` is connected to.
pub fn connected_node(state: &ConnState) -> Option<String> {
    match state {
        ConnState::Connected(device) => Some(device.node_id.clone()),
        _ => None,
    }
}

/// What a mounted [`Chat`] belongs to: its node and, for a channel thread,
/// the secret of the slot's occupant when the chat was opened.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ChatTarget {
    /// The open thread.
    thread: ThreadId,
    /// The node whose data the chat shows.
    node: Option<String>,
    /// The secret held by the thread's channel slot; `None` for a contact
    /// thread or an empty slot.
    secret: Option<[u8; 16]>,
}

impl ChatTarget {
    /// Reads the target of `thread` on `node` from the store.
    fn of(store: &Store, node: Option<String>, thread: ThreadId) -> ChatTarget {
        let channels = match (&thread, &node) {
            (ThreadId::Channel(_), Some(node)) => store.channels(node).unwrap_or_default(),
            _ => Vec::new(),
        };

        ChatTarget::in_channels(&channels, node, thread)
    }

    /// The target of `thread` on `node` given the node's stored `channels`.
    pub(crate) fn in_channels(
        channels: &[Channel],
        node: Option<String>,
        thread: ThreadId,
    ) -> ChatTarget {
        let secret = match &thread {
            ThreadId::Channel(idx) => channels.iter().find(|c| c.idx == *idx).map(|c| c.secret),
            ThreadId::Contact(_) => None,
        };

        ChatTarget {
            thread,
            node,
            secret,
        }
    }

    /// The key that remounts the chat when the node, the thread, or the
    /// channel in the thread's slot changes.
    fn key(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.node.as_deref().unwrap_or_default(),
            self.thread.kind(),
            self.thread.key(),
            self.secret.map(|s| text::to_hex(&s)).unwrap_or_default(),
        )
    }
}

/// Whether the open thread should be marked read again: only when its newest
/// incoming message id grew and the window has focus.
pub fn should_mark_read(previous: Option<i64>, newest: Option<i64>, focused: bool) -> bool {
    focused && newest > previous
}

/// Whether the window has focus. Tests have no native window, so they
/// report a focused one.
pub fn window_focused() -> bool {
    #[cfg(test)]
    {
        true
    }

    #[cfg(not(test))]
    {
        dioxus::desktop::window().is_focused()
    }
}

/// Sets the window title. A no-op in tests, which have no native window.
fn set_window_title(title: &str) {
    #[cfg(not(test))]
    dioxus::desktop::window().set_title(title);

    #[cfg(test)]
    let _ = title;
}

/// Current unix time in seconds.
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// A sidebar divider drag in progress.
#[derive(Clone, Copy)]
struct Drag {
    /// Pointer column where the drag began.
    start_x: f64,
    /// Sidebar width when the drag began.
    start_width: u32,
}

/// Shared state, provided as context by [`App`]. Cheap to clone.
#[derive(Clone)]
pub struct Ui {
    /// The message store.
    pub store: Store,
    /// The session actor.
    pub session: Session,
    /// The configuration in force; the connection settings edit it.
    pub config: Signal<Config>,
    /// Whether the app runs on the demo radio (UI-test mode).
    pub ui_test: bool,
    /// Connection state, from session updates.
    pub state: Signal<ConnState>,
    /// The node whose data is shown.
    pub node: Signal<Option<String>>,
    /// Bumped whenever stored data may have changed; views re-read on it.
    pub revision: Signal<u64>,
    /// What the main region shows.
    pub view: Signal<View>,
    /// The toast text, if any.
    pub toast: Signal<Option<String>>,
    /// Current time, refreshed every [`CLOCK_TICK_SECS`].
    pub now: Signal<i64>,
    /// The contact card to show in the export dialog, if any.
    pub exported: Signal<Option<String>>,
    /// Whether the sidebar is shown. Not saved.
    pub sidebar_open: Signal<bool>,
    /// Sidebar width in pixels, always within the divider's limits.
    pub sidebar_width: Signal<u32>,
    /// The text in the sidebar's search box.
    pub search: Signal<String>,
    /// Set to ask the sidebar's search box to take focus; the box clears it.
    pub focus_search: Signal<bool>,
    /// The message the open chat should scroll to and flash, once.
    pub reveal: Signal<Option<i64>>,
    /// Kept alive between copies: on X11 the contents die with the handle.
    clipboard: Rc<RefCell<Option<arboard::Clipboard>>>,
    /// Identifies the latest toast, so an old timer does not clear a new one.
    toast_seq: Signal<u64>,
    /// The (thread, node) pair last marked read by opening it.
    opened: Signal<Option<(ThreadId, String)>>,
    /// The sidebar divider drag in progress, if any.
    drag: Signal<Option<Drag>>,
    /// The view a search replaced, which clearing the search brings back.
    before_search: Signal<Option<View>>,
}

impl Ui {
    /// Spawns the session (the caller must be inside the tokio runtime) and
    /// creates the signals in the current scope.
    fn new(boot: &Boot) -> Ui {
        let connector = match &boot.fake {
            Some(radio) => Connector::Fake(radio.clone()),
            None => Connector::Real,
        };
        let session = Session::spawn(
            boot.store.clone(),
            boot.config.connection.clone(),
            connector,
        );
        let state = session.state();
        let node = node_for(&state, &boot.store);
        let view = boot
            .hooks
            .start_view
            .as_deref()
            .and_then(View::parse_start)
            .unwrap_or(View::None);

        Ui {
            store: boot.store.clone(),
            session,
            config: Signal::new(boot.config.clone()),
            ui_test: boot.fake.is_some(),
            state: Signal::new(state),
            node: Signal::new(node),
            revision: Signal::new(0),
            view: Signal::new(view),
            toast: Signal::new(None),
            exported: Signal::new(None),
            sidebar_open: Signal::new(true),
            sidebar_width: Signal::new(clamp_sidebar(boot.config.ui.sidebar_width)),
            search: Signal::new(String::new()),
            focus_search: Signal::new(false),
            reveal: Signal::new(None),
            clipboard: Rc::new(RefCell::new(None)),
            now: Signal::new(now_secs()),
            toast_seq: Signal::new(0),
            opened: Signal::new(None),
            drag: Signal::new(None),
            before_search: Signal::new(None),
        }
    }

    /// Tells every view that stored data may have changed.
    pub fn bump(&self) {
        let mut revision = self.revision;

        *revision.write() += 1;
    }

    /// Shows `text` for [`TOAST_SECS`].
    pub fn show_toast(&self, text: String) {
        let (mut toast, mut seq) = (self.toast, self.toast_seq);
        let id = *seq.peek() + 1;

        seq.set(id);
        toast.set(Some(text));

        spawn(async move {
            tokio::time::sleep(Duration::from_secs(TOAST_SECS)).await;

            if *seq.peek() == id {
                toast.set(None);
            }
        });
    }

    /// Tells the user that a confirmed action was dropped because the radio
    /// it was meant for is gone or its state no longer matches.
    pub fn show_cancelled(&self) {
        self.show_toast("The radio changed; action cancelled".to_string());
    }

    /// Puts `text` on the system clipboard; a failure becomes a toast.
    pub fn copy(&self, text: &str) {
        let mut clipboard = self.clipboard.borrow_mut();
        let result = match clipboard.as_mut() {
            Some(held) => held.set_text(text),
            None => arboard::Clipboard::new().and_then(|mut fresh| {
                let result = fresh.set_text(text);

                *clipboard = Some(fresh);

                result
            }),
        };

        match result {
            Ok(()) => self.show_toast("Copied".to_string()),
            Err(e) => self.show_toast(format!("Could not copy: {e}")),
        }
    }

    /// Logs a failed store write and tells the user.
    fn report(&self, what: &str, error: &hedgerow_core::Error) {
        tracing::warn!("{what}: {error}");
        self.show_toast(format!("{what}: {error}"));
    }

    /// Takes the state from the session (the authority) and recomputes the node.
    fn sync_state(&self) {
        self.apply_state(self.session.state());
    }

    /// Stores `state` and recomputes the node.
    pub(crate) fn apply_state(&self, state: ConnState) {
        let (mut node, mut current) = (self.node, self.state);

        node.set(node_for(&state, &self.store));
        current.set(state);
    }

    /// Consumes session updates until the session stops.
    async fn run_updates(&self) {
        // Subscribe first, then read the state: nothing sent in between is lost.
        let mut updates = self.session.subscribe();

        self.sync_state();
        self.bump();

        loop {
            match updates.recv().await {
                Ok(Update::State(state)) => self.apply_state(state),
                Ok(Update::StoreChanged) => self.bump(),
                Ok(Update::Notice(text)) => self.show_toast(text),
                Ok(Update::Exported(uri)) => {
                    let mut exported = self.exported;

                    exported.set(Some(uri));
                }
                Err(RecvError::Lagged(_)) => {
                    self.sync_state();
                    self.bump();
                }
                Err(RecvError::Closed) => break,
            }
        }
    }

    /// Opens `thread`. The App's opening effect marks it read once the chat
    /// has taken its first snapshot, so a message that lands in between is
    /// not marked read before the chat has seen it.
    pub fn select(&self, thread: ThreadId) {
        let (mut view, mut opened) = (self.view, self.opened);

        // Forget the last open so clicking the open thread marks it read again.
        opened.set(None);
        view.set(View::Thread(thread));

        let mut reveal = self.reveal;

        reveal.set(None);
    }

    /// Opens the thread of `rendered` and has its chat scroll to message `id`
    /// and flash it. `rendered` is the node and channel the clicked result
    /// was drawn for; if the radio or the slot's channel has changed since,
    /// nothing opens and the user is told.
    pub(crate) fn open_message(&self, rendered: &ChatTarget, id: i64) {
        let live = ChatTarget::of(
            &self.store,
            self.node.peek().clone(),
            rendered.thread.clone(),
        );
        let connected = connected_node(&self.session.state());

        if live != *rendered || connected.is_some_and(|n| Some(&n) != rendered.node.as_ref()) {
            self.show_cancelled();

            return;
        }

        let mut reveal = self.reveal;

        self.select(rendered.thread.clone());
        reveal.set(Some(id));
    }

    /// Shows or hides the sidebar.
    pub fn toggle_sidebar(&self) {
        let mut open = self.sidebar_open;
        let shown = *open.peek();

        open.set(!shown);
    }

    /// Opens the sidebar if needed and asks its search box for focus.
    fn open_search(&self) {
        let (mut open, mut focus) = (self.sidebar_open, self.focus_search);

        open.set(true);
        focus.set(true);
    }

    /// Runs the action of a global key chord.
    fn run_shortcut(&self, shortcut: Shortcut) {
        match shortcut {
            Shortcut::ToggleSidebar => self.toggle_sidebar(),
            Shortcut::FocusSearch => self.open_search(),
        }
    }

    /// Takes new text for the search box. From [`MIN_SEARCH_CHARS`] on the
    /// main region shows the results, remembering the view they replace;
    /// shorter text brings that view back.
    pub fn set_search(&self, text: String) {
        let (mut search, mut view, mut before) = (self.search, self.view, self.before_search);
        let active = text.trim().chars().count() >= MIN_SEARCH_CHARS;
        let current = view.peek().clone();

        search.set(text.clone());

        match (active, current) {
            (true, View::Search(_)) => view.set(View::Search(text)),
            (true, other) => {
                before.set(Some(other));
                view.set(View::Search(text));
            }
            (false, View::Search(_)) => view.set(before.take().unwrap_or(View::None)),
            (false, _) => {}
        }
    }

    /// Starts a sidebar divider drag at pointer column `x`.
    pub fn begin_drag(&self, x: f64) {
        let mut drag = self.drag;

        drag.set(Some(Drag {
            start_x: x,
            start_width: *self.sidebar_width.peek(),
        }));
    }

    /// Moves the dragged divider to pointer column `x`; does nothing when no
    /// drag is on.
    pub fn drag_to(&self, x: f64) {
        let Some(drag) = *self.drag.peek() else {
            return;
        };
        let mut width = self.sidebar_width;
        let dragged = dragged_width(drag.start_width, drag.start_x, x);

        // Skip the write at a limit, or every pointer move would re-render.
        if *width.peek() != dragged {
            width.set(dragged);
        }
    }

    /// Ends a divider drag and saves the new width when it changed. UI-test
    /// mode keeps it in memory only.
    pub fn end_drag(&self) {
        let mut drag = self.drag;

        if drag.take().is_none() {
            return;
        }

        let Some(updated) = with_sidebar_width(&self.config.peek(), *self.sidebar_width.peek())
        else {
            return;
        };
        let mut config = self.config;

        config.set(updated.clone());

        if self.ui_test {
            return;
        }

        if let Err(e) = updated.save() {
            self.report("Could not save settings", &e);
        }
    }

    /// The node id of the connected radio, `None` unless the session is
    /// connected. Reads the state at call time.
    pub fn connected_node(&self) -> Option<String> {
        connected_node(&self.state.peek())
    }

    /// Sends `cmd` so that the session runs it only while it is still
    /// connected to `node` and `guard` holds. The check happens in the
    /// session, so it covers state changes this UI has not consumed yet.
    pub fn send_guarded(&self, node: &str, guard: Guard, cmd: Command) {
        self.session.send(Command::Guarded {
            node: node.to_string(),
            guard,
            cmd: Box::new(cmd),
        });
    }

    /// Marks the open thread read once per (thread, node) pair; covers a
    /// thread opened before the node was known.
    pub fn mark_open_read(&self) {
        let (View::Thread(thread), Some(node)) =
            (self.view.peek().clone(), self.node.peek().clone())
        else {
            return;
        };
        let key = (thread, node);

        if self.opened.peek().as_ref() == Some(&key) {
            return;
        }

        let mut opened = self.opened;

        opened.set(Some(key.clone()));
        self.mark_read_on(&key.1, &key.0);
    }

    /// Marks `thread` read.
    pub fn mark_read(&self, thread: &ThreadId) {
        if let Some(node) = self.node.peek().clone() {
            self.mark_read_on(&node, thread);
        }
    }

    /// Marks `thread` of `node` read and bumps the revision.
    fn mark_read_on(&self, node: &str, thread: &ThreadId) {
        if let Err(e) = self.store.mark_read(node, thread) {
            self.report("Could not mark as read", &e);
        }

        self.bump();
    }

    /// Marks `thread` unread. When it is the open thread the selection is
    /// cleared, or the open-thread rules would mark it read again.
    pub fn mark_unread(&self, thread: &ThreadId) {
        let Some(node) = self.node.peek().clone() else {
            return;
        };

        if let Err(e) = self.store.mark_unread(&node, thread) {
            self.report("Could not mark as unread", &e);
        }

        if *self.view.peek() == View::Thread(thread.clone()) {
            let (mut view, mut opened) = (self.view, self.opened);

            view.set(View::None);
            opened.set(None);
        }

        self.bump();
    }

    /// Marks every conversation read.
    pub fn mark_all_read(&self) {
        let Some(node) = self.node.peek().clone() else {
            return;
        };

        if let Err(e) = self.store.mark_all_read(&node) {
            self.report("Could not mark all as read", &e);
        }

        self.bump();
    }
}

/// Keeps the theme watcher alive and hands theme changes to the UI.
struct ThemeWatch {
    _watcher: Option<Watcher>,
    rx: RefCell<Option<UnboundedReceiver<Theme>>>,
}

/// Root component.
#[component]
pub fn App() -> Element {
    let boot = consume_context::<Boot>();
    let ui = use_context_provider(|| Ui::new(&boot));
    let mut theme_css = use_signal(|| {
        theme::detect()
            .map(|t| theme::css_vars(&t))
            .unwrap_or_default()
    });

    // The watcher callback runs on notify's thread; a tokio channel carries
    // the Theme to a task on the UI thread, which awaits it (no polling).
    let watch = use_hook(|| {
        let (tx, rx) = unbounded_channel();
        let watcher = theme::watch(move |t| {
            // The receiver is gone only at shutdown.
            let _ = tx.send(t);
        })
        .inspect_err(|e| tracing::warn!("watch theme: {e}"))
        .ok()
        .flatten();

        Rc::new(ThemeWatch {
            _watcher: watcher,
            rx: RefCell::new(Some(rx)),
        })
    });

    use_future(move || {
        let watch = watch.clone();

        async move {
            let Some(mut rx) = watch.rx.borrow_mut().take() else {
                return;
            };

            while let Some(t) = rx.recv().await {
                theme_css.set(theme::css_vars(&t));
            }
        }
    });

    use_future({
        let ui = ui.clone();

        move || {
            let ui = ui.clone();

            async move { ui.run_updates().await }
        }
    });

    let mut now = ui.now;

    use_future(move || async move {
        loop {
            tokio::time::sleep(Duration::from_secs(CLOCK_TICK_SECS)).await;
            now.set(now_secs());
        }
    });

    use_future(move || {
        let hooks = boot.hooks.clone();

        async move {
            if let Some(ms) = hooks.exit_after_ms {
                run_exit_hook(ms, hooks.dump_text).await;
            }
        }
    });

    // A thread opened before the node was known is marked read when it appears.
    use_effect({
        let ui = ui.clone();

        move || {
            let _ = (ui.view)();
            let _ = (ui.node)();
            ui.mark_open_read();
        }
    });

    let unread = use_memo({
        let ui = ui.clone();

        move || {
            let _ = (ui.revision)();

            (ui.node)().map_or(0, |node| {
                ui.store.unread_total(&node).unwrap_or_else(|e| {
                    tracing::warn!("unread total: {e}");
                    0
                })
            })
        }
    });

    use_effect(move || {
        set_window_title(&window_title(unread()));
    });

    let chat_target = use_memo({
        let ui = ui.clone();

        move || {
            let View::Thread(thread) = (ui.view)() else {
                return None;
            };
            let _ = (ui.revision)();

            Some(ChatTarget::of(&ui.store, (ui.node)(), thread))
        }
    });
    let main_view = match (ui.view)() {
        View::None => rsx! { div { class: "placeholder", "Select a conversation" } },
        // A keyed one-item list, because a key alone does not remount a lone
        // component: the draft belongs to one radio and one channel.
        View::Thread(_) => match chat_target() {
            Some(target) => rsx! {
                for target in [target] {
                    Chat {
                        key: "{target.key()}",
                        thread: target.thread.clone(),
                        node: target.node.clone(),
                        secret: target.secret,
                    }
                }
            },
            None => rsx! {},
        },
        View::Contacts => rsx! { Contacts {} },
        View::Settings(tab) => rsx! { Settings { tab } },
        View::Search(query) => rsx! { SearchResults { query } },
    };
    let open = (ui.sidebar_open)();
    let columns = if open {
        format!("{}px 6px minmax(0, 1fr)", (ui.sidebar_width)())
    } else {
        "minmax(0, 1fr)".to_string()
    };

    rsx! {
        BaseStyle {}
        div {
            class: "app",
            // Focusable, so the shortcuts work before anything else has focus.
            tabindex: "0",
            onmounted: move |evt| async move {
                let _ = evt.set_focus(true).await;
            },
            // No WebKit context menu ("Inspect Element"); rows open their own.
            oncontextmenu: |evt| evt.prevent_default(),
            onkeydown: {
                let ui = ui.clone();

                move |evt: KeyboardEvent| {
                    if let Some(shortcut) = shortcut(&evt.key(), evt.modifiers()) {
                        evt.prevent_default();
                        ui.run_shortcut(shortcut);
                    }
                }
            },
            style { {theme_css()} }
            div {
                class: if (ui.drag)().is_some() { "panes dragging" } else { "panes" },
                style: "grid-template-columns: {columns}",
                onmousemove: {
                    let ui = ui.clone();

                    move |evt: MouseEvent| ui.drag_to(evt.client_coordinates().x)
                },
                onmouseup: {
                    let ui = ui.clone();

                    move |_| ui.end_drag()
                },
                onmouseleave: {
                    let ui = ui.clone();

                    move |_| ui.end_drag()
                },
                if open {
                    Sidebar {}
                    div {
                        class: "resizer",
                        onmousedown: {
                            let ui = ui.clone();

                            move |evt: MouseEvent| {
                                evt.prevent_default();
                                ui.begin_drag(evt.client_coordinates().x);
                            }
                        },
                    }
                }
                main {
                    // The composer and the contact and settings fields live
                    // here; a focused button in this pane loses Ctrl+B too.
                    onkeydown: |evt| keep_typing(&evt),
                    if !open {
                        div { class: "main-bar",
                            button {
                                class: "tool",
                                title: "Show sidebar",
                                onclick: {
                                    let ui = ui.clone();

                                    move |_| ui.toggle_sidebar()
                                },
                                Icon { name: "panel-open" }
                            }
                        }
                    }
                    {main_view}
                }
            }
            ExportDialog {}
            Toast {}
        }
    }
}

/// Waits `ms`, optionally dumps the page text, and exits the process.
///
/// Exit code 0 on success; 2 when the dump fails or takes longer than
/// [`DUMP_TIMEOUT`].
async fn run_exit_hook(ms: u64, dump: Option<PathBuf>) {
    tokio::time::sleep(Duration::from_millis(ms)).await;

    let Some(path) = dump else {
        std::process::exit(0);
    };
    let failure = match tokio::time::timeout(DUMP_TIMEOUT, dump_text(&path)).await {
        Ok(Ok(())) => std::process::exit(0),
        Ok(Err(e)) => e,
        Err(_) => "timed out".to_string(),
    };

    eprintln!("hedgerow: dump failed: {failure}");
    std::process::exit(2);
}

/// Writes the page's `innerText` to `path` and the `<main>` text to
/// `<path>.main`.
async fn dump_text(path: &std::path::Path) -> Result<(), String> {
    let body = document::eval("return document.body.innerText;")
        .join::<String>()
        .await
        .map_err(|e| e.to_string())?;

    std::fs::write(path, body).map_err(|e| e.to_string())?;

    let main = document::eval("return document.querySelector('main').innerText;")
        .join::<String>()
        .await
        .map_err(|e| e.to_string())?;
    let mut main_path = path.as_os_str().to_owned();

    main_path.push(".main");
    std::fs::write(main_path, main).map_err(|e| e.to_string())
}

/// The static stylesheet. Lives in its own component that reads no signals, so it renders once;
/// re-rendering `document::Style` from `App` on every state change logs a Dioxus warning.
#[component]
fn BaseStyle() -> Element {
    rsx! {
        document::Style { {include_str!("../assets/style.css")} }
    }
}

#[cfg(test)]
mod tests {
    use hedgerow_core::fixtures;

    use super::*;

    mod parse_start {
        use super::*;

        #[test]
        fn should_parse_a_channel_thread() {
            assert_eq!(
                View::parse_start("thread:channel:2"),
                Some(View::Thread(ThreadId::Channel(2)))
            );
        }

        #[test]
        fn should_parse_a_contact_thread() {
            assert_eq!(
                View::parse_start("thread:contact:a1a1a1a1a1a1"),
                Some(View::Thread(ThreadId::Contact("a1a1a1a1a1a1".into())))
            );
        }

        #[test]
        fn should_parse_contacts() {
            assert_eq!(View::parse_start("contacts"), Some(View::Contacts));
        }

        #[test]
        fn should_parse_each_settings_tab() {
            for (value, tab) in [
                ("settings:connection", SettingsTab::Connection),
                ("settings:device", SettingsTab::Device),
                ("settings:channels", SettingsTab::Channels),
            ] {
                assert_eq!(
                    View::parse_start(value),
                    Some(View::Settings(tab)),
                    "{value}"
                );
            }
        }

        #[test]
        fn should_ignore_unknown_values() {
            for value in [
                "",
                "contact",
                "settings",
                "settings:",
                "settings:other",
                "thread:",
                "thread:channel:",
                "thread:channel:x",
                "thread:channel:300",
                "thread:contact:",
                "thread:room:1",
            ] {
                assert_eq!(View::parse_start(value), None, "{value:?}");
            }
        }
    }

    mod clamp_sidebar {
        use super::*;

        #[test]
        fn should_raise_a_narrow_sidebar_to_180() {
            assert_eq!(clamp_sidebar(0), 180);
            assert_eq!(clamp_sidebar(179), 180);
        }

        #[test]
        fn should_cut_a_wide_sidebar_to_480() {
            assert_eq!(clamp_sidebar(481), 480);
            assert_eq!(clamp_sidebar(u32::MAX), 480);
        }

        #[test]
        fn should_keep_the_limits_and_everything_between() {
            assert_eq!(clamp_sidebar(180), 180);
            assert_eq!(clamp_sidebar(260), 260);
            assert_eq!(clamp_sidebar(480), 480);
        }
    }

    mod dragged_width {
        use super::*;

        #[test]
        fn should_follow_the_pointer_from_the_start_width() {
            assert_eq!(dragged_width(260, 100.0, 140.0), 300);
            assert_eq!(dragged_width(260, 100.0, 60.0), 220);
        }

        #[test]
        fn should_clamp_the_sidebar_far_beyond_either_limit() {
            assert_eq!(dragged_width(260, 100.0, 1.0e9), 480);
            assert_eq!(dragged_width(260, 100.0, -1.0e9), 180);
        }
    }

    mod with_sidebar_width {
        use super::*;

        #[test]
        fn should_return_none_when_the_width_is_unchanged() {
            assert_eq!(with_sidebar_width(&Config::default(), 260), None);
        }

        #[test]
        fn should_change_only_the_sidebar_width() {
            let updated = with_sidebar_width(&Config::default(), 300).unwrap();
            let mut expected = Config::default();

            expected.ui.sidebar_width = 300;

            assert_eq!(updated, expected);
        }
    }

    mod shortcut {
        use super::*;

        fn letter(c: &str) -> Key {
            Key::Character(c.to_string())
        }

        #[test]
        fn should_map_ctrl_b_to_the_sidebar_toggle() {
            assert_eq!(
                shortcut(&letter("b"), Modifiers::CONTROL),
                Some(Shortcut::ToggleSidebar)
            );
            assert_eq!(
                shortcut(&letter("B"), Modifiers::CONTROL),
                Some(Shortcut::ToggleSidebar)
            );
        }

        #[test]
        fn should_map_ctrl_f_to_search_focus() {
            assert_eq!(
                shortcut(&letter("f"), Modifiers::CONTROL),
                Some(Shortcut::FocusSearch)
            );
        }

        #[test]
        fn should_ignore_plain_letters_and_other_chords() {
            assert_eq!(shortcut(&letter("b"), Modifiers::empty()), None);
            assert_eq!(shortcut(&letter("x"), Modifiers::CONTROL), None);
            assert_eq!(shortcut(&Key::Enter, Modifiers::CONTROL), None);
            assert_eq!(
                shortcut(&letter("b"), Modifiers::CONTROL | Modifiers::SHIFT),
                None
            );
            assert_eq!(
                shortcut(&letter("b"), Modifiers::CONTROL | Modifiers::ALT),
                None
            );
        }

        #[test]
        fn should_take_meta_only_on_macos() {
            assert_eq!(
                shortcut(&letter("b"), Modifiers::META).is_some(),
                cfg!(target_os = "macos")
            );
        }
    }

    mod window_title {
        use super::*;

        #[test]
        fn should_be_plain_without_unread() {
            assert_eq!(window_title(0), "Hedgerow");
        }

        #[test]
        fn should_lead_with_the_unread_count() {
            assert_eq!(window_title(7), "(7) Hedgerow");
        }
    }

    mod should_mark_read {
        use super::*;

        #[test]
        fn should_mark_when_a_newer_message_arrives_in_focus() {
            assert!(should_mark_read(Some(4), Some(5), true));
            assert!(should_mark_read(None, Some(1), true));
        }

        #[test]
        fn should_not_mark_when_the_window_is_unfocused() {
            assert!(!should_mark_read(Some(4), Some(5), false));
        }

        #[test]
        fn should_not_mark_when_nothing_newer_arrived() {
            assert!(!should_mark_read(Some(5), Some(5), true));
            assert!(!should_mark_read(Some(5), None, true));
            assert!(!should_mark_read(None, None, true));
        }
    }

    mod node_for {
        use super::*;

        #[test]
        fn should_use_the_connected_device() {
            let store = Store::open_memory().unwrap();
            let device = fixtures::demo_radio().state().device.clone();

            assert_eq!(
                node_for(&ConnState::Connected(device.clone()), &store),
                Some(device.node_id)
            );
        }

        #[test]
        fn should_fall_back_to_the_last_node_when_not_connected() {
            let store = Store::open_memory().unwrap();
            let device = fixtures::demo_radio().state().device.clone();

            store.save_device(&device).unwrap();

            assert_eq!(
                node_for(&ConnState::Disconnected, &store),
                Some(device.node_id)
            );
        }

        #[test]
        fn should_be_none_on_a_first_run() {
            let store = Store::open_memory().unwrap();

            assert_eq!(node_for(&ConnState::Connecting, &store), None);
        }
    }
}
