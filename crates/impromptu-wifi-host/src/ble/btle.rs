//! Bluetooth LE through `btleplug`, for systems other than Linux.

use std::pin::Pin;
use std::time::Duration;

use btleplug::api::{
    Central, Characteristic, Manager as _, Peripheral as _, ScanFilter, ValueNotification,
    WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::{Stream, StreamExt};
use impromptu_wifi_ble::{GattClient, Notification};
use impromptu_wifi_client::proto::ble::{
    parse_service_data, CAPABILITIES_UUID, CURRENT_STATE_UUID, ERROR_STATE_UUID, RPC_COMMAND_UUID,
    RPC_RESULT_UUID, SERVICE_DATA_UUID, SERVICE_UUID,
};
use uuid::Uuid;

use super::{sort_by_signal, BleError, Discovered};

pub use btleplug::platform::{Adapter as BleAdapter, Peripheral as BlePeripheral};
const SERVICE: Uuid = Uuid::from_u128(SERVICE_UUID);
const SERVICE_DATA: Uuid = Uuid::from_u128(SERVICE_DATA_UUID);
const STATE: Uuid = Uuid::from_u128(CURRENT_STATE_UUID);
const ERROR: Uuid = Uuid::from_u128(ERROR_STATE_UUID);
const RPC_COMMAND: Uuid = Uuid::from_u128(RPC_COMMAND_UUID);
const RPC_RESULT: Uuid = Uuid::from_u128(RPC_RESULT_UUID);
const CAPABILITIES: Uuid = Uuid::from_u128(CAPABILITIES_UUID);

impl From<btleplug::Error> for BleError {
    fn from(e: btleplug::Error) -> Self {
        Self::Btle(e)
    }
}

/// The first Bluetooth adapter on the system.
pub async fn first_adapter() -> Result<Adapter, BleError> {
    let manager = Manager::new().await?;
    manager
        .adapters()
        .await?
        .into_iter()
        .next()
        .ok_or(BleError::NoAdapter)
}

/// Scan for `duration` and return the Improv devices seen, strongest first.
pub async fn discover(adapter: &Adapter, duration: Duration) -> Result<Vec<Discovered>, BleError> {
    adapter.start_scan(improv_wifi_filter()).await?;
    tokio::time::sleep(duration).await;
    adapter.stop_scan().await?;
    seen(adapter).await
}

/// Scan until a device matching `query` (see [`Discovered::matches`])
/// appears, or `timeout` passes. Returns early on the first sighting; if
/// several match by then, the strongest wins.
pub async fn find(
    adapter: &Adapter,
    query: &str,
    timeout: Duration,
) -> Result<Option<Discovered>, BleError> {
    adapter.start_scan(improv_wifi_filter()).await?;
    let deadline = tokio::time::Instant::now() + timeout;
    let found = loop {
        let mut matching = seen(adapter)
            .await?
            .into_iter()
            .filter(|d| d.matches(query));
        if let Some(best) = matching.next() {
            break Some(best);
        }
        if tokio::time::Instant::now() >= deadline {
            break None;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    adapter.stop_scan().await?;
    Ok(found)
}

fn improv_wifi_filter() -> ScanFilter {
    ScanFilter {
        services: vec![SERVICE],
    }
}

/// The Improv devices the adapter has seen so far, strongest first.
async fn seen(adapter: &Adapter) -> Result<Vec<Discovered>, BleError> {
    let mut found = Vec::new();
    for peripheral in adapter.peripherals().await? {
        let Some(props) = peripheral.properties().await? else {
            continue;
        };
        let data = props.service_data.get(&SERVICE_DATA);
        if !props.services.contains(&SERVICE) && data.is_none() {
            continue;
        }
        let parsed = data.and_then(|d| parse_service_data(d));
        found.push(Discovered {
            address: peripheral.id().to_string(),
            name: props.local_name.or(props.advertisement_name),
            rssi: props.rssi,
            state: parsed.map(|(s, _)| s),
            capabilities: parsed.map(|(_, c)| c),
            peripheral,
        });
    }
    sort_by_signal(&mut found);
    Ok(found)
}

/// An Improv [`GattClient`] on a `btleplug` peripheral.
///
/// Writes are 20-byte chunks with response. `btleplug` doesn't expose the
/// negotiated MTU, and a longer write would make BlueZ fall back to a
/// prepared ("long") write, which not every device supports.
pub struct BleGattClient {
    peripheral: Peripheral,
    state: Characteristic,
    rpc_command: Characteristic,
    rpc_result: Characteristic,
    capabilities: Characteristic,
    notifications: Pin<Box<dyn Stream<Item = ValueNotification> + Send>>,
}

impl BleGattClient {
    /// Connect (if not already connected), find the Improv characteristics
    /// and subscribe to their notifications.
    pub async fn connect(peripheral: Peripheral) -> Result<Self, BleError> {
        if !peripheral.is_connected().await? {
            peripheral.connect().await?;
        }
        peripheral.discover_services().await?;
        let chars = peripheral.characteristics();
        let find = |uuid: Uuid, what: &'static str| {
            chars
                .iter()
                .find(|c| c.uuid == uuid && c.service_uuid == SERVICE)
                .cloned()
                .ok_or(BleError::MissingCharacteristic(what))
        };
        let state = find(STATE, "current state")?;
        let error = find(ERROR, "error state")?;
        let rpc_command = find(RPC_COMMAND, "RPC command")?;
        let rpc_result = find(RPC_RESULT, "RPC result")?;
        let capabilities = find(CAPABILITIES, "capabilities")?;

        // Open the stream before subscribing so nothing slips past.
        let notifications = peripheral.notifications().await?;
        for c in [&state, &error, &rpc_result] {
            peripheral.subscribe(c).await?;
        }
        Ok(Self {
            peripheral,
            state,
            rpc_command,
            rpc_result,
            capabilities,
            notifications,
        })
    }

    /// The underlying peripheral.
    pub fn peripheral(&self) -> &Peripheral {
        &self.peripheral
    }

    /// Disconnect from the device.
    pub async fn disconnect(self) -> Result<(), BleError> {
        Ok(self.peripheral.disconnect().await?)
    }
}

/// Read a one-byte characteristic. A free function on purpose: borrowing the
/// whole client across the await would make the future require
/// `BleGattClient: Sync`, which the notification stream isn't.
async fn read_byte(peripheral: &Peripheral, c: &Characteristic) -> Result<u8, BleError> {
    Ok(peripheral.read(c).await?.first().copied().unwrap_or(0))
}

impl GattClient for BleGattClient {
    type Error = BleError;

    async fn write_rpc_command(&mut self, chunk: &[u8]) -> Result<(), BleError> {
        Ok(self
            .peripheral
            .write(&self.rpc_command, chunk, WriteType::WithResponse)
            .await?)
    }

    async fn notification(&mut self, buf: &mut [u8]) -> Result<Notification, BleError> {
        loop {
            let n = self
                .notifications
                .next()
                .await
                .ok_or(BleError::Disconnected)?;
            if n.service_uuid != SERVICE {
                continue;
            }
            let first = n.value.first().copied().unwrap_or(0);
            if n.uuid == STATE {
                return Ok(Notification::State(first));
            }
            if n.uuid == ERROR {
                return Ok(Notification::Error(first));
            }
            if n.uuid == RPC_RESULT {
                // A notification can be cut to the MTU; the stack's read
                // returns the whole value.
                let full = self.peripheral.read(&self.rpc_result).await?;
                let n = full.len().min(buf.len());
                buf[..n].copy_from_slice(&full[..n]);
                return Ok(Notification::RpcResult(full.len()));
            }
        }
    }

    async fn read_state(&mut self) -> Result<u8, BleError> {
        read_byte(&self.peripheral, &self.state).await
    }

    async fn read_capabilities(&mut self) -> Result<u8, BleError> {
        read_byte(&self.peripheral, &self.capabilities).await
    }
}
