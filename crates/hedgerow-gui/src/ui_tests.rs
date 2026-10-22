//! Headless UI tests: the real [`App`] in a Dioxus `VirtualDom` over the demo
//! fake radio and an in-memory store. No window opens and nothing touches the
//! desktop session; events are delivered straight to the VDOM, and the page
//! is read back from the rendered virtual nodes.
//!
//! Test seams in the app: `app::window_focused` reports a focused window
//! (unless `app::set_test_focus` says otherwise), `app::set_window_title`
//! does nothing under `cfg(test)`, because tests have no native window, and
//! `notifier::show` collects posts instead of raising OS notifications. The fake radio makes `Ui::ui_test` true, so the
//! connection tab never saves a config or lists serial ports or Bluetooth.

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use dioxus::core::{
    AttributeValue, DynamicNode, ElementId, TemplateAttribute, TemplateNode, VNode,
};
use dioxus::html::geometry::{ClientPoint, Coordinates, ElementPoint, PagePoint, ScreenPoint};
use dioxus::html::input_data::{MouseButton, MouseButtonSet};
use dioxus::html::{
    Code, Location, Modifiers, PlatformEventData, SerializedFormData, SerializedHtmlEventConverter,
    SerializedKeyboardData, SerializedMouseData,
};
use dioxus::prelude::*;
use hedgerow_core::config::Config;
use hedgerow_core::model::{DeliveryStatus, Direction, NewMessage, NotifyMode, ThreadId};
use hedgerow_core::notify::Post;
use hedgerow_core::radio::{FakeRadio, FakeSent, IncomingMessage};
use hedgerow_core::session::{Command, ConnState};
use hedgerow_core::store::Store;

use crate::app::{self, App, Boot, Hooks, SettingsTab, Ui, View, now_secs};
use crate::notifier;

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

    /// Whether the element's class list includes `class`.
    fn has_class(&self, class: &str) -> bool {
        self.attr("class")
            .is_some_and(|list| list.split_whitespace().any(|c| c == class))
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
    key_down_with(dom, el, key, code, Modifiers::empty());
}

/// Presses `key` on `el` while `modifiers` are held.
fn key_down_with(dom: &VirtualDom, el: &El, key: Key, code: Code, modifiers: Modifiers) {
    event(
        dom,
        el,
        "keydown",
        SerializedKeyboardData::new(key, code, Location::Standard, false, modifiers, false),
    );
}

