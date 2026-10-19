//! The contact list: search, kind filter, per-contact actions, and the
//! import and export dialogs.

use dioxus::prelude::*;
use hedgerow_core::model::{Contact, ContactKind, ThreadId};
use hedgerow_core::session::{Command, Guard};

use super::confirm::{Confirm, Modal, Pending, use_pending};
use crate::app::{Ui, connected_node};
use crate::format;

/// The filter chips, in display order.
const KIND_CHIPS: [(Option<ContactKind>, &str); 5] = [
    (None, "All"),
    (Some(ContactKind::Chat), "Chat"),
    (Some(ContactKind::Repeater), "Repeater"),
    (Some(ContactKind::Room), "Room"),
    (Some(ContactKind::Sensor), "Sensor"),
];

/// The tooltip of a button that needs a connected session, `None` when it
/// has one.
fn needs_connection(connected: bool) -> Option<&'static str> {
    if connected {
        None
    } else {
        Some("Not connected")
    }
}

/// The header's detail line: how many contacts the filters leave of `total`.
fn contact_count(shown: usize, total: usize) -> String {
    match (shown, total) {
        (_, 1) if shown == total => "1 contact".to_string(),
        (s, t) if s == t => format!("{t} contacts"),
        (s, t) => format!("{s} of {t} contacts"),
    }
}

/// Keeps the contacts of `kind` (any kind for `None`) whose name contains
/// `query`, ignoring case, or whose public key starts with it.
fn filter_contacts(contacts: &[Contact], query: &str, kind: Option<ContactKind>) -> Vec<Contact> {
    let query = query.trim().to_lowercase();

    contacts
        .iter()
        .filter(|c| kind.is_none_or(|k| c.kind == k))
        .filter(|c| {
            query.is_empty()
                || c.name.to_lowercase().contains(&query)
                || c.public_key.to_lowercase().starts_with(&query)
        })
        .cloned()
        .collect()
}

/// The Contacts view.
#[component]
pub fn Contacts() -> Element {
    let ui = use_context::<Ui>();
    let mut query = use_signal(String::new);
    let mut kind = use_signal(|| None::<ContactKind>);
    let mut importing = use_pending::<()>();
    let mut removing = use_pending::<Contact>();
    let contacts = use_memo({
        let ui = ui.clone();

        move || {
            let _ = (ui.revision)();

            match (ui.node)() {
                Some(node) => ui.store.contacts(&node).unwrap_or_else(|e| {
                    tracing::warn!("list contacts: {e}");
                    Vec::new()
                }),
                None => Vec::new(),
            }
        }
    });
    // The radio these buttons were drawn for; the session refuses a command
    // whose radio has changed since.
    let node = connected_node(&(ui.state)());
    let connected = node.is_some();
    let title = needs_connection(connected);
    let all = contacts();
    let visible = filter_contacts(&all, &query(), kind());
    let now = (ui.now)();
    let send = |command: Command| {
        let (ui, node) = (ui.clone(), node.clone());

        move |_| {
            if let Some(node) = &node {
                ui.send_guarded(node, Guard::Node, command.clone());
            }
        }
    };

    rsx! {
        div { class: "contacts",
            div { class: "pane-head",
                div { class: "pane-head-text",
                    div { class: "pane-title", "Contacts" }
                    div { class: "pane-detail", "{contact_count(visible.len(), all.len())}" }
                }
            }
            div { class: "toolbar",
                input {
                    r#type: "text",
                    class: "search",
                    placeholder: "Search by name or key",
                    value: "{query}",
                    oninput: move |evt| query.set(evt.value()),
                }
                for (chip , label) in KIND_CHIPS {
                    button {
                        key: "{label}",
                        class: if kind() == chip { "chip active" } else { "chip" },
                        onclick: move |_| kind.set(chip),
                        "{label}"
                        span { class: "chip-count", "{filter_contacts(&all, \"\", chip).len()}" }
                    }
                }
                span { class: "grow" }
                button {
                    disabled: !connected,
                    title,
                    onclick: {
                        let ui = ui.clone();

                        let node = node.clone();

                        move |_| {
                            if let Some(node) = &node {
                                importing.set(Pending::open(&ui, node, ()));
                            }
                        }
                    },
                    "Import…"
                }
                button {
                    disabled: !connected,
                    title,
                    onclick: send(Command::ExportContact(None)),
                    "Share my contact"
                }
                button {
                    disabled: !connected,
                    title,
                    onclick: send(Command::SendAdvert { flood: false }),
                    "Advert"
                }
                button {
                    disabled: !connected,
                    title,
                    onclick: send(Command::SendAdvert { flood: true }),
                    "Flood advert"
                }
                button {
                    disabled: !connected,
                    title,
                    onclick: send(Command::Refresh),
                    "Refresh"
                }
            }
            div { class: "scroll",
                if visible.is_empty() {
                    div { class: "empty",
                        if all.is_empty() { "No contacts yet" } else { "No contacts match" }
                    }
                } else {
                    div { class: "table-head",
                        div { class: "col-kind", "Kind" }
                        div { class: "cell-name", "Name" }
                        div { class: "col-key", "Key" }
                        div { class: "col-path num", "Path" }
                        div { class: "col-heard num", "Heard" }
                    }
                }
                for contact in visible {
                    ContactRow {
                        key: "{contact.public_key}",
                        contact,
                        now,
                        node: node.clone(),
                        onremove: {
                            let ui = ui.clone();

                            move |(node, c): (String, Contact)| {
                                removing.set(Pending::open(&ui, &node, c));
                            }
                        },
                    }
                }
            }
            if let Some(pending) = importing() {
                ImportDialog { pending, onclose: move |_| importing.set(None) }
            }
            if let Some(pending) = removing() {
                Confirm {
                    title: "Remove contact",
                    lines: vec![format!("Remove {} from the radio's contacts?", pending.action.name)],
                    confirm_label: "Remove",
                    needs_connection: true,
                    onconfirm: {
                        let ui = ui.clone();

                        move |_| {
                            removing.set(None);

                            if !pending.is_current(&ui.state.peek()) {
                                ui.show_cancelled();

                                return;
                            }

                            pending.send(
                                &ui,
                                Guard::Node,
                                Command::RemoveContact(pending.action.public_key.clone()),
                            );
                        }
                    },
                    oncancel: move |_| removing.set(None),
                }
            }
        }
    }
}

