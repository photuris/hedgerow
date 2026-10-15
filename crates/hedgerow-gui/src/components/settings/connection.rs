//! The Connection tab: transport, target, and the connect buttons.

use dioxus::prelude::*;
use hedgerow_core::config::Transport;
use hedgerow_core::radio::Radio;
use hedgerow_core::session::{Command, ConnState};

use crate::app::Ui;

/// Baud rates offered for a serial port.
const BAUDS: [u32; 8] = [
    9600, 19200, 38400, 57600, 115_200, 230_400, 460_800, 921_600,
];

/// How long a Bluetooth scan listens, in seconds.
const SCAN_SECS: u64 = 5;

/// The tooltip of a scan button in UI-test mode.
const UI_TEST_TITLE: &str = "Unavailable in UI test mode";

/// The one-line connection status.
fn status_line(state: &ConnState) -> String {
    match state {
        ConnState::Connected(device) => format!("Connected to {}", device.name),
        ConnState::Connecting => "Connecting…".to_string(),
        ConnState::Failed(reason) => format!("Failed: {reason}"),
        ConnState::Disconnected => "Disconnected".to_string(),
    }
}

/// Transport, target, and auto-connect, saved with one button.
#[component]
pub fn Connection() -> Element {
    let ui = use_context::<Ui>();
    let initial = (ui.config).peek().connection.clone();
    let mut draft = use_signal(|| initial.clone());
    let mut port_text = use_signal(|| initial.tcp_port.to_string());
    let mut found = use_signal(Vec::<String>::new);
    let mut scanning = use_signal(|| false);
    let mut ports = use_signal(|| {
        // Listing ports opens none, but UI-test mode never looks at hardware.
        if ui.ui_test {
            Vec::new()
        } else {
            Radio::serial_ports()
        }
    });
    let mut error = use_signal(|| None::<String>);
    let state = (ui.state)();
    let cfg = draft();
    let scan_title = ui.ui_test.then_some(UI_TEST_TITLE);
    let can_disconnect = matches!(state, ConnState::Connected(_) | ConnState::Connecting);
    let scan = {
        let ui = ui.clone();

        move |_| {
            scanning.set(true);

            let ui = ui.clone();

            spawn(async move {
                // Off the UI thread: the scan holds the Bluetooth adapter for seconds.
                let result = tokio::spawn(Radio::scan_ble(SCAN_SECS)).await;

                match result {
                    Ok(Ok(names)) => found.set(names),
                    Ok(Err(e)) => ui.show_toast(format!("Scan failed: {e}")),
                    Err(e) => ui.show_toast(format!("Scan failed: {e}")),
                }

                scanning.set(false);
            });
        }
    };
    let save = {
        let ui = ui.clone();

        move |_| {
            let mut connection = draft();

            // The port field is only on screen for TCP; for the other
            // transports the last valid port stays in the config.
            if connection.transport == Transport::Tcp {
                let Ok(port) = port_text().trim().parse::<u16>() else {
                    error.set(Some("The TCP port must be a number from 0 to 65535".into()));

                    return;
                };

                connection.tcp_port = port;
            }

            let mut config = (ui.config).peek().clone();
            let mut stored = ui.config;

            config.connection = connection.clone();

            // UI-test mode never writes the user's files.
            if !ui.ui_test
                && let Err(e) = config.save()
            {
                error.set(Some(format!("Could not save the settings: {e}")));

                return;
            }

            error.set(None);
            stored.set(config);
            ui.session.send(Command::Reconfigure(connection));
        }
    };

    rsx! {
        div { class: "setting-row",
            span { class: "label", "Transport" }
            for (transport , label) in [
                (Transport::Ble, "Bluetooth"),
                (Transport::Serial, "USB serial"),
                (Transport::Tcp, "TCP"),
            ] {
                label { key: "{label}", class: "check",
                    input {
                        r#type: "radio",
                        name: "transport",
                        checked: cfg.transport == transport,
                        onchange: move |_| draft.write().transport = transport,
                    }
                    "{label}"
                }
            }
        }
        match cfg.transport {
            Transport::Ble => rsx! {
                div { class: "setting-row",
                    label { r#for: "ble-device", "Device name or address" }
                    input {
                        id: "ble-device",
                        r#type: "text",
                        value: "{cfg.ble_device}",
                        oninput: move |evt| draft.write().ble_device = evt.value(),
                    }
                    button { disabled: ui.ui_test || scanning(), title: scan_title, onclick: scan, "Scan" }
                }
                if scanning() {
                    div { class: "hint", "Scanning…" }
                }
                div { class: "found",
                    for name in found() {
                        button {
                            key: "{name}",
                            class: "link",
                            onclick: {
                                let name = name.clone();

                                move |_| draft.write().ble_device = name.clone()
                            },
                            "{name}"
                        }
                    }
                }
            },
            Transport::Serial => rsx! {
                div { class: "setting-row",
                    label { r#for: "serial-port", "Port" }
                    select {
                        id: "serial-port",
                        value: "{cfg.serial_port}",
                        onchange: move |evt| draft.write().serial_port = evt.value(),
                        option { value: "", "Choose a port…" }
                        for port in ports() {
                            option { key: "{port}", value: "{port}", selected: port == cfg.serial_port, "{port}" }
                        }
                    }
                    button {
                        disabled: ui.ui_test,
                        title: scan_title,
                        onclick: move |_| ports.set(Radio::serial_ports()),
                        "Rescan"
                    }
                }
                div { class: "setting-row",
                    label { r#for: "serial-path", "Or a path" }
                    input {
                        id: "serial-path",
                        r#type: "text",
                        placeholder: "/dev/ttyACM0",
                        value: "{cfg.serial_port}",
                        oninput: move |evt| draft.write().serial_port = evt.value(),
                    }
                }
                div { class: "setting-row",
                    label { r#for: "baud", "Baud rate" }
                    select {
                        id: "baud",
                        value: "{cfg.baud}",
                        onchange: move |evt| {
                            if let Ok(baud) = evt.value().parse() {
                                draft.write().baud = baud;
                            }
                        },
                        for baud in BAUDS {
                            option { key: "{baud}", value: "{baud}", selected: baud == cfg.baud, "{baud}" }
                        }
                    }
                }
            },
            Transport::Tcp => rsx! {
                div { class: "setting-row",
                    label { r#for: "tcp-host", "Host" }
                    input {
                        id: "tcp-host",
                        r#type: "text",
                        value: "{cfg.tcp_host}",
                        oninput: move |evt| draft.write().tcp_host = evt.value(),
                    }
                }
                div { class: "setting-row",
                    label { r#for: "tcp-port", "Port" }
                    input {
                        id: "tcp-port",
                        r#type: "text",
                        class: "number",
                        value: "{port_text}",
                        oninput: move |evt| port_text.set(evt.value()),
                    }
                }
            },
        }
        div { class: "setting-row",
            label { class: "check",
                input {
                    r#type: "checkbox",
                    checked: cfg.auto_connect,
                    onchange: move |evt| draft.write().auto_connect = evt.checked(),
                }
                "Connect automatically on start"
            }
        }
        div { class: "setting-actions",
            button { class: "primary", onclick: save, "Save and connect" }
            button {
                disabled: !can_disconnect,
                onclick: {
                    let session = ui.session.clone();

                    move |_| session.send(Command::Disconnect)
                },
                "Disconnect"
            }
        }
        if let Some(e) = error() {
            div { class: "error", "{e}" }
        }
        div { class: "status-line", "{status_line(&state)}" }
    }
}
