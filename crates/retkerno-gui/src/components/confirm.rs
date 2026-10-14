//! Modal dialogs: the generic [`Modal`] frame and the [`Confirm`] question,
//! and [`Pending`], the state of a confirmation that sends a radio command.

use dioxus::prelude::*;
use retkerno_core::model::DeviceInfo;
use retkerno_core::session::{Command, ConnState, Guard};

use crate::app::{Ui, connected_node};

/// A command awaiting confirmation, bound to the radio it was opened for.
#[derive(Clone, Debug, PartialEq)]
pub struct Pending<T> {
    /// The node id of the radio the dialog was opened on.
    node: String,
    /// What the dialog will send.
    pub action: T,
}

impl<T> Pending<T> {
    /// Opens a confirmation for the radio `shown` that the clicked control
    /// was rendered for. When the session is no longer connected to `shown`
    /// nothing opens and a toast says so: a dialog never adopts the live
    /// radio in place of the one the user was looking at.
    pub fn open(ui: &Ui, shown: &str, action: T) -> Option<Self> {
        if ui.connected_node().as_deref() != Some(shown) {
            ui.show_cancelled();

            return None;
        }

        Some(Pending {
            node: shown.to_string(),
            action,
        })
    }

    /// Whether `state` is a connection to the radio this was opened on.
    pub fn is_current(&self, state: &ConnState) -> bool {
        connected_node(state).is_some_and(|node| node == self.node)
    }

    /// Sends `cmd` to the session, which runs it only while it is still
    /// connected to this dialog's radio and `guard` holds.
    pub fn send(&self, ui: &Ui, guard: Guard, cmd: Command) {
        ui.send_guarded(&self.node, guard, cmd);
    }

    /// The current device, if the session is connected to this dialog's radio.
    pub fn device(&self, state: &ConnState) -> Option<DeviceInfo> {
        match state {
            ConnState::Connected(device) if device.node_id == self.node => Some(device.clone()),
            _ => None,
        }
    }
}

/// The signal behind a confirmation dialog. It is cleared whenever the
/// session leaves the connection the dialog was opened on (disconnect or
/// another node), so a dialog never outlives its radio.
pub fn use_pending<T: Clone + 'static>() -> Signal<Option<Pending<T>>> {
    let state = use_context::<Ui>().state;
    let mut pending = use_signal(|| None::<Pending<T>>);

    use_effect(move || {
        let state = state();

        if pending
            .peek()
            .as_ref()
            .is_some_and(|p| !p.is_current(&state))
        {
            pending.set(None);
        }
    });

    pending
}

/// A centred dialog over a dimmed backdrop. Closes on Escape and when the
/// backdrop is clicked.
#[component]
pub fn Modal(title: String, onclose: EventHandler<()>, children: Element) -> Element {
    rsx! {
        div { class: "overlay", onclick: move |_| onclose.call(()),
            div {
                class: "dialog",
                tabindex: "0",
                onclick: |evt| evt.stop_propagation(),
                onmounted: move |evt| async move {
                    let _ = evt.set_focus(true).await;
                },
                onkeydown: move |evt| {
                    if evt.key() == Key::Escape {
                        onclose.call(());
                    }
                },
                div { class: "dialog-title", "{title}" }
                {children}
            }
        }
    }
}

/// A yes/no question: `lines` are the body, `confirm_label` names the action.
/// Escape and Cancel call `oncancel`. With `needs_connection` the confirm
/// button is disabled unless the session is connected.
#[component]
pub fn Confirm(
    title: String,
    lines: Vec<String>,
    confirm_label: String,
    needs_connection: bool,
    onconfirm: EventHandler<()>,
    oncancel: EventHandler<()>,
) -> Element {
    let ui = use_context::<Ui>();
    let offline = needs_connection && !matches!((ui.state)(), ConnState::Connected(_));

    rsx! {
        Modal { title, onclose: oncancel,
            div { class: "dialog-body",
                for (i , line) in lines.iter().enumerate() {
                    div { key: "{i}", "{line}" }
                }
            }
            div { class: "dialog-buttons",
                button { onclick: move |_| oncancel.call(()), "Cancel" }
                button {
                    class: "primary",
                    disabled: offline,
                    title: if offline { Some("Not connected") } else { None },
                    onclick: move |_| onconfirm.call(()),
                    "{confirm_label}"
                }
            }
        }
    }
}
