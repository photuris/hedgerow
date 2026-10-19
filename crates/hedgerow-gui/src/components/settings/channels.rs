//! The Channels tab: the stored channel slots, and a form to add one.

use dioxus::prelude::*;
use hedgerow_core::model::Channel;
use hedgerow_core::session::{Command, ConnState, Guard};
use hedgerow_core::text::{self, PUBLIC_CHANNEL_SECRET, hashtag_secret};

use super::device::current_device;
use crate::app::{Ui, connected_node};
use crate::components::confirm::{Confirm, Pending, use_pending};

/// Most bytes a channel name can use.
const MAX_NAME_BYTES: usize = 31;

/// Channel slots assumed when the device does not report its capacity.
const DEFAULT_MAX_CHANNELS: u8 = 8;

/// What the key of a new channel comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NewKind {
    /// A key derived from the `#name`.
    Hashtag,
    /// A key chosen by the user.
    Private,
}

/// The lowest unused slot in `1..max`; slot 0 is never offered.
fn next_free_slot(used: &[u8], max: u8) -> Option<u8> {
    (1..max).find(|slot| !used.contains(slot))
}

/// `Public`, `Hashtag` (the key derives from the name), or `Private`.
fn channel_kind(ch: &Channel) -> &'static str {
    if ch.secret == PUBLIC_CHANNEL_SECRET {
        "Public"
    } else if hashtag_secret(&ch.name).is_ok_and(|secret| secret == ch.secret) {
        "Hashtag"
    } else {
        "Private"
    }
}

/// Turns what the user typed into a hashtag channel name: trimmed, with a
/// leading `#` added when missing.
fn normalize_hashtag(input: &str) -> Result<String, String> {
    let input = input.trim();
    let body = input.strip_prefix('#').unwrap_or(input);

    if body.is_empty() {
        return Err("Enter a channel name".to_string());
    }

    let name = format!("#{body}");

    if name.len() > MAX_NAME_BYTES {
        return Err(format!(
            "A channel name is at most {MAX_NAME_BYTES} bytes, this one has {}",
            name.len()
        ));
    }

    Ok(name)
}

/// Builds the channel the add form asks for, in the lowest free slot of
/// `existing`.
fn build_channel(
    kind: NewKind,
    name: &str,
    key: &str,
    existing: &[Channel],
    max: u8,
) -> Result<Channel, String> {
    let used: Vec<u8> = existing.iter().map(|c| c.idx).collect();
    let idx = next_free_slot(&used, max).ok_or_else(|| "No free channel slots".to_string())?;
    let name = match kind {
        NewKind::Hashtag => normalize_hashtag(name)?,
        NewKind::Private => {
            let name = name.trim();

            if name.is_empty() || name.len() > MAX_NAME_BYTES {
                return Err(format!("A channel name is 1 to {MAX_NAME_BYTES} bytes"));
            }

            name.to_string()
        }
    };

    if existing.iter().any(|c| c.name == name) {
        return Err(format!("Already have a channel named {name}"));
    }

    let secret = match kind {
        NewKind::Hashtag => hashtag_secret(&name).map_err(|e| e.to_string())?,
        NewKind::Private => {
            text::parse_secret(key).map_err(|_| "The key must be 32 hex characters".to_string())?
        }
    };

    Ok(Channel { idx, name, secret })
}

/// Whether the stored slot of `pending` still holds the channel (its secret)
/// the dialog asked about.
fn slot_holds(ui: &Ui, pending: &Pending<Channel>) -> bool {
    let Some(node) = ui.connected_node() else {
        return false;
    };

    ui.store.channels(&node).is_ok_and(|channels| {
        channels
            .iter()
            .any(|c| c.idx == pending.action.idx && c.secret == pending.action.secret)
    })
}

