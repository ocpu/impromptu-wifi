//! Improv Wi-Fi over Bluetooth LE through BlueZ, using zbus.
//!
//! [`BluezGatt::start`] exports the Improv Wi-Fi GATT service as a BlueZ
//! GATT application and implements [`GattServer`] for
//! [`BleLink`](impromptu_wifi_ble::BleLink). This module talks to BlueZ's
//! D-Bus API directly, so it needs no C library (no libdbus). It is written
//! against the BlueZ D-Bus API documentation (`doc/org.bluez.GattManager.rst`,
//! `org.bluez.GattService.rst`, `org.bluez.GattCharacteristic.rst`,
//! `org.bluez.LEAdvertisingManager.rst` and `org.bluez.LEAdvertisement.rst`
//! in the BlueZ source tree).
//!
//! * **Objects:** one `org.freedesktop.DBus.ObjectManager` at the
//!   application root, one `org.bluez.GattService1` below it and one
//!   `org.bluez.GattCharacteristic1` per Improv Wi-Fi characteristic. The
//!   application is registered with `org.bluez.GattManager1` on the adapter.
//! * **Reads** honour the `offset` option BlueZ passes for long reads: an
//!   RPC result can be 258 bytes, more than one packet, and BlueZ fetches
//!   the rest with increasing offsets.
//! * **Notifications** are `PropertiesChanged` signals for the
//!   characteristic's `Value`, sent while BlueZ has called `StartNotify`.
//!   They carry the whole value; BlueZ cuts them to the MTU, and clients
//!   read the full RPC result after being notified.
//! * **Advertising** carries the service UUID and the service data, which
//!   together fill a legacy 31-byte advertisement exactly, so the device
//!   name isn't included. The spec requires both in the same advertisement
//!   rather than the scan response. Each state change registers a new
//!   `org.bluez.LEAdvertisement1` with fresh service data.
//! * **One client at a time**, as Improv Wi-Fi intends. A client that
//!   disconnects mid-command can leave a partial frame, which the next
//!   client sees as one invalid command before things recover.
//!
//! [`BluezGatt::stop`] unregisters the application and the advertisement.
//! Dropping a `BluezGatt` without calling `stop` starts the same clean-up
//! on the current tokio runtime. BlueZ also removes both when the D-Bus
//! connection that registered them closes.
//!
//! Needs BlueZ (`bluetoothd`) and permission to use it over D-Bus: root, or
//! a user in the `bluetooth` group on most distributions.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use impromptu_wifi_ble::GattServer;
use impromptu_wifi_core::proto::ble::{
    Capabilities, CAPABILITIES_UUID, CURRENT_STATE_UUID, ERROR_STATE_UUID, RPC_COMMAND_UUID,
    RPC_RESULT_UUID, SERVICE_DATA_UUID, SERVICE_UUID,
};
use tokio::sync::mpsc;
use zbus::fdo::{ObjectManager, ObjectManagerProxy, PropertiesProxy};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::{interface, Connection};

/// The well-known bus name of `bluetoothd`.
pub const BLUEZ_SERVICE: &str = "org.bluez";

const ADAPTER_IFACE: &str = "org.bluez.Adapter1";
const GATT_MANAGER_IFACE: &str = "org.bluez.GattManager1";
const ADVERTISING_MANAGER_IFACE: &str = "org.bluez.LEAdvertisingManager1";

/// Root of the objects this process exports. Each [`BluezGatt`] gets its
/// own numbered subtree, so a new one can start while an old one is still
/// being cleaned up.
const ROOT: &str = "/io/github/impromptu_wifi";

/// Why the BlueZ side failed.
#[derive(Debug)]
pub enum BluezError {
    /// A D-Bus call failed, or BlueZ refused it.
    Dbus(zbus::Error),
    /// BlueZ has no adapter that supports GATT applications and LE
    /// advertising.
    NoAdapter,
    /// The GATT application was removed underneath us.
    Closed,
    /// The adapter is switched off by rfkill, so BlueZ can't power it.
    Blocked,
}

