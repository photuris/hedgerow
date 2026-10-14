//! The message box under a conversation.

use dioxus::prelude::*;
use retkerno_core::model::ThreadId;
use retkerno_core::session::{Command, ConnState};
use retkerno_core::text;

use crate::app::Ui;

/// Multiline input with a byte counter. Enter sends, Shift+Enter inserts a
/// newline; disabled unless the session is connected.
#[component]
pub fn Composer(thread: ThreadId) -> Element {
    let ui = use_context::<Ui>();
    let mut input = use_signal(String::new);
    let (connected, device_name) = match (ui.state)() {
        ConnState::Connected(device) => (true, device.name),
        _ => (false, String::new()),
    };
    let max = text::max_text_bytes(&thread, &device_name);
    let body = input().trim().to_string();
    let used = body.len();
    let over = used > max;
    let can_send = connected && !body.is_empty() && !over;
    let send = {
        let thread = thread.clone();

        move || {
            ui.session.send(Command::SendText {
                thread: thread.clone(),
                text: body.clone(),
            });
            input.set(String::new());
        }
    };
    let (send_on_key, send_on_click) = (send.clone(), send);

    rsx! {
        div { class: "composer",
            div { class: "composer-row",
                textarea {
                    rows: "2",
                    placeholder: if connected { "Message" } else { "Not connected" },
                    disabled: !connected,
                    value: "{input}",
                    oninput: move |evt| input.set(evt.value()),
                    onkeydown: {
                        let mut send = send_on_key;

                        move |evt| {
                            if evt.key() == Key::Enter && !evt.modifiers().shift() && !evt.is_composing() {
                                evt.prevent_default();

                                if can_send {
                                    send();
                                }
                            }
                        }
                    },
                    onmounted: move |evt| async move {
                        let _ = evt.set_focus(true).await;
                    },
                }
                button {
                    class: "primary",
                    disabled: !can_send,
                    onclick: {
                        let mut send = send_on_click;

                        move |_| send()
                    },
                    "Send"
                }
            }
            div { class: if over { "counter over" } else { "counter" }, "{used}/{max}" }
        }
    }
}
