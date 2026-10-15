//! Read-only hardware check: connect, read the device report, channel
//! names, the contact count, and the radio's own contact card, print one
//! JSON object, and disconnect.
//!
//! Usage: `probe [--ble <name-or-mac> | --serial <port> [--baud <n>] |
//! --tcp <host:port>]`. With no arguments it uses the saved connection
//! settings. Channel secrets are never printed.

use hedgerow_core::config::Config;
use hedgerow_core::radio::{ConnectTarget, Radio};
use hedgerow_core::{Error, Result};
use serde_json::{Value, json};

/// Channel slots to read when the device does not report a count.
const DEFAULT_MAX_CHANNELS: u8 = 8;

/// Parses the command line into a connect target, falling back to the
/// saved settings when no transport flag is given.
fn target_from_args(args: &[String]) -> Result<ConnectTarget> {
    let mut target = None;
    let mut baud = None;
    let mut it = args.iter();

    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| Error::Config(format!("{flag} needs a value")))
        };

        match flag.as_str() {
            "--ble" => target = Some(ConnectTarget::Ble(value()?)),
            "--serial" => {
                target = Some(ConnectTarget::Serial {
                    port: value()?,
                    baud: 115_200,
                });
            }
            "--baud" => {
                baud = Some(
                    value()?
                        .parse::<u32>()
                        .map_err(|e| Error::Config(format!("--baud: {e}")))?,
                );
            }
            "--tcp" => {
                let addr = value()?;
                let (host, port) = addr
                    .rsplit_once(':')
                    .ok_or_else(|| Error::Config("--tcp needs host:port".into()))?;
                let port = port
                    .parse::<u16>()
                    .map_err(|e| Error::Config(format!("--tcp port: {e}")))?;

                target = Some(ConnectTarget::Tcp {
                    host: host.to_string(),
                    port,
                });
            }
            other => return Err(Error::Config(format!("unknown argument {other}"))),
        }
    }

    let mut target = match target {
        Some(target) => target,
        None => ConnectTarget::from_config(&Config::load()?.connection)?,
    };

    if let (ConnectTarget::Serial { baud: b, .. }, Some(baud)) = (&mut target, baud) {
        *b = baud;
    }

    Ok(target)
}

/// Runs the read-only queries against a connected radio.
async fn read_radio(radio: &Radio) -> Result<Value> {
    let device = radio.start().await?;
    let max = device.max_channels.unwrap_or(DEFAULT_MAX_CHANNELS);
    let channels = radio.channels(max).await?;
    let contacts = radio.contacts(0).await?;
    let self_uri = radio.export_contact(None).await?;
    let channels: Vec<Value> = channels
        .iter()
        .map(|c| json!({ "idx": c.idx, "name": c.name }))
        .collect();

    Ok(json!({
        "device": device,
        "channels": channels,
        "contact_count": contacts.len(),
        "self_uri": self_uri,
    }))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let target = target_from_args(&args)?;
    let radio = Radio::connect(&target).await?;
    let report = read_radio(&radio).await;

    radio.disconnect().await;

    let report = report?;

    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|e| Error::Config(e.to_string()))?
    );

    Ok(())
}
