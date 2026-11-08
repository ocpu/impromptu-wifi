//! Bluetooth LE through BlueZ's D-Bus API, using zbus.
//!
//! Written against the BlueZ D-Bus API documentation
//! (`doc/org.bluez.Adapter.rst`, `org.bluez.Device.rst`,
//! `org.bluez.GattService.rst` and `org.bluez.GattCharacteristic.rst` in
//! the BlueZ source tree):
//!
//! * **Scanning** sets a discovery filter for the Improv Wi-Fi service UUID
//!   on the LE transport, then calls `StartDiscovery`. Devices are the
//!   adapter's `org.bluez.Device1` objects that list the service UUID or
//!   carry Improv Wi-Fi service data.
//! * **Connecting** calls `Device1.Connect` and waits for
//!   `ServicesResolved`, then looks up the Improv Wi-Fi service's
//!   characteristics among BlueZ's objects.
//! * **Notifications** are `PropertiesChanged` signals for a
//!   characteristic's `Value` after `StartNotify`. BlueZ performs long
//!   reads itself, so `ReadValue` returns the whole RPC result.
//! * **Disconnects** show up as `Device1.Connected` turning false.

use std::collections::HashMap;
use std::pin::Pin;
use std::time::Duration;

use futures::{Stream, StreamExt};
use impromptu_wifi_ble::{GattClient, Notification};
use impromptu_wifi_client::proto::ble::{
    parse_service_data, CAPABILITIES_UUID, CURRENT_STATE_UUID, ERROR_STATE_UUID, RPC_COMMAND_UUID,
    RPC_RESULT_UUID, SERVICE_DATA_UUID, SERVICE_UUID,
};
use zbus::fdo::{ManagedObjects, ObjectManagerProxy, PropertiesProxy};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::Connection;

use super::{sort_by_signal, BleError, Discovered};

const BLUEZ: &str = "org.bluez";
const ADAPTER: &str = "org.bluez.Adapter1";
const DEVICE: &str = "org.bluez.Device1";
const SERVICE: &str = "org.bluez.GattService1";
const CHARACTERISTIC: &str = "org.bluez.GattCharacteristic1";

/// How long `Connect` and service discovery may take together.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

impl From<zbus::Error> for BleError {
    fn from(e: zbus::Error) -> Self {
        Self::Dbus(e)
    }
}

impl From<zbus::fdo::Error> for BleError {
    fn from(e: zbus::fdo::Error) -> Self {
        Self::Dbus(e.into())
    }
}

/// The 128-bit UUID in the lowercase text form BlueZ uses.
fn uuid(u: u128) -> String {
    let h = format!("{u:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

type Props = HashMap<String, OwnedValue>;

/// The properties of `iface` on one object, if the object has it.
fn iface<'a>(
    ifaces: &'a HashMap<zbus::names::OwnedInterfaceName, Props>,
    name: &str,
) -> Option<&'a Props> {
    ifaces
        .iter()
        .find(|(n, _)| n.as_str() == name)
        .map(|(_, p)| p)
}

fn prop<T: TryFrom<OwnedValue>>(props: &Props, key: &str) -> Option<T> {
    T::try_from(props.get(key)?.try_clone().ok()?).ok()
}

async fn managed_objects(conn: &Connection) -> Result<ManagedObjects, BleError> {
    Ok(ObjectManagerProxy::builder(conn)
        .destination(BLUEZ)?
        .path("/")?
        .build()
        .await?
        .get_managed_objects()
        .await?)
}

async fn call<B>(
    conn: &Connection,
    path: &ObjectPath<'_>,
    iface: &str,
    method: &str,
    body: &B,
) -> Result<zbus::Message, BleError>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    Ok(conn
        .call_method(Some(BLUEZ), path, Some(iface), method, body)
        .await?)
}

fn no_options() -> HashMap<&'static str, Value<'static>> {
    HashMap::new()
}

/// A BlueZ adapter, such as `hci0`, with the D-Bus connection to reach it.
#[derive(Debug, Clone)]
pub struct BleAdapter {
    conn: Connection,
    path: OwnedObjectPath,
}

