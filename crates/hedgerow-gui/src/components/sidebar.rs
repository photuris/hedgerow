//! The sidebar: connection chip, conversation lists, and navigation.

use dioxus::prelude::*;
use hedgerow_core::model::{NotifyMode, ThreadId, ThreadSummary};
use hedgerow_core::session::ConnState;

use super::Icon;
use super::confirm::ContextMenu;
use crate::app::{SettingsTab, Ui, View, keep_typing};
use crate::format;

/// Where, for which thread, and with which notification mode the row menu is
/// open.
type MenuAt = (ThreadId, f64, f64, NotifyMode);

/// The left column.
#[component]
pub fn Sidebar() -> Element {
    let ui = use_context::<Ui>();
    let mut menu = use_signal(|| None::<MenuAt>);
    let view = ui.view;
    let threads = use_memo({
        let ui = ui.clone();

        move || {
            let _ = (ui.revision)();

            match (ui.node)() {
                Some(node) => ui.store.threads(&node).unwrap_or_else(|e| {
                    tracing::warn!("list threads: {e}");
                    Vec::new()
                }),
                None => Vec::new(),
            }
        }
    });
    let selected = match view() {
        View::Thread(thread) => Some(thread),
        _ => None,
    };
    let now = (ui.now)();
    let (channels, direct): (Vec<_>, Vec<_>) = threads()
        .into_iter()
        .partition(|t| matches!(t.thread, ThreadId::Channel(_)));
    let row = |summary: ThreadSummary| {
        let is_selected = selected.as_ref() == Some(&summary.thread);
        let key = format!("{}:{}", summary.thread.kind(), summary.thread.key());

        rsx! {
            Row {
                key: "{key}",
                summary,
                selected: is_selected,
                now,
                onselect: {
                    let ui = ui.clone();

                    move |thread: ThreadId| ui.select(thread)
                },
                onmenu: move |at: MenuAt| menu.set(Some(at)),
            }
        }
    };
    let channel_rows = channels.into_iter().map(row);
    let direct_rows = direct.into_iter().map(row);
    let has_node = (ui.node)().is_some();

    rsx! {
        aside { class: "sidebar",
            SideHead {}
            ConnChip {}
            div { class: "threads",
                if has_node {
                    div { class: "group-head",
                        span { class: "grow", "Channels" }
                        button {
                            class: "link",
                            onclick: {
                                let ui = ui.clone();

                                move |_| ui.mark_all_read()
                            },
                            "Mark all read"
                        }
                        button {
                            class: "link",
                            title: "Add a channel",
                            onclick: {
                                let ui = ui.clone();

                                move |_| ui.open_settings(SettingsTab::Channels)
                            },
                            "+"
                        }
                    }
                    {channel_rows}
                    div { class: "group-head", "Direct messages" }
                    {direct_rows}
                } else {
                    div { class: "side-empty",
                        div { "Not connected yet" }
                        button {
                            onclick: {
                                let ui = ui.clone();

                                move |_| ui.open_settings(SettingsTab::Connection)
                            },
                            "Connection settings"
                        }
                    }
                }
            }
            NavFooter {}
            RowMenu { menu }
        }
    }
}

/// The top row: the collapse button and the message search box.
#[component]
fn SideHead() -> Element {
    let ui = use_context::<Ui>();
    let mut focus = ui.focus_search;
    let search = (ui.search)();

    // Ctrl+F lands here once the box exists, which includes the render that
    // follows opening a collapsed sidebar.
    use_effect(move || {
        if focus() {
            focus.set(false);

            let _ = document::eval(
                "const e = document.getElementById('sidebar-search'); if (e) { e.focus(); e.select(); }",
            );
        }
    });

    rsx! {
        div { class: "side-head",
            button {
                class: "tool",
                title: "Hide sidebar",
                onclick: {
                    let ui = ui.clone();

                    move |_| ui.toggle_sidebar()
                },
                Icon { name: "panel-close" }
            }
            input {
                id: "sidebar-search",
                r#type: "text",
                placeholder: "Search messages",
                value: "{search}",
                oninput: {
                    let ui = ui.clone();

                    move |evt| ui.set_search(evt.value())
                },
                onkeydown: move |evt| {
                    keep_typing(&evt);

                    if evt.key() == Key::Escape {
                        ui.set_search(String::new());
                    }
                },
            }
        }
    }
}

/// The connection chip: coloured dot, device name, state, battery.
#[component]
fn ConnChip() -> Element {
    let ui = use_context::<Ui>();
    let state = (ui.state)();
    let (class, label, battery) = match &state {
        ConnState::Connected(device) => {
            let battery = device
                .battery_mv
                .map(|mv| format!(" · {}%", format::battery_percent(mv)));

            ("connected", "Connected".to_string(), battery)
        }
        ConnState::Connecting => ("connecting", "Connecting…".to_string(), None),
        ConnState::Failed(reason) => ("failed", format!("Failed: {reason}"), None),
        ConnState::Disconnected => ("disconnected", "Disconnected".to_string(), None),
    };
    let name = match &state {
        ConnState::Connected(device) => Some(device.name.clone()),
        _ => (ui.node)()
            .and_then(|node| ui.store.device(&node).ok().flatten())
            .map(|device| device.name),
    }
    .unwrap_or_else(|| "No radio".to_string());
    let battery = battery.unwrap_or_default();

    rsx! {
        div { class: "conn", title: "{label}",
            span { class: "conn-dot {class}" }
            div { class: "conn-text",
                div { class: "conn-name", "{name}" }
                div { class: "conn-state", "{label}{battery}" }
            }
        }
    }
}

