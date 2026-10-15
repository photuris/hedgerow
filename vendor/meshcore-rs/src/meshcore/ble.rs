use crate::events::EventPayload;
use crate::{Error, EventType, MeshCore, MeshCoreEvent};
use btleplug::api::{
    Central, CentralEvent, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Manager, Peripheral};
use futures::stream::StreamExt;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::mpsc::Receiver;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::events::EventDispatcher;

// MeshCore BLE service and characteristic UUIDs
// These are the standard UUIDs used by MeshCore devices
const MESHCORE_SERVICE_UUID: Uuid = Uuid::from_u128(0x6e400001_b5a3_f393_e0a9_e50e24dcca9e);
const MESHCORE_TX_CHAR_UUID: Uuid = Uuid::from_u128(0x6e400002_b5a3_f393_e0a9_e50e24dcca9e);
const MESHCORE_RX_CHAR_UUID: Uuid = Uuid::from_u128(0x6e400003_b5a3_f393_e0a9_e50e24dcca9e);

/// Marks the client disconnected and emits `Disconnected`, but only for the
/// caller that flips `connected` from true to false under the write lock. The
/// write task and the read task both call this for one lost link; the loser
/// emits nothing.
async fn lose_link(connected: &RwLock<bool>, dispatcher: &EventDispatcher) {
    let won = std::mem::replace(&mut *connected.write().await, false);

    if won {
        dispatcher
            .emit(MeshCoreEvent::new(
                EventType::Disconnected,
                EventPayload::None,
            ))
            .await;
    }
}

impl MeshCore {
    /// Find MeshCore radios on BTLE upto the time specified by `scan_duration` and return their names
    pub async fn ble_discover(scan_duration: Duration) -> crate::Result<Vec<String>> {
        // Get the Bluetooth adapter
        let manager = Manager::new()
            .await
            .map_err(|e| Error::connection(format!("Failed to create BLE manager: {}", e)))?;

        let adapters = manager
            .adapters()
            .await
            .map_err(|e| Error::connection(format!("Failed to get BLE adapters: {}", e)))?;

        let adapter = adapters
            .into_iter()
            .next()
            .ok_or_else(|| Error::connection("No BLE adapters found"))?;

        // Subscribe to adapter events
        let mut events = adapter
            .events()
            .await
            .map_err(|e| Error::connection(format!("Failed to get adapter events: {}", e)))?;

        // Start scanning
        adapter
            .start_scan(ScanFilter {
                services: vec![MESHCORE_SERVICE_UUID],
            })
            .await
            .map_err(|e| Error::connection(format!("Failed to start BLE scan: {}", e)))?;

        tracing::info!("Scanning for MeshCore devices...");

        let mut discovered_meshcore_radios = Vec::new();

        let _ = tokio::time::timeout(scan_duration, async {
            while let Some(event) = events.next().await {
                if let CentralEvent::DeviceDiscovered(id) = event {
                    if let Ok(peripheral) = adapter.peripheral(&id).await {
                        if let Ok(Some(props)) = peripheral.properties().await {
                            if let Some(name) = &props.local_name {
                                discovered_meshcore_radios.push(name.clone());
                            }
                        }
                    }
                }
            }
        })
        .await;

        // Stop scanning
        let _ = adapter.stop_scan().await;
        tracing::info!("Stopped scanning for MeshCore devices...");

        Ok(discovered_meshcore_radios)
    }