impl BleAdapter {
    /// The first adapter, by object path, reachable over `conn`.
    pub async fn first_on(conn: Connection) -> Result<Self, BleError> {
        let mut adapters: Vec<_> = managed_objects(&conn)
            .await?
            .into_iter()
            .filter(|(_, ifaces)| iface(ifaces, ADAPTER).is_some())
            .map(|(path, _)| path)
            .collect();
        adapters.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        let path = adapters.into_iter().next().ok_or(BleError::NoAdapter)?;
        Ok(Self { conn, path })
    }

    /// The object path, for example `/org/bluez/hci0`.
    pub fn path(&self) -> &ObjectPath<'_> {
        &self.path
    }
}

/// A device BlueZ knows about.
#[derive(Debug, Clone)]
pub struct BlePeripheral {
    conn: Connection,
    path: OwnedObjectPath,
}

impl BlePeripheral {
    /// The object path, for example `/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF`.
    pub fn path(&self) -> &ObjectPath<'_> {
        &self.path
    }
}

/// The first Bluetooth adapter, on the system bus.
pub async fn first_adapter() -> Result<BleAdapter, BleError> {
    BleAdapter::first_on(Connection::system().await?).await
}

/// Scan for `duration` and return the Improv Wi-Fi devices seen, strongest
/// first.
pub async fn discover(
    adapter: &BleAdapter,
    duration: Duration,
) -> Result<Vec<Discovered>, BleError> {
    start_scan(adapter).await?;
    tokio::time::sleep(duration).await;
    stop_scan(adapter).await;
    seen(adapter).await
}

/// Scan until a device matching `query` (see [`Discovered::matches`])
/// appears, or `timeout` passes. Returns early on the first sighting; if
/// several match by then, the strongest wins.
pub async fn find(
    adapter: &BleAdapter,
    query: &str,
    timeout: Duration,
) -> Result<Option<Discovered>, BleError> {
    start_scan(adapter).await?;
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
    stop_scan(adapter).await;
    Ok(found)
}

async fn start_scan(adapter: &BleAdapter) -> Result<(), BleError> {
    let filter = HashMap::from([
        ("UUIDs", Value::from(vec![uuid(SERVICE_UUID)])),
        ("Transport", Value::from("le")),
    ]);
    call(
        &adapter.conn,
        &adapter.path,
        ADAPTER,
        "SetDiscoveryFilter",
        &(filter,),
    )
    .await?;
    match call(&adapter.conn, &adapter.path, ADAPTER, "StartDiscovery", &()).await {
        // Another client's discovery is fine: BlueZ merges the filters.
        Err(BleError::Dbus(zbus::Error::MethodError(name, _, _)))
            if name.as_str() == "org.bluez.Error.InProgress" =>
        {
            Ok(())
        }
        other => other.map(drop),
    }
}

/// Best effort: a scan that already stopped is not an error.
async fn stop_scan(adapter: &BleAdapter) {
    let _ = call(&adapter.conn, &adapter.path, ADAPTER, "StopDiscovery", &()).await;
}

/// The Improv Wi-Fi devices the adapter knows, strongest first.
async fn seen(adapter: &BleAdapter) -> Result<Vec<Discovered>, BleError> {
    let service = uuid(SERVICE_UUID);
    let service_data = uuid(SERVICE_DATA_UUID);
    let mut found = Vec::new();
    for (path, ifaces) in managed_objects(&adapter.conn).await? {
        let Some(props) = iface(&ifaces, DEVICE) else {
            continue;
        };
        if prop::<OwnedObjectPath>(props, "Adapter").as_ref() != Some(&adapter.path) {
            continue;
        }
        let uuids: Vec<String> = prop(props, "UUIDs").unwrap_or_default();
        let data: Option<Vec<u8>> = prop::<HashMap<String, OwnedValue>>(props, "ServiceData")
            .and_then(|d| Vec::<u8>::try_from(d.get(&service_data)?.try_clone().ok()?).ok());
        if !uuids.iter().any(|u| u.eq_ignore_ascii_case(&service)) && data.is_none() {
            continue;
        }
        let parsed = data.as_deref().and_then(parse_service_data);
        found.push(Discovered {
            address: prop(props, "Address").unwrap_or_else(|| path.to_string()),
            name: prop(props, "Name"),
            rssi: prop(props, "RSSI"),
            state: parsed.map(|(s, _)| s),
            capabilities: parsed.map(|(_, c)| c),
            peripheral: BlePeripheral {
                conn: adapter.conn.clone(),
                path,
            },
        });
    }
    sort_by_signal(&mut found);
    Ok(found)
}

