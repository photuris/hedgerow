//! A conversation: header, message list, and composer.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use chrono::NaiveDate;
use dioxus::prelude::*;
use hedgerow_core::model::{DeliveryStatus, Direction, Message, ThreadId};
use hedgerow_core::session::{Command, Guard};
use hedgerow_core::store::Store;

use super::composer::Composer;
use crate::app::{Ui, should_mark_read, window_focused};
use crate::format;

/// How many of the newest messages a thread shows.
const MESSAGE_LIMIT: u32 = 500;

/// How long a message found by search stays highlighted.
const FLASH: Duration = Duration::from_secs(2);

/// Title and detail line of a thread header.
#[derive(Clone, Debug, PartialEq)]
struct Header {
    /// Channel or contact name.
    title: String,
    /// For contacts: type and route.
    detail: Option<String>,
}

/// Looks up the header of `thread` on `node`.
fn header_for(store: &Store, node: &str, thread: &ThreadId) -> Header {
    match thread {
        ThreadId::Channel(idx) => {
            let name = store
                .channels(node)
                .unwrap_or_default()
                .into_iter()
                .find(|c| c.idx == *idx)
                .map_or_else(|| format!("Channel {idx}"), |c| c.name);

            Header {
                title: name,
                detail: None,
            }
        }
        ThreadId::Contact(prefix) => {
            let contact = store
                .contacts(node)
                .unwrap_or_default()
                .into_iter()
                .find(|c| c.public_key.starts_with(prefix.as_str()));

            match contact {
                Some(c) => Header {
                    detail: Some(format!(
                        "{} · {}",
                        c.kind.label(),
                        format::path_label(c.path_len)
                    )),
                    title: c.name,
                },
                None => Header {
                    title: prefix.clone(),
                    detail: None,
                },
            }
        }
    }
}

/// The newest messages of `thread` on `node`: at least `limit` of them, and
/// when `reveal` names a message, enough to include it. The count doubles
/// until the oldest loaded message is no newer than `reveal` or the thread
/// is exhausted, and `limit` keeps the final count so later reloads still
/// include the message.
fn load_messages(
    store: &Store,
    node: &str,
    thread: &ThreadId,
    reveal: Option<i64>,
    limit: &Cell<u32>,
) -> Vec<Message> {
    loop {
        let wanted = limit.get();
        let messages = store.messages(node, thread, wanted).unwrap_or_else(|e| {
            tracing::warn!("read messages: {e}");
            Vec::new()
        });
        let exhausted = u32::try_from(messages.len()).is_ok_and(|n| n < wanted);
        let reaches = reveal.is_none_or(|id| messages.first().is_some_and(|m| m.id <= id));

        if exhausted || reaches {
            return messages;
        }

        limit.set(wanted.saturating_mul(2));
    }
}

/// The script that brings message `reveal` to the middle of the list, or the
/// newest message to the bottom when there is no such message.
fn scroll_script(reveal: Option<i64>) -> String {
    let target = reveal.map_or_else(String::new, |id| {
        format!("const m = document.getElementById('msg-{id}'); if (m) {{ m.scrollIntoView({{ block: 'center' }}); return; }}")
    });

    format!(
        "const e = document.getElementById('msgs'); {target} if (e) {{ e.scrollTop = e.scrollHeight; }}"
    )
}

/// The messages of `thread`, each with the day separator that precedes it.
fn with_day_labels(messages: &[Message], now: i64) -> Vec<(Message, Option<String>)> {
    let mut previous: Option<NaiveDate> = None;

    messages
        .iter()
        .map(|m| {
            let ts = format::display_ts(m);
            let day = format::local_date(ts);
            let label = (previous != Some(day)).then(|| format::day_label(now, ts));

            previous = Some(day);

            (m.clone(), label)
        })
        .collect()
}