impl fmt::Display for BluezError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dbus(e) => write!(f, "BlueZ: {e}"),
            Self::NoAdapter => f.write_str("no Bluetooth LE adapter found"),
            Self::Closed => f.write_str("the Improv Wi-Fi GATT service was unregistered"),
            Self::Blocked => f.write_str(
                "the Bluetooth adapter is blocked by rfkill; unblock it with \
                 `rfkill unblock bluetooth`, or by writing 0 to \
                 /sys/class/rfkill/<n>/soft for the bluetooth entry",
            ),
        }
    }
}

impl std::error::Error for BluezError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Dbus(e) => Some(e),
            _ => None,
        }
    }
}

impl From<zbus::Error> for BluezError {
    fn from(e: zbus::Error) -> Self {
        Self::Dbus(e)
    }
}

impl From<zbus::fdo::Error> for BluezError {
    fn from(e: zbus::fdo::Error) -> Self {
        Self::Dbus(e.into())
    }
}

/// A BlueZ adapter, such as `hci0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adapter {
    path: OwnedObjectPath,
}

impl Adapter {
    /// The first adapter, by object path, that supports GATT applications
    /// and LE advertising.
    pub async fn first(bus: &Connection) -> Result<Self, BluezError> {
        let manager = ObjectManagerProxy::builder(bus)
            .destination(BLUEZ_SERVICE)?
            .path("/")?
            .build()
            .await?;
        let objects = manager.get_managed_objects().await?;
        let mut adapters: Vec<_> = objects
            .into_iter()
            .filter(|(_, ifaces)| {
                [ADAPTER_IFACE, GATT_MANAGER_IFACE, ADVERTISING_MANAGER_IFACE]
                    .iter()
                    .all(|i| ifaces.keys().any(|k| k.as_str() == *i))
            })
            .map(|(path, _)| path)
            .collect();
        adapters.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        adapters
            .into_iter()
            .next()
            .map(|path| Self { path })
            .ok_or(BluezError::NoAdapter)
    }

    /// The adapter with this object path, for example `/org/bluez/hci0`.
    /// Not checked until it is used.
    pub fn at(path: OwnedObjectPath) -> Self {
        Self { path }
    }

    /// The object path, for example `/org/bluez/hci0`.
    pub fn path(&self) -> &ObjectPath<'_> {
        &self.path
    }

    /// The last path element, for example `hci0`.
    pub fn name(&self) -> &str {
        self.path.as_str().rsplit('/').next().unwrap_or_default()
    }
}

/// Current characteristic values, shared by the exported objects.
#[derive(Default)]
struct Values {
    state: u8,
    error: u8,
    result: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    State,
    Error,
    RpcCommand,
    RpcResult,
    Capabilities,
}

impl Kind {
    const ALL: [Kind; 5] = [
        Kind::State,
        Kind::Error,
        Kind::RpcCommand,
        Kind::RpcResult,
        Kind::Capabilities,
    ];

    fn uuid(self) -> u128 {
        match self {
            Kind::State => CURRENT_STATE_UUID,
            Kind::Error => ERROR_STATE_UUID,
            Kind::RpcCommand => RPC_COMMAND_UUID,
            Kind::RpcResult => RPC_RESULT_UUID,
            Kind::Capabilities => CAPABILITIES_UUID,
        }
    }

    fn flags(self) -> &'static [&'static str] {
        match self {
            Kind::State | Kind::Error | Kind::RpcResult => &["read", "notify"],
            Kind::RpcCommand => &["write", "write-without-response"],
            Kind::Capabilities => &["read"],
        }
    }

    fn notifies(self) -> bool {
        self.flags().contains(&"notify")
    }
}

/// Errors returned to BlueZ, named as the GATT API documents them.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.bluez.Error")]
enum GattError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Failed(String),
    InvalidOffset(String),
    NotSupported(String),
}