enum Event {
    Value(OwnedObjectPath, Vec<u8>),
    Disconnected,
}

type Events = Pin<Box<dyn Stream<Item = Event> + Send>>;

/// Changes of `Value` on one characteristic.
async fn value_changes(conn: &Connection, path: OwnedObjectPath) -> Result<Events, BleError> {
    let changes = PropertiesProxy::builder(conn)
        .destination(BLUEZ)?
        .path(path.clone())?
        .build()
        .await?
        .receive_properties_changed()
        .await?;
    Ok(Box::pin(changes.filter_map(move |signal| {
        let path = path.clone();
        async move {
            let args = signal.args().ok()?;
            if args.interface_name().as_str() != CHARACTERISTIC {
                return None;
            }
            let value = args.changed_properties().get("Value")?;
            let bytes = Vec::<u8>::try_from(value.try_clone().ok()?).ok()?;
            Some(Event::Value(path, bytes))
        }
    })))
}

/// An Improv Wi-Fi [`GattClient`] on a BlueZ device.
///
/// Writes are 20-byte chunks with response: a longer write makes BlueZ fall
/// back to a prepared ("long") write, which not every device supports.
pub struct BleGattClient {
    conn: Connection,
    device: OwnedObjectPath,
    state: OwnedObjectPath,
    error: OwnedObjectPath,
    rpc_command: OwnedObjectPath,
    rpc_result: OwnedObjectPath,
    capabilities: OwnedObjectPath,
    events: Events,
}

impl BleGattClient {
    /// Connect (if not already connected), find the Improv Wi-Fi
    /// characteristics and subscribe to their notifications.
    pub async fn connect(peripheral: BlePeripheral) -> Result<Self, BleError> {
        tokio::time::timeout(CONNECT_TIMEOUT, Self::connect_inner(peripheral))
            .await
            .map_err(|_| BleError::Timeout("connecting"))?
    }

    async fn connect_inner(peripheral: BlePeripheral) -> Result<Self, BleError> {
        let BlePeripheral { conn, path: device } = peripheral;
        let device_props = PropertiesProxy::builder(&conn)
            .destination(BLUEZ)?
            .path(device.clone())?
            .build()
            .await?;
        // Watch before acting, so a quick change can't slip past.
        let mut device_changes = device_props.receive_properties_changed().await?;
        let device_iface = || DEVICE.try_into().expect("valid interface name");

        let connected =
            bool::try_from(device_props.get(device_iface(), "Connected").await?).unwrap_or(false);
        if !connected {
            call(&conn, &device, DEVICE, "Connect", &()).await?;
        }
        loop {
            let resolved =
                bool::try_from(device_props.get(device_iface(), "ServicesResolved").await?)
                    .unwrap_or(false);
            if resolved {
                break;
            }
            // Re-check after any change of the device's properties.
            device_changes.next().await.ok_or(BleError::Disconnected)?;
        }

        let objects = managed_objects(&conn).await?;
        let service_uuid = uuid(SERVICE_UUID);
        let service = objects
            .iter()
            .find(|(_, ifaces)| {
                iface(ifaces, SERVICE).is_some_and(|p| {
                    prop::<OwnedObjectPath>(p, "Device").as_ref() == Some(&device)
                        && prop::<String>(p, "UUID")
                            .is_some_and(|u| u.eq_ignore_ascii_case(&service_uuid))
                })
            })
            .map(|(p, _)| p.clone())
            .ok_or(BleError::MissingCharacteristic("service"))?;
        let characteristics: Vec<(String, OwnedObjectPath)> = objects
            .iter()
            .filter_map(|(path, ifaces)| {
                let p = iface(ifaces, CHARACTERISTIC)?;
                (prop::<OwnedObjectPath>(p, "Service").as_ref() == Some(&service))
                    .then(|| Some((prop::<String>(p, "UUID")?.to_lowercase(), path.clone())))?
            })
            .collect();
        let find = |u: u128, what: &'static str| {
            let u = uuid(u);
            characteristics
                .iter()
                .find(|(c, _)| *c == u)
                .map(|(_, p)| p.clone())
                .ok_or(BleError::MissingCharacteristic(what))
        };
        let state = find(CURRENT_STATE_UUID, "current state")?;
        let error = find(ERROR_STATE_UUID, "error state")?;
        let rpc_command = find(RPC_COMMAND_UUID, "RPC command")?;
        let rpc_result = find(RPC_RESULT_UUID, "RPC result")?;
        let capabilities = find(CAPABILITIES_UUID, "capabilities")?;

