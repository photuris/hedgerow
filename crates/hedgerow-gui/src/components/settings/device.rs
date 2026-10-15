//! The Device tab: what the radio reports, and the form that changes it.

use dioxus::prelude::*;
use hedgerow_core::Error;
use hedgerow_core::model::DeviceInfo;
use hedgerow_core::radio::DeviceSettings;
use hedgerow_core::session::{Command, ConnState, Guard};

use crate::app::Ui;
use crate::components::confirm::{Confirm, Pending, use_pending};
use crate::format;

/// The bandwidths the radio supports, in kHz x 1000.
const BANDWIDTHS: [u32; 10] = [
    7_800, 10_400, 15_600, 20_800, 31_250, 41_700, 62_500, 125_000, 250_000, 500_000,
];

/// The advert location policies and their labels.
const ADVERT_LOCATIONS: [(u8, &str); 3] =
    [(0, "Don't share"), (1, "Share"), (2, "Share (preferences)")];

/// The reminder shown when a change affects how radios hear each other.
const RADIO_WARNING: &str = "Radios only hear each other with identical frequency, bandwidth, spreading factor, and coding rate.";

/// Formats a value in thousandths without trailing zeros: 910525 becomes
/// `910.525` (MHz), 62500 becomes `62.5` (kHz), 915000 becomes `915`.
fn freq_to_mhz(freq: u32) -> String {
    let (whole, frac) = (freq / 1000, freq % 1000);

    if frac == 0 {
        return whole.to_string();
    }

    format!("{whole}.{}", format!("{frac:03}").trim_end_matches('0'))
}

/// Parses megahertz with up to three decimals into thousandths:
/// `910.525` becomes 910525 and `915` becomes 915000.
fn freq_from_mhz(s: &str) -> Result<u32, String> {
    let s = s.trim();
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    let digits = |part: &str| part.bytes().all(|b| b.is_ascii_digit());

    if whole.is_empty() || !digits(whole) || !digits(frac) || (s.contains('.') && frac.is_empty()) {
        return Err("Frequency must be a number in MHz, like 910.525".to_string());
    }

    if frac.len() > 3 {
        return Err("Frequency has at most 3 decimals".to_string());
    }

    let thousandths = format!("{frac:0<3}")
        .parse::<u32>()
        .map_err(|e| e.to_string())?;

    whole
        .parse::<u32>()
        .ok()
        .and_then(|w| w.checked_mul(1000))
        .and_then(|w| w.checked_add(thousandths))
        .ok_or_else(|| "Frequency is too large".to_string())
}

/// The label of an advert location policy.
fn advert_location_label(policy: u8) -> String {
    ADVERT_LOCATIONS
        .iter()
        .find(|(value, _)| *value == policy)
        .map_or_else(
            || format!("Unknown ({policy})"),
            |(_, label)| (*label).to_string(),
        )
}

/// `on` or `off` for a flag.
fn on_off(flag: bool) -> String {
    if flag { "on" } else { "off" }.to_string()
}

/// One `<Field>: <old> → <new>` line per field that differs, in human units.
fn settings_diff(current: &DeviceSettings, desired: &DeviceSettings) -> Vec<String> {
    let mut lines = Vec::new();
    let mut compare = |field: &str, old: String, new: String| {
        if old != new {
            lines.push(format!("{field}: {old} → {new}"));
        }
    };

    compare(
        "Name",
        current.name.trim().to_string(),
        desired.name.trim().to_string(),
    );
    compare(
        "TX power",
        format!("{} dBm", current.tx_power),
        format!("{} dBm", desired.tx_power),
    );
    compare(
        "Frequency",
        format!("{} MHz", freq_to_mhz(current.radio_freq)),
        format!("{} MHz", freq_to_mhz(desired.radio_freq)),
    );
    compare(
        "Bandwidth",
        format!("{} kHz", freq_to_mhz(current.radio_bw)),
        format!("{} kHz", freq_to_mhz(desired.radio_bw)),
    );
    compare(
        "Spreading factor",
        current.sf.to_string(),
        desired.sf.to_string(),
    );
    compare(
        "Coding rate",
        format!("4/{}", current.cr),
        format!("4/{}", desired.cr),
    );
    compare(
        "Add contacts manually",
        on_off(current.manual_add_contacts),
        on_off(desired.manual_add_contacts),
    );
    compare(
        "Advert location",
        advert_location_label(current.adv_loc_policy),
        advert_location_label(desired.adv_loc_policy),
    );
    compare(
        "Multiple acks",
        on_off(current.multi_acks != 0),
        on_off(desired.multi_acks != 0),
    );

    lines
}