/// Delivers the mouse event `name` to `el` with the pointer at column `x`.
fn mouse(dom: &VirtualDom, el: &El, name: &str, x: f64) {
    let at = Coordinates::new(
        ScreenPoint::new(x, 0.0),
        ClientPoint::new(x, 0.0),
        ElementPoint::new(0.0, 0.0),
        PagePoint::new(x, 0.0),
    );

    event(
        dom,
        el,
        name,
        SerializedMouseData::new(
            Some(MouseButton::Primary),
            MouseButtonSet::empty(),
            at,
            Modifiers::empty(),
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

/// Starts the app on `view` (a `HEDGEROW_START_VIEW` value) over the demo
/// radio and waits for the connection.
async fn setup(view: &str) -> (VirtualDom, Ui, FakeRadio) {
    dioxus::html::set_event_converter(Box::new(SerializedHtmlEventConverter));

    // A test thread may have run an earlier test: start focused, with no posts.
    app::set_test_focus(true);
    notifier::take_posted();

    let fake = hedgerow_core::fixtures::demo_radio();
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
fn device(ui: &Ui) -> hedgerow_core::model::DeviceInfo {
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
        hedgerow_core::text::hashtag_secret("#smoke").unwrap()
    );

    // Pick the Remove button of the new row: the first one after its name.
    let page = els(&dom);
    let row = page
        .iter()
        .position(|e| e.has_class("cell-name") && e.text == "#smoke")
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

    let dialog = els(&dom)
        .into_iter()
        .find(|e| e.has_class("dialog"))
        .unwrap();

    key_down(&dom, &dialog, Key::Enter, Code::Enter);
    settle(&mut dom).await;

    assert!(
        !fake.state().channels.iter().any(|c| c.name == "#smoke"),
        "Enter confirms the removal"
    );
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

    click(&dom, &button(&dom, "Save", false));
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
    click(&dom, &button(&dom, "Save", false));
    settle(&mut dom).await;

    assert!(page_text(&dom).contains("The TCP port must be a number from 0 to 65535"));
    assert_ne!(ui.config.peek().connection.tcp_host, "radio.invalid");
}

#[tokio::test]
async fn save_waits_for_a_change_then_closes_back_to_the_view_it_replaced() {
    let (mut dom, ui, _fake) = setup("contacts").await;

    in_app(&dom, || ui.open_settings(SettingsTab::Connection));
    settle(&mut dom).await;
    assert!(
        button(&dom, "Save", false).disabled(),
        "nothing to save yet"
    );

    type_in(&dom, "ble-device", "Other Node");
    dom.render_immediate_to_vec();
    click(&dom, &button(&dom, "Save", false));
    settle(&mut dom).await;

    assert_eq!(ui.config.peek().connection.ble_device, "Other Node");
    assert_eq!(*ui.view.peek(), View::Contacts);
}

#[tokio::test]
async fn close_and_escape_drop_the_draft() {
    let (mut dom, ui, _fake) = setup("contacts").await;
    let saved = ui.config.peek().connection.ble_device.clone();

    in_app(&dom, || ui.open_settings(SettingsTab::Connection));
    settle(&mut dom).await;
    type_in(&dom, "ble-device", "Other Node");
    dom.render_immediate_to_vec();
    click(&dom, &button(&dom, "Close", false));
    settle(&mut dom).await;

    assert_eq!(*ui.view.peek(), View::Contacts);
    assert_eq!(ui.config.peek().connection.ble_device, saved);

    in_app(&dom, || ui.open_settings(SettingsTab::Connection));
    settle(&mut dom).await;
    assert_eq!(
        value_of(&dom, "ble-device").as_deref(),
        Some(saved.as_str()),
        "a reopened page starts from the saved settings"
    );

    let page = els(&dom)
        .into_iter()
        .find(|e| e.has_class("settings"))
        .unwrap();

    key_down(&dom, &page, Key::Escape, Code::Escape);
    settle(&mut dom).await;
    assert_eq!(*ui.view.peek(), View::Contacts);
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

// ── 008: commands guarded by the session ────────────────────────────────────

/// Waits, in real time, until `predicate` holds for the session's state.
async fn wait_session(ui: &Ui, predicate: impl Fn(&ConnState) -> bool) {
    for _ in 0..100 {
        if predicate(&ui.session.state()) {
            return;
        }

        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    panic!(
        "session did not reach the requested state: {:?}",
        ui.session.state()
    );
}

/// The title of the open chat.
fn chat_title(dom: &VirtualDom) -> String {
    els(dom)
        .into_iter()
        .find(|e| e.has_class("pane-title"))
        .unwrap()
        .text
}

#[tokio::test]
async fn confirmation_rechecks_session_before_ui_consumes_updates() {
    let (mut dom, ui, fake) = setup("settings:channels").await;

    click(&dom, &button(&dom, "Remove", false));
    settle(&mut dom).await;

    let confirm = button(&dom, "Remove", true);
    let original_node = device(&ui).node_id;

    ui.session.send(Command::Disconnect);
    wait_session(&ui, |s| *s == ConnState::Disconnected).await;

    {
        let mut radio = fake.state();

        radio.device.node_id = other_node();
        radio.channels[0].secret = [7; 16];
        radio.channels[0].name = "Other channel".into();
    }

    ui.session.send(Command::Connect);
    wait_session(
        &ui,
        |s| matches!(s, ConnState::Connected(d) if d.node_id == other_node()),
    )
    .await;

    // The session has switched radios; the UI's update future has not run.
    assert_eq!(device(&ui).node_id, original_node);

    click(&dom, &confirm);
    settle(&mut dom).await;

    assert!(
        fake.state().channels.iter().any(|c| c.idx == 0),
        "an old confirmation cleared a channel on another radio: {:?}",
        fake.state().calls
    );
}

#[tokio::test]
async fn settings_plan_rechecks_actor_before_ui_consumes_updates() {
    let (mut dom, ui, fake) = setup("settings:device").await;

    type_in(&dom, "dev-power", "18");
    dom.render_immediate_to_vec();
    click(&dom, &button(&dom, "Apply", false));
    settle(&mut dom).await;

    let confirm = button(&dom, "Apply", true);

    assert!(page_text(&dom).contains("TX power: 20 dBm → 18 dBm"));

    fake.state().device.radio_freq = 915_000;
    ui.session.send(Command::Refresh);
    wait_session(
        &ui,
        |s| matches!(s, ConnState::Connected(d) if d.radio_freq == 915_000),
    )
    .await;

    assert_eq!(
        device(&ui).radio_freq,
        910_525,
        "the UI consumed the update"
    );

    click(&dom, &confirm);
    settle(&mut dom).await;

    assert_eq!(
        fake.state().device.radio_freq,
        915_000,
        "a TX-power-only dialog reset the frequency the UI had not yet seen"
    );
}

#[tokio::test]
async fn import_does_not_cross_nodes() {
    let (mut dom, ui, fake) = setup("contacts").await;

    click(&dom, &button(&dom, "Import…", false));
    settle(&mut dom).await;

    let textarea = els(&dom)
        .into_iter()
        .find(|e| e.tag == "textarea" && e.dialog)
        .unwrap();

    // A minimal contact card: type 2, a 32-byte key, and a name.
    type_into(
        &dom,
        &textarea,
        &format!("meshcore://02{}{}", "77".repeat(32), "526576696577"),
    );
    settle(&mut dom).await;

    ui.session.send(Command::Disconnect);
    settle(&mut dom).await;
    fake.state().device.node_id = other_node();
    ui.session.send(Command::Connect);
    settle(&mut dom).await;
    assert_eq!(device(&ui).node_id, other_node());

    if dialog_has_button(&dom, "Import") {
        click(&dom, &button(&dom, "Import", true));
        settle(&mut dom).await;
    }

    assert!(
        !fake.state().calls.iter().any(|c| c == "import_contact"),
        "an import opened for the first radio reached the second: {:?}",
        fake.state().calls
    );
}

#[tokio::test]
async fn chat_draft_does_not_cross_nodes() {
    let (mut dom, ui, fake) = setup("contacts").await;

    open_composer(&mut dom, &ui).await;
    type_into(
        &dom,
        &first_tag(&dom, "textarea"),
        "private draft for first radio",
    );
    settle(&mut dom).await;

    ui.session.send(Command::Disconnect);
    settle(&mut dom).await;

    {
        let mut radio = fake.state();

        radio.device.node_id = other_node();
        radio.channels[0].name = "Other radio channel".into();
        radio.channels[0].secret = [7; 16];
    }

    ui.session.send(Command::Connect);
    settle(&mut dom).await;
    assert_eq!(device(&ui).node_id, other_node());
    assert_eq!(chat_title(&dom), "Other radio channel");

    press_enter(&dom, &first_tag(&dom, "textarea"));
    settle(&mut dom).await;

    assert!(
        sent_channel_texts(&fake).is_empty(),
        "a draft for the first radio was sent on the second: {:?}",
        fake.state().sent
    );
}

#[tokio::test]
async fn chat_draft_does_not_cross_channels_in_one_slot() {
    let (mut dom, ui, fake) = setup("contacts").await;

    open_composer(&mut dom, &ui).await;
    type_into(
        &dom,
        &first_tag(&dom, "textarea"),
        "draft for the public channel",
    );
    settle(&mut dom).await;

    {
        let mut radio = fake.state();

        radio.channels[0].name = "Replacement".into();
        radio.channels[0].secret = [7; 16];
    }

    ui.session.send(Command::Refresh);
    settle(&mut dom).await;
    assert_eq!(chat_title(&dom), "Replacement");

    press_enter(&dom, &first_tag(&dom, "textarea"));
    settle(&mut dom).await;

    assert!(
        sent_channel_texts(&fake).is_empty(),
        "a draft typed for one channel was sent to its replacement: {:?}",
        fake.state().sent
    );
}

#[tokio::test]
async fn chat_send_is_refused_when_slot_changed_before_ui_consumes_updates() {
    let (mut dom, ui, fake) = setup("contacts").await;
    let node = device(&ui).node_id;
    let textarea = open_composer(&mut dom, &ui).await;

    type_into(&dom, &textarea, "draft for the public channel");
    settle(&mut dom).await;

    fake.state().channels[0].secret = [7; 16];
    ui.session.send(Command::Refresh);

    // The session stores the new channel before it publishes anything; the
    // UI's update future has not run.
    for _ in 0..100 {
        let stored = ui.store.channels(&node).unwrap();

        if stored.iter().any(|c| c.idx == 0 && c.secret == [7; 16]) {
            break;
        }

        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    press_enter(&dom, &textarea);
    settle(&mut dom).await;

    assert!(
        sent_channel_texts(&fake).is_empty(),
        "a draft was sent to a channel that replaced the one it was typed for: {:?}",
        fake.state().sent
    );
}

/// A channel form filled on A must not write to B after reconnect.
#[tokio::test]
async fn r2_add_channel_draft_does_not_cross_nodes() {
    let (mut dom, ui, fake) = setup("settings:channels").await;
    change(&dom, "new-kind", "private");
    settle(&mut dom).await;
    type_in(&dom, "new-name", "Private for A");
    type_in(&dom, "new-key", &"42".repeat(16));
    settle(&mut dom).await;
    ui.session.send(Command::Disconnect);
    settle(&mut dom).await;
    fake.state().device.node_id = other_node();
    ui.session.send(Command::Connect);
    settle(&mut dom).await;
    assert_eq!(device(&ui).node_id, other_node());
    assert_eq!(
        value_of(&dom, "new-name").as_deref(),
        Some(""),
        "the draft prepared for A survived onto B"
    );
    click(&dom, &button(&dom, "Add", false));
    settle(&mut dom).await;
    assert!(
        !fake
            .state()
            .calls
            .iter()
            .any(|c| c.starts_with("set_channel")),
        "channel credentials prepared for A were installed on B: {:?}",
        fake.state().calls
    );
}

/// A local stale-slot refusal must explain why Remove did nothing.
#[tokio::test]
async fn r2_channel_remove_refusal_is_visible() {
    let (mut dom, ui, fake) = setup("settings:channels").await;
    click(&dom, &button(&dom, "Remove", false));
    settle(&mut dom).await;
    let confirm = button(&dom, "Remove", true);
    let node = device(&ui).node_id;
    fake.state().channels[0].secret = [7; 16];
    ui.session.send(Command::Refresh);
    for _ in 0..100 {
        if ui
            .store
            .channels(&node)
            .unwrap()
            .iter()
            .any(|c| c.idx == 0 && c.secret == [7; 16])
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        ui.store
            .channels(&node)
            .unwrap()
            .iter()
            .any(|c| c.idx == 0 && c.secret == [7; 16])
    );
    click(&dom, &confirm);
    settle(&mut dom).await;
    assert!(
        !fake
            .state()
            .calls
            .iter()
            .any(|c| c.starts_with("clear_channel"))
    );
    assert!(!dialog_has_button(&dom, "Remove"));
    assert!(
        ui.toast.peek().is_some(),
        "stale-slot Remove closed with no cancellation notice"
    );
}

/// The actor guard must compare the original untrimmed name.
#[tokio::test]
async fn r2_apply_accepts_untrimmed_device_name() {
    let (mut dom, ui, fake) = setup("settings:device").await;
    fake.state().device.name = " Demo Node ".into();
    ui.session.send(Command::Refresh);
    settle(&mut dom).await;
    assert_eq!(device(&ui).name, " Demo Node ");
    type_in(&dom, "dev-power", "18");
    dom.render_immediate_to_vec();
    click(&dom, &button(&dom, "Apply", false));
    settle(&mut dom).await;
    click(&dom, &button(&dom, "Apply", true));
    settle(&mut dom).await;
    assert_eq!(
        fake.state().device.tx_power,
        18,
        "legitimate Apply was refused"
    );
    assert!(!fake.state().frames.is_empty());
}

/// Retry from an old bubble must carry its original node.
#[tokio::test]
async fn r2_retry_refuses_stale_node() {
    let (mut dom, ui, fake) = setup("contacts").await;
    let node = device(&ui).node_id;
    let textarea = open_composer(&mut dom, &ui).await;
    fake.state().fail_call = Some((
        "send_channel_text".into(),
        hedgerow_core::radio::FakeFailure::Rejected("test".into()),
    ));
    type_into(&dom, &textarea, "failed draft on A");
    press_enter(&dom, &textarea);
    settle(&mut dom).await;
    let retry = button(&dom, "Retry", false);
    ui.session.send(Command::Disconnect);
    wait_session(&ui, |s| *s == ConnState::Disconnected).await;
    fake.state().device.node_id = other_node();
    ui.session.send(Command::Connect);
    wait_session(
        &ui,
        |s| matches!(s, ConnState::Connected(d) if d.node_id == other_node()),
    )
    .await;
    assert_eq!(device(&ui).node_id, node);
    click(&dom, &retry);
    settle(&mut dom).await;
    assert!(fake.state().sent.is_empty());
    assert_eq!(
        ui.toast.peek().as_deref(),
        Some("The radio changed; action cancelled")
    );
}

/// A stale Remove control must not create a confirmation bound to B.
#[tokio::test]
async fn r2_contact_remove_opening_uses_displayed_node() {
    let (mut dom, ui, fake) = setup("contacts").await;
    let remove = button(&dom, "Remove", false);
    let alice = fake.state().contacts[0].public_key.clone();
    ui.session.send(Command::Disconnect);
    wait_session(&ui, |s| *s == ConnState::Disconnected).await;
    fake.state().device.node_id = other_node();
    fake.state().contacts[0].name = "B's Alice".into();
    ui.session.send(Command::Connect);
    wait_session(
        &ui,
        |s| matches!(s, ConnState::Connected(d) if d.node_id == other_node()),
    )
    .await;
    // Consume the actor state without rendering new rows: the old button is visible.
    in_app(&dom, || ui.apply_state(ui.session.state()));
    click(&dom, &remove);
    settle(&mut dom).await;
    if dialog_has_button(&dom, "Remove") {
        click(&dom, &button(&dom, "Remove", true));
        settle(&mut dom).await;
    }
    assert!(
        fake.state().contacts.iter().any(|c| c.public_key == alice),
        "a Remove drawn for A deleted B's contact: {:?}",
        fake.state().calls
    );
}

/// A stale channel-row Remove must retain the node it was drawn for.
#[tokio::test]
async fn r2_channel_remove_opening_uses_displayed_node() {
    let (mut dom, ui, fake) = setup("settings:channels").await;
    let remove = button(&dom, "Remove", false);
    ui.session.send(Command::Disconnect);
    wait_session(&ui, |s| *s == ConnState::Disconnected).await;
    fake.state().device.node_id = other_node();
    fake.state().channels[0].name = "B's Public".into();
    ui.session.send(Command::Connect);
    wait_session(
        &ui,
        |s| matches!(s, ConnState::Connected(d) if d.node_id == other_node()),
    )
    .await;
    in_app(&dom, || ui.apply_state(ui.session.state()));
    click(&dom, &remove);
    settle(&mut dom).await;
    if dialog_has_button(&dom, "Remove") {
        click(&dom, &button(&dom, "Remove", true));
        settle(&mut dom).await;
    }
    assert!(
        fake.state().channels.iter().any(|c| c.idx == 0),
        "a Remove drawn for A deleted B's Public channel: {:?}",
        fake.state().calls
    );
}

/// Channel Retry must use stored secret identity rather than the stale slot.
#[tokio::test]
async fn r2_retry_refuses_replaced_channel() {
    let (mut dom, ui, fake) = setup("contacts").await;
    let node = device(&ui).node_id;
    let textarea = open_composer(&mut dom, &ui).await;
    fake.state().fail_call = Some((
        "send_channel_text".into(),
        hedgerow_core::radio::FakeFailure::Rejected("test".into()),
    ));
    type_into(&dom, &textarea, "failed on Public");
    press_enter(&dom, &textarea);
    settle(&mut dom).await;
    let retry = button(&dom, "Retry", false);
    fake.state().channels[0].secret = [7; 16];
    ui.session.send(Command::Refresh);
    for _ in 0..100 {
        if ui
            .store
            .channels(&node)
            .unwrap()
            .iter()
            .any(|c| c.idx == 0 && c.secret == [7; 16])
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        ui.store
            .channels(&node)
            .unwrap()
            .iter()
            .any(|c| c.idx == 0 && c.secret == [7; 16])
    );
    click(&dom, &retry);
    settle(&mut dom).await;
    assert!(
        fake.state().sent.is_empty(),
        "Retry sent through the replacement channel"
    );
}

/// Import's actor guard must also work before the pending effect can close it.
#[tokio::test]
async fn r2_import_refuses_actor_node_change_before_ui_update() {
    let (mut dom, ui, fake) = setup("contacts").await;
    let node = device(&ui).node_id;
    click(&dom, &button(&dom, "Import…", false));
    settle(&mut dom).await;
    let textarea = els(&dom)
        .into_iter()
        .find(|e| e.tag == "textarea" && e.dialog)
        .unwrap();
    type_into(
        &dom,
        &textarea,
        &format!("meshcore://02{}{}", "77".repeat(32), "526576696577"),
    );
    settle(&mut dom).await;
    let import = button(&dom, "Import", true);
    ui.session.send(Command::Disconnect);
    wait_session(&ui, |s| *s == ConnState::Disconnected).await;
    fake.state().device.node_id = other_node();
    ui.session.send(Command::Connect);
    wait_session(
        &ui,
        |s| matches!(s, ConnState::Connected(d) if d.node_id == other_node()),
    )
    .await;
    assert_eq!(device(&ui).node_id, node);
    click(&dom, &import);
    settle(&mut dom).await;
    assert!(!fake.state().calls.iter().any(|c| c == "import_contact"));
    assert_eq!(
        ui.toast.peek().as_deref(),
        Some("The radio changed; action cancelled")
    );
}

// ── 011: sidebar and search ─────────────────────────────────────────────────

/// The element carrying the root key handler.
fn app_root(dom: &VirtualDom) -> El {
    els(dom)
        .into_iter()
        .find(|e| e.has_class("app"))
        .expect("the .app element is missing")
}

/// The grid that holds the sidebar, the divider, and `<main>`.
fn panes(dom: &VirtualDom) -> El {
    els(dom)
        .into_iter()
        .find(|e| e.attr("class").is_some_and(|c| c.starts_with("panes")))
        .expect("the .panes element is missing")
}

/// The sidebar divider.
fn divider(dom: &VirtualDom) -> El {
    els(dom)
        .into_iter()
        .find(|e| e.has_class("resizer"))
        .expect("the divider is missing")
}

/// The button whose `title` is `title`.
fn titled_button(dom: &VirtualDom, title: &str) -> El {
    els(dom)
        .into_iter()
        .find(|e| e.tag == "button" && e.attr("title") == Some(title))
        .unwrap_or_else(|| panic!("button titled {title:?} missing"))
}

/// Whether the sidebar and its divider are on the page.
fn sidebar_shown(dom: &VirtualDom) -> bool {
    let page = els(dom);

    page.iter().any(|e| e.tag == "aside") && page.iter().any(|e| e.has_class("resizer"))
}

/// Whether neither the sidebar nor its divider is on the page.
fn sidebar_gone(dom: &VirtualDom) -> bool {
    let page = els(dom);

    !page.iter().any(|e| e.tag == "aside") && !page.iter().any(|e| e.has_class("resizer"))
}

/// Presses Ctrl plus the letter `letter` on the root element.
fn ctrl(dom: &VirtualDom, letter: &str) {
    key_down_with(
        dom,
        &app_root(dom),
        Key::Character(letter.into()),
        Code::KeyB,
        Modifiers::CONTROL,
    );
}

/// Drags the divider from column 260 to `to` and lets the page settle.
async fn drag_divider(dom: &mut VirtualDom, to: f64) {
    mouse(dom, &divider(dom), "mousedown", 260.0);
    mouse(dom, &panes(dom), "mousemove", to);
    settle(dom).await;
}

/// Types `text` into the sidebar's search box.
async fn search_for(dom: &mut VirtualDom, text: &str) {
    type_in(dom, "sidebar-search", text);
    settle(dom).await;
}

/// The search results on the page.
fn results(dom: &VirtualDom) -> Vec<El> {
    els(dom)
        .into_iter()
        .filter(|e| e.has_class("result"))
        .collect()
}

/// The text of every `<mark>` on the page.
fn marks(dom: &VirtualDom) -> Vec<String> {
    els(dom)
        .into_iter()
        .filter(|e| e.tag == "mark")
        .map(|e| e.text)
        .collect()
}

/// The newest stored message of the demo node that contains `needle`.
fn found(ui: &Ui, needle: &str) -> hedgerow_core::model::Message {
    let node = ui.node.peek().clone().unwrap();

    ui.store
        .search(&node, needle, 1)
        .unwrap()
        .pop()
        .unwrap_or_else(|| panic!("no stored message contains {needle:?}"))
}

#[tokio::test]
async fn sidebar_divider_drag_sets_the_width_and_clamps_at_both_ends() {
    let (mut dom, ui, _fake) = setup("contacts").await;

    assert_eq!(*ui.sidebar_width.peek(), 260);

    drag_divider(&mut dom, 300.0).await;
    assert_eq!(*ui.sidebar_width.peek(), 300);
    assert!(
        panes(&dom)
            .attr("style")
            .is_some_and(|s| s.contains("300px 6px")),
        "the grid follows the width: {:?}",
        panes(&dom).attr("style")
    );

    mouse(&dom, &panes(&dom), "mousemove", 9000.0);
    settle(&mut dom).await;
    assert_eq!(*ui.sidebar_width.peek(), 480);

    mouse(&dom, &panes(&dom), "mousemove", -9000.0);
    settle(&mut dom).await;
    assert_eq!(*ui.sidebar_width.peek(), 180);
}

#[tokio::test]
async fn sidebar_divider_moves_without_a_drag_change_nothing() {
    let (mut dom, ui, _fake) = setup("contacts").await;

    mouse(&dom, &panes(&dom), "mousemove", 400.0);
    settle(&mut dom).await;

    assert_eq!(*ui.sidebar_width.peek(), 260);
}

#[tokio::test]
async fn sidebar_divider_drag_stops_on_mouse_up_and_keeps_the_config_in_step() {
    let (mut dom, ui, _fake) = setup("contacts").await;

    drag_divider(&mut dom, 320.0).await;
    mouse(&dom, &panes(&dom), "mouseup", 320.0);
    settle(&mut dom).await;
    mouse(&dom, &panes(&dom), "mousemove", 400.0);
    settle(&mut dom).await;

    assert_eq!(
        *ui.sidebar_width.peek(),
        320,
        "no drag after the button is up"
    );
    assert_eq!(ui.config.peek().ui.sidebar_width, 320);
    assert_eq!(panes(&dom).attr("class"), Some("panes"));
}

#[tokio::test]
async fn sidebar_toggle_button_collapses_and_reopens_keeping_the_width() {
    let (mut dom, ui, _fake) = setup("contacts").await;

    drag_divider(&mut dom, 330.0).await;
    mouse(&dom, &panes(&dom), "mouseup", 330.0);
    assert!(sidebar_shown(&dom));

    click(&dom, &titled_button(&dom, "Hide sidebar"));
    settle(&mut dom).await;

    assert!(
        sidebar_gone(&dom),
        "collapsed means no sidebar and no divider"
    );
    assert!(
        panes(&dom).attr("style").is_some_and(|s| !s.contains("px")),
        "only the main column is left: {:?}",
        panes(&dom).attr("style")
    );

    click(&dom, &titled_button(&dom, "Show sidebar"));
    settle(&mut dom).await;

    assert!(sidebar_shown(&dom));
    assert_eq!(*ui.sidebar_width.peek(), 330);
}

#[tokio::test]
async fn sidebar_folds_in_a_narrow_window_and_returns_when_it_widens() {
    let (mut dom, ui, _fake) = setup("contacts").await;
    let width = *ui.sidebar_width.peek();
    let resize = |dom: &VirtualDom, w: u32| in_app(dom, || ui.set_window_width(f64::from(w)));

    resize(&dom, width + 360);
    settle(&mut dom).await;
    assert!(
        sidebar_shown(&dom),
        "room for the sidebar and the main pane"
    );

    resize(&dom, width + 359);
    settle(&mut dom).await;
    assert!(sidebar_gone(&dom), "too narrow folds it");

    resize(&dom, width + 360);
    settle(&mut dom).await;
    assert!(sidebar_shown(&dom), "widening brings a folded sidebar back");

    click(&dom, &titled_button(&dom, "Hide sidebar"));
    resize(&dom, 400);
    resize(&dom, 1200);
    settle(&mut dom).await;
    assert!(sidebar_gone(&dom), "a sidebar hidden by hand stays hidden");

    click(&dom, &titled_button(&dom, "Show sidebar"));
    resize(&dom, 400);
    settle(&mut dom).await;
    assert!(sidebar_gone(&dom), "narrowing folds it again");

    click(&dom, &titled_button(&dom, "Show sidebar"));
    resize(&dom, 1200);
    settle(&mut dom).await;
    assert!(
        sidebar_shown(&dom),
        "a sidebar shown by hand while narrow stays shown"
    );
}

#[tokio::test]
async fn sidebar_ctrl_b_toggles_and_other_chords_do_not() {
    let (mut dom, _ui, _fake) = setup("contacts").await;

    ctrl(&dom, "b");
    settle(&mut dom).await;
    assert!(sidebar_gone(&dom));

    ctrl(&dom, "b");
    settle(&mut dom).await;
    assert!(sidebar_shown(&dom));

    key_down(
        &dom,
        &app_root(&dom),
        Key::Character("b".into()),
        Code::KeyB,
    );
    ctrl(&dom, "x");
    settle(&mut dom).await;
    assert!(sidebar_shown(&dom), "a bare b and Ctrl+X leave it alone");
}

#[tokio::test]
async fn search_ctrl_f_opens_a_collapsed_sidebar_and_asks_the_box_for_focus() {
    let (mut dom, ui, _fake) = setup("contacts").await;

    in_app(&dom, || ui.toggle_sidebar());
    settle(&mut dom).await;
    assert!(sidebar_gone(&dom));

    ctrl(&dom, "f");
    assert!(*ui.focus_search.peek(), "Ctrl+F asks for focus");

    settle(&mut dom).await;

    assert!(sidebar_shown(&dom));
    assert!(
        !*ui.focus_search.peek(),
        "the search box took the focus request"
    );
}

#[tokio::test]
async fn search_two_characters_show_matching_results_and_update_as_you_type() {
    let (mut dom, ui, _fake) = setup("contacts").await;

    search_for(&mut dom, "a").await;
    assert!(results(&dom).is_empty(), "one character shows no results");
    assert_eq!(*ui.view.peek(), View::Contacts);

    search_for(&mut dom, "ANTENNA").await;

    let shown = results(&dom);

    assert_eq!(shown.len(), 1, "{}", page_text(&dom));
    assert!(
        shown[0]
            .text
            .contains("Can you bring the antenna on Saturday?")
    );
    assert!(
        shown[0].text.contains("Bob"),
        "the thread title: {}",
        shown[0].text
    );
    assert_eq!(marks(&dom), ["antenna"], "the text's own case is kept");

    search_for(&mut dom, "net").await;

    let shown = results(&dom);

    assert_eq!(shown.len(), 1, "{}", page_text(&dom));
    assert!(shown[0].text.contains("Net starts at 8pm"));
    assert!(
        shown[0].text.contains("Alice"),
        "the sender: {}",
        shown[0].text
    );
    assert_eq!(marks(&dom), ["Net"]);
    assert!(!first_tag(&dom, "main").text.contains("antenna"));
    assert!(matches!(*ui.view.peek(), View::Search(_)));
}

#[tokio::test]
async fn search_sender_matches_are_marked_too() {
    let (mut dom, _ui, _fake) = setup("contacts").await;

    search_for(&mut dom, "carol").await;

    assert_eq!(results(&dom).len(), 1);
    assert_eq!(marks(&dom), ["Carol"]);
}

#[tokio::test]
async fn search_lists_at_most_200_results() {
    let (mut dom, ui, _fake) = setup("contacts").await;
    let node = ui.node.peek().clone().unwrap();

    in_app(&dom, || {
        for n in 0..250 {
            ui.store
                .insert_message(&node, &incoming(&format!("bulk line {n}")))
                .unwrap();
        }

        ui.bump();
    });
    search_for(&mut dom, "bulk line").await;

    assert_eq!(results(&dom).len(), 200);
}

#[tokio::test]
async fn search_miss_says_no_messages_match() {
    let (mut dom, _ui, _fake) = setup("contacts").await;

    search_for(&mut dom, "zzzz").await;

    assert!(results(&dom).is_empty());
    assert!(first_tag(&dom, "main").text.contains("No messages match"));
}

#[tokio::test]
async fn search_click_on_a_result_opens_its_thread_at_the_message() {
    let (mut dom, ui, _fake) = setup("contacts").await;
    let hit = found(&ui, "antenna");

    search_for(&mut dom, "antenna").await;
    click(&dom, &results(&dom)[0]);
    settle(&mut dom).await;

    assert_eq!(*ui.view.peek(), View::Thread(hit.thread.clone()));

    let bubble = by_id(&dom, &format!("msg-{}", hit.id));

    assert_eq!(
        bubble.attr("class"),
        Some("msg in flash"),
        "the found message is highlighted"
    );
    assert_eq!(*ui.reveal.peek(), None, "the request is used up");
    assert_eq!(
        ui.search.peek().as_str(),
        "antenna",
        "the box keeps its text"
    );
}

#[tokio::test]
async fn search_click_loads_a_message_older_than_the_newest_500() {
    let (mut dom, ui, _fake) = setup("contacts").await;
    let node = ui.node.peek().clone().unwrap();
    let oldest = in_app(&dom, || {
        let oldest = ui
            .store
            .insert_message(&node, &incoming("zebra crossing ahead"))
            .unwrap()
            .unwrap();

        for n in 0..600 {
            ui.store
                .insert_message(&node, &incoming(&format!("filler {n}")))
                .unwrap();
        }

        ui.bump();

        oldest
    });
    let bubble = format!("msg-{oldest}");

    in_app(&dom, || ui.select(ThreadId::Channel(0)));
    settle(&mut dom).await;
    assert!(
        !els(&dom).iter().any(|e| e.attr("id") == Some(&bubble)),
        "a plain open shows only the newest 500"
    );

    search_for(&mut dom, "zebra").await;
    click(&dom, &results(&dom)[0]);
    settle(&mut dom).await;

    assert_eq!(by_id(&dom, &bubble).attr("class"), Some("msg in flash"));
}

#[tokio::test]
async fn search_escape_clears_the_box_and_returns_to_the_previous_view() {
    let (mut dom, ui, _fake) = setup("thread:channel:0").await;

    search_for(&mut dom, "antenna").await;
    assert!(matches!(*ui.view.peek(), View::Search(_)));

    key_down(
        &dom,
        &by_id(&dom, "sidebar-search"),
        Key::Escape,
        Code::Escape,
    );
    settle(&mut dom).await;

    assert_eq!(*ui.view.peek(), View::Thread(ThreadId::Channel(0)));
    assert_eq!(value_of(&dom, "sidebar-search").as_deref(), Some(""));
    assert!(results(&dom).is_empty());
}

#[tokio::test]
async fn search_shortening_the_text_returns_to_the_previous_view() {
    let (mut dom, ui, _fake) = setup("contacts").await;

    search_for(&mut dom, "antenna").await;
    assert!(matches!(*ui.view.peek(), View::Search(_)));

    search_for(&mut dom, "a").await;

    assert_eq!(*ui.view.peek(), View::Contacts);
    assert_eq!(value_of(&dom, "sidebar-search").as_deref(), Some("a"));
}

// ── 012: review fixes ───────────────────────────────────────────────────────

/// Unread count of `thread` on `node`.
fn unread_of(ui: &Ui, node: &str, thread: &ThreadId) -> u32 {
    ui.store
        .threads(node)
        .unwrap()
        .into_iter()
        .find(|t| t.thread == *thread)
        .unwrap()
        .unread
}

#[tokio::test]
async fn search_result_drawn_for_another_node_opens_nothing_and_marks_nothing_read() {
    let (mut dom, ui, fake) = setup("contacts").await;

    search_for(&mut dom, "antenna").await;

    let hit = results(&dom)[0].clone();
    let thread = found(&ui, "antenna").thread;

    ui.session.send(Command::Disconnect);
    wait_session(&ui, |s| *s == ConnState::Disconnected).await;
    fake.state().device.node_id = other_node();
    ui.session.send(Command::Connect);
    wait_session(
        &ui,
        |s| matches!(s, ConnState::Connected(d) if d.node_id == other_node()),
    )
    .await;

    let mut unread = incoming("B has private unread history");

    unread.thread = thread.clone();
    ui.store.insert_message(&other_node(), &unread).unwrap();
    assert_eq!(unread_of(&ui, &other_node(), &thread), 1);

    // Apply B's state without rendering new results: A's result stays live.
    in_app(&dom, || ui.apply_state(ui.session.state()));
    click(&dom, &hit);
    settle(&mut dom).await;

    assert_eq!(unread_of(&ui, &other_node(), &thread), 1);
    assert!(matches!(*ui.view.peek(), View::Search(_)));
    assert_eq!(
        ui.toast.peek().as_deref(),
        Some("The radio changed; action cancelled")
    );
}

#[tokio::test]
async fn search_result_for_a_replaced_channel_opens_nothing_and_marks_nothing_read() {
    let (mut dom, ui, _fake) = setup("contacts").await;
    let node = ui.node.peek().clone().unwrap();

    ui.store
        .insert_message(&node, &incoming("zebra crossing"))
        .unwrap();
    search_for(&mut dom, "zebra").await;

    let hit = results(&dom)[0].clone();
    let mut channels = ui.store.channels(&node).unwrap();

    channels[0].secret = [0xEE; 16];
    ui.store.replace_channels(&node, &channels).unwrap();
    ui.store
        .insert_message(&node, &incoming("fresh unread in the new channel"))
        .unwrap();
    click(&dom, &hit);
    settle(&mut dom).await;

    assert!(matches!(*ui.view.peek(), View::Search(_)));
    assert_eq!(
        unread_of(&ui, &node, &ThreadId::Channel(0)),
        1,
        "the new channel's unread must stay"
    );
    assert_eq!(
        ui.toast.peek().as_deref(),
        Some("The radio changed; action cancelled")
    );
}

#[tokio::test]
async fn ctrl_b_in_the_composer_leaves_the_sidebar_open() {
    let (mut dom, _ui, _fake) = setup("thread:channel:0").await;

    key_down_with(
        &dom,
        &first_tag(&dom, "textarea"),
        Key::Character("b".into()),
        Code::KeyB,
        Modifiers::CONTROL,
    );
    settle(&mut dom).await;

    assert!(sidebar_shown(&dom), "Ctrl+B from the composer collapsed it");
}

#[tokio::test]
async fn ctrl_b_in_a_settings_input_leaves_the_sidebar_open() {
    let (mut dom, _ui, _fake) = setup("settings:device").await;

    key_down_with(
        &dom,
        &first_tag(&dom, "input"),
        Key::Character("b".into()),
        Code::KeyB,
        Modifiers::CONTROL,
    );
    settle(&mut dom).await;

    assert!(sidebar_shown(&dom), "Ctrl+B from an input collapsed it");
}

#[tokio::test]
async fn ctrl_b_in_the_search_box_leaves_the_sidebar_open() {
    let (mut dom, _ui, _fake) = setup("contacts").await;

    key_down_with(
        &dom,
        &by_id(&dom, "sidebar-search"),
        Key::Character("b".into()),
        Code::KeyB,
        Modifiers::CONTROL,
    );
    settle(&mut dom).await;

    assert!(sidebar_shown(&dom));
}

#[tokio::test]
async fn ctrl_f_in_the_composer_asks_the_search_box_for_focus() {
    let (mut dom, ui, _fake) = setup("thread:channel:0").await;

    in_app(&dom, || ui.toggle_sidebar());
    settle(&mut dom).await;
    assert!(sidebar_gone(&dom));

    key_down_with(
        &dom,
        &first_tag(&dom, "textarea"),
        Key::Character("f".into()),
        Code::KeyF,
        Modifiers::CONTROL,
    );

    assert!(
        *ui.focus_search.peek(),
        "Ctrl+F from the composer is ignored"
    );

    settle(&mut dom).await;

    assert!(sidebar_shown(&dom));
}

#[tokio::test]
async fn ctrl_b_outside_inputs_still_toggles_the_sidebar() {
    let (mut dom, _ui, _fake) = setup("thread:channel:0").await;

    ctrl(&dom, "b");
    settle(&mut dom).await;
    assert!(sidebar_gone(&dom));

    ctrl(&dom, "b");
    settle(&mut dom).await;
    assert!(sidebar_shown(&dom));
}

// ── 014: notifications ──────────────────────────────────────────────────────

/// A `sender_ts` no earlier push used, so the store never folds two pushes.
fn fresh_ts() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(2_000_000_000);

    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// The direct thread with the demo contact Alice.
fn alice() -> ThreadId {
    ThreadId::Contact("a1".repeat(6))
}

/// A direct message row from Alice for `node`, not yet stored.
fn direct_from_alice(text: &str) -> NewMessage {
    NewMessage {
        thread: alice(),
        sender: None,
        ..incoming(text)
    }
}

/// Queues a direct message `text` on the radio for `thread`, waits until the
/// session has stored it and the notifier has handled it, and returns its row
/// id.
async fn push_and_wait(
    dom: &mut VirtualDom,
    ui: &Ui,
    fake: &FakeRadio,
    thread: ThreadId,
    text: &str,
) -> i64 {
    let node = device(ui).node_id;

    fake.push_incoming(IncomingMessage {
        thread,
        sender: None,
        text: text.into(),
        sender_ts: fresh_ts(),
        path_len: None,
        snr: None,
    });

    let mut id = None;

    for _ in 0..500 {
        settle(dom).await;

        if let Some(row) = ui.store.search(&node, text, 1).unwrap().pop() {
            id = Some(row.id);

            break;
        }
    }

    let id = id.unwrap_or_else(|| panic!("{text:?} was never stored"));

    for _ in 0..500 {
        if notifier::handled(id) {
            return id;
        }

        settle(dom).await;
    }

    panic!("the notifier never handled row {id}");
}

/// The sidebar row of the conversation titled `title`.
fn row_titled(dom: &VirtualDom, title: &str) -> El {
    els(dom)
        .into_iter()
        .find(|e| e.has_class("row") && e.text.starts_with(title))
        .unwrap_or_else(|| panic!("row {title:?} missing"))
}

/// Opens the right-click menu of the row titled `title`.
async fn open_row_menu(dom: &mut VirtualDom, title: &str) {
    event(
        dom,
        &row_titled(dom, title),
        "contextmenu",
        SerializedMouseData::default(),
    );
    settle(dom).await;
}

/// The stored notification mode of `thread` on `node`.
fn mode_of(ui: &Ui, node: &str, thread: &ThreadId) -> NotifyMode {
    ui.store
        .threads(node)
        .unwrap()
        .into_iter()
        .find(|t| t.thread == *thread)
        .unwrap()
        .notify
}

/// Makes the UI show another radio than the one the session holds.
fn show_other_node(dom: &VirtualDom, ui: &Ui) {
    in_app(dom, || {
        let mut node = ui.node;

        node.set(Some(other_node()));
    });
}

#[tokio::test]
async fn notification_for_a_direct_message_when_unfocused() {
    let (mut dom, ui, fake) = setup("contacts").await;

    app::set_test_focus(false);

    let id = push_and_wait(&mut dom, &ui, &fake, alice(), "pingalpha").await;
    let posted = notifier::take_posted();

    assert_eq!(posted.len(), 1, "{posted:?}");
    assert_eq!(posted[0].id, id);
    assert_eq!(posted[0].count, 1);
    assert_eq!(posted[0].body, "pingalpha");
}

#[tokio::test]
async fn no_notification_while_focused() {
    let (mut dom, ui, fake) = setup("contacts").await;

    push_and_wait(&mut dom, &ui, &fake, alice(), "pingbravo").await;

    assert_eq!(notifier::take_posted(), Vec::<Post>::new());
}

#[tokio::test]
async fn muted_from_the_row_menu_stays_silent() {
    let (mut dom, ui, fake) = setup("contacts").await;
    let node = device(&ui).node_id;

    open_row_menu(&mut dom, "Alice").await;
    click(&dom, &button(&dom, "Mute", false));
    settle(&mut dom).await;

    assert_eq!(mode_of(&ui, &node, &alice()), NotifyMode::Mute);
    assert!(
        els(&dom).iter().any(|e| e.has_class("muted-icon")),
        "the muted row shows the bell-off icon"
    );

    app::set_test_focus(false);
    push_and_wait(&mut dom, &ui, &fake, alice(), "pingcharlie").await;

    assert_eq!(notifier::take_posted(), Vec::<Post>::new());
}

#[tokio::test]
async fn archive_from_the_row_menu_hides_the_dm_until_it_hears_again() {
    let (mut dom, ui, fake) = setup("contacts").await;

    open_row_menu(&mut dom, "Alice").await;
    click(&dom, &button(&dom, "Archive", false));
    settle(&mut dom).await;

    assert!(
        !els(&dom)
            .iter()
            .any(|e| e.has_class("row") && e.text.starts_with("Alice")),
        "the archived row is gone"
    );

    push_and_wait(&mut dom, &ui, &fake, alice(), "pingecho").await;

    row_titled(&dom, "Alice");
}

#[tokio::test]
async fn unarchive_from_the_archived_group_lists_the_dm_again() {
    let (mut dom, _ui, _fake) = setup("contacts").await;

    open_row_menu(&mut dom, "Alice").await;
    click(&dom, &button(&dom, "Archive", false));
    settle(&mut dom).await;
    click(&dom, &button(&dom, "Show archived (1)", false));
    settle(&mut dom).await;
    open_row_menu(&mut dom, "Alice").await;
    click(&dom, &button(&dom, "Unarchive", false));
    settle(&mut dom).await;

    row_titled(&dom, "Alice");
    assert!(
        !els(&dom)
            .iter()
            .any(|e| e.text.starts_with("Hide archived")),
        "the archived group is gone once it is empty"
    );
}

#[tokio::test]
async fn row_menu_offers_no_archive_for_a_channel() {
    let (mut dom, ui, _fake) = setup("contacts").await;
    let node = device(&ui).node_id;
    let title = ui
        .store
        .threads(&node)
        .unwrap()
        .into_iter()
        .find(|t| t.thread == ThreadId::Channel(0))
        .unwrap()
        .title;

    open_row_menu(&mut dom, &title).await;
    button(&dom, "Mark as read", false);

    assert!(
        !els(&dom).iter().any(|e| e.text == "Archive"),
        "a channel row menu has no Archive"
    );
}

#[tokio::test]
async fn row_menu_marks_the_current_mode() {
    let (mut dom, ui, _fake) = setup("contacts").await;
    let node = device(&ui).node_id;
    let title = ui
        .store
        .threads(&node)
        .unwrap()
        .into_iter()
        .find(|t| t.thread == ThreadId::Channel(0))
        .unwrap()
        .title;

    open_row_menu(&mut dom, &title).await;

    assert!(button(&dom, "✓ Mentions only", false).has_class("menu-check"));
    assert_eq!(button(&dom, "All messages", false).attr("class"), None);

    click(&dom, &button(&dom, "All messages", false));
    settle(&mut dom).await;
    open_row_menu(&mut dom, &title).await;

    assert!(button(&dom, "✓ All messages", false).has_class("menu-check"));
    assert_eq!(button(&dom, "Mentions only", false).attr("class"), None);
}

#[tokio::test]
async fn posts_even_when_the_ui_node_lags() {
    let (mut dom, ui, fake) = setup("contacts").await;
    let node = device(&ui).node_id;

    show_other_node(&dom, &ui);
    app::set_test_focus(false);

    push_and_wait(&mut dom, &ui, &fake, alice(), "pingdelta").await;

    let posted = notifier::take_posted();

    assert_eq!(posted.len(), 1, "{posted:?}");
    assert_eq!(posted[0].node, node);
}

#[tokio::test]
async fn notification_click_opens_the_rows_thread() {
    let (mut dom, ui, _fake) = setup("contacts").await;
    let node = device(&ui).node_id;
    let id = ui
        .store
        .insert_message(&node, &direct_from_alice("clickme"))
        .unwrap()
        .unwrap();

    in_app(&dom, || ui.open_notification(&node, id));
    settle(&mut dom).await;

    assert_eq!(*ui.view.peek(), View::Thread(alice()));
}

#[tokio::test]
async fn notification_click_for_another_radio_is_ignored() {
    let (mut dom, ui, _fake) = setup("contacts").await;
    let id = ui
        .store
        .insert_message(&other_node(), &direct_from_alice("elsewhere"))
        .unwrap()
        .unwrap();

    show_other_node(&dom, &ui);
    in_app(&dom, || ui.open_notification(&other_node(), id));
    settle(&mut dom).await;

    assert_eq!(*ui.view.peek(), View::Contacts);
}

#[tokio::test]
async fn notification_click_waits_for_the_ui_node() {
    let (mut dom, ui, _fake) = setup("contacts").await;
    let node = device(&ui).node_id;
    let id = ui
        .store
        .insert_message(&node, &direct_from_alice("not yet"))
        .unwrap()
        .unwrap();

    show_other_node(&dom, &ui);
    in_app(&dom, || ui.open_notification(&node, id));
    settle(&mut dom).await;

    assert_eq!(*ui.view.peek(), View::Contacts);
}

#[tokio::test]
async fn notification_click_for_a_replaced_channel_is_ignored() {
    let (mut dom, ui, _fake) = setup("contacts").await;
    let node = device(&ui).node_id;
    let id = ui
        .store
        .insert_message(&node, &incoming("old channel text"))
        .unwrap()
        .unwrap();
    let mut channels = ui.store.channels(&node).unwrap();

    channels[0].secret = [0xEE; 16];
    ui.store.replace_channels(&node, &channels).unwrap();

    in_app(&dom, || ui.open_notification(&node, id));
    settle(&mut dom).await;

    assert_eq!(*ui.view.peek(), View::Contacts);
}
