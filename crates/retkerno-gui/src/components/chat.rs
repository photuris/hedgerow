//! A conversation: header, message list, and composer.

use chrono::NaiveDate;
use dioxus::prelude::*;
use retkerno_core::model::{DeliveryStatus, Direction, Message, ThreadId};
use retkerno_core::session::Command;
use retkerno_core::store::Store;

use super::composer::Composer;
use crate::app::{Ui, should_mark_read};
use crate::format;

/// How many of the newest messages a thread shows.
const MESSAGE_LIMIT: u32 = 500;

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

/// The chat view of one thread. Mount it with a key per thread.
#[component]
pub fn Chat(thread: ThreadId) -> Element {
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
    let messages = use_memo({
        let (ui, thread) = (ui.clone(), thread.clone());

        move || {
            let _ = (ui.revision)();

            (ui.node)().map_or_else(Vec::new, |node| {
                ui.store
                    .messages(&node, &thread, MESSAGE_LIMIT)
                    .unwrap_or_else(|e| {
                        tracing::warn!("read messages: {e}");
                        Vec::new()
                    })
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
            let focused = dioxus::desktop::window().is_focused();

            if should_mark_read(*seen.peek(), latest, focused) {
                ui.mark_read(&thread);
            }

            seen.set(latest);
        }
    });

    // Scroll when the thread opens and whenever the newest displayed message changes.
    use_effect(move || {
        let _ = newest();
        let _ = document::eval(
            "const e = document.getElementById('msgs'); if (e) { e.scrollTop = e.scrollHeight; }",
        );
    });

    let is_channel = matches!(thread, ThreadId::Channel(_));
    let items = with_day_labels(&messages.read(), (ui.now)());
    let Header { title, detail } = header();

    rsx! {
        div { class: "chat",
            div { class: "chat-head",
                div { class: "chat-head-text",
                    div { class: "chat-title", "{title}" }
                    if let Some(detail) = detail {
                        div { class: "chat-detail", "{detail}" }
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
                    div { class: "msgs-empty", "No messages yet" }
                }
                for (message , day) in items {
                    Bubble { key: "{message.id}", message, day, is_channel }
                }
            }
            Composer { thread: thread.clone() }
        }
    }
}

/// One message bubble, with the day separator above it when it starts a day.
#[component]
fn Bubble(message: Message, day: Option<String>, is_channel: bool) -> Element {
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
        div { class: if outgoing { "msg out" } else { "msg in" },
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
                    button { onclick: move |_| ui.session.send(Command::Retry { message_id: id }), "Retry" }
                }
            }
        }
    }
}