    /// Connect to a Btle peripheral that is a MeshCore radio and return the [MeshCore] to use to
    /// communicate with it
    async fn ble_connect_peripheral(
        peripheral: &Peripheral,
    ) -> crate::Result<(MeshCore, Receiver<Vec<u8>>, Characteristic)> {
        // Check if already connected, disconnect first if so
        if peripheral.is_connected().await.unwrap_or(false) {
            let _ = peripheral.disconnect().await;
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        // Connect to the device with retry
        let mut connect_attempts = 0;
        const MAX_CONNECT_ATTEMPTS: u32 = 3;

        loop {
            connect_attempts += 1;
            tracing::info!(
                "Connecting to device (attempt {}/{})",
                connect_attempts,
                MAX_CONNECT_ATTEMPTS
            );

            match peripheral.connect().await {
                Ok(_) => {
                    tracing::info!("Connected to MeshCore device");
                    break;
                }
                Err(e) => {
                    tracing::warn!("Connection attempt {} failed: {}", connect_attempts, e);
                    if connect_attempts >= MAX_CONNECT_ATTEMPTS {
                        return Err(Error::connection(format!(
                            "Failed to connect after {} attempts: {}",
                            MAX_CONNECT_ATTEMPTS, e
                        )));
                    }
                    // Short delay before retry
                    tokio::time::sleep(Duration::from_millis(1000)).await;
                }
            }
        }

        // Discover services
        peripheral
            .discover_services()
            .await
            .map_err(|e| Error::connection(format!("Failed to discover services: {}", e)))?;

        // Find the MeshCore service and characteristics
        let services = peripheral.services();
        let meshcore_service = services
            .iter()
            .find(|s| s.uuid == MESHCORE_SERVICE_UUID)
            .ok_or_else(|| Error::connection("MeshCore service not found on device"))?;

        let tx_char = meshcore_service
            .characteristics
            .iter()
            .find(|c| c.uuid == MESHCORE_TX_CHAR_UUID)
            .ok_or_else(|| Error::connection("TX characteristic not found"))?
            .clone();

        let rx_char = meshcore_service
            .characteristics
            .iter()
            .find(|c| c.uuid == MESHCORE_RX_CHAR_UUID)
            .ok_or_else(|| Error::connection("RX characteristic not found"))?
            .clone();

        // Subscribe to notifications on RX characteristic
        peripheral
            .subscribe(&rx_char)
            .await
            .map_err(|e| Error::connection(format!("Failed to subscribe to RX: {}", e)))?;

        tracing::info!("Subscribed to MeshCore notifications");

        let (tx, rx) = mpsc::channel::<Vec<u8>>(64);
        Ok((MeshCore::new_with_sender(tx), rx, tx_char))
    }

    /// Given a peripheral's name or mac address (as a &str formatted thus
    /// "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}" using BDAddr.to_string()),
    /// return the [Peripheral] struct
    async fn find_peripheral(target_name_or_mac: &str) -> crate::Result<Peripheral> {
        let manager = Manager::new()
            .await
            .map_err(|e| Error::connection(format!("Failed to create BLE manager: {}", e)))?;

        let adapters = manager
            .adapters()
            .await
            .map_err(|e| Error::connection(format!("Failed to get BLE adapters: {}", e)))?;

        let adapter = adapters
            .into_iter()
            .next()
            .ok_or_else(|| Error::connection("No BLE adapters found"))?;

        // Subscribe to adapter events
        let mut events = adapter
            .events()
            .await
            .map_err(|e| Error::connection(format!("Failed to get adapter events: {}", e)))?;

        adapter
            .start_scan(ScanFilter {
                services: vec![MESHCORE_SERVICE_UUID],
            })
            .await
            .map_err(|e| Error::connection(format!("Failed to start BLE scan: {}", e)))?;

        let target_peripheral: Option<Peripheral> = {
            let timeout = tokio::time::timeout(Duration::from_secs(2), async {
                while let Some(event) = events.next().await {
                    if let CentralEvent::DeviceDiscovered(id) = event {
                        if let Ok(peripheral) = adapter.peripheral(&id).await {
                            if let Ok(Some(props)) = peripheral.properties().await {
                                // return this peripheral if the name matches
                                if props.local_name.as_deref() == Some(target_name_or_mac) {
                                    return Some(peripheral);
                                }

                                // return this peripheral if the MAC address matches
                                if props.address.to_string() == target_name_or_mac {
                                    return Some(peripheral);
                                }
                            }
                        }
                    }
                }
                None
            })
            .await;

            timeout.unwrap_or_else(|_| None)
        };

        adapter
            .stop_scan()
            .await
            .map_err(|e| Error::connection(format!("Failed to stop BLE scan: {}", e)))?;

        target_peripheral.ok_or_else(|| Error::connection("MeshCore device not found"))
    }

    /// This method connects to a MeshCore radio by BTLE device name
    pub async fn ble_connect(name: &str) -> crate::Result<MeshCore> {
        let peripheral = Self::find_peripheral(name).await?;
        let (meshcore, mut rx, tx_char) = Self::ble_connect_peripheral(&peripheral).await?;

        // Connected before the tasks can lose the link: `lose_link` only
        // emits for the task that flips this flag from true to false.
        *meshcore.connected.write().await = true;

        // Clone peripheral for tasks
        let peripheral_write = peripheral.clone();
        let peripheral_read = peripheral.clone();

        // Spawn write task
        // BLE does NOT use framing - send raw payload directly (unlike serial which uses [0x3c][len][payload])
        // The first write error marks the client disconnected, emits
        // Disconnected (unless the read task already did), and ends the task. Dropping `rx` makes later
        // sends fail fast instead of queueing.
        let write_connected = meshcore.connected.clone();
        let write_dispatcher = meshcore.dispatcher.clone();

        let write_task = tokio::spawn(async move {
            while let Some(data) = rx.recv().await {
                tracing::debug!("BLE TX: {} bytes: {:02x?}", data.len(), &data);
                // BLE has MTU limits, so we may need to chunk the data
                for chunk in data.chunks(244) {
                    match peripheral_write
                        .write(&tx_char, chunk, WriteType::WithoutResponse)
                        .await
                    {
                        Ok(_) => tracing::trace!("BLE TX chunk: {} bytes sent", chunk.len()),
                        Err(e) => {
                            tracing::error!("BLE TX error: {}", e);
                            lose_link(&write_connected, &write_dispatcher).await;
                            return;
                        }
                    }
                }
            }
        });

        // Spawn read task
        let msg_reader = meshcore.reader.clone();
        let connected = meshcore.connected.clone();
        let dispatcher = meshcore.dispatcher.clone();

        let read_task = tokio::spawn(async move {
            let mut notification_stream = match peripheral_read.notifications().await {
                Ok(stream) => stream,
                Err(_) => {
                    lose_link(&connected, &dispatcher).await;
                    return;
                }
            };

            while let Some(data) = notification_stream.next().await {
                // BLE does NOT use framing - each notification IS a complete packet
                // (unlike serial which uses [0x3c][len][payload])
                let frame = data.value;
                tracing::debug!(
                    "BLE RX: type=0x{:02x}, len={}, data={:02x?}",
                    frame.first().unwrap_or(&0),
                    frame.len(),
                    &frame
                );

                if !frame.is_empty() {
                    if let Err(e) = msg_reader.handle_rx(frame).await {
                        tracing::error!("Error handling BLE message: {}", e);
                    }
                }
            }

            // Notification stream ended - disconnected
            lose_link(&connected, &dispatcher).await;
        });

        meshcore.tasks.lock().await.push(write_task);
        meshcore.tasks.lock().await.push(read_task);

        meshcore.setup_event_handlers().await;

        Ok(meshcore)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The number of `Disconnected` events waiting on `events`.
    fn disconnects(events: &mut tokio::sync::broadcast::Receiver<MeshCoreEvent>) -> usize {
        let mut count = 0;

        while let Ok(event) = events.try_recv() {
            if event.event_type == EventType::Disconnected {
                count += 1;
            }
        }

        count
    }

    #[tokio::test]
    async fn lose_link_emits_one_disconnected_when_both_tasks_report_one_loss() {
        let dispatcher = EventDispatcher::new();
        let mut events = dispatcher.receiver();
        let connected = RwLock::new(true);

        lose_link(&connected, &dispatcher).await;
        lose_link(&connected, &dispatcher).await;

        assert!(!*connected.read().await);
        assert_eq!(disconnects(&mut events), 1);
    }

    #[tokio::test]
    async fn lose_link_emits_nothing_when_already_disconnected() {
        let dispatcher = EventDispatcher::new();
        let mut events = dispatcher.receiver();

        lose_link(&RwLock::new(false), &dispatcher).await;

        assert_eq!(disconnects(&mut events), 0);
    }
}
