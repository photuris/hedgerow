//! The contact list: search, kind filter, per-contact actions, and the
//! import and export dialogs.

use dioxus::prelude::*;
use retkerno_core::model::{Contact, ContactKind, ThreadId};
use retkerno_core::session::{Command, ConnState};

use super::confirm::{Confirm, Modal};
use crate::app::Ui;
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
    let mut importing = use_signal(|| false);
    let mut removing = use_signal(|| None::<Contact>);
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
    let connected = matches!((ui.state)(), ConnState::Connected(_));
    let title = needs_connection(connected);
    let all = contacts();
    let visible = filter_contacts(&all, &query(), kind());
    let now = (ui.now)();
    let send = |command: Command| {
        let session = ui.session.clone();

        move |_| session.send(command.clone())
    };

    rsx! {
        div { class: "contacts",
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
                    onclick: move |_| importing.set(true),
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
            div { class: "contact-list",
                if visible.is_empty() {
                    div { class: "msgs-empty",
                        if all.is_empty() { "No contacts yet" } else { "No contacts match" }
                    }
                }
                for contact in visible {
                    ContactRow {
                        key: "{contact.public_key}",
                        contact,
                        now,
                        connected,
                        onremove: move |c: Contact| removing.set(Some(c)),
                    }
                }
            }
            if importing() {
                ImportDialog { onclose: move |_| importing.set(false) }
            }
            if let Some(contact) = removing() {
                Confirm {
                    title: "Remove contact",
                    lines: vec![format!("Remove {} from the radio's contacts?", contact.name)],
                    confirm_label: "Remove",
                    onconfirm: {
                        let session = ui.session.clone();

                        move |_| {
                            session.send(Command::RemoveContact(contact.public_key.clone()));
                            removing.set(None);
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
    connected: bool,
    onremove: EventHandler<Contact>,
) -> Element {
    let ui = use_context::<Ui>();
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
    let (session_share, session_reset) = (ui.session.clone(), ui.session.clone());
    let removed = contact.clone();

    rsx! {
        div { class: "contact-row",
            div { class: "contact-kind", "{contact.kind.label()}" }
            div { class: "contact-name", "{contact.name}" }
            div { class: "contact-key", "{prefix}" }
            div { class: "contact-path", "{format::path_label(contact.path_len)}" }
            div { class: "contact-heard", "{heard}" }
            div { class: "contact-actions",
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
                    onclick: move |_| session_share.send(Command::ExportContact(Some(key.clone()))),
                    "Share"
                }
                button {
                    disabled: !connected || contact.path_len == -1,
                    title,
                    onclick: move |_| session_reset.send(Command::ResetPath(reset_key.clone())),
                    "Reset path"
                }
                button {
                    class: "danger",
                    disabled: !connected,
                    title,
                    onclick: move |_| onremove.call(removed.clone()),
                    "Remove"
                }
            }
        }
    }
}

/// Asks for a `meshcore://` card and sends it; the session validates it.
#[component]
fn ImportDialog(onclose: EventHandler<()>) -> Element {
    let ui = use_context::<Ui>();
    let mut text = use_signal(String::new);
    let connected = matches!((ui.state)(), ConnState::Connected(_));
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
                        ui.session.send(Command::ImportContact(text()));
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
