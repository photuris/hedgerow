//! Desktop notifications: turns the posts of the core
//! [`Notifier`](hedgerow_core::notify::Notifier) into OS notifications.
//!
//! Every timing and filtering decision is made in `hedgerow_core::notify`;
//! this module only feeds it the session's updates, the clock, and the window
//! focus, and shows what it returns. Under `cfg(test)` nothing reaches the OS:
//! posts are collected for the tests to read.

#[cfg(test)]
use std::cell::RefCell;
#[cfg(all(unix, not(target_os = "macos")))]
use std::collections::HashMap;
#[cfg(test)]
use std::collections::HashSet;
#[cfg(all(unix, not(target_os = "macos")))]
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Instant;

use hedgerow_core::notify::{Notifier, Post, THROTTLE_WINDOW};
use hedgerow_core::session::Update;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{self, Ui};

/// A conversation: its node and thread key.
#[cfg(all(unix, not(target_os = "macos")))]
type ConversationKey = (String, String);

/// The OS notification of each conversation: its OS id and the row id last
/// shown in it.
#[cfg(all(unix, not(target_os = "macos")))]
static NOTIFICATION_IDS: OnceLock<Mutex<HashMap<ConversationKey, (u32, i64)>>> = OnceLock::new();

#[cfg(test)]
thread_local! {
    /// The posts [`show`] collected instead of raising them.
    static POSTED: RefCell<Vec<Post>> = const { RefCell::new(Vec::new()) };
    /// The row ids of every `Received` batch handed to the notifier.
    static HANDLED: RefCell<HashSet<i64>> = RefCell::new(HashSet::new());
}

/// Runs for the life of the app: feeds session updates to a core notifier and
/// shows its posts. A click on a Linux notification sends `(node, row id)` on
/// `clicks`.
pub async fn run(ui: Ui, clicks: UnboundedSender<(String, i64)>) {
    let mut updates = ui.session.subscribe();
    let mut notifier = Notifier::new(THROTTLE_WINDOW);

    loop {
        let due = notifier.next_due();
        let wait = async move {
            match due {
                Some(due) => tokio::time::sleep_until(tokio::time::Instant::from_std(due)).await,
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            event = updates.recv() => match event {
                Ok(Update::Received { node, ids }) => {
                    let (current, focused) = surroundings(&ui);
                    let posts = notifier.on_received(
                        &ui.store,
                        current.as_deref(),
                        focused,
                        &node,
                        &ids,
                        Instant::now(),
                    );

                    #[cfg(test)]
                    HANDLED.with(|handled| handled.borrow_mut().extend(&ids));

                    for post in posts {
                        show(&ui, post, &clicks);
                    }
                }
                // Batches lost to lag raise nothing; PLAN.md accepts that.
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return,
            },
            () = wait => {
                let (current, focused) = surroundings(&ui);

                for post in notifier.on_due(&ui.store, current.as_deref(), focused, Instant::now())
                {
                    show(&ui, post, &clicks);
                }
            }
        }
    }
}

/// The node of the radio the session holds right now, and whether the window
/// has focus.
fn surroundings(ui: &Ui) -> (Option<String>, bool) {
    (
        app::connected_node(&ui.session.state()),
        app::window_focused(),
    )
}

/// Collects `post` for the tests instead of raising it.
#[cfg(test)]
fn show(_ui: &Ui, post: Post, _clicks: &UnboundedSender<(String, i64)>) {
    POSTED.with(|posted| posted.borrow_mut().push(post));
}

/// Raises `post` as an OS notification on its own thread, because
/// notify-rust blocks. UI-test mode raises nothing.
#[cfg(not(test))]
fn show(ui: &Ui, post: Post, clicks: &UnboundedSender<(String, i64)>) {
    if ui.ui_test {
        return;
    }

    let clicks = clicks.clone();

    std::thread::spawn(move || deliver(&post, &clicks));
}

/// Shows `post` and, on Linux, waits for a click on it.
#[cfg(not(test))]
fn deliver(post: &Post, clicks: &UnboundedSender<(String, i64)>) {
    #[cfg(target_os = "macos")]
    {
        static APPLICATION: std::sync::Once = std::sync::Once::new();

        APPLICATION.call_once(|| {
            // A failure leaves the default identity; the notification still shows.
            let _ = notify_rust::set_application("io.github.photuris.hedgerow");
        });
    }

    let mut notification = notify_rust::Notification::new();

    notification
        .appname("Hedgerow")
        .summary(&post.summary)
        .body(&post.body);

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let key = (post.node.clone(), post.thread.key());

        if let Some(os_id) = shown_id(&key) {
            notification.id(os_id);
        }

        notification
            .icon("hedgerow")
            .hint(notify_rust::Hint::DesktopEntry("hedgerow".into()))
            .action("default", "Open");

        match notification.show() {
            Err(e) => tracing::warn!("show notification: {e}"),
            Ok(handle) => {
                record_shown(key.clone(), handle.id(), post.id);

                // ponytail: one waiter thread per shown notification. An older
                // waiter for a replaced notification stays blocked until the
                // daemon closes it, and `click_is_current` makes it ignore the
                // click; a panic inside notify-rust ends only that thread.
                handle.wait_for_action(|action| {
                    if action == "default" && click_is_current(&key, post.id) {
                        // The receiver is gone only at shutdown.
                        let _ = clicks.send((post.node.clone(), post.id));
                    }
                });
            }
        }
    }

    // macOS sends on handle drop and cannot report delivery errors; Windows
    // shows under PowerShell's identity. PLAN.md accepts both.
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        let _ = clicks;

        if let Err(e) = notification.show() {
            tracing::warn!("show notification: {e}");
        }
    }
}

/// The OS id of the notification currently standing for `key`, if any.
#[cfg(all(unix, not(target_os = "macos"), not(test)))]
fn shown_id(key: &ConversationKey) -> Option<u32> {
    ids().get(key).map(|(os_id, _)| *os_id)
}

/// Locks the notification table, recovering from a poisoned lock.
#[cfg(all(unix, not(target_os = "macos")))]
fn ids() -> std::sync::MutexGuard<'static, HashMap<ConversationKey, (u32, i64)>> {
    NOTIFICATION_IDS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Records that the notification `os_id` of conversation `key` now shows row
/// `row`.
#[cfg(all(unix, not(target_os = "macos")))]
fn record_shown(key: ConversationKey, os_id: u32, row: i64) {
    ids().insert(key, (os_id, row));
}

/// Whether a click on a notification that showed row `row` still belongs to
/// the conversation's current notification.
#[cfg(all(unix, not(target_os = "macos")))]
fn click_is_current(key: &ConversationKey, row: i64) -> bool {
    ids().get(key).is_some_and(|(_, shown)| *shown == row)
}

/// Takes the posts collected so far.
#[cfg(test)]
pub fn take_posted() -> Vec<Post> {
    POSTED.with(|posted| std::mem::take(&mut *posted.borrow_mut()))
}

/// Whether a `Received` batch carrying row `id` has been handed to the
/// notifier.
#[cfg(test)]
pub fn handled(id: i64) -> bool {
    HANDLED.with(|handled| handled.borrow().contains(&id))
}

#[cfg(all(test, unix, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn older_notification_click_is_stale() {
        let key = (
            "stale-click-node".to_string(),
            "stale-click-thread".to_string(),
        );

        record_shown(key.clone(), 7, 1);
        record_shown(key.clone(), 7, 2);

        assert!(!click_is_current(&key, 1));
        assert!(click_is_current(&key, 2));
    }
}
