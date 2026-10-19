//! The settings view: a tab list on the left, the selected tab's panel on
//! the right, and a footer that saves or drops the config draft.
//!
//! Settings that live in `config.toml` (the Connection tab) edit one draft
//! shared by every tab; Save validates, writes, applies, and closes. Settings
//! that live on the radio (Device, Channels) act at once, behind their own
//! confirmations.

mod channels;
mod connection;
mod device;

use dioxus::prelude::*;
use hedgerow_core::config::{Config, Transport};
use hedgerow_core::session::Command;

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

/// The config edits not yet saved. The TCP port is kept as typed, so an
/// invalid one is reported on Save instead of being lost.
#[derive(Clone, Copy, PartialEq)]
pub struct Draft {
    /// The config as edited.
    pub config: Signal<Config>,
    /// The TCP port field, as typed.
    pub port_text: Signal<String>,
}

impl Draft {
    /// A draft of `saved`, created in the current scope.
    fn new(saved: &Config) -> Draft {
        Draft {
            config: Signal::new(saved.clone()),
            port_text: Signal::new(saved.connection.tcp_port.to_string()),
        }
    }

    /// The config this draft would save over `saved`: the edited connection,
    /// everything else (the sidebar width the divider writes) as saved now.
    ///
    /// # Errors
    ///
    /// Returns the message to show when the TCP port is not a number. The
    /// port field is only on screen for TCP; for the other transports the
    /// last valid port stays in the config.
    fn resolve(&self, saved: &Config) -> Result<Config, String> {
        let mut connection = self.config.peek().connection.clone();

        if connection.transport == Transport::Tcp {
            connection.tcp_port = self
                .port_text
                .peek()
                .trim()
                .parse()
                .map_err(|_| "The TCP port must be a number from 0 to 65535".to_string())?;
        }

        Ok(Config {
            connection,
            ..saved.clone()
        })
    }

    /// Whether the draft differs from `saved`. An unparsable port counts as
    /// a change, so Save can report it.
    pub fn is_dirty(&self, saved: &Config) -> bool {
        !matches!(self.resolve(saved), Ok(c) if c == *saved)
    }
}

/// The tab list, the panel of `tab`, and the Save and Close footer.
#[component]
pub fn Settings(tab: SettingsTab) -> Element {
    let ui = use_context::<Ui>();
    let mut view = ui.view;
    let draft = use_hook(|| Draft::new(&ui.config.peek()));
    let mut error = use_signal(|| None::<String>);
    let dirty = {
        let _ = ((draft.config)(), (draft.port_text)());

        draft.is_dirty(&(ui.config)())
    };
    let save = {
        let ui = ui.clone();

        move |_| {
            let saved = ui.config.peek().clone();
            let config = match draft.resolve(&saved) {
                Ok(config) => config,
                Err(e) => {
                    error.set(Some(e));

                    return;
                }
            };

            // UI-test mode never writes the user's files.
            if !ui.ui_test
                && let Err(e) = config.save()
            {
                error.set(Some(format!("Could not save the settings: {e}")));

                return;
            }

            let mut stored = ui.config;

            error.set(None);
            ui.session
                .send(Command::Reconfigure(config.connection.clone()));
            stored.set(config);
            ui.close_settings();
        }
    };

    rsx! {
        div {
            class: "settings",
            onkeydown: {
                let ui = ui.clone();

                move |evt: KeyboardEvent| {
                    if evt.key() == Key::Escape {
                        evt.stop_propagation();
                        ui.close_settings();
                    }
                }
            },
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
            div { class: "settings-body",
                div { class: "settings-page",
                    h2 { "{tab_label(tab)}" }
                    match tab {
                        SettingsTab::Connection => rsx! { Connection { draft, dirty } },
                        SettingsTab::Device => rsx! { Device {} },
                        SettingsTab::Channels => rsx! { Channels {} },
                    }
                }
                div { class: "settings-foot",
                    if ui.ui_test {
                        span { class: "ui-test", "UI test mode" }
                    }
                    if let Some(e) = error() {
                        span { class: "error", "{e}" }
                    }
                    span { class: "spacer" }
                    button {
                        onclick: {
                            let ui = ui.clone();

                            move |_| ui.close_settings()
                        },
                        "Close"
                    }
                    button {
                        class: "primary",
                        disabled: !dirty,
                        title: if dirty { None } else { Some("No changes to save") },
                        onclick: save,
                        "Save"
                    }
                }
            }
        }
    }
}
