//! The message search results shown in the main region.

use dioxus::prelude::*;
use hedgerow_core::model::{Message, ThreadId, ThreadSummary};

use crate::app::{ChatTarget, Ui};
use crate::format;

/// How many results the list shows at most.
const RESULT_LIMIT: u32 = 200;

/// The title to show for `thread`: its row in `summaries`, else a name made
/// from the thread itself (a slot with no stored channel has no row).
fn thread_title(summaries: &[ThreadSummary], thread: &ThreadId) -> String {
    summaries.iter().find(|s| s.thread == *thread).map_or_else(
        || match thread {
            ThreadId::Channel(idx) => format!("Channel {idx}"),
            ThreadId::Contact(prefix) => prefix.clone(),
        },
        |s| s.title.clone(),
    )
}

/// The newest messages of the connected node that contain `query`, newest
/// first. The query is the text as typed.
#[component]
pub fn SearchResults(query: String) -> Element {
    let ui = use_context::<Ui>();
    let _ = (ui.revision)();

    let query = query.trim().to_string();
    let node = (ui.node)();
    let (hits, summaries, channels) = match &node {
        Some(node) => (
            ui.store
                .search(node, &query, RESULT_LIMIT)
                .unwrap_or_else(|e| {
                    tracing::warn!("search messages: {e}");
                    Vec::new()
                }),
            ui.store.threads(node).unwrap_or_default(),
            ui.store.channels(node).unwrap_or_default(),
        ),
        None => (Vec::new(), Vec::new(), Vec::new()),
    };
    let now = (ui.now)();
    let detail = result_count(hits.len(), &query);

    rsx! {
        div { class: "pane-head",
            div { class: "pane-head-text",
                div { class: "pane-title", "Search" }
                div { class: "pane-detail", "{detail}" }
            }
        }
        div { class: "scroll",
            if hits.is_empty() {
                div { class: "empty", "No messages match" }
            }
            for message in hits {
                Hit {
                    key: "{message.id}",
                    title: thread_title(&summaries, &message.thread),
                    target: ChatTarget::in_channels(&channels, node.clone(), message.thread.clone()),
                    query: query.clone(),
                    now,
                    message,
                }
            }
        }
    }
}

/// The header's detail line: how many messages match `query`, and that only
/// the newest are listed when there are more than [`RESULT_LIMIT`].
fn result_count(count: usize, query: &str) -> String {
    match count {
        1 => format!("1 message matches “{query}”"),
        n if n >= RESULT_LIMIT as usize => format!("Newest {n} messages matching “{query}”"),
        n => format!("{n} messages match “{query}”"),
    }
}

/// One result: thread title, sender, time, and the text with the match marked.
/// Clicking it opens the thread at that message, unless the node or channel
/// it was drawn for (`target`) has changed.
#[component]
fn Hit(message: Message, title: String, target: ChatTarget, query: String, now: i64) -> Element {
    let ui = use_context::<Ui>();
    let time = format::relative_time(now, format::display_ts(&message));
    let id = message.id;

    rsx! {
        div {
            class: "result",
            onclick: move |_| ui.open_message(&target, id),
            div { class: "result-top",
                div { class: "result-title", "{title}" }
                if let Some(sender) = &message.sender {
                    div { class: "result-sender", {marked(sender, &query)} }
                }
                div { class: "result-time", "{time}" }
            }
            div { class: "result-text", {marked(&message.text, &query)} }
        }
    }
}

/// `text` with every match of `query` wrapped in `<mark>`.
fn marked(text: &str, query: &str) -> Element {
    let pieces = format::highlight(text, query);

    rsx! {
        for (i , (piece , hit)) in pieces.into_iter().enumerate() {
            span { key: "{i}",
                if hit {
                    mark { "{piece}" }
                } else {
                    "{piece}"
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hedgerow_core::model::NotifyMode;

    fn summary(thread: ThreadId, title: &str) -> ThreadSummary {
        ThreadSummary {
            thread,
            title: title.into(),
            last_text: None,
            last_at: None,
            unread: 0,
            notify: NotifyMode::Default,
            archived: false,
        }
    }

    mod thread_title {
        use super::*;

        #[test]
        fn should_use_the_summary_title_when_the_thread_is_listed() {
            let listed = [summary(ThreadId::Channel(1), "#test")];

            assert_eq!(thread_title(&listed, &ThreadId::Channel(1)), "#test");
        }

        #[test]
        fn should_name_an_unlisted_slot_and_an_unlisted_contact() {
            assert_eq!(thread_title(&[], &ThreadId::Channel(4)), "Channel 4");
            assert_eq!(
                thread_title(&[], &ThreadId::Contact("a1a1a1".into())),
                "a1a1a1"
            );
        }
    }

    mod result_count {
        use super::*;

        #[test]
        fn should_say_newest_when_the_list_is_capped() {
            assert_eq!(result_count(1, "hi"), "1 message matches “hi”");
            assert_eq!(result_count(3, "hi"), "3 messages match “hi”");
            assert_eq!(result_count(200, "hi"), "Newest 200 messages matching “hi”");
        }
    }
}
