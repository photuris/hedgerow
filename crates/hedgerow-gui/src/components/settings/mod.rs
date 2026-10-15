//! The settings view: a tab list on the left, the selected tab's panel on
//! the right.

mod channels;
mod connection;
mod device;

use dioxus::prelude::*;

use crate::app::{SettingsTab, Ui, View};
use channels::Channels;
use connection::Connection;
use device::Device;

/// The tabs in display order.
const TABS: [SettingsTab; 3] = [
    SettingsTab::Connection,
    SettingsTab::Device,
    SettingsTab::Channels,
];

/// The label of a tab.
fn tab_label(tab: SettingsTab) -> &'static str {
    match tab {
        SettingsTab::Connection => "Connection",
        SettingsTab::Device => "Device",
        SettingsTab::Channels => "Channels",
    }
}

/// The tab list and the panel of `tab`.
#[component]
pub fn Settings(tab: SettingsTab) -> Element {
    let ui = use_context::<Ui>();
    let mut view = ui.view;

    rsx! {
        div { class: "settings",
            nav { class: "settings-nav",
                for t in TABS {
                    button {
                        key: "{tab_label(t)}",
                        class: if t == tab { "nav-item active" } else { "nav-item" },
                        onclick: move |_| view.set(View::Settings(t)),
                        "{tab_label(t)}"
                    }
                }
            }
            div { class: "settings-page",
                h2 { "{tab_label(tab)}" }
                match tab {
                    SettingsTab::Connection => rsx! { Connection {} },
                    SettingsTab::Device => rsx! { Device {} },
                    SettingsTab::Channels => rsx! { Channels {} },
                }
            }
        }
    }
}
