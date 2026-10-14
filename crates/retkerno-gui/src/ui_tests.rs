//! Headless UI tests: the real [`App`] in a Dioxus `VirtualDom` over the demo
//! fake radio and an in-memory store. No window opens and nothing touches the
//! desktop session; events are delivered straight to the VDOM, and the page
//! is read back from the rendered virtual nodes.
//!
//! Test seams in the app: `app::window_focused` reports a focused window and
//! `app::set_window_title` does nothing under `cfg(test)`, because tests have
//! no native window. The fake radio makes `Ui::ui_test` true, so the
//! connection tab never saves a config or lists serial ports or Bluetooth.

use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use dioxus::core::{
    AttributeValue, DynamicNode, ElementId, TemplateAttribute, TemplateNode, VNode,
};
use dioxus::html::{
    Code, Location, Modifiers, PlatformEventData, SerializedFormData, SerializedHtmlEventConverter,
    SerializedKeyboardData, SerializedMouseData,
};
use dioxus::prelude::*;
use retkerno_core::config::Config;
use retkerno_core::model::{DeliveryStatus, Direction, NewMessage, ThreadId};
use retkerno_core::radio::{FakeRadio, FakeSent};
use retkerno_core::session::{Command, ConnState};
use retkerno_core::store::Store;

use crate::app::{App, Boot, Hooks, Ui, now_secs};

/// The warning the device confirmation appends when radio parameters change.
const RADIO_WARNING: &str = "Radios only hear each other with identical frequency, bandwidth, spreading factor, and coding rate.";

// ── Harness ─────────────────────────────────────────────────────────────────

/// One rendered element: its event id (when it has listeners), tag,
/// attributes, descendant text, and whether it sits inside a dialog.
#[derive(Debug, Clone)]
struct El {
    id: Option<ElementId>,
    tag: String,
    attrs: BTreeMap<String, String>,
    text: String,
    dialog: bool,
}

impl El {
    /// The value of attribute `name`.
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.get(name).map(String::as_str)
    }

    /// Whether the element has the CSS class `class` (exactly).
    fn has_class(&self, class: &str) -> bool {
        self.attr("class") == Some(class)
    }

    /// Whether the element is disabled.
    fn disabled(&self) -> bool {
        self.attr("disabled") == Some("true")
    }
}

/// The text form of a dynamic attribute value, when it has one.
fn attr_text(value: &AttributeValue) -> Option<String> {
    match value {
        AttributeValue::Text(s) => Some(s.clone()),
        AttributeValue::Bool(b) => Some(b.to_string()),
        AttributeValue::Int(i) => Some(i.to_string()),
        _ => None,
    }
}