/// Whether `desired` changes a parameter the other radios must match.
fn radio_changed(current: &DeviceSettings, desired: &DeviceSettings) -> bool {
    (current.radio_freq, current.radio_bw, current.sf, current.cr)
        != (desired.radio_freq, desired.radio_bw, desired.sf, desired.cr)
}

/// What a device report says about its settings, with the name trimmed the
/// way [`settings_diff`] and the core compare it.
fn settings_of(info: &DeviceInfo) -> DeviceSettings {
    let mut settings = DeviceSettings::from_device(info);

    settings.name = settings.name.trim().to_string();

    settings
}

/// What the Apply dialog will send, and the device settings its lines were
/// computed from.
#[derive(Clone, Debug, PartialEq)]
struct ApplyPlan {
    /// The settings to send.
    wanted: DeviceSettings,
    /// The device settings `lines` is the diff against.
    from: DeviceSettings,
    /// The device report `from` was taken from, untouched, for the
    /// session's check that the radio still has these settings.
    snapshot: DeviceSettings,
    /// The confirmation text.
    lines: Vec<String>,
}

/// The settings the form asks for: `form` with the numbers as typed and the
/// name trimmed.
fn form_settings(form: &DeviceSettings, power: &str, freq: &str) -> Result<DeviceSettings, String> {
    let tx_power = power
        .trim()
        .parse()
        .map_err(|_| "TX power must be a whole number of dBm".to_string())?;

    Ok(DeviceSettings {
        tx_power,
        radio_freq: freq_from_mhz(freq)?,
        name: form.name.trim().to_string(),
        ..form.clone()
    })
}

/// The message of a validation error, without the error class.
fn problem_text(error: Error) -> String {
    match error {
        Error::Invalid(message) => message,
        other => other.to_string(),
    }
}

/// The device report to show: the live one when connected, else the last
/// stored one for the node.
pub(super) fn current_device(ui: &Ui) -> Option<DeviceInfo> {
    if let ConnState::Connected(device) = (ui.state)() {
        return Some(device);
    }

    let _ = (ui.revision)();
    let node = (ui.node)()?;

    ui.store.device(&node).unwrap_or_else(|e| {
        tracing::warn!("read device: {e}");
        None
    })
}

/// The device report and its settings form.
#[component]
pub fn Device() -> Element {
    let ui = use_context::<Ui>();
    let info = use_memo({
        let ui = ui.clone();

        move || current_device(&ui)
    });

    match info() {
        // A keyed one-item list, because a key alone does not remount a lone
        // component: the form and its dialog belong to one radio.
        Some(info) => rsx! {
            for info in [info] {
                DeviceView { key: "{info.node_id}", info }
            }
        },
        None => rsx! { div { class: "hint", "Connect to a radio to see its settings" } },
    }
}

/// One label and value line of the read-only block.
#[component]
fn Fact(label: String, value: String, children: Element) -> Element {
    rsx! {
        div { class: "fact",
            span { class: "fact-label", "{label}" }
            span { class: "fact-value", "{value}" }
            {children}
        }
    }
}