/// The 128-bit UUID in the textual form BlueZ uses.
pub(crate) fn uuid_string(uuid: u128) -> String {
    let h = format!("{uuid:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// The part of `value` a read at `offset` returns.
pub(crate) fn read_at(value: &[u8], offset: u16) -> Option<Vec<u8>> {
    value.get(usize::from(offset)..).map(<[u8]>::to_vec)
}

/// The `offset` option of `ReadValue`, 0 if absent.
fn offset(options: &HashMap<String, OwnedValue>) -> Result<u16, GattError> {
    match options.get("offset") {
        None => Ok(0),
        Some(v) => v
            .downcast_ref::<u16>()
            .map_err(|_| GattError::Failed("offset is not a uint16".into())),
    }
}

struct Service {
    uuid: String,
}

#[interface(name = "org.bluez.GattService1")]
impl Service {
    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> String {
        self.uuid.clone()
    }

    #[zbus(property)]
    fn primary(&self) -> bool {
        true
    }
}

struct Characteristic {
    kind: Kind,
    service: OwnedObjectPath,
    values: Arc<Mutex<Values>>,
    caps: u8,
    /// Set on the RPC command characteristic only.
    writes: Option<mpsc::Sender<Vec<u8>>>,
    notifying: bool,
}

impl Characteristic {
    fn current(&self) -> Vec<u8> {
        let v = self.values.lock().unwrap();
        match self.kind {
            Kind::State => vec![v.state],
            Kind::Error => vec![v.error],
            Kind::RpcResult => v.result.clone(),
            Kind::Capabilities => vec![self.caps],
            Kind::RpcCommand => Vec::new(),
        }
    }
}

#[interface(name = "org.bluez.GattCharacteristic1")]
impl Characteristic {
    async fn read_value(&self, options: HashMap<String, OwnedValue>) -> Result<Vec<u8>, GattError> {
        if self.kind == Kind::RpcCommand {
            return Err(GattError::NotSupported("write-only".into()));
        }
        read_at(&self.current(), offset(&options)?)
            .ok_or_else(|| GattError::InvalidOffset("past the end of the value".into()))
    }

    async fn write_value(
        &self,
        value: Vec<u8>,
        _options: HashMap<String, OwnedValue>,
    ) -> Result<(), GattError> {
        let Some(tx) = &self.writes else {
            return Err(GattError::NotSupported("read-only".into()));
        };
        // Chunks arrive in order (long writes too), so they can go straight
        // to the driver's reassembly.
        tx.send(value)
            .await
            .map_err(|_| GattError::Failed("the Improv Wi-Fi driver stopped".into()))
    }

    async fn start_notify(&mut self) -> Result<(), GattError> {
        if !self.kind.notifies() {
            return Err(GattError::NotSupported("no notifications".into()));
        }
        self.notifying = true;
        Ok(())
    }

    async fn stop_notify(&mut self) {
        self.notifying = false;
    }

    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> String {
        uuid_string(self.kind.uuid())
    }

    #[zbus(property)]
    fn service(&self) -> OwnedObjectPath {
        self.service.clone()
    }

    #[zbus(property)]
    fn flags(&self) -> Vec<String> {
        self.kind.flags().iter().map(|f| f.to_string()).collect()
    }

    #[zbus(property)]
    fn value(&self) -> Vec<u8> {
        self.current()
    }

    #[zbus(property)]
    fn notifying(&self) -> bool {
        self.notifying
    }
}

struct Advertisement {
    data: [u8; 6],
}

#[interface(name = "org.bluez.LEAdvertisement1")]
impl Advertisement {
    /// BlueZ dropped the advertisement; the next state change registers a
    /// new one.
    fn release(&self) {}

    #[zbus(property, name = "Type")]
    fn kind(&self) -> String {
        "peripheral".into()
    }

    #[zbus(property, name = "ServiceUUIDs")]
    fn service_uuids(&self) -> Vec<String> {
        vec![uuid_string(SERVICE_UUID)]
    }

    #[zbus(property)]
    fn service_data(&self) -> HashMap<String, OwnedValue> {
        let data = Value::from(self.data.to_vec())
            .try_to_owned()
            .expect("a byte array holds no file descriptors");
        HashMap::from([(uuid_string(SERVICE_DATA_UUID), data)])
    }

    #[zbus(property)]
    fn discoverable(&self) -> bool {
        true
    }

    /// The system's Bluetooth name. The advertisement is full with the
    /// service UUID and service data, so BlueZ sends the name in the scan
    /// response, where clients such as `impromptu-wifi discover` show it.
    #[zbus(property)]
    fn includes(&self) -> Vec<String> {
        vec!["local-name".into()]
    }

    /// Advertise every 100 to 150 ms. BlueZ's default, about 1.28 s, made
    /// a 5-second scan miss a Raspberry Pi Zero W one time in three: its
    /// Wi-Fi shares the radio and keeps scanning while offline. Read by
    /// BlueZ 5.55 and later, ignored by older versions.
    #[zbus(property)]
    fn min_interval(&self) -> u32 {
        ADVERTISING_INTERVAL_MS.0
    }

    #[zbus(property)]
    fn max_interval(&self) -> u32 {
        ADVERTISING_INTERVAL_MS.1
    }
}

/// Advertising interval, minimum and maximum, in milliseconds.
const ADVERTISING_INTERVAL_MS: (u32, u32) = (100, 150);

/// Object paths of one exported application.
#[derive(Clone)]
struct Paths {
    app: OwnedObjectPath,
    service: OwnedObjectPath,
    characteristics: Vec<(Kind, OwnedObjectPath)>,
}

impl Paths {
    fn new(instance: u32) -> Self {
        let app = format!("{ROOT}/app{instance}");
        let service = format!("{app}/service0");
        let characteristics = Kind::ALL
            .iter()
            .enumerate()
            .map(|(i, k)| (*k, path(&format!("{service}/char{i}"))))
            .collect();
        Self {
            app: path(&app),
            service: path(&service),
            characteristics,
        }
    }

    fn characteristic(&self, kind: Kind) -> &OwnedObjectPath {
        &self
            .characteristics
            .iter()
            .find(|(k, _)| *k == kind)
            .expect("every kind has a path")
            .1
    }
}

fn path(p: &str) -> OwnedObjectPath {
    OwnedObjectPath::try_from(p).expect("constant object paths are valid")
}

/// No options: the GATT and advertising APIs define none we need.
fn no_options() -> HashMap<&'static str, Value<'static>> {
    HashMap::new()
}

/// The Improv Wi-Fi GATT service on one BlueZ adapter.
pub struct BluezGatt {
    bus: Connection,
    adapter: Adapter,
    paths: Paths,
    values: Arc<Mutex<Values>>,
    writes: mpsc::Receiver<Vec<u8>>,
    advertisement: Option<OwnedObjectPath>,
    next_advertisement: u32,
    stopped: bool,
}

impl BluezGatt {
    /// Power on `adapter` and register the Improv Wi-Fi service with `caps`
    /// (see [`impromptu_wifi_ble::capabilities`]) on the system bus `bus`.
    /// Advertising starts with the driver's first state update.
    pub async fn start(
        bus: &Connection,
        adapter: &Adapter,
        caps: Capabilities,
    ) -> Result<Self, BluezError> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let paths = Paths::new(NEXT.fetch_add(1, Ordering::Relaxed));

        let props = PropertiesProxy::builder(bus)
            .destination(BLUEZ_SERVICE)?
            .path(adapter.path().clone())?
            .build()
            .await?;
        let iface = || ADAPTER_IFACE.try_into().expect("valid interface name");
        if let Err(e) = props.set(iface(), "Powered", Value::from(true)).await {
            // BlueZ 5.66 and later say why: `PowerState` is `off-blocked`
            // while rfkill blocks the adapter. Its own error is just "Failed".
            let state = props.get(iface(), "PowerState").await.ok();
            let blocked = state
                .and_then(|v| String::try_from(v).ok())
                .is_some_and(|s| s == "off-blocked");
            return Err(if blocked {
                BluezError::Blocked
            } else {
                e.into()
            });
        }

        let values = Arc::new(Mutex::new(Values::default()));
        let (tx, writes) = mpsc::channel::<Vec<u8>>(16);
        let server = bus.object_server();
        server.at(&paths.app, ObjectManager).await?;
        server
            .at(
                &paths.service,
                Service {
                    uuid: uuid_string(SERVICE_UUID),
                },
            )
            .await?;
        for (kind, p) in &paths.characteristics {
            let characteristic = Characteristic {
                kind: *kind,
                service: paths.service.clone(),
                values: values.clone(),
                caps: caps.bits(),
                writes: (*kind == Kind::RpcCommand).then(|| tx.clone()),
                notifying: false,
            };
            server.at(p, characteristic).await?;
        }
        drop(tx);

        let mut gatt = Self {
            bus: bus.clone(),
            adapter: adapter.clone(),
            paths,
            values,
            writes,
            advertisement: None,
            next_advertisement: 0,
            stopped: false,
        };
        // BlueZ reads the objects back with GetManagedObjects before it
        // replies; the object server answers that while this call waits.
        let registered = gatt
            .bus
            .call_method(
                Some(BLUEZ_SERVICE),
                adapter.path(),
                Some(GATT_MANAGER_IFACE),
                "RegisterApplication",
                &(&gatt.paths.app, no_options()),
            )
            .await;
        if let Err(e) = registered {
            gatt.remove_objects().await;
            gatt.stopped = true;
            return Err(e.into());
        }
        Ok(gatt)
    }

    /// The adapter this service is registered on.
    pub fn adapter(&self) -> &Adapter {
        &self.adapter
    }

    /// Unregister the advertisement and the GATT application, and remove the
    /// exported objects.
    pub async fn stop(mut self) {
        self.stopped = true;
        cleanup(
            self.bus.clone(),
            self.adapter.clone(),
            self.paths.clone(),
            self.advertisement.take(),
        )
        .await;
    }

    async fn remove_objects(&self) {
        remove_objects(&self.bus, &self.paths).await;
    }

    async fn notify(&self, kind: Kind) -> Result<(), BluezError> {
        let iface = self
            .bus
            .object_server()
            .interface::<_, Characteristic>(self.paths.characteristic(kind))
            .await?;
        let characteristic = iface.get().await;
        if characteristic.notifying {
            characteristic.value_changed(iface.signal_emitter()).await?;
        }
        Ok(())
    }

    /// Stop advertising, and keep the service registered for clients that
    /// are already connected. [`stop`](Self::stop) removes the rest.
    pub async fn stop_advertising(&mut self) {
        self.unregister_advertisement().await;
    }

    async fn unregister_advertisement(&mut self) {
        if let Some(p) = self.advertisement.take() {
            unregister_advertisement(&self.bus, &self.adapter, &p).await;
        }
    }
}

impl Drop for BluezGatt {
    fn drop(&mut self) {
        if self.stopped {
            return;
        }
        let task = cleanup(
            self.bus.clone(),
            self.adapter.clone(),
            self.paths.clone(),
            self.advertisement.take(),
        );
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                rt.spawn(task);
            }
            // BlueZ still removes both once this connection closes.
            Err(_) => log::warn!("BluezGatt dropped outside a tokio runtime; not unregistered"),
        }
    }
}