/// One contact and its actions.
#[component]
fn ContactRow(
    contact: Contact,
    now: i64,
    node: Option<String>,
    onremove: EventHandler<(String, Contact)>,
) -> Element {
    let ui = use_context::<Ui>();
    let connected = node.is_some();
    let title = needs_connection(connected);
    let heard = match contact.last_advert {
        0 => "never".to_string(),
        ts => format::relative_time(now, i64::from(ts)),
    };
    let (prefix, key, reset_key) = (
        contact.prefix(),
        contact.public_key.clone(),
        contact.public_key.clone(),
    );
    let (ui_share, ui_reset) = (ui.clone(), ui.clone());
    let (node_share, node_reset, node_remove) = (node.clone(), node.clone(), node);
    let removed = contact.clone();

    rsx! {
        div { class: "table-row",
            div { class: "col-kind cell-muted", "{contact.kind.label()}" }
            div { class: "cell-name", "{contact.name}" }
            div { class: "col-key cell-key cell-muted", "{prefix}" }
            div { class: "col-path num cell-muted", "{format::path_label(contact.path_len)}" }
            div { class: "col-heard num cell-muted", "{heard}" }
            div { class: "cell-actions",
                button {
                    onclick: {
                        let thread = ThreadId::Contact(prefix.clone());

                        move |_| ui.select(thread.clone())
                    },
                    "Message"
                }
                button {
                    disabled: !connected,
                    title,
                    onclick: move |_| {
                        if let Some(node) = &node_share {
                            ui_share.send_guarded(node, Guard::Node, Command::ExportContact(Some(key.clone())));
                        }
                    },
                    "Share"
                }
                button {
                    disabled: !connected || contact.path_len == -1,
                    title,
                    onclick: move |_| {
                        if let Some(node) = &node_reset {
                            ui_reset.send_guarded(node, Guard::Node, Command::ResetPath(reset_key.clone()));
                        }
                    },
                    "Reset path"
                }
                button {
                    class: "danger",
                    disabled: !connected,
                    title,
                    onclick: move |_| {
                        if let Some(node) = &node_remove {
                            onremove.call((node.clone(), removed.clone()));
                        }
                    },
                    "Remove"
                }
            }
        }
    }
}