/// The Contacts and Settings buttons.
#[component]
fn NavFooter() -> Element {
    let ui = use_context::<Ui>();
    let mut view = ui.view;

    rsx! {
        div { class: "side-foot",
            button { onclick: move |_| view.set(View::Contacts), "Contacts" }
            button { onclick: move |_| ui.open_settings(SettingsTab::Connection), "Settings" }
        }
    }
}

/// One conversation row.
#[component]
fn Row(
    summary: ThreadSummary,
    selected: bool,
    now: i64,
    onselect: EventHandler<ThreadId>,
    onmenu: EventHandler<MenuAt>,
) -> Element {
    let time = summary
        .last_at
        .map(|at| format::relative_time(now, at))
        .unwrap_or_default();
    let preview = summary.last_text.clone().unwrap_or_default();
    let (select_thread, menu_thread) = (summary.thread.clone(), summary.thread.clone());
    let notify = summary.notify;
    let class = match (selected, summary.unread > 0) {
        (true, true) => "row selected unread",
        (true, false) => "row selected",
        (false, true) => "row unread",
        (false, false) => "row",
    };

    rsx! {
        div {
            class,
            onclick: move |_| onselect.call(select_thread.clone()),
            oncontextmenu: move |evt| {
                evt.prevent_default();

                let at = evt.client_coordinates();

                onmenu.call((menu_thread.clone(), at.x, at.y, notify));
            },
            div { class: "row-top",
                div { class: "row-title",
                    "{summary.title}"
                    if notify == NotifyMode::Mute {
                        span { class: "muted-icon", title: "Muted",
                            Icon { name: "bell-off", size: 14 }
                        }
                    }
                }
                div { class: "row-time", "{time}" }
            }
            div { class: "row-bottom",
                div { class: "row-preview", "{preview}" }
                if summary.unread > 0 {
                    span { class: "badge", "{summary.unread}" }
                }
            }
        }
    }
}

/// The right-click menu of a row. Closes on a choice, Escape, or a click
/// elsewhere.
#[component]
fn RowMenu(menu: Signal<Option<MenuAt>>) -> Element {
    let ui = use_context::<Ui>();
    let mut menu = menu;
    let Some((thread, x, y, current)) = menu() else {
        return rsx! {};
    };
    let (read_thread, unread_thread) = (thread.clone(), thread.clone());
    let (read_ui, archive_ui) = (ui.clone(), ui.clone());
    let archive_thread = match &thread {
        ThreadId::Contact(_) => Some(thread.clone()),
        ThreadId::Channel(_) => None,
    };
    let modes: &[(&str, NotifyMode)] = match thread {
        ThreadId::Channel(_) => &[
            ("Mentions only", NotifyMode::Default),
            ("All messages", NotifyMode::All),
            ("Mute", NotifyMode::Mute),
        ],
        ThreadId::Contact(_) => &[("Notify", NotifyMode::Default), ("Mute", NotifyMode::Mute)],
    };
    // A contact has no "all" mode of its own: it already notifies on every
    // message, so a stored `All` shows as `Notify`.
    let current = match (&thread, current) {
        (ThreadId::Contact(_), NotifyMode::All) => NotifyMode::Default,
        (_, mode) => mode,
    };

    rsx! {
        ContextMenu { x, y, onclose: move |_| menu.set(None),
            button {
                onclick: move |_| {
                    read_ui.mark_read(&read_thread);
                    menu.set(None);
                },
                "Mark as read"
            }
            button {
                onclick: move |_| {
                    ui.mark_unread(&unread_thread);
                    menu.set(None);
                },
                "Mark as unread"
            }
            div { class: "menu-sep" }
            for (label, mode) in modes.iter().copied() {
                button {
                    key: "{label}",
                    class: if mode == current { "menu-check" },
                    onclick: {
                        let (ui, thread) = (ui.clone(), thread.clone());

                        move |_| {
                            ui.set_notify(&thread, mode);
                            menu.set(None);
                        }
                    },
                    if mode == current {
                        "✓ {label}"
                    } else {
                        "{label}"
                    }
                }
            }
            if let Some(thread) = archive_thread {
                div { class: "menu-sep" }
                button {
                    title: "Hide until the next message; the history stays",
                    onclick: move |_| {
                        archive_ui.archive(&thread);
                        menu.set(None);
                    },
                    "Archive"
                }
            }
        }
    }
}