/// The read-only report and the form for one device snapshot.
#[component]
fn DeviceView(info: DeviceInfo) -> Element {
    let ui = use_context::<Ui>();
    let start = settings_of(&info);
    let mut form = use_signal(|| start.clone());
    let mut power = use_signal(|| start.tx_power.to_string());
    let mut freq = use_signal(|| freq_to_mhz(start.radio_freq));
    let mut base = use_signal(|| start);
    let mut pending = use_pending::<ApplyPlan>();

    // A new report of this node replaces the form, unless the user has
    // unapplied edits. It also closes an open dialog: its lines describe a
    // diff against settings the radio no longer has.
    use_effect(use_reactive!(|info| {
        let latest = settings_of(&info);

        if *base.peek() == latest {
            return;
        }

        pending.set(None);

        let clean =
            form_settings(&form.peek(), &power.peek(), &freq.peek()) == Ok(base.peek().clone());

        base.set(latest.clone());

        if clean {
            power.set(latest.tx_power.to_string());
            freq.set(freq_to_mhz(latest.radio_freq));
            form.set(latest);
        }
    }));

    let connected = matches!((ui.state)(), ConnState::Connected(_));
    let title = if connected {
        None
    } else {
        Some("Not connected")
    };
    let current = settings_of(&info);
    let wanted = form_settings(&form(), &power(), &freq());
    let problem = match &wanted {
        Err(e) => Some(e.clone()),
        Ok(w) => w.validate(&info).err().map(problem_text),
    };
    let dirty = wanted.as_ref().map_or(true, |w| *w != current);
    let can_apply = connected && dirty && problem.is_none();
    let settings = form();
    let mut bandwidths = BANDWIDTHS.to_vec();

    if !bandwidths.contains(&settings.radio_bw) {
        bandwidths.push(settings.radio_bw);
    }

    let battery = info.battery_mv.map_or("Unknown".to_string(), |mv| {
        format!("{mv} mV · {}%", format::battery_percent(mv))
    });
    let max_contacts = match info.max_contacts {
        Some(255) => "≥255".to_string(),
        Some(n) => n.to_string(),
        None => "Unknown".to_string(),
    };
    let unknown = |v: &Option<String>| v.clone().unwrap_or_else(|| "Unknown".to_string());
    let firmware = match (&info.firmware, &info.fw_build) {
        (Some(version), Some(build)) => format!("{version} ({build})"),
        (version, _) => unknown(version),
    };
    let command = |command: Command| {
        let (ui, node) = (ui.clone(), info.node_id.clone());

        move |_| ui.send_guarded(&node, Guard::Node, command.clone())
    };
    let apply = {
        let (ui, shown) = (ui.clone(), info.node_id.clone());

        // Everything is read now, not at render: the dialog's lines must
        // describe the radio as it is when Apply is clicked.
        move |_| {
            let ConnState::Connected(device) = ui.state.peek().clone() else {
                return;
            };
            let Ok(wanted) = form_settings(&form.peek(), &power.peek(), &freq.peek()) else {
                return;
            };
            let from = settings_of(&device);
            let mut lines = settings_diff(&from, &wanted);

            if radio_changed(&from, &wanted) {
                lines.push(RADIO_WARNING.to_string());
            }

            pending.set(Pending::open(
                &ui,
                &shown,
                ApplyPlan {
                    wanted,
                    from,
                    snapshot: DeviceSettings::from_device(&device),
                    lines,
                },
            ));
        }
    };

    rsx! {
        div { class: "facts",
            Fact { label: "Name", value: info.name.clone() }
            Fact { label: "Model", value: unknown(&info.model) }
            Fact { label: "Firmware", value: firmware }
            Fact { label: "Public key", value: info.node_id.clone(),
                button {
                    class: "link",
                    onclick: {
                        let (ui, key) = (ui.clone(), info.node_id.clone());

                        move |_| ui.copy(&key)
                    },
                    "Copy"
                }
            }
            Fact { label: "Battery", value: battery }
            Fact { label: "Max contacts", value: max_contacts }
            Fact { label: "Max channels", value: info.max_channels.map_or("Unknown".to_string(), |n| n.to_string()) }
        }
        h3 { "Settings" }
        div { class: "setting-row",
            label { r#for: "dev-name", "Name" }
            input {
                id: "dev-name",
                r#type: "text",
                value: "{settings.name}",
                oninput: move |evt| form.write().name = evt.value(),
            }
        }
        div { class: "setting-row",
            label { r#for: "dev-power", "TX power" }
            input {
                id: "dev-power",
                r#type: "number",
                class: "number",
                min: "1",
                max: "{info.max_tx_power}",
                value: "{power}",
                oninput: move |evt| power.set(evt.value()),
            }
            span { class: "unit", "dBm" }
        }
        div { class: "setting-row",
            label { r#for: "dev-freq", "Frequency" }
            input {
                id: "dev-freq",
                r#type: "text",
                class: "number",
                value: "{freq}",
                oninput: move |evt| freq.set(evt.value()),
            }
            span { class: "unit", "MHz" }
        }
        div { class: "setting-row",
            label { r#for: "dev-bw", "Bandwidth" }
            select {
                id: "dev-bw",
                value: "{settings.radio_bw}",
                onchange: move |evt| {
                    if let Ok(bw) = evt.value().parse() {
                        form.write().radio_bw = bw;
                    }
                },
                for bw in bandwidths {
                    option { key: "{bw}", value: "{bw}", selected: bw == settings.radio_bw, "{freq_to_mhz(bw)} kHz" }
                }
            }
        }
        div { class: "setting-row",
            label { r#for: "dev-sf", "Spreading factor" }
            select {
                id: "dev-sf",
                value: "{settings.sf}",
                onchange: move |evt| {
                    if let Ok(sf) = evt.value().parse() {
                        form.write().sf = sf;
                    }
                },
                for sf in 5u8..=12 {
                    option { key: "{sf}", value: "{sf}", selected: sf == settings.sf, "{sf}" }
                }
            }
        }
        div { class: "setting-row",
            label { r#for: "dev-cr", "Coding rate" }
            select {
                id: "dev-cr",
                value: "{settings.cr}",
                onchange: move |evt| {
                    if let Ok(cr) = evt.value().parse() {
                        form.write().cr = cr;
                    }
                },
                for cr in 5u8..=8 {
                    option { key: "{cr}", value: "{cr}", selected: cr == settings.cr, "4/{cr}" }
                }
            }
        }
        div { class: "setting-row",
            label { class: "check",
                input {
                    r#type: "checkbox",
                    checked: settings.manual_add_contacts,
                    onchange: move |evt| form.write().manual_add_contacts = evt.checked(),
                }
                "Add contacts manually"
            }
        }
        div { class: "setting-row",
            label { r#for: "dev-loc", "Advert location" }
            select {
                id: "dev-loc",
                value: "{settings.adv_loc_policy}",
                onchange: move |evt| {
                    if let Ok(policy) = evt.value().parse() {
                        form.write().adv_loc_policy = policy;
                    }
                },
                for (value , label) in ADVERT_LOCATIONS {
                    option { key: "{value}", value: "{value}", selected: value == settings.adv_loc_policy, "{label}" }
                }
            }
        }
        div { class: "setting-row",
            label { class: "check",
                input {
                    r#type: "checkbox",
                    checked: settings.multi_acks != 0,
                    onchange: move |evt| form.write().multi_acks = u8::from(evt.checked()),
                }
                "Multiple acks"
            }
        }
        div { class: "setting-actions",
            button { class: "primary", disabled: !can_apply, title, onclick: apply, "Apply" }
            button {
                disabled: !dirty,
                onclick: {
                    let current = current.clone();

                    move |_| {
                        power.set(current.tx_power.to_string());
                        freq.set(freq_to_mhz(current.radio_freq));
                        form.set(current.clone());
                    }
                },
                "Revert"
            }
        }
        if let Some(problem) = problem {
            div { class: "error", "{problem}" }
        }
        h3 { "Radio" }
        div { class: "setting-actions",
            button { disabled: !connected, title, onclick: command(Command::SendAdvert { flood: false }), "Send advert" }
            button { disabled: !connected, title, onclick: command(Command::SendAdvert { flood: true }), "Send flood advert" }
            button { disabled: !connected, title, onclick: command(Command::SyncClock), "Sync clock" }
            button { disabled: !connected, title, onclick: command(Command::Refresh), "Refresh" }
        }
        if let Some(plan) = pending() {
            Confirm {
                title: "Apply settings",
                lines: plan.action.lines.clone(),
                confirm_label: "Apply",
                needs_connection: true,
                onconfirm: {
                    let ui = ui.clone();

                    move |_| {
                        pending.set(None);

                        // Send only what the lines described: same radio, same
                        // settings as when they were computed, still valid.
                        let Some(device) = plan.device(&ui.state.peek()) else {
                            ui.show_cancelled();

                            return;
                        };

                        if settings_of(&device) != plan.action.from
                            || plan.action.wanted.validate(&device).is_err()
                        {
                            ui.show_cancelled();

                            return;
                        }

                        plan.send(
                            &ui,
                            Guard::Device(plan.action.snapshot.clone()),
                            Command::ApplySettings(plan.action.wanted.clone()),
                        );
                    }
                },
                oncancel: move |_| pending.set(None),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plausible device setup.
    fn settings() -> DeviceSettings {
        DeviceSettings {
            name: "Demo Node".to_string(),
            tx_power: 22,
            radio_freq: 910_525,
            radio_bw: 62_500,
            sf: 7,
            cr: 5,
            manual_add_contacts: false,
            adv_loc_policy: 0,
            multi_acks: 0,
        }
    }

    mod freq_to_mhz {
        use super::*;

        #[test]
        fn should_put_the_decimal_point_after_the_megahertz() {
            assert_eq!(freq_to_mhz(910_525), "910.525");
        }

        #[test]
        fn should_keep_leading_zeros_of_the_fraction() {
            assert_eq!(freq_to_mhz(433_005), "433.005");
        }

        #[test]
        fn should_drop_a_zero_fraction_and_trailing_zeros() {
            assert_eq!(freq_to_mhz(915_000), "915");
            assert_eq!(freq_to_mhz(62_500), "62.5");
        }
    }

    mod freq_from_mhz {
        use super::*;

        #[test]
        fn should_read_three_decimals() {
            assert_eq!(freq_from_mhz("910.525"), Ok(910_525));
        }

        #[test]
        fn should_read_fewer_decimals_and_whole_numbers() {
            assert_eq!(freq_from_mhz("915"), Ok(915_000));
            assert_eq!(freq_from_mhz(" 868.1 "), Ok(868_100));
            assert_eq!(freq_from_mhz("433.05"), Ok(433_050));
        }

        #[test]
        fn should_reject_more_than_three_decimals() {
            assert!(freq_from_mhz("910.5251").is_err());
        }

        #[test]
        fn should_reject_text_that_is_not_a_plain_decimal() {
            for text in [
                "", ".", "abc", "9x0", "-915", "1e3", "915.", ".5", "1.2.3", "99999999",
            ] {
                assert!(freq_from_mhz(text).is_err(), "{text:?}");
            }
        }
    }

    mod settings_diff {
        use super::*;

        #[test]
        fn should_be_empty_when_nothing_changed() {
            assert!(settings_diff(&settings(), &settings()).is_empty());
        }

        #[test]
        fn should_name_the_field_and_both_values_in_human_units() {
            let desired = DeviceSettings {
                tx_power: 20,
                radio_freq: 915_000,
                radio_bw: 125_000,
                ..settings()
            };

            assert_eq!(
                settings_diff(&settings(), &desired),
                [
                    "TX power: 22 dBm → 20 dBm",
                    "Frequency: 910.525 MHz → 915 MHz",
                    "Bandwidth: 62.5 kHz → 125 kHz",
                ]
            );
        }

        #[test]
        fn should_describe_flags_coding_rate_and_location() {
            let desired = DeviceSettings {
                cr: 6,
                manual_add_contacts: true,
                adv_loc_policy: 2,
                multi_acks: 1,
                ..settings()
            };

            assert_eq!(
                settings_diff(&settings(), &desired),
                [
                    "Coding rate: 4/5 → 4/6",
                    "Add contacts manually: off → on",
                    "Advert location: Don't share → Share (preferences)",
                    "Multiple acks: off → on",
                ]
            );
        }

        #[test]
        fn should_ignore_whitespace_around_the_name() {
            let desired = DeviceSettings {
                name: " Demo Node ".to_string(),
                ..settings()
            };

            assert!(settings_diff(&settings(), &desired).is_empty());
        }
    }

    mod radio_changed {
        use super::*;

        #[test]
        fn should_be_true_for_each_radio_parameter() {
            for desired in [
                DeviceSettings {
                    radio_freq: 915_000,
                    ..settings()
                },
                DeviceSettings {
                    radio_bw: 125_000,
                    ..settings()
                },
                DeviceSettings {
                    sf: 8,
                    ..settings()
                },
                DeviceSettings {
                    cr: 6,
                    ..settings()
                },
            ] {
                assert!(radio_changed(&settings(), &desired), "{desired:?}");
            }
        }

        #[test]
        fn should_be_false_for_name_power_and_flags() {
            let desired = DeviceSettings {
                name: "Other".to_string(),
                tx_power: 10,
                manual_add_contacts: true,
                adv_loc_policy: 1,
                multi_acks: 1,
                ..settings()
            };

            assert!(!radio_changed(&settings(), &desired));
        }
    }

    mod form_settings {
        use super::*;

        #[test]
        fn should_apply_the_typed_numbers() {
            let wanted = form_settings(&settings(), " 17 ", "915.5").unwrap();

            assert_eq!((wanted.tx_power, wanted.radio_freq), (17, 915_500));
        }

        #[test]
        fn should_report_a_power_that_is_not_a_number() {
            assert!(form_settings(&settings(), "", "915").is_err());
            assert!(form_settings(&settings(), "high", "915").is_err());
        }
    }
}
