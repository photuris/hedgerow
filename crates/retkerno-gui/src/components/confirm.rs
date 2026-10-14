//! Modal dialogs: the generic [`Modal`] frame and the [`Confirm`] question.

use dioxus::prelude::*;

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
/// Escape and Cancel call `oncancel`.
#[component]
pub fn Confirm(
    title: String,
    lines: Vec<String>,
    confirm_label: String,
    onconfirm: EventHandler<()>,
    oncancel: EventHandler<()>,
) -> Element {
    rsx! {
        Modal { title, onclose: oncancel,
            div { class: "dialog-body",
                for (i , line) in lines.iter().enumerate() {
                    div { key: "{i}", "{line}" }
                }
            }
            div { class: "dialog-buttons",
                button { onclick: move |_| oncancel.call(()), "Cancel" }
                button { class: "primary", onclick: move |_| onconfirm.call(()), "{confirm_label}" }
            }
        }
    }
}