/// Asks for a `meshcore://` card and sends it to the radio it was opened
/// on; the session validates the card.
#[component]
fn ImportDialog(pending: Pending<()>, onclose: EventHandler<()>) -> Element {
    let ui = use_context::<Ui>();
    let mut text = use_signal(String::new);
    let connected = pending.is_current(&(ui.state)());
    let ready = connected && !text().trim().is_empty();

    rsx! {
        Modal { title: "Import contact", onclose,
            div { class: "dialog-body",
                div { "Paste a meshcore:// contact link." }
                textarea {
                    class: "wide",
                    rows: "4",
                    placeholder: "meshcore://…",
                    value: "{text}",
                    oninput: move |evt| text.set(evt.value()),
                    onmounted: move |evt| async move {
                        let _ = evt.set_focus(true).await;
                    },
                }
            }
            div { class: "dialog-buttons",
                button { onclick: move |_| onclose.call(()), "Cancel" }
                button {
                    class: "primary",
                    disabled: !ready,
                    title: needs_connection(connected),
                    onclick: move |_| {
                        if pending.is_current(&ui.state.peek()) {
                            pending.send(&ui, Guard::Node, Command::ImportContact(text.peek().clone()));
                        } else {
                            ui.show_cancelled();
                        }

                        onclose.call(());
                    },
                    "Import"
                }
            }
        }
    }
}

/// Shows an exported contact card, set by `Update::Exported`.
#[component]
pub fn ExportDialog() -> Element {
    let ui = use_context::<Ui>();
    let mut exported = ui.exported;
    let Some(uri) = exported() else {
        return rsx! {};
    };
    let copied = uri.clone();

    rsx! {
        Modal { title: "Contact card", onclose: move |_| exported.set(None),
            div { class: "dialog-body",
                div { "Share this link so others can import the contact." }
                textarea { class: "wide", rows: "4", readonly: true, value: "{uri}" }
            }
            div { class: "dialog-buttons",
                button { onclick: move |_| ui.copy(&copied), "Copy" }
                button { class: "primary", onclick: move |_| exported.set(None), "Close" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contact_count_says_how_many_the_filters_leave() {
        assert_eq!(contact_count(1, 1), "1 contact");
        assert_eq!(contact_count(4, 4), "4 contacts");
        assert_eq!(contact_count(2, 4), "2 of 4 contacts");
    }

    /// A contact whose key is `fill` followed by 63 `1`s.
    fn contact(name: &str, fill: char, kind: ContactKind) -> Contact {
        Contact {
            public_key: format!("{fill}{}", "1".repeat(63)),
            name: name.to_string(),
            kind,
            path_len: -1,
            last_advert: 0,
            lat: 0,
            lon: 0,
            last_modified: 0,
        }
    }

    /// Alice (chat, `a…`), Hilltop RPT (repeater, `b…`), Town Room (room, `c…`).
    fn sample() -> Vec<Contact> {
        vec![
            contact("Alice", 'a', ContactKind::Chat),
            contact("Hilltop RPT", 'b', ContactKind::Repeater),
            contact("Town Room", 'c', ContactKind::Room),
        ]
    }

    /// The names of `contacts`.
    fn names(contacts: &[Contact]) -> Vec<&str> {
        contacts.iter().map(|c| c.name.as_str()).collect()
    }

    mod filter_contacts {
        use super::*;

        #[test]
        fn should_keep_everything_for_an_empty_query_and_no_kind() {
            assert_eq!(filter_contacts(&sample(), "", None).len(), 3);
        }

        #[test]
        fn should_match_the_name_ignoring_case() {
            let found = filter_contacts(&sample(), "hILL", None);

            assert_eq!(names(&found), ["Hilltop RPT"]);
        }

        #[test]
        fn should_match_the_start_of_the_key() {
            let found = filter_contacts(&sample(), "C11", None);

            assert_eq!(names(&found), ["Town Room"]);
        }

        #[test]
        fn should_not_match_a_key_fragment_in_the_middle() {
            assert!(filter_contacts(&sample(), "11", None).is_empty());
        }

        #[test]
        fn should_keep_only_the_chosen_kind() {
            let found = filter_contacts(&sample(), "", Some(ContactKind::Repeater));

            assert_eq!(names(&found), ["Hilltop RPT"]);
        }

        #[test]
        fn should_apply_the_query_and_the_kind_together() {
            let found = filter_contacts(&sample(), "alice", Some(ContactKind::Room));

            assert!(found.is_empty(), "{found:?}");
        }
    }
}