async fn cleanup(
    bus: Connection,
    adapter: Adapter,
    paths: Paths,
    advertisement: Option<OwnedObjectPath>,
) {
    if let Some(p) = advertisement {
        unregister_advertisement(&bus, &adapter, &p).await;
    }
    if let Err(e) = bus
        .call_method(
            Some(BLUEZ_SERVICE),
            adapter.path(),
            Some(GATT_MANAGER_IFACE),
            "UnregisterApplication",
            &(&paths.app,),
        )
        .await
    {
        log::warn!("unregistering the Improv Wi-Fi GATT application: {e}");
    }
    remove_objects(&bus, &paths).await;
}

async fn remove_objects(bus: &Connection, paths: &Paths) {
    let server = bus.object_server();
    for (_, p) in &paths.characteristics {
        let _ = server.remove::<Characteristic, _>(p).await;
    }
    let _ = server.remove::<Service, _>(&paths.service).await;
    let _ = server.remove::<ObjectManager, _>(&paths.app).await;
}

async fn unregister_advertisement(bus: &Connection, adapter: &Adapter, p: &OwnedObjectPath) {
    if let Err(e) = bus
        .call_method(
            Some(BLUEZ_SERVICE),
            adapter.path(),
            Some(ADVERTISING_MANAGER_IFACE),
            "UnregisterAdvertisement",
            &(p,),
        )
        .await
    {
        log::debug!("unregistering advertisement {p}: {e}");
    }
    let _ = bus.object_server().remove::<Advertisement, _>(p).await;
}

