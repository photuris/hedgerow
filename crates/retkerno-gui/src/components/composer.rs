//! The message box under a conversation.

use dioxus::prelude::*;
use retkerno_core::model::ThreadId;
use retkerno_core::session::{Command, ConnState, Guard};
use retkerno_core::text;

use crate::app::Ui;

/// What a send from the composer needs to stay true: the node and, for a
/// channel thread, the secret in its slot, as the chat was opened with them.
#[derive(Clone, PartialEq)]
struct Target {
    /// The thread the text goes to.
    thread: ThreadId,
    /// The node the chat belongs to.
    node: Option<String>,
    /// The secret the thread's channel slot held when the chat opened.
    secret: Option<[u8; 16]>,
}

impl Target {
    /// What the session must still find when the send runs.
    fn guard(&self) -> Guard {
        match self.thread {
            ThreadId::Channel(idx) => Guard::ChannelSlot {
                idx,
                secret: self.secret,
            },
            ThreadId::Contact(_) => Guard::Node,
        }
    }
}

/// Sends the composer's current text to `target`. Reads the input, the
/// connection, and the device name now, not what the last render saw: an
/// Enter can arrive before the render that follows an edit. Does nothing
/// when not connected, when the text is blank, or when it is too long. The
/// session refuses the send if the radio or the channel changed meanwhile.
fn send_current(ui: &Ui, target: &Target, mut input: Signal<String>) {
    let (ConnState::Connected(device), Some(node)) = (ui.state.peek().clone(), &target.node) else {
        return;
    };
    let thread = &target.thread;
    let body = input.peek().trim().to_string();

    if body.is_empty() || body.len() > text::max_text_bytes(thread, &device.name) {
        return;
    }

    ui.send_guarded(
        node,
        target.guard(),
        Command::SendText {
            thread: thread.clone(),
            text: body,
        },
    );
    input.set(String::new());
}

/// Multiline input with a byte counter. Enter sends, Shift+Enter inserts a
/// newline; disabled unless the session is connected. The draft belongs to
/// `node` and `secret`, so mount it with a key that changes with them.
#[component]
pub fn Composer(thread: ThreadId, node: Option<String>, secret: Option<[u8; 16]>) -> Element {
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
    let target = Target {
        thread: thread.clone(),
        node,
        secret,
    };
    let (ui_on_key, target_on_key) = (ui.clone(), target.clone());

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
                            send_current(&ui_on_key, &target_on_key, input);
                        }
                    },
                    onmounted: move |evt| async move {
                        let _ = evt.set_focus(true).await;
                    },
                }
                button {
                    class: "primary",
                    disabled: !can_send,
                    onclick: move |_| send_current(&ui, &target, input),
                    "Send"
                }
            }
            div { class: if over { "counter over" } else { "counter" }, "{used}/{max}" }
        }
    }
}
