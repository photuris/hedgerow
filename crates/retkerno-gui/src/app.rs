//! Root component and the state every view shares.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dioxus::prelude::*;
use retkerno_core::config::Config;
use retkerno_core::model::ThreadId;
use retkerno_core::radio::FakeRadio;
use retkerno_core::session::{ConnState, Connector, Session, Update};
use retkerno_core::store::Store;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::components::{Chat, Contacts, ExportDialog, Settings, Sidebar, Toast};
use crate::theme::{self, Theme, Watcher};

/// How long a toast stays up.
const TOAST_SECS: u64 = 4;

/// How often relative times are recomputed.
const CLOCK_TICK_SECS: u64 = 30;

/// How long the text dump may take after the exit timer fired.
const DUMP_TIMEOUT: Duration = Duration::from_secs(10);

/// The test hooks, read from the environment once in `main`.
#[derive(Clone, Default)]
pub struct Hooks {
    /// `RETKERNO_EXIT_AFTER_MS`: exit after this many milliseconds.
    pub exit_after_ms: Option<u64>,
    /// `RETKERNO_DUMP_TEXT`: write the page text here before exiting.
    pub dump_text: Option<PathBuf>,
    /// `RETKERNO_START_VIEW`: the initial view, see [`View::parse_start`].
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
}

impl View {
    /// Parses `RETKERNO_START_VIEW`: `contacts`,
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

/// The window title: `Retkerno`, or `(N) Retkerno` with unread messages.
pub fn window_title(unread: u32) -> String {
    if unread == 0 {
        "Retkerno".to_string()
    } else {
        format!("({unread}) Retkerno")
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

/// Whether the open thread should be marked read again: only when its newest
/// incoming message id grew and the window has focus.
pub fn should_mark_read(previous: Option<i64>, newest: Option<i64>, focused: bool) -> bool {
    focused && newest > previous
}

/// Current unix time in seconds.
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
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
    /// Kept alive between copies: on X11 the contents die with the handle.
    clipboard: Rc<RefCell<Option<arboard::Clipboard>>>,
    /// Identifies the latest toast, so an old timer does not clear a new one.
    toast_seq: Signal<u64>,
    /// The (thread, node) pair last marked read by opening it.
    opened: Signal<Option<(ThreadId, String)>>,
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
            clipboard: Rc::new(RefCell::new(None)),
            now: Signal::new(now_secs()),
            toast_seq: Signal::new(0),
            opened: Signal::new(None),
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
    fn report(&self, what: &str, error: &retkerno_core::Error) {
        tracing::warn!("{what}: {error}");
        self.show_toast(format!("{what}: {error}"));
    }

    /// Takes the state from the session (the authority) and recomputes the node.
    fn sync_state(&self) {
        self.apply_state(self.session.state());
    }

    /// Stores `state` and recomputes the node.
    fn apply_state(&self, state: ConnState) {
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

    /// Opens `thread`: shows it and marks it read.
    pub fn select(&self, thread: ThreadId) {
        let (mut view, mut opened) = (self.view, self.opened);

        // Forget the last open so clicking the open thread marks it read again.
        opened.set(None);
        view.set(View::Thread(thread));
        self.mark_open_read();
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
        dioxus::desktop::window().set_title(&window_title(unread()));
    });

    let main_view = match (ui.view)() {
        View::None => rsx! { div { class: "placeholder", "Select a conversation" } },
        View::Thread(thread) => {
            let key = format!("{}:{}", thread.kind(), thread.key());

            rsx! { Chat { key: "{key}", thread } }
        }
        View::Contacts => rsx! { Contacts {} },
        View::Settings(tab) => rsx! { Settings { tab } },
    };

    rsx! {
        BaseStyle {}
        div {
            class: "app",
            // No WebKit context menu ("Inspect Element"); rows open their own.
            oncontextmenu: |evt| evt.prevent_default(),
            style { {theme_css()} }
            Sidebar {}
            main { {main_view} }
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

    eprintln!("retkerno: dump failed: {failure}");
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
    use retkerno_core::fixtures;

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

    mod window_title {
        use super::*;

        #[test]
        fn should_be_plain_without_unread() {
            assert_eq!(window_title(0), "Retkerno");
        }

        #[test]
        fn should_lead_with_the_unread_count() {
            assert_eq!(window_title(7), "(7) Retkerno");
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