/// The stored channels and the add form.
#[component]
pub fn Channels() -> Element {
    let ui = use_context::<Ui>();
    let channels = use_memo({
        let ui = ui.clone();

        move || {
            let _ = (ui.revision)();

            match (ui.node)() {
                Some(node) => ui.store.channels(&node).unwrap_or_else(|e| {
                    tracing::warn!("list channels: {e}");
                    Vec::new()
                }),
                None => Vec::new(),
            }
        }
    });
    let max = use_memo({
        let ui = ui.clone();

        move || {
            current_device(&ui)
                .and_then(|d| d.max_channels)
                .unwrap_or(DEFAULT_MAX_CHANNELS)
        }
    });
    let mut shown = use_signal(Vec::<u8>::new);
    // The radio these rows are drawn for; Remove opens a dialog for it only.
    let drawn = connected_node(&(ui.state)());
    let mut removing = use_pending::<Channel>();
    let connected = matches!((ui.state)(), ConnState::Connected(_));
    let title = if connected {
        None
    } else {
        Some("Not connected")
    };

    rsx! {
        div {
            if channels().is_empty() {
                div { class: "hint", "No channels yet" }
            } else {
                div { class: "table-head",
                    div { class: "col-slot num", "Slot" }
                    div { class: "cell-name", "Name" }
                    div { class: "col-kind", "Kind" }
                    div { class: "col-secret", "Key" }
                }
            }
            for ch in channels() {
                {
                    let revealed = shown().contains(&ch.idx);
                    let key = text::to_hex(&ch.secret);
                    let (idx, copy_key, removed) = (ch.idx, key.clone(), ch.clone());
                    let copier = ui.clone();
                    let drawn = drawn.clone();

                    rsx! {
                        div { key: "{ch.idx}", class: "table-row",
                            div { class: "col-slot num cell-muted", "{ch.idx}" }
                            div { class: "cell-name", "{ch.name}" }
                            div { class: "col-kind cell-muted", "{channel_kind(&ch)}" }
                            code { class: "col-secret cell-key cell-muted", if revealed { "{key}" } else { "••••••••" } }
                            button {
                                class: "link",
                                onclick: move |_| {
                                    let mut open = shown.write();

                                    if let Some(at) = open.iter().position(|i| *i == idx) {
                                        open.remove(at);
                                    } else {
                                        open.push(idx);
                                    }
                                },
                                if revealed { "Hide" } else { "Show" }
                            }
                            button { class: "link", onclick: move |_| copier.copy(&copy_key), "Copy" }
                            button {
                                class: "danger",
                                disabled: !connected,
                                title,
                                onclick: {
                                    let ui = ui.clone();

                                    move |_| {
                                        if let Some(node) = &drawn {
                                            removing.set(Pending::open(&ui, node, removed.clone()));
                                        }
                                    }
                                },
                                "Remove"
                            }
                        }
                    }
                }
            }
        }
        // Keyed by node, so a draft made for one radio is dropped, not
        // carried over, when the session moves to another.
        for node in [drawn.clone()] {
            AddChannel {
                key: "{node.as_deref().unwrap_or_default()}",
                node,
                channels: channels(),
                max: max(),
                connected,
            }
        }
        if let Some(pending) = removing() {
            Confirm {
                title: "Remove channel",
                lines: {
                    let ch = &pending.action;
                    let mut lines = vec![format!("Remove channel {} from slot {}?", ch.name, ch.idx)];

                    if ch.idx == 0 {
                        lines.push("Slot 0 is the Public channel by convention.".to_string());
                    }

                    lines
                },
                confirm_label: "Remove",
                needs_connection: true,
                onconfirm: {
                    let ui = ui.clone();

                    move |_| {
                        removing.set(None);

                        if !pending.is_current(&ui.state.peek()) || !slot_holds(&ui, &pending) {
                            ui.show_cancelled();

                            return;
                        }

                        pending.send(
                            &ui,
                            Guard::ChannelSlot {
                                idx: pending.action.idx,
                                secret: Some(pending.action.secret),
                            },
                            Command::ClearChannel(pending.action.idx),
                        );
                    }
                },
                oncancel: move |_| removing.set(None),
            }
        }
    }
}

