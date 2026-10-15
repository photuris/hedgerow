//! Desktop notification policy: which incoming messages deserve a
//! notification, what it says, and when it may appear.
//!
//! [`Notifier`] decides from the [`Store`] and an explicit clock. It does no
//! I/O besides the store it is handed and never reads the time, so a caller
//! (or a test) drives it by passing `now` to every handler.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::model::{NotifyMode, ThreadId};
use crate::store::Store;

/// Minimum time between two notifications for one conversation.
pub const THROTTLE_WINDOW: Duration = Duration::from_secs(60);

/// Longest notification body, in characters, ellipsis included.
pub const BODY_MAX_CHARS: usize = 200;

/// Returns whether `text` mentions `name`, as `@[name]` or as `@name` not
/// followed by a letter, digit, or underscore. Case-insensitive; a blank
/// `name` never matches.
pub fn mentions(text: &str, name: &str) -> bool {
    if name.trim().is_empty() {
        return false;
    }

    let text = text.to_lowercase();
    let name = name.to_lowercase();

    if text.contains(&format!("@[{name}]")) {
        return true;
    }

    let needle = format!("@{name}");

    text.match_indices(&needle).any(|(at, _)| {
        text[at + needle.len()..]
            .chars()
            .next()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
    })
}

/// Returns whether a message with `text` in `thread` raises a notification
/// under `mode`, for a radio named `self_name`.
pub fn wants(mode: NotifyMode, thread: &ThreadId, text: &str, self_name: &str) -> bool {
    match mode {
        NotifyMode::Mute => false,
        NotifyMode::All => true,
        NotifyMode::Default => match thread {
            ThreadId::Contact(_) => true,
            ThreadId::Channel(_) => mentions(text, self_name),
        },
    }
}

/// Returns the notification title: `title`, plus the count when `count`
/// rows are folded into one notification.
pub fn summary_text(title: &str, count: u32) -> String {
    if count <= 1 {
        title.to_string()
    } else {
        format!("{title} ({count} new)")
    }
}

/// Returns the notification body for a message: `sender: text`, or just
/// `text` without a sender, cut to [`BODY_MAX_CHARS`] characters with a
/// trailing `…`.
pub fn body_text(sender: Option<&str>, text: &str) -> String {
    let full = match sender {
        Some(sender) => format!("{sender}: {text}"),
        None => text.to_string(),
    };

    if full.chars().count() <= BODY_MAX_CHARS {
        return full;
    }

    let mut cut: String = full.chars().take(BODY_MAX_CHARS - 1).collect();

    cut.push('…');

    cut
}

/// One notification to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Post {
    /// The radio the rows belong to.
    pub node: String,
    /// The conversation as it is addressed now (its current slot).
    pub thread: ThreadId,
    /// Store id of the newest row the post stands for.
    pub id: i64,
    /// How many rows the post stands for; at least 1.
    pub count: u32,
    /// Title, from [`summary_text`] with the conversation's title.
    pub summary: String,
    /// Body, from [`body_text`] with the newest row's sender and text.
    pub body: String,
}

/// Decides what to post for newly stored rows, coalescing bursts so a
/// conversation notifies at most once per window.
pub struct Notifier {
    /// Cooldown between two posts for one conversation.
    window: Duration,
    /// The node the state below belongs to.
    bound: Option<String>,
    /// Row ids announced while the window was unfocused and not yet covered
    /// by a post decision.
    fresh: HashSet<i64>,
    /// When each conversation last posted.
    last_shown: HashMap<ThreadId, Instant>,
    /// Deadline of each conversation that wanted fresh rows inside its
    /// cooldown.
    waiting: HashMap<ThreadId, Instant>,
}

impl Notifier {
    /// Creates a notifier whose conversations cool down for `window` after a
    /// post.
    pub fn new(window: Duration) -> Notifier {
        Notifier {
            window,
            bound: None,
            fresh: HashSet::new(),
            last_shown: HashMap::new(),
            waiting: HashMap::new(),
        }
    }

