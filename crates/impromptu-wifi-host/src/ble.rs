//! Bluetooth LE through `btleplug`.
//!
//! [`discover`] lists nearby Improv devices with their state and
//! capabilities, read from their advertisements without connecting.
//! [`BtleGattClient`] connects to one and implements [`GattClient`] for
//! [`BleClientLink`](impromptu_wifi_ble::BleClientLink).

use std::fmt;
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
    parse_service_data, Capabilities, CAPABILITIES_UUID, CURRENT_STATE_UUID, ERROR_STATE_UUID,
    RPC_COMMAND_UUID, RPC_RESULT_UUID, SERVICE_DATA_UUID, SERVICE_UUID,
};
use impromptu_wifi_client::proto::State;
use uuid::Uuid;

pub use btleplug::platform::{Adapter as BleAdapter, Peripheral as BlePeripheral};

const SERVICE: Uuid = Uuid::from_u128(SERVICE_UUID);
const SERVICE_DATA: Uuid = Uuid::from_u128(SERVICE_DATA_UUID);
const STATE: Uuid = Uuid::from_u128(CURRENT_STATE_UUID);
const ERROR: Uuid = Uuid::from_u128(ERROR_STATE_UUID);
const RPC_COMMAND: Uuid = Uuid::from_u128(RPC_COMMAND_UUID);
const RPC_RESULT: Uuid = Uuid::from_u128(RPC_RESULT_UUID);
const CAPABILITIES: Uuid = Uuid::from_u128(CAPABILITIES_UUID);

/// Why a BLE operation failed.
#[derive(Debug)]
pub enum BleError {
    /// The platform BLE stack failed.
    Btle(btleplug::Error),
    /// The system has no Bluetooth adapter.
    NoAdapter,
    /// The peripheral lacks this Improv characteristic.
    MissingCharacteristic(&'static str),
    /// The notification stream ended (the device disconnected).
    Disconnected,
}

impl fmt::Display for BleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Btle(e) => write!(f, "Bluetooth error: {e}"),
            Self::NoAdapter => f.write_str("no Bluetooth adapter found"),
            Self::MissingCharacteristic(c) => write!(f, "device has no Improv {c} characteristic"),
            Self::Disconnected => f.write_str("device disconnected"),
        }
    }
}

// No `source()`: the message already includes btleplug's, whose own text
// repeats its cause, so exposing it again would print it a third time.
impl std::error::Error for BleError {}

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

/// An Improv device seen while scanning.
#[derive(Debug, Clone)]
pub struct Discovered {
    /// The device, for [`BtleGattClient::connect`].
    pub peripheral: Peripheral,
    /// Its address, or an opaque identifier on macOS.
    pub address: String,
    /// Its advertised name, if any.
    pub name: Option<String>,
    /// Signal strength of the last advertisement, in dBm.
    pub rssi: Option<i16>,
    /// State from the advertised service data (spec 2.0 and later).
    pub state: Option<State>,
    /// Capabilities from the advertised service data.
    pub capabilities: Option<Capabilities>,
}

impl Discovered {
    /// Whether `query` names this device: its address (case-insensitive) or
    /// part of its name.
    pub fn matches(&self, query: &str) -> bool {
        self.address.eq_ignore_ascii_case(query)
            || self
                .name
                .as_deref()
                .is_some_and(|n| n.to_lowercase().contains(&query.to_lowercase()))
    }
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
    let mut filter = ScanFilter::default();
    filter.services = vec![SERVICE];
    filter
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
    found.sort_by_key(|d| std::cmp::Reverse(d.rssi.unwrap_or(i16::MIN)));
    Ok(found)
}

/// An Improv [`GattClient`] on a `btleplug` peripheral.
///
/// Writes are 20-byte chunks with response. `btleplug` doesn't expose the
/// negotiated MTU, and a longer write would make BlueZ fall back to a
/// prepared ("long") write, which not every device supports.
pub struct BtleGattClient {
    peripheral: Peripheral,
    state: Characteristic,
    rpc_command: Characteristic,
    rpc_result: Characteristic,
    capabilities: Characteristic,
    notifications: Pin<Box<dyn Stream<Item = ValueNotification> + Send>>,
}

impl BtleGattClient {
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
/// `BtleGattClient: Sync`, which the notification stream isn't.
async fn read_byte(peripheral: &Peripheral, c: &Characteristic) -> Result<u8, BleError> {
    Ok(peripheral.read(c).await?.first().copied().unwrap_or(0))
}

impl GattClient for BtleGattClient {
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
