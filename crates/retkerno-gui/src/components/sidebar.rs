//! The sidebar: connection chip, conversation lists, and navigation.

use dioxus::prelude::*;
use retkerno_core::model::{ThreadId, ThreadSummary};
use retkerno_core::session::ConnState;

use crate::app::{SettingsTab, Ui, View};
use crate::format;

/// Where and for which thread the row menu is open.
type MenuAt = (ThreadId, f64, f64);

/// The left column.
#[component]
pub fn Sidebar() -> Element {
    let ui = use_context::<Ui>();
    let mut menu = use_signal(|| None::<MenuAt>);
    let mut view = ui.view;
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
                            onclick: move |_| view.set(View::Settings(SettingsTab::Channels)),
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
                            onclick: move |_| view.set(View::Settings(SettingsTab::Connection)),
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
            button {
                onclick: move |_| view.set(View::Settings(SettingsTab::Connection)),
                "Settings"
            }
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

    rsx! {
        div {
            class: if selected { "row selected" } else { "row" },
            onclick: move |_| onselect.call(select_thread.clone()),
            oncontextmenu: move |evt| {
                evt.prevent_default();

                let at = evt.client_coordinates();

                onmenu.call((menu_thread.clone(), at.x, at.y));
            },
            div { class: "row-top",
                div { class: "row-title", "{summary.title}" }
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
    let Some((thread, x, y)) = menu() else {
        return rsx! {};
    };
    let (read_thread, unread_thread) = (thread.clone(), thread);
    let read_ui = ui.clone();

    rsx! {
        div {
            class: "menu-backdrop",
            onclick: move |_| menu.set(None),
            oncontextmenu: move |evt| {
                evt.prevent_default();
                menu.set(None);
            },
        }
        div {
            class: "menu",
            style: "left: {x}px; top: {y}px",
            tabindex: "0",
            onmounted: move |evt| async move {
                let _ = evt.set_focus(true).await;
            },
            onkeydown: move |evt| {
                if evt.key() == Key::Escape {
                    menu.set(None);
                }
            },
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
        }
    }
}