    /// Handles a drain that stored `ids` for `node`.
    ///
    /// `current` is the node of the radio the session holds (`None` when
    /// disconnected) and `focused` whether the app window has focus. Returns
    /// the posts to show right now; rows held back by a cooldown are posted
    /// by [`Notifier::on_due`] once [`Notifier::next_due`] passes.
    pub fn on_received(
        &mut self,
        store: &Store,
        current: Option<&str>,
        focused: bool,
        node: &str,
        ids: &[i64],
        now: Instant,
    ) -> Vec<Post> {
        self.rebind(current);

        if self.bound.as_deref() != Some(node) {
            return Vec::new();
        }

        if focused {
            self.forget_pending();

            return Vec::new();
        }

        self.fresh.extend(ids);

        self.evaluate(store, node, now)
    }

    /// Handles the clock reaching [`Notifier::next_due`] (or any later
    /// moment): posts what a cooldown held back and is still wanted.
    pub fn on_due(
        &mut self,
        store: &Store,
        current: Option<&str>,
        focused: bool,
        now: Instant,
    ) -> Vec<Post> {
        self.rebind(current);

        let Some(node) = self.bound.clone() else {
            return Vec::new();
        };

        if focused {
            self.forget_pending();

            return Vec::new();
        }

        self.evaluate(store, &node, now)
    }

    /// Returns the earliest moment [`Notifier::on_due`] has something to
    /// post, or `None` when nothing is waiting.
    pub fn next_due(&self) -> Option<Instant> {
        self.waiting.values().min().copied()
    }

    /// Drops all state when the held radio changed.
    fn rebind(&mut self, current: Option<&str>) {
        if self.bound.as_deref() == current {
            return;
        }

        self.fresh.clear();
        self.last_shown.clear();
        self.waiting.clear();
        self.bound = current.map(str::to_string);
    }

    /// Forgets everything announced so far; cooldowns stay.
    fn forget_pending(&mut self) {
        self.fresh.clear();
        self.waiting.clear();
    }

    /// Walks the unread conversations of `node`, posting those whose
    /// cooldown allows it and scheduling the rest.
    fn evaluate(&mut self, store: &Store, node: &str, now: Instant) -> Vec<Post> {
        let self_name = match store.device(node) {
            Ok(device) => device.map(|d| d.name).unwrap_or_default(),
            Err(e) => {
                tracing::warn!(error = %e, "notify: reading the device failed");

                return Vec::new();
            }
        };
        let summaries = match store.threads(node) {
            Ok(summaries) => summaries,
            Err(e) => {
                tracing::warn!(error = %e, "notify: reading the threads failed");

                return Vec::new();
            }
        };
        let mut posts = Vec::new();
        let mut live = HashSet::new();

        self.waiting.clear();

        for summary in summaries.iter().filter(|s| s.unread > 0) {
            let rows = match store.unread_incoming(node, &summary.thread) {
                Ok(rows) => rows,
                Err(e) => {
                    tracing::warn!(error = %e, "notify: reading unread rows failed");

                    continue;
                }
            };

            live.extend(rows.iter().map(|r| r.id));

            let pending: Vec<_> = rows.iter().filter(|r| self.fresh.contains(&r.id)).collect();

            if pending.is_empty() {
                continue;
            }

            let wanted: Vec<_> = pending
                .iter()
                .filter(|r| wants(summary.notify, &summary.thread, &r.text, &self_name))
                .collect();
            let cooled_at = self
                .last_shown
                .get(&summary.thread)
                .map(|t| *t + self.window);

            if let Some(cooled_at) = cooled_at
                && now < cooled_at
            {
                self.waiting.insert(summary.thread.clone(), cooled_at);

                continue;
            }

            for row in &pending {
                self.fresh.remove(&row.id);
            }

            let Some(newest) = wanted.last() else {
                continue;
            };
            let count = u32::try_from(wanted.len()).unwrap_or(u32::MAX);

            self.last_shown.insert(summary.thread.clone(), now);
            posts.push(Post {
                node: node.to_string(),
                thread: summary.thread.clone(),
                id: newest.id,
                count,
                summary: summary_text(&summary.title, count),
                body: body_text(newest.sender.as_deref(), &newest.text),
            });
        }

        self.fresh.retain(|id| live.contains(id));

        posts
    }
}