impl GattServer for BluezGatt {
    type Error = BluezError;

    async fn rpc_command_written(&mut self, buf: &mut [u8]) -> Result<usize, BluezError> {
        let chunk = self.writes.recv().await.ok_or(BluezError::Closed)?;
        let n = chunk.len().min(buf.len());
        buf[..n].copy_from_slice(&chunk[..n]);
        Ok(n)
    }

    async fn set_state(&mut self, value: u8) -> Result<(), BluezError> {
        self.values.lock().unwrap().state = value;
        self.notify(Kind::State).await
    }

    async fn set_error(&mut self, value: u8) -> Result<(), BluezError> {
        self.values.lock().unwrap().error = value;
        self.notify(Kind::Error).await
    }

    async fn set_rpc_result(&mut self, value: &[u8]) -> Result<(), BluezError> {
        self.values.lock().unwrap().result = value.to_vec();
        self.notify(Kind::RpcResult).await
    }

    async fn set_service_data(&mut self, data: [u8; 6]) -> Result<(), BluezError> {
        // Release the old advertisement first: adapters allow only a few.
        self.unregister_advertisement().await;
        let p = path(&format!(
            "{}/advertisement{}",
            self.paths.app.as_str(),
            self.next_advertisement
        ));
        self.next_advertisement += 1;
        self.bus
            .object_server()
            .at(&p, Advertisement { data })
            .await?;
        let registered = self
            .bus
            .call_method(
                Some(BLUEZ_SERVICE),
                self.adapter.path(),
                Some(ADVERTISING_MANAGER_IFACE),
                "RegisterAdvertisement",
                &(&p, no_options()),
            )
            .await;
        match registered {
            Ok(_) => self.advertisement = Some(p),
            // Provisioning still works for a client that's already connected,
            // so a failed advertisement shouldn't end the session.
            Err(e) => {
                log::warn!("advertising failed: {e}");
                let _ = self
                    .bus
                    .object_server()
                    .remove::<Advertisement, _>(&p)
                    .await;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_reads_return_the_rest_of_the_value() {
        let value: Vec<u8> = (0..=255).collect();
        assert_eq!(read_at(&value, 0).unwrap().len(), 256);
        assert_eq!(read_at(&value, 22).unwrap(), &value[22..]);
        assert_eq!(
            read_at(&value, 256).unwrap(),
            Vec::<u8>::new(),
            "reading at the end is empty, not an error"
        );
        assert_eq!(read_at(&value, 257), None);
    }

    #[test]
    fn uuids_use_the_bluez_text_form() {
        assert_eq!(
            uuid_string(SERVICE_UUID),
            "00467768-6228-2272-4663-277478268000"
        );
        assert_eq!(
            uuid_string(SERVICE_DATA_UUID),
            "00004677-0000-1000-8000-00805f9b34fb"
        );
    }
}