/// The text of a dynamic node.
fn dynamic_text(node: &DynamicNode, idx: usize, vnode: &VNode, dom: &VirtualDom) -> String {
    match node {
        DynamicNode::Text(t) => t.value.clone(),
        DynamicNode::Fragment(children) => children.iter().map(|v| vnode_text(v, dom)).collect(),
        DynamicNode::Component(c) => c
            .mounted_scope(idx, vnode, dom)
            .and_then(|scope| scope.try_root_node())
            .map(|v| vnode_text(v, dom))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// The text of a template node.
fn template_text(node: &TemplateNode, vnode: &VNode, dom: &VirtualDom) -> String {
    match node {
        TemplateNode::Text { text } => text.to_string(),
        TemplateNode::Dynamic { id } => dynamic_text(&vnode.dynamic_nodes[*id], *id, vnode, dom),
        TemplateNode::Element { children, .. } => children
            .iter()
            .map(|t| template_text(t, vnode, dom))
            .collect(),
    }
}

/// The text of a rendered vnode.
fn vnode_text(vnode: &VNode, dom: &VirtualDom) -> String {
    vnode
        .template
        .roots
        .iter()
        .map(|t| template_text(t, vnode, dom))
        .collect()
}

/// Collects the elements of one template node (and below) into `out`.
fn walk_template(
    node: &TemplateNode,
    vnode: &VNode,
    dom: &VirtualDom,
    dialog: bool,
    out: &mut Vec<El>,
) {
    match node {
        TemplateNode::Element {
            tag,
            attrs,
            children,
            ..
        } => {
            let mut attributes = BTreeMap::new();
            let mut id = None;

            for attr in *attrs {
                match attr {
                    TemplateAttribute::Static { name, value, .. } => {
                        attributes.insert(name.to_string(), value.to_string());
                    }
                    TemplateAttribute::Dynamic { id: idx } => {
                        id = vnode.mounted_dynamic_attribute(*idx, dom);

                        for attr in &vnode.dynamic_attrs[*idx] {
                            if let Some(value) = attr_text(&attr.value) {
                                attributes.insert(attr.name.to_string(), value);
                            }
                        }
                    }
                }
            }

            let dialog = dialog || attributes.get("class").is_some_and(|c| c == "dialog");

            out.push(El {
                id,
                tag: tag.to_string(),
                attrs: attributes,
                text: children
                    .iter()
                    .map(|t| template_text(t, vnode, dom))
                    .collect(),
                dialog,
            });

            for child in *children {
                walk_template(child, vnode, dom, dialog, out);
            }
        }
        TemplateNode::Dynamic { id } => match &vnode.dynamic_nodes[*id] {
            DynamicNode::Fragment(children) => {
                for child in children {
                    walk(child, dom, dialog, out);
                }
            }
            DynamicNode::Component(c) => {
                if let Some(scope) = c.mounted_scope(*id, vnode, dom)
                    && let Some(root) = scope.try_root_node()
                {
                    walk(root, dom, dialog, out);
                }
            }
            _ => {}
        },
        TemplateNode::Text { .. } => {}
    }
}

/// Collects the elements of `vnode` in document order.
fn walk(vnode: &VNode, dom: &VirtualDom, dialog: bool, out: &mut Vec<El>) {
    for root in vnode.template.roots {
        walk_template(root, vnode, dom, dialog, out);
    }
}

/// Every element of the current page, in document order.
fn els(dom: &VirtualDom) -> Vec<El> {
    let mut out = Vec::new();

    walk(
        dom.get_scope(ScopeId::APP).unwrap().root_node(),
        dom,
        false,
        &mut out,
    );

    out
}

/// The text of the whole page.
fn page_text(dom: &VirtualDom) -> String {
    vnode_text(dom.get_scope(ScopeId::APP).unwrap().root_node(), dom)
}

/// The element with the HTML id `name`.
fn by_id(dom: &VirtualDom, name: &str) -> El {
    els(dom)
        .into_iter()
        .find(|e| e.attr("id") == Some(name))
        .unwrap_or_else(|| panic!("element #{name} missing"))
}

/// The value attribute of the element with the HTML id `name`.
fn value_of(dom: &VirtualDom, name: &str) -> Option<String> {
    by_id(dom, name).attr("value").map(str::to_string)
}

/// The button labelled `label`, inside a dialog or not.
fn button(dom: &VirtualDom, label: &str, dialog: bool) -> El {
    els(dom)
        .into_iter()
        .find(|e| e.tag == "button" && e.text == label && e.dialog == dialog)
        .unwrap_or_else(|| panic!("button {label:?} (dialog: {dialog}) missing"))
}

/// Whether a button labelled `label` is open inside a dialog.
fn dialog_has_button(dom: &VirtualDom, label: &str) -> bool {
    els(dom)
        .iter()
        .any(|e| e.dialog && e.tag == "button" && e.text == label)
}

/// The first non-dialog element with the tag `tag`.
fn first_tag(dom: &VirtualDom, tag: &str) -> El {
    els(dom)
        .into_iter()
        .find(|e| e.tag == tag && !e.dialog)
        .unwrap_or_else(|| panic!("<{tag}> missing"))
}

/// Delivers `data` as the event `name` to `el`.
fn event<T: 'static>(dom: &VirtualDom, el: &El, name: &str, data: T) {
    let data = Rc::new(PlatformEventData::new(Box::new(data)));

    dom.runtime().handle_event(
        name,
        dioxus::core::Event::new(data, true),
        el.id.expect("element has an event listener"),
    );
}

/// Clicks `el`.
fn click(dom: &VirtualDom, el: &El) {
    event(dom, el, "click", SerializedMouseData::default());
}

/// Delivers an `input` event with `value` to `el`.
fn type_into(dom: &VirtualDom, el: &El, value: &str) {
    event(
        dom,
        el,
        "input",
        SerializedFormData::new(value.into(), vec![]),
    );
}

/// Types `value` into the element with the HTML id `name`.
fn type_in(dom: &VirtualDom, name: &str, value: &str) {
    type_into(dom, &by_id(dom, name), value);
}

/// Delivers a `change` event with `value` to the element with the HTML id `name`.
fn change(dom: &VirtualDom, name: &str, value: &str) {
    event(
        dom,
        &by_id(dom, name),
        "change",
        SerializedFormData::new(value.into(), vec![]),
    );
}

/// Presses `key` on `el`.
fn key_down(dom: &VirtualDom, el: &El, key: Key, code: Code) {
    event(
        dom,
        el,
        "keydown",
        SerializedKeyboardData::new(
            key,
            code,
            Location::Standard,
            false,
            Modifiers::empty(),
            false,
        ),
    );
}

/// Lets the session, the effects, and the renders run until things settle.
async fn settle(dom: &mut VirtualDom) {
    for _ in 0..12 {
        let _ = tokio::time::timeout(Duration::from_millis(10), dom.wait_for_work()).await;
        dom.render_immediate_to_vec();
    }
}

/// Runs `f` inside the app's root scope (signals and context live there).
fn in_app<R>(dom: &VirtualDom, f: impl FnOnce() -> R) -> R {
    dom.runtime().in_scope(ScopeId::APP, f)
}

/// Starts the app on `view` (a `RETKERNO_START_VIEW` value) over the demo
/// radio and waits for the connection.
async fn setup(view: &str) -> (VirtualDom, Ui, FakeRadio) {
    dioxus::html::set_event_converter(Box::new(SerializedHtmlEventConverter));

    let fake = retkerno_core::fixtures::demo_radio();
    let boot = Boot {
        store: Store::open_memory().unwrap(),
        config: Config::default(),
        fake: Some(fake.clone()),
        hooks: Hooks {
            start_view: Some(view.into()),
            ..Hooks::default()
        },
    };
    let mut dom = VirtualDom::new(App).with_root_context(boot);

    dom.rebuild_to_vec();
    settle(&mut dom).await;

    let ui = in_app(&dom, consume_context::<Ui>);

    assert!(
        matches!(*ui.state.peek(), ConnState::Connected(_)),
        "the fake radio must be connected"
    );

    (dom, ui, fake)
}

/// The connected device's report.
fn device(ui: &Ui) -> retkerno_core::model::DeviceInfo {
    match ui.state.peek().clone() {
        ConnState::Connected(device) => device,
        other => panic!("not connected: {other:?}"),
    }
}

/// A node id other than the demo radio's.
fn other_node() -> String {
    "99".repeat(32)
}

// ── 003: chat ───────────────────────────────────────────────────────────────

/// An incoming message for channel 0.
fn incoming(text: &str) -> NewMessage {
    NewMessage {
        thread: ThreadId::Channel(0),
        direction: Direction::In,
        sender: Some("Review".into()),
        text: text.into(),
        sender_ts: u32::try_from(now_secs()).unwrap_or(0),
        received_at: now_secs(),
        path_len: None,
        snr: None,
        status: DeliveryStatus::Received,
    }
}

/// The unread count of channel 0.
fn channel_unread(ui: &Ui) -> u32 {
    let node = ui.node.peek().clone().unwrap();

    ui.store
        .threads(&node)
        .unwrap()
        .into_iter()
        .find(|t| t.thread == ThreadId::Channel(0))
        .unwrap()
        .unread
}

#[tokio::test]
async fn incoming_between_open_read_and_initial_snapshot_is_read_when_focused() {
    let (mut dom, ui, _fake) = setup("contacts").await;
    let node = ui.node.peek().clone().unwrap();

    // `select` runs; the session commits a message before the chat has
    // rendered its first snapshot.
    in_app(&dom, || {
        ui.select(ThreadId::Channel(0));
        ui.store
            .insert_message(&node, &incoming("arrived during opening"))
            .unwrap();
        ui.bump();
    });
    settle(&mut dom).await;

    assert_eq!(
        channel_unread(&ui),
        0,
        "a message that lands while the thread opens must be read in the focused thread"
    );
}

#[tokio::test]
async fn incoming_after_mount_is_read_in_focused_thread() {
    let (mut dom, ui, _fake) = setup("contacts").await;
    let node = ui.node.peek().clone().unwrap();

    in_app(&dom, || ui.select(ThreadId::Channel(0)));
    settle(&mut dom).await;
    in_app(&dom, || {
        ui.store
            .insert_message(&node, &incoming("arrived after mount"))
            .unwrap();
        ui.bump();
    });
    settle(&mut dom).await;

    assert_eq!(channel_unread(&ui), 0);
}

/// Opens channel 0 and returns the composer's textarea.
async fn open_composer(dom: &mut VirtualDom, ui: &Ui) -> El {
    in_app(dom, || ui.select(ThreadId::Channel(0)));
    settle(dom).await;

    first_tag(dom, "textarea")
}

/// The channel texts the fake radio was asked to send.
fn sent_channel_texts(fake: &FakeRadio) -> Vec<String> {
    fake.state()
        .sent
        .iter()
        .filter_map(|m| match m {
            FakeSent::Channel { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// Presses Enter in the composer.
fn press_enter(dom: &VirtualDom, textarea: &El) {
    key_down(dom, textarea, Key::Enter, Code::Enter);
}

#[tokio::test]
async fn enter_uses_the_current_input_before_the_next_render() {
    let (mut dom, ui, fake) = setup("contacts").await;
    let textarea = open_composer(&mut dom, &ui).await;

    // Desktop delivers the input event and the Enter without a render between.
    type_into(&dom, &textarea, "current text");
    press_enter(&dom, &textarea);
    settle(&mut dom).await;

    assert_eq!(sent_channel_texts(&fake), ["current text"]);
}

#[tokio::test]
async fn enter_sends_after_input_has_rendered() {
    let (mut dom, ui, fake) = setup("contacts").await;
    let textarea = open_composer(&mut dom, &ui).await;

    type_into(&dom, &textarea, "current text");
    dom.render_immediate_to_vec();
    press_enter(&dom, &textarea);
    settle(&mut dom).await;

    assert_eq!(sent_channel_texts(&fake), ["current text"]);
}

#[tokio::test]
async fn enter_keeps_new_edit_when_the_previous_render_had_text() {
    let (mut dom, ui, fake) = setup("contacts").await;
    let textarea = open_composer(&mut dom, &ui).await;

    type_into(&dom, &textarea, "previous text");
    dom.render_immediate_to_vec();
    type_into(&dom, &textarea, "current text");
    press_enter(&dom, &textarea);
    settle(&mut dom).await;

    assert_eq!(sent_channel_texts(&fake), ["current text"]);
}

#[tokio::test]
async fn enter_sends_nothing_once_the_radio_is_gone() {
    let (mut dom, ui, fake) = setup("contacts").await;
    let textarea = open_composer(&mut dom, &ui).await;

    type_into(&dom, &textarea, "too late");
    dom.render_immediate_to_vec();
    in_app(&dom, || ui.apply_state(ConnState::Disconnected));
    press_enter(&dom, &textarea);
    settle(&mut dom).await;

    assert!(sent_channel_texts(&fake).is_empty());
}

// ── 004: settings, channels, contacts ───────────────────────────────────────

#[tokio::test]
async fn channel_commands_and_modal_close() {
    let (mut dom, _ui, fake) = setup("settings:channels").await;

    type_in(&dom, "new-name", "smoke");
    dom.render_immediate_to_vec();
    click(&dom, &button(&dom, "Add", false));
    settle(&mut dom).await;

    let added = fake
        .state()
        .channels
        .iter()
        .find(|c| c.name == "#smoke")
        .cloned()
        .unwrap();

    assert_eq!(added.idx, 3);
    assert_eq!(
        added.secret,
        retkerno_core::text::hashtag_secret("#smoke").unwrap()
    );

    // Pick the Remove button of the new row: the first one after its name.
    let page = els(&dom);
    let row = page
        .iter()
        .position(|e| e.has_class("channel-name") && e.text == "#smoke")
        .unwrap();
    let remove = page[row..]
        .iter()
        .find(|e| e.tag == "button" && e.text == "Remove")
        .unwrap()
        .clone();
    let question = "Remove channel #smoke from slot 3?";

    click(&dom, &remove);
    settle(&mut dom).await;
    assert!(page_text(&dom).contains(question));

    let dialog = els(&dom)
        .into_iter()
        .find(|e| e.has_class("dialog"))
        .unwrap();

    key_down(&dom, &dialog, Key::Escape, Code::Escape);
    settle(&mut dom).await;
    assert!(
        !page_text(&dom).contains(question),
        "Escape closes the dialog"
    );

    click(&dom, &remove);
    settle(&mut dom).await;

    let overlay = els(&dom)
        .into_iter()
        .find(|e| e.has_class("overlay"))
        .unwrap();

    click(&dom, &overlay);
    settle(&mut dom).await;
    assert!(
        !page_text(&dom).contains(question),
        "the backdrop closes the dialog"
    );

    click(&dom, &remove);
    settle(&mut dom).await;
    click(&dom, &button(&dom, "Remove", true));
    settle(&mut dom).await;

    assert!(!fake.state().channels.iter().any(|c| c.name == "#smoke"));
    assert!(
        fake.state().calls.iter().any(|c| c == "clear_channel:3"),
        "{:?}",
        fake.state().calls
    );
}

#[tokio::test]
async fn device_confirm_builds_all_settings_and_revert() {
    let (mut dom, _ui, fake) = setup("settings:device").await;

    type_in(&dom, "dev-power", "18");
    type_in(&dom, "dev-freq", "915.125");
    change(&dom, "dev-bw", "125000");
    change(&dom, "dev-sf", "9");
    change(&dom, "dev-cr", "6");
    change(&dom, "dev-loc", "2");
    type_in(&dom, "dev-name", "New node");
    dom.render_immediate_to_vec();

    for check in els(&dom)
        .into_iter()
        .filter(|e| e.tag == "input" && e.attr("type") == Some("checkbox"))
    {
        event(
            &dom,
            &check,
            "change",
            SerializedFormData::new("true".into(), vec![]),
        );
    }

    dom.render_immediate_to_vec();
    click(&dom, &button(&dom, "Apply", false));
    settle(&mut dom).await;

    let shown = page_text(&dom);

    assert!(shown.contains("TX power: 20 dBm → 18 dBm"), "{shown}");
    assert!(shown.contains("Frequency: 910.525 MHz → 915.125 MHz"));
    assert!(shown.contains("Bandwidth: 62.5 kHz → 125 kHz"));
    assert!(shown.contains(RADIO_WARNING));
    assert!(
        fake.state().frames.is_empty(),
        "Apply must not send before confirming"
    );

    click(&dom, &button(&dom, "Apply", true));
    settle(&mut dom).await;

    let d = fake.state().device.clone();

    assert_eq!(
        (
            d.tx_power,
            d.radio_freq,
            d.radio_bw,
            d.sf,
            d.cr,
            d.adv_loc_policy
        ),
        (18, 915_125, 125_000, 9, 6, 2)
    );
    assert_eq!(d.name, "New node");
    assert!(d.manual_add_contacts);
    assert_eq!(d.multi_acks, 1);
    assert!(button(&dom, "Apply", false).disabled());

    type_in(&dom, "dev-power", "17");
    dom.render_immediate_to_vec();
    click(&dom, &button(&dom, "Revert", false));
    settle(&mut dom).await;

    assert_eq!(value_of(&dom, "dev-power").as_deref(), Some("18"));
}

#[tokio::test]
async fn clean_form_reinitializes_and_dirty_form_is_preserved() {
    let (mut dom, ui, _fake) = setup("settings:device").await;
    let mut report = device(&ui);

    report.tx_power = 19;
    in_app(&dom, || {
        ui.apply_state(ConnState::Connected(report.clone()))
    });
    settle(&mut dom).await;
    assert_eq!(value_of(&dom, "dev-power").as_deref(), Some("19"));

    type_in(&dom, "dev-power", "17");
    dom.render_immediate_to_vec();
    report.tx_power = 18;
    in_app(&dom, || ui.apply_state(ConnState::Connected(report)));
    settle(&mut dom).await;

    assert_eq!(value_of(&dom, "dev-power").as_deref(), Some("17"));
}

#[tokio::test]
async fn confirmation_diff_stays_valid_when_device_changes() {
    let (mut dom, ui, fake) = setup("settings:device").await;

    type_in(&dom, "dev-power", "18");
    dom.render_immediate_to_vec();
    click(&dom, &button(&dom, "Apply", false));
    settle(&mut dom).await;
    assert!(
        dialog_has_button(&dom, "Apply"),
        "the TX power confirmation is open"
    );

    // The radio changes frequency behind the open dialog.
    fake.state().device.radio_freq = 915_000;
    ui.session.send(Command::Refresh);
    settle(&mut dom).await;
    assert_eq!(device(&ui).radio_freq, 915_000);

    // The dialog lists only TX power: it must not stay open to send a frequency.
    if dialog_has_button(&dom, "Apply") {
        click(&dom, &button(&dom, "Apply", true));
        settle(&mut dom).await;
    }

    assert_eq!(
        fake.state().device.radio_freq,
        915_000,
        "a confirmation that lists only TX power must not reset a newer frequency"
    );
}

#[tokio::test]
async fn confirmation_closes_when_device_changes() {
    let (mut dom, ui, fake) = setup("settings:device").await;

    type_in(&dom, "dev-power", "18");
    dom.render_immediate_to_vec();
    click(&dom, &button(&dom, "Apply", false));
    settle(&mut dom).await;

    fake.state().device.radio_freq = 915_000;
    ui.session.send(Command::Refresh);
    settle(&mut dom).await;

    assert!(
        !dialog_has_button(&dom, "Apply"),
        "the stale dialog must close"
    );
}

#[tokio::test]
async fn device_confirmation_is_disabled_after_disconnect() {
    let (mut dom, ui, _fake) = setup("settings:device").await;

    type_in(&dom, "dev-power", "18");
    dom.render_immediate_to_vec();
    click(&dom, &button(&dom, "Apply", false));
    settle(&mut dom).await;

    // One render after the state change, before the effect that closes the
    // dialog has run: the confirm button must already be disabled.
    in_app(&dom, || ui.apply_state(ConnState::Disconnected));
    dom.render_immediate_to_vec();

    let confirm = button(&dom, "Apply", true);

    assert!(
        confirm.disabled(),
        "every radio command button must be disabled after disconnect"
    );
    assert_eq!(confirm.attr("title"), Some("Not connected"));
}

#[tokio::test]
async fn confirmation_closes_when_the_session_disconnects() {
    let (mut dom, ui, _fake) = setup("settings:device").await;

    type_in(&dom, "dev-power", "18");
    dom.render_immediate_to_vec();
    click(&dom, &button(&dom, "Apply", false));
    settle(&mut dom).await;

    ui.session.send(Command::Disconnect);
    settle(&mut dom).await;

    assert!(matches!(*ui.state.peek(), ConnState::Disconnected));
    assert!(!dialog_has_button(&dom, "Apply"));
}

#[tokio::test]
async fn dirty_form_does_not_cross_nodes() {
    let (mut dom, ui, _fake) = setup("settings:device").await;

    type_in(&dom, "dev-power", "18");
    dom.render_immediate_to_vec();

    let mut other = device(&ui);

    other.node_id = other_node();
    other.name = "Other radio".into();
    other.tx_power = 7;
    in_app(&dom, || ui.apply_state(ConnState::Connected(other)));
    settle(&mut dom).await;

    assert_eq!(
        value_of(&dom, "dev-name").as_deref(),
        Some("Other radio"),
        "a draft for the old node must not appear as the new radio's settings"
    );
    assert_eq!(value_of(&dom, "dev-power").as_deref(), Some("7"));
}

#[tokio::test]
async fn pending_channel_remove_does_not_cross_connections() {
    let (mut dom, ui, fake) = setup("settings:channels").await;

    click(&dom, &button(&dom, "Remove", false));
    settle(&mut dom).await;
    assert!(page_text(&dom).contains("Remove channel Public from slot 0?"));

    ui.session.send(Command::Disconnect);
    settle(&mut dom).await;

    {
        let mut radio = fake.state();

        radio.device.node_id = other_node();
        radio.device.name = "Other radio".into();
        radio.channels[0].name = "Other channel".into();
    }

    ui.session.send(Command::Connect);
    settle(&mut dom).await;
    assert!(matches!(&*ui.state.peek(), ConnState::Connected(d) if d.name == "Other radio"));

    if dialog_has_button(&dom, "Remove") {
        click(&dom, &button(&dom, "Remove", true));
        settle(&mut dom).await;
    }

    assert!(
        fake.state().channels.iter().any(|c| c.idx == 0),
        "the confirmation for Public must not clear Other channel on another node"
    );
}

#[tokio::test]
async fn channel_remove_is_refused_when_the_slot_changed() {
    let (mut dom, ui, fake) = setup("settings:channels").await;

    click(&dom, &button(&dom, "Remove", false));
    settle(&mut dom).await;
    assert!(dialog_has_button(&dom, "Remove"));

    // Same node, same slot, different channel: the question no longer applies.
    fake.state().channels[0].secret = [7; 16];
    ui.session.send(Command::Refresh);
    settle(&mut dom).await;

    if dialog_has_button(&dom, "Remove") {
        click(&dom, &button(&dom, "Remove", true));
        settle(&mut dom).await;
    }

    assert!(
        fake.state().channels.iter().any(|c| c.idx == 0),
        "slot 0 now holds another channel and must not be cleared"
    );
}

#[tokio::test]
async fn pending_contact_remove_does_not_cross_connections() {
    let (mut dom, ui, fake) = setup("contacts").await;

    click(&dom, &button(&dom, "Remove", false));
    settle(&mut dom).await;
    assert!(dialog_has_button(&dom, "Remove"));

    ui.session.send(Command::Disconnect);
    settle(&mut dom).await;
    fake.state().device.node_id = other_node();
    ui.session.send(Command::Connect);
    settle(&mut dom).await;

    assert!(
        !dialog_has_button(&dom, "Remove"),
        "the dialog must not outlive its radio"
    );
    assert!(
        !fake
            .state()
            .calls
            .iter()
            .any(|c| c.starts_with("remove_contact")),
        "{:?}",
        fake.state().calls
    );
}

/// Selects the transport radio button `radio`.
fn pick_radio(dom: &VirtualDom, radio: &El) {
    event(
        dom,
        radio,
        "change",
        SerializedFormData::new(String::new(), vec![]),
    );
}

#[tokio::test]
async fn hidden_tcp_port_does_not_block_ble_save() {
    let (mut dom, ui, fake) = setup("settings:connection").await;
    let radios: Vec<El> = els(&dom)
        .into_iter()
        .filter(|e| e.tag == "input" && e.attr("type") == Some("radio"))
        .collect();
    let (ble, tcp) = (radios[0].clone(), radios[2].clone());

    pick_radio(&dom, &tcp);
    dom.render_immediate_to_vec();
    type_in(&dom, "tcp-port", "oops");
    dom.render_immediate_to_vec();
    pick_radio(&dom, &ble);
    dom.render_immediate_to_vec();
    type_in(&dom, "ble-device", "Demo Node");
    dom.render_immediate_to_vec();

    let before = fake.state().calls.len();

    click(&dom, &button(&dom, "Save and connect", false));
    settle(&mut dom).await;

    assert_eq!(
        ui.config.peek().connection.ble_device,
        "Demo Node",
        "a BLE save must ignore a hidden invalid TCP port"
    );
    assert!(fake.state().calls.len() > before);
}

#[tokio::test]
async fn invalid_tcp_port_still_blocks_a_tcp_save() {
    let (mut dom, ui, _fake) = setup("settings:connection").await;
    let tcp = els(&dom)
        .into_iter()
        .filter(|e| e.tag == "input" && e.attr("type") == Some("radio"))
        .nth(2)
        .unwrap();

    event(
        &dom,
        &tcp,
        "change",
        SerializedFormData::new(String::new(), vec![]),
    );
    dom.render_immediate_to_vec();
    type_in(&dom, "tcp-host", "radio.invalid");
    type_in(&dom, "tcp-port", "oops");
    dom.render_immediate_to_vec();
    click(&dom, &button(&dom, "Save and connect", false));
    settle(&mut dom).await;

    assert!(page_text(&dom).contains("The TCP port must be a number from 0 to 65535"));
    assert_ne!(ui.config.peek().connection.tcp_host, "radio.invalid");
}

#[tokio::test]
async fn whitespace_name_has_no_empty_confirmation() {
    let (mut dom, _ui, _fake) = setup("settings:device").await;

    type_in(&dom, "dev-name", " Demo Node ");
    dom.render_immediate_to_vec();

    assert!(
        button(&dom, "Apply", false).disabled(),
        "the trimmed name is unchanged, so Apply must stay disabled"
    );
}