        // Open the streams before subscribing so nothing slips past.
        let disconnects: Events = Box::pin(device_changes.filter_map(|signal| async move {
            let args = signal.args().ok()?;
            let connected = args.changed_properties().get("Connected")?;
            (!bool::try_from(connected).ok()?).then_some(Event::Disconnected)
        }));
        let mut streams = vec![disconnects];
        for c in [&state, &error, &rpc_result] {
            streams.push(value_changes(&conn, c.clone()).await?);
        }
        for c in [&state, &error, &rpc_result] {
            call(&conn, c, CHARACTERISTIC, "StartNotify", &()).await?;
        }
        Ok(Self {
            conn,
            device,
            state,
            error,
            rpc_command,
            rpc_result,
            capabilities,
            events: Box::pin(futures::stream::select_all(streams)),
        })
    }

    /// The device's object path.
    pub fn device(&self) -> &ObjectPath<'_> {
        &self.device
    }

    /// Disconnect from the device.
    pub async fn disconnect(self) -> Result<(), BleError> {
        call(&self.conn, &self.device, DEVICE, "Disconnect", &()).await?;
        Ok(())
    }
}

/// Read a characteristic. A free function on purpose: borrowing the whole
/// client across the await would make the future require
/// `BleGattClient: Sync`, which the notification stream isn't.
async fn read(conn: &Connection, path: &OwnedObjectPath) -> Result<Vec<u8>, BleError> {
    Ok(
        call(conn, path, CHARACTERISTIC, "ReadValue", &(no_options(),))
            .await?
            .body()
            .deserialize()?,
    )
}

impl GattClient for BleGattClient {
    type Error = BleError;

    async fn write_rpc_command(&mut self, chunk: &[u8]) -> Result<(), BleError> {
        let options = HashMap::from([("type", Value::from("request"))]);
        call(
            &self.conn,
            &self.rpc_command,
            CHARACTERISTIC,
            "WriteValue",
            &(chunk, options),
        )
        .await
        .map(drop)
    }

    async fn notification(&mut self, buf: &mut [u8]) -> Result<Notification, BleError> {
        loop {
            let (path, value) = match self.events.next().await {
                Some(Event::Value(path, value)) => (path, value),
                Some(Event::Disconnected) | None => return Err(BleError::Disconnected),
            };
            let first = value.first().copied().unwrap_or(0);
            if path == self.state {
                return Ok(Notification::State(first));
            }
            if path == self.error {
                return Ok(Notification::Error(first));
            }
            if path == self.rpc_result {
                // A notification can be cut to the MTU; BlueZ's read returns
                // the whole value.
                let full = read(&self.conn, &self.rpc_result).await?;
                let n = full.len().min(buf.len());
                buf[..n].copy_from_slice(&full[..n]);
                return Ok(Notification::RpcResult(full.len()));
            }
        }
    }

    async fn read_state(&mut self) -> Result<u8, BleError> {
        Ok(read(&self.conn, &self.state)
            .await?
            .first()
            .copied()
            .unwrap_or(0))
    }

    async fn read_capabilities(&mut self) -> Result<u8, BleError> {
        Ok(read(&self.conn, &self.capabilities)
            .await?
            .first()
            .copied()
            .unwrap_or(0))
    }
}
