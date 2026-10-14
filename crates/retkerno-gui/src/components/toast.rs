//! The transient notice shown for session `Notice` updates.

use dioxus::prelude::*;

use crate::app::Ui;

/// Bottom-right toast; empty while there is no notice.
#[component]
pub fn Toast() -> Element {
    let ui = use_context::<Ui>();

    match (ui.toast)() {
        Some(text) => rsx! { div { class: "toast", "{text}" } },
        None => rsx! {},
    }
}