/// The form that adds a hashtag or private channel on the radio `node`, the
/// one the session was connected to when the form was mounted.
#[component]
fn AddChannel(node: Option<String>, channels: Vec<Channel>, max: u8, connected: bool) -> Element {
    let ui = use_context::<Ui>();
    let mut kind = use_signal(|| NewKind::Hashtag);
    let mut name = use_signal(String::new);
    let mut key = use_signal(String::new);
    let mut error = use_signal(|| None::<String>);
    let title = if connected {
        None
    } else {
        Some("Not connected")
    };
    let derived = (kind() == NewKind::Hashtag)
        .then(|| normalize_hashtag(&name()).ok())
        .flatten()
        .and_then(|n| hashtag_secret(&n).ok())
        .map(|secret| text::to_hex(&secret));
    let add = {
        let ui = ui.clone();
        let channels = channels.clone();

        // The session refuses the write if the radio, or the chosen slot,
        // changed since the form was mounted.
        move |_| {
            let Some(node) = &node else {
                return;
            };

            match build_channel(kind(), &name(), &key(), &channels, max) {
                Ok(channel) => {
                    error.set(None);
                    name.set(String::new());
                    key.set(String::new());
                    ui.send_guarded(
                        node,
                        Guard::ChannelSlot {
                            idx: channel.idx,
                            secret: None,
                        },
                        Command::SetChannel(channel),
                    );
                }
                Err(e) => error.set(Some(e)),
            }
        }
    };

    rsx! {
        h3 { "Add channel" }
        div { class: "setting-row",
            label { r#for: "new-kind", "Kind" }
            select {
                id: "new-kind",
                value: if kind() == NewKind::Hashtag { "hashtag" } else { "private" },
                onchange: move |evt| {
                    kind.set(if evt.value() == "private" { NewKind::Private } else { NewKind::Hashtag });
                    error.set(None);
                },
                option { value: "hashtag", selected: kind() == NewKind::Hashtag, "Hashtag" }
                option { value: "private", selected: kind() == NewKind::Private, "Private" }
            }
        }
        div { class: "setting-row",
            label { r#for: "new-name", "Name" }
            input {
                id: "new-name",
                r#type: "text",
                placeholder: if kind() == NewKind::Hashtag { "#name" } else { "Name" },
                value: "{name}",
                oninput: move |evt| name.set(evt.value()),
            }
        }
        if kind() == NewKind::Private {
            div { class: "setting-row",
                label { r#for: "new-key", "Key" }
                input {
                    id: "new-key",
                    r#type: "text",
                    placeholder: "32 hex characters",
                    value: "{key}",
                    oninput: move |evt| key.set(evt.value()),
                }
                button {
                    onclick: {
                        let ui = ui.clone();

                        move |_| {
                            let mut bytes = [0u8; 16];

                            match getrandom::fill(&mut bytes) {
                                Ok(()) => key.set(text::to_hex(&bytes)),
                                Err(e) => ui.show_toast(format!("Could not generate a key: {e}")),
                            }
                        }
                    },
                    "Generate"
                }
            }
        }
        if let Some(derived) = derived {
            div { class: "hint", "Key: {derived}" }
        }
        div { class: "setting-actions",
            button { class: "primary", disabled: !connected, title, onclick: add, "Add" }
        }
        if let Some(e) = error() {
            div { class: "error", "{e}" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A channel in `idx` called `name` with a secret of `fill`s.
    fn channel(idx: u8, name: &str, fill: u8) -> Channel {
        Channel {
            idx,
            name: name.to_string(),
            secret: [fill; 16],
        }
    }

    mod next_free_slot {
        use super::*;

        #[test]
        fn should_pick_the_lowest_free_slot() {
            assert_eq!(next_free_slot(&[0, 1, 3], 8), Some(2));
        }

        #[test]
        fn should_never_offer_slot_zero() {
            assert_eq!(next_free_slot(&[], 8), Some(1));
            assert_eq!(next_free_slot(&[5], 8), Some(1));
        }

        #[test]
        fn should_be_none_when_every_slot_below_max_is_used() {
            assert_eq!(next_free_slot(&[0, 1, 2], 3), None);
            assert_eq!(next_free_slot(&[], 1), None);
        }

        #[test]
        fn should_ignore_slots_at_or_above_max() {
            assert_eq!(next_free_slot(&[1, 2], 3), None);
            assert_eq!(next_free_slot(&[1, 7], 3), Some(2));
        }
    }

    mod channel_kind {
        use super::*;

        #[test]
        fn should_recognise_the_public_channel() {
            let public = Channel {
                idx: 0,
                name: "Public".to_string(),
                secret: PUBLIC_CHANNEL_SECRET,
            };

            assert_eq!(channel_kind(&public), "Public");
        }

        #[test]
        fn should_recognise_a_key_derived_from_the_name() {
            let ch = Channel {
                idx: 1,
                name: "#test".to_string(),
                secret: hashtag_secret("#test").unwrap(),
            };

            assert_eq!(channel_kind(&ch), "Hashtag");
        }

        #[test]
        fn should_call_a_hashtag_name_with_another_key_private() {
            assert_eq!(channel_kind(&channel(1, "#test", 7)), "Private");
        }

        #[test]
        fn should_call_any_other_channel_private() {
            assert_eq!(channel_kind(&channel(2, "Family", 9)), "Private");
        }
    }

    mod normalize_hashtag {
        use super::*;

        #[test]
        fn should_add_a_missing_hash() {
            assert_eq!(normalize_hashtag("test").as_deref(), Ok("#test"));
        }

        #[test]
        fn should_keep_an_existing_hash_and_trim() {
            assert_eq!(normalize_hashtag("  #test ").as_deref(), Ok("#test"));
        }

        #[test]
        fn should_reject_an_empty_name() {
            assert!(normalize_hashtag("").is_err());
            assert!(normalize_hashtag("  # ").is_err());
        }

        #[test]
        fn should_reject_a_name_over_31_bytes() {
            assert!(normalize_hashtag(&"a".repeat(30)).is_ok());
            assert!(normalize_hashtag(&"a".repeat(31)).is_err());
        }
    }

    mod build_channel {
        use super::*;

        #[test]
        fn should_derive_the_key_of_a_hashtag_in_the_lowest_free_slot() {
            let existing = [channel(0, "Public", 1), channel(2, "#x", 2)];
            let built = build_channel(NewKind::Hashtag, "smoke", "", &existing, 8).unwrap();

            assert_eq!(built.idx, 1);
            assert_eq!(built.name, "#smoke");
            assert_eq!(built.secret, hashtag_secret("#smoke").unwrap());
        }

        #[test]
        fn should_use_the_pasted_key_of_a_private_channel() {
            let key = "00112233445566778899aabbccddeeff";
            let built = build_channel(NewKind::Private, " Family ", key, &[], 8).unwrap();

            assert_eq!(built.name, "Family");
            assert_eq!(text::to_hex(&built.secret), key);
        }

        #[test]
        fn should_refuse_when_no_slot_is_free() {
            let existing = [channel(0, "Public", 1), channel(1, "#a", 2)];

            assert_eq!(
                build_channel(NewKind::Hashtag, "b", "", &existing, 2)
                    .err()
                    .as_deref(),
                Some("No free channel slots")
            );
        }

        #[test]
        fn should_refuse_a_name_that_is_already_stored() {
            let existing = [channel(0, "Public", 1), channel(1, "#test", 2)];

            assert_eq!(
                build_channel(NewKind::Hashtag, "test", "", &existing, 8)
                    .err()
                    .as_deref(),
                Some("Already have a channel named #test")
            );
        }

        #[test]
        fn should_refuse_a_private_key_that_is_not_32_hex_characters() {
            assert!(build_channel(NewKind::Private, "Family", "abc", &[], 8).is_err());
        }
    }
}