/// The chat view of one thread on `node`. `secret` is what the thread's
/// channel slot held when it opened. Mount it with a key that changes with
/// the node, the thread, and that secret: the draft belongs to them.
#[component]
pub fn Chat(thread: ThreadId, node: Option<String>, secret: Option<[u8; 16]>) -> Element {
    let ui = use_context::<Ui>();
    let header = use_memo({
        let (ui, thread) = (ui.clone(), thread.clone());

        move || {
            let _ = (ui.revision)();

            (ui.node)().map_or_else(
                || Header {
                    title: String::new(),
                    detail: None,
                },
                |node| header_for(&ui.store, &node, &thread),
            )
        }
    });
    // The message a search result pointed at, read once when the chat opens.
    let reveal = use_hook(|| *ui.reveal.peek());
    let limit = use_hook(|| Rc::new(Cell::new(MESSAGE_LIMIT)));
    let messages = use_memo({
        let (ui, thread) = (ui.clone(), thread.clone());

        move || {
            let _ = (ui.revision)();

            (ui.node)().map_or_else(Vec::new, |node| {
                load_messages(&ui.store, &node, &thread, reveal, &limit)
            })
        }
    });
    let newest = use_memo(move || messages.read().last().map(|m| m.id));
    let newest_in = use_memo(move || {
        messages
            .read()
            .iter()
            .rev()
            .find(|m| m.direction == Direction::In)
            .map(|m| m.id)
    });

    // Newest incoming id seen so far; a growing id means a new arrival.
    let mut seen = use_signal(|| *newest_in.peek());

    use_effect({
        let (ui, thread) = (ui.clone(), thread.clone());

        move || {
            let latest = newest_in();
            let focused = window_focused();

            if should_mark_read(*seen.peek(), latest, focused) {
                ui.mark_read(&thread);
            }

            seen.set(latest);
        }
    });

    // Scroll when the thread opens and whenever the newest displayed message
    // changes. Opening from a search result scrolls to that message instead.
    let opening = use_hook(|| Rc::new(Cell::new(true)));

    use_effect(move || {
        let _ = newest();

        let target = opening.replace(false).then_some(reveal).flatten();
        let _ = document::eval(&scroll_script(target));
    });

    // Flash the found message, and let the next opening start fresh.
    let mut flash = use_signal(|| None::<i64>);

    use_future({
        let ui = ui.clone();

        move || {
            let mut shared = ui.reveal;

            async move {
                let Some(id) = reveal else {
                    return;
                };

                shared.set(None);
                flash.set(Some(id));
                tokio::time::sleep(FLASH).await;
                flash.set(None);
            }
        }
    });

    let is_channel = matches!(thread, ThreadId::Channel(_));
    let flashing = flash();
    let items = with_day_labels(&messages.read(), (ui.now)());
    let Header { title, detail } = header();

    rsx! {
        div { class: "chat",
            div { class: "pane-head",
                div { class: "pane-head-text",
                    div { class: "pane-title", "{title}" }
                    if let Some(detail) = detail {
                        div { class: "pane-detail", "{detail}" }
                    }
                }
                button {
                    onclick: {
                        let thread = thread.clone();

                        move |_| ui.mark_unread(&thread)
                    },
                    "Mark as unread"
                }
            }
            div { id: "msgs", class: "msgs",
                if items.is_empty() {
                    div { class: "empty", "No messages yet" }
                }
                for (message , day) in items {
                    Bubble {
                        key: "{message.id}",
                        flash: flashing == Some(message.id),
                        message,
                        day,
                        is_channel,
                        node: node.clone(),
                    }
                }
            }
            Composer { thread: thread.clone(), node: node.clone(), secret }
        }
    }
}

/// One message bubble, with the day separator above it when it starts a day.
#[component]
fn Bubble(
    message: Message,
    day: Option<String>,
    is_channel: bool,
    node: Option<String>,
    flash: bool,
) -> Element {
    let ui = use_context::<Ui>();
    let outgoing = message.direction == Direction::Out;
    let sender = (is_channel && !outgoing)
        .then(|| message.sender.clone())
        .flatten();
    let clock = format::clock(format::display_ts(&message));
    let glyph = format::status_glyph(message.status);
    let failed = message.status == DeliveryStatus::Failed;
    let id = message.id;

    rsx! {
        if let Some(day) = day {
            div { class: "day", "{day}" }
        }
        div {
            id: "msg-{id}",
            class: match (outgoing, flash) {
                (true, false) => "msg out",
                (false, false) => "msg in",
                (true, true) => "msg out flash",
                (false, true) => "msg in flash",
            },
            if let Some(sender) = sender {
                div {
                    class: "msg-sender",
                    style: "color: hsl({format::name_hue(&sender)} 60% 60%)",
                    "{sender}"
                }
            }
            div { class: "bubble", title: "{format::hover_title(&message)}", "{message.text}" }
            div { class: "msg-meta",
                span { "{clock}" }
                if outgoing && !glyph.is_empty() {
                    span { class: if failed { "failed" } else { "" }, "{glyph}" }
                }
                if failed {
                    button {
                        onclick: move |_| {
                            if let Some(node) = &node {
                                ui.send_guarded(node, Guard::Node, Command::Retry { message_id: id });
                            }
                        },
                        "Retry"
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod scroll_script {
        use super::*;

        #[test]
        fn should_aim_at_the_revealed_message_and_fall_back_to_the_bottom() {
            let script = scroll_script(Some(42));

            assert!(script.contains("getElementById('msg-42')"), "{script}");
            assert!(script.contains("scrollIntoView"), "{script}");
            assert!(script.contains("e.scrollTop = e.scrollHeight"), "{script}");
        }

        #[test]
        fn should_only_scroll_to_the_bottom_without_a_reveal() {
            let script = scroll_script(None);

            assert!(!script.contains("msg-"), "{script}");
            assert!(script.contains("e.scrollTop = e.scrollHeight"), "{script}");
        }
    }
}
