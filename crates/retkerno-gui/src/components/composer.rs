//! The message box under a conversation.

use dioxus::prelude::*;
use retkerno_core::model::ThreadId;
use retkerno_core::session::{Command, ConnState};
use retkerno_core::text;

use crate::app::Ui;

/// Sends the composer's current text to `thread`. Reads the input, the
/// connection, and the device name now, not what the last render saw: an
/// Enter can arrive before the render that follows an edit. Does nothing
/// when not connected, when the text is blank, or when it is too long.
fn send_current(ui: &Ui, thread: &ThreadId, mut input: Signal<String>) {
    let ConnState::Connected(device) = ui.state.peek().clone() else {
        return;
    };
    let body = input.peek().trim().to_string();

    if body.is_empty() || body.len() > text::max_text_bytes(thread, &device.name) {
        return;
    }

    ui.session.send(Command::SendText {
        thread: thread.clone(),
        text: body,
    });
    input.set(String::new());
}

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
    let (ui_on_key, thread_on_key) = (ui.clone(), thread.clone());

    rsx! {
        div { class: "composer",
            div { class: "composer-row",
                textarea {
                    rows: "2",
                    placeholder: if connected { "Message" } else { "Not connected" },
                    disabled: !connected,
                    value: "{input}",
                    oninput: move |evt| input.set(evt.value()),
                    onkeydown: move |evt| {
                        if evt.key() == Key::Enter && !evt.modifiers().shift() && !evt.is_composing() {
                            evt.prevent_default();
                            send_current(&ui_on_key, &thread_on_key, input);
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
                        let thread = thread.clone();

                        move |_| send_current(&ui, &thread, input)
                    },
                    "Send"
                }
            }
            div { class: if over { "counter over" } else { "counter" }, "{used}/{max}" }
        }
    }
}
