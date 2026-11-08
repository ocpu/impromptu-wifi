//! The zbus BlueZ peripheral over real D-Bus: a private `dbus-daemon` with a
//! stand-in `org.bluez` written from the BlueZ D-Bus API documentation.
//!
//! The stand-in does what `bluetoothd` does with an external GATT
//! application: it reads the application's objects back with
//! `GetManagedObjects` during `RegisterApplication`, reads advertisement
//! properties during `RegisterAdvertisement`, and forwards a central's
//! reads, writes and subscriptions as `ReadValue`, `WriteValue`,
//! `StartNotify` and `Value` property changes. The real `Client` then
//! provisions the real `Driver` through it. This can't prove `bluetoothd`
//! behaves exactly like the stand-in; run `impromptu-wifi-device --ble` on
//! a real machine for that.

mod common;

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{connect, private_bus};
use futures::{Stream, StreamExt};
use impromptu_wifi_ble::{capabilities, BleClientLink, BleLink, GattClient, Notification};
use impromptu_wifi_client::Client;
use impromptu_wifi_core::proto::ble::{
    Capabilities, CAPABILITIES_UUID, CURRENT_STATE_UUID, ERROR_STATE_UUID, RPC_COMMAND_UUID,
    RPC_RESULT_UUID, SERVICE_DATA_UUID, SERVICE_UUID,
};
use impromptu_wifi_core::proto::State;
use impromptu_wifi_core::traits::{
    unsupported, ConnectError, DeviceInfoSource, NoAuth, Url, WifiConnect,
};
use impromptu_wifi_core::Driver;
use impromptu_wifi_linux::bluez::{Adapter, BluezGatt};
use zbus::fdo::{ManagedObjects, ObjectManager, ObjectManagerProxy, PropertiesProxy};
use zbus::message::Header;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{interface, Connection};

const ADAPTER: &str = "/org/bluez/hci0";
const CHARACTERISTIC: &str = "org.bluez.GattCharacteristic1";

/// Text form of a 128-bit UUID, as BlueZ uses it.
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

// --- the stand-in BlueZ --------------------------------------------------------

#[derive(Default)]
struct Registry {
    powered: bool,
    /// Owner (unique bus name), application root and its objects.
    app: Option<(String, OwnedObjectPath, ManagedObjects)>,
    /// Registered advertisements: path and properties.
    ads: Vec<(OwnedObjectPath, HashMap<String, OwnedValue>)>,
    ads_registered: usize,
}

type Shared = Arc<Mutex<Registry>>;

struct FakeAdapter(Shared);

#[interface(name = "org.bluez.Adapter1")]
impl FakeAdapter {
    #[zbus(property)]
    fn powered(&self) -> bool {
        self.0.lock().unwrap().powered
    }

    #[zbus(property)]
    fn set_powered(&mut self, value: bool) {
        self.0.lock().unwrap().powered = value;
    }
}

struct FakeGattManager(Shared);

#[interface(name = "org.bluez.GattManager1")]
impl FakeGattManager {
    async fn register_application(
        &self,
        application: OwnedObjectPath,
        _options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        let owner = header.sender().expect("calls have a sender").to_string();
        // bluetoothd reads the whole tree before replying.
        let objects = ObjectManagerProxy::builder(conn)
            .destination(owner.clone())?
            .path(application.clone())?
            .build()
            .await?
            .get_managed_objects()
            .await?;
        self.0.lock().unwrap().app = Some((owner, application, objects));
        Ok(())
    }

    async fn unregister_application(&self, application: OwnedObjectPath) -> zbus::fdo::Result<()> {
        let mut reg = self.0.lock().unwrap();
        match &reg.app {
            Some((_, p, _)) if *p == application => {
                reg.app = None;
                Ok(())
            }
            _ => Err(zbus::fdo::Error::Failed(
                "org.bluez.Error.DoesNotExist".into(),
            )),
        }
    }
}

struct FakeAdvertisingManager(Shared);

#[interface(name = "org.bluez.LEAdvertisingManager1")]
impl FakeAdvertisingManager {
    async fn register_advertisement(
        &self,
        advertisement: OwnedObjectPath,
        _options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        let owner = header.sender().expect("calls have a sender").to_string();
        let props = PropertiesProxy::builder(conn)
            .destination(owner)?
            .path(advertisement.clone())?
            .build()
            .await?
            .get_all("org.bluez.LEAdvertisement1".try_into().unwrap())
            .await?;
        let mut reg = self.0.lock().unwrap();
        reg.ads.push((advertisement, props));
        reg.ads_registered += 1;
        Ok(())
    }

    async fn unregister_advertisement(
        &self,
        advertisement: OwnedObjectPath,
    ) -> zbus::fdo::Result<()> {
        self.0
            .lock()
            .unwrap()
            .ads
            .retain(|(p, _)| *p != advertisement);
        Ok(())
    }
}

/// Start the stand-in on its own connection, as `org.bluez`.
async fn fake_bluez(bus: &common::Bus) -> (Shared, Connection) {
    let reg = Shared::default();
    let conn = connect(bus).await;
    let server = conn.object_server();
    server.at("/", ObjectManager).await.unwrap();
    server.at(ADAPTER, FakeAdapter(reg.clone())).await.unwrap();
    server
        .at(ADAPTER, FakeGattManager(reg.clone()))
        .await
        .unwrap();
    server
        .at(ADAPTER, FakeAdvertisingManager(reg.clone()))
        .await
        .unwrap();
    conn.request_name("org.bluez").await.unwrap();
    (reg, conn)
}

/// UUID → object path of each characteristic the application exported.
fn characteristics(objects: &ManagedObjects) -> HashMap<String, OwnedObjectPath> {
    objects
        .iter()
        .filter_map(|(path, ifaces)| {
            let props = ifaces
                .iter()
                .find(|(name, _)| name.as_str() == CHARACTERISTIC)?
                .1;
            let uuid = String::try_from(props.get("UUID")?.try_clone().ok()?).ok()?;
            Some((uuid, path.clone()))
        })
        .collect()
}

async fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..200 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting until {what}");
}

// --- a central, through the stand-in ----------------------------------------------

type Events = Pin<Box<dyn Stream<Item = (OwnedObjectPath, Vec<u8>)> + Send>>;

/// A `GattClient` that does what BlueZ does for a remote central: calls
/// the application's characteristic methods and listens for `Value`
/// changes.
struct Central {
    conn: Connection,
    owner: String,
    paths: HashMap<String, OwnedObjectPath>,
    events: Events,
}

impl Central {
    async fn new(conn: Connection, owner: String, paths: HashMap<String, OwnedObjectPath>) -> Self {
        let mut streams = Vec::new();
        for u in [CURRENT_STATE_UUID, ERROR_STATE_UUID, RPC_RESULT_UUID] {
            let path = paths[&uuid(u)].clone();
            let changes = PropertiesProxy::builder(&conn)
                .destination(owner.clone())
                .unwrap()
                .path(path.clone())
                .unwrap()
                .build()
                .await
                .unwrap()
                .receive_properties_changed()
                .await
                .unwrap();
            streams.push(Box::pin(changes.filter_map(move |signal| {
                let path = path.clone();
                async move {
                    let args = signal.args().ok()?;
                    let value = args.changed_properties().get("Value")?;
                    let bytes = Vec::<u8>::try_from(value.try_clone().ok()?).ok()?;
                    Some((path, bytes))
                }
            })) as Events);
        }
        let events: Events = Box::pin(futures::stream::select_all(streams));
        let central = Self {
            conn,
            owner,
            paths,
            events,
        };
        for u in [CURRENT_STATE_UUID, ERROR_STATE_UUID, RPC_RESULT_UUID] {
            central.call(u, "StartNotify", &()).await.unwrap();
        }
        central
    }

    async fn call<B>(&self, u: u128, method: &str, body: &B) -> zbus::Result<zbus::Message>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        self.conn
            .call_method(
                Some(self.owner.as_str()),
                &self.paths[&uuid(u)],
                Some(CHARACTERISTIC),
                method,
                body,
            )
            .await
    }

    async fn read(&self, u: u128, offset: u16) -> zbus::Result<Vec<u8>> {
        let options = HashMap::from([("offset", Value::from(offset))]);
        self.call(u, "ReadValue", &(options,))
            .await?
            .body()
            .deserialize()
    }

    /// A long read the way BlueZ does it: 22-byte pieces at increasing
    /// offsets until a short piece.
    async fn long_read(&self, u: u128) -> zbus::Result<Vec<u8>> {
        let mut value = Vec::new();
        loop {
            let piece = self.read(u, value.len() as u16).await?;
            let done = piece.len() < 22;
            value.extend(piece);
            if done {
                return Ok(value);
            }
        }
    }
}

impl GattClient for Central {
    type Error = zbus::Error;

    async fn write_rpc_command(&mut self, chunk: &[u8]) -> Result<(), zbus::Error> {
        let options = HashMap::from([("type", Value::from("request"))]);
        self.call(RPC_COMMAND_UUID, "WriteValue", &(chunk.to_vec(), options))
            .await
            .map(drop)
    }

    async fn notification(&mut self, buf: &mut [u8]) -> Result<Notification, zbus::Error> {
        let (path, value) = self
            .events
            .next()
            .await
            .ok_or_else(|| zbus::Error::Failure("signal stream ended".into()))?;
        let kind = self
            .paths
            .iter()
            .find(|(_, p)| **p == path)
            .map(|(u, _)| u.clone())
            .unwrap();
        if kind == uuid(CURRENT_STATE_UUID) {
            return Ok(Notification::State(value[0]));
        }
        if kind == uuid(ERROR_STATE_UUID) {
            return Ok(Notification::Error(value[0]));
        }
        // As a client with a 23-byte MTU would: read the full result.
        let full = self.long_read(RPC_RESULT_UUID).await?;
        assert_eq!(full, value, "the notification carried the whole value");
        buf[..full.len()].copy_from_slice(&full);
        Ok(Notification::RpcResult(full.len()))
    }

    async fn read_state(&mut self) -> Result<u8, zbus::Error> {
        Ok(self.read(CURRENT_STATE_UUID, 0).await?[0])
    }

    async fn read_capabilities(&mut self) -> Result<u8, zbus::Error> {
        Ok(self.read(CAPABILITIES_UUID, 0).await?[0])
    }
}

// --- the device's backends ---------------------------------------------------------

struct Wifi;

impl WifiConnect for Wifi {
    async fn connect(&mut self, ssid: &str, password: &str) -> Result<Option<Url>, ConnectError> {
        if (ssid, password) == ("home", "hunter22") {
            Ok(Some(Url::try_from("http://192.168.1.50").unwrap()))
        } else {
            Err(ConnectError::InvalidCredentials)
        }
    }
}
unsupported!(Wifi: WifiScanner, NetworkStatus);

struct Pi;

impl DeviceInfoSource for Pi {
    fn firmware_name(&self) -> &str {
        "impromptu-wifi-linux"
    }
    fn firmware_version(&self) -> &str {
        "0.1.0"
    }
    fn chip(&self) -> &str {
        "Raspberry Pi"
    }
    fn device_name(&self) -> &str {
        "kitchen-pi"
    }
}
unsupported!(Pi: Identify, HostnameProvider);

const CAPS: Capabilities = capabilities::<Wifi, Pi>();

// --- tests ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn registers_the_improv_wifi_application_as_bluez_expects() {
    let bus = private_bus().await;
    let (reg, _bluez) = fake_bluez(&bus).await;
    let conn = connect(&bus).await;

    let adapter = Adapter::first(&conn).await.unwrap();
    assert_eq!(adapter.name(), "hci0");
    let gatt = BluezGatt::start(&conn, &adapter, CAPS).await.unwrap();
    assert!(reg.lock().unwrap().powered, "the adapter was powered on");

    let (owner, objects) = {
        let reg = reg.lock().unwrap();
        let (owner, _, objects) = reg.app.clone().expect("application registered");
        (owner, objects)
    };
    assert_eq!(owner, conn.unique_name().unwrap().to_string());

    let services: Vec<_> = objects
        .iter()
        .filter_map(|(path, ifaces)| {
            let props = ifaces
                .iter()
                .find(|(n, _)| n.as_str() == "org.bluez.GattService1")?
                .1;
            Some((path.clone(), props.clone()))
        })
        .collect();
    assert_eq!(services.len(), 1, "one service");
    let (service_path, service) = &services[0];
    assert_eq!(
        String::try_from(service["UUID"].try_clone().unwrap()).unwrap(),
        uuid(SERVICE_UUID)
    );
    assert!(bool::try_from(&service["Primary"]).unwrap());

    let chars = characteristics(&objects);
    let mut uuids: Vec<_> = chars.keys().cloned().collect();
    uuids.sort();
    let mut expected: Vec<_> = [
        CURRENT_STATE_UUID,
        ERROR_STATE_UUID,
        RPC_COMMAND_UUID,
        RPC_RESULT_UUID,
        CAPABILITIES_UUID,
    ]
    .map(uuid)
    .to_vec();
    expected.sort();
    assert_eq!(uuids, expected, "the five Improv Wi-Fi characteristics");

    for (u, path) in &chars {
        let props = &objects[path]
            .iter()
            .find(|(n, _)| n.as_str() == CHARACTERISTIC)
            .unwrap()
            .1;
        let parent = OwnedObjectPath::try_from(props["Service"].try_clone().unwrap()).unwrap();
        assert_eq!(&parent, service_path, "{u} belongs to the service");
        let flags = Vec::<String>::try_from(props["Flags"].try_clone().unwrap()).unwrap();
        let want: &[&str] = if *u == uuid(RPC_COMMAND_UUID) {
            &["write", "write-without-response"]
        } else if *u == uuid(CAPABILITIES_UUID) {
            &["read"]
        } else {
            &["read", "notify"]
        };
        assert_eq!(flags, want, "flags of {u}");
    }

    gatt.stop().await;
    assert!(reg.lock().unwrap().app.is_none(), "unregistered on stop");
}

#[tokio::test(flavor = "multi_thread")]
async fn reads_honour_offsets_and_reject_bad_ones() {
    let bus = private_bus().await;
    let (reg, bluez) = fake_bluez(&bus).await;
    let conn = connect(&bus).await;
    let adapter = Adapter::first(&conn).await.unwrap();
    let gatt = BluezGatt::start(&conn, &adapter, CAPS).await.unwrap();

    let (owner, objects) = {
        let reg = reg.lock().unwrap();
        let (owner, _, objects) = reg.app.clone().unwrap();
        (owner, objects)
    };
    let central = Central::new(bluez.clone(), owner, characteristics(&objects)).await;
    assert_eq!(
        central.read(CAPABILITIES_UUID, 0).await.unwrap(),
        [CAPS.bits()]
    );
    assert_eq!(
        central.read(CAPABILITIES_UUID, 1).await.unwrap(),
        Vec::<u8>::new()
    );
    let err = central.read(CAPABILITIES_UUID, 2).await.unwrap_err();
    assert!(
        err.to_string().contains("org.bluez.Error.InvalidOffset"),
        "got {err}"
    );
    let err = central.read(RPC_COMMAND_UUID, 0).await.unwrap_err();
    assert!(
        err.to_string().contains("org.bluez.Error.NotSupported"),
        "got {err}"
    );
    gatt.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn client_provisions_through_bluez_end_to_end() {
    let bus = private_bus().await;
    let (reg, bluez) = fake_bluez(&bus).await;
    let conn = connect(&bus).await;
    let adapter = Adapter::first(&conn).await.unwrap();
    let gatt = BluezGatt::start(&conn, &adapter, CAPS).await.unwrap();

    let (owner, objects) = {
        let reg = reg.lock().unwrap();
        let (owner, _, objects) = reg.app.clone().unwrap();
        (owner, objects)
    };
    let central = Central::new(bluez.clone(), owner, characteristics(&objects)).await;

    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
    let device = tokio::spawn(async move {
        let mut driver = Driver::new(BleLink::new(gatt, CAPS), Wifi, Pi, false);
        let url = driver.provision(&mut NoAuth).await.unwrap();
        // The client reads the full result after being notified.
        let _ = done_rx.await;
        driver.into_parts().0.into_inner().stop().await;
        url
    });

    let mut client = Client::new(BleClientLink::new(central));
    // The driver publishes its first state once its task runs; until then
    // the characteristic holds its initial value.
    let mut state = client.current_state().await.unwrap();
    for _ in 0..200 {
        if state == State::Authorized {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        state = client.current_state().await.unwrap();
    }
    assert_eq!(state, State::Authorized);
    assert_eq!(client.capabilities().await.unwrap(), CAPS);

    wait_until("the first advertisement is registered", || {
        !reg.lock().unwrap().ads.is_empty()
    })
    .await;
    {
        let reg = reg.lock().unwrap();
        let (_, ad) = &reg.ads[0];
        assert_eq!(
            String::try_from(ad["Type"].try_clone().unwrap()).unwrap(),
            "peripheral"
        );
        assert_eq!(
            Vec::<String>::try_from(ad["ServiceUUIDs"].try_clone().unwrap()).unwrap(),
            [uuid(SERVICE_UUID)]
        );
        let data = HashMap::<String, OwnedValue>::try_from(ad["ServiceData"].try_clone().unwrap())
            .unwrap();
        let bytes =
            Vec::<u8>::try_from(data[&uuid(SERVICE_DATA_UUID)].try_clone().unwrap()).unwrap();
        assert_eq!(bytes.len(), 6, "Improv Wi-Fi service data is 6 bytes");
        assert_eq!(bytes[0], State::Authorized as u8);
    }

    let info = client.device_info().await.unwrap();
    assert_eq!(
        (info.firmware_name.as_str(), info.device_name.as_str()),
        ("impromptu-wifi-linux", "kitchen-pi")
    );

    let url = client.provision("home", "hunter22").await.unwrap();
    assert_eq!(url.as_deref(), Some("http://192.168.1.50"));
    assert_eq!(client.current_state().await.unwrap(), State::Provisioned);

    done_tx.send(()).unwrap();
    let device_url = device.await.unwrap();
    assert_eq!(device_url.as_deref(), Some("http://192.168.1.50"));

    let reg = reg.lock().unwrap();
    assert!(reg.app.is_none(), "application unregistered");
    assert!(reg.ads.is_empty(), "advertisement unregistered");
    assert!(
        reg.ads_registered >= 2,
        "a new advertisement for each state change"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_without_stop_still_unregisters() {
    let bus = private_bus().await;
    let (reg, _bluez) = fake_bluez(&bus).await;
    let conn = connect(&bus).await;
    let adapter = Adapter::first(&conn).await.unwrap();
    let gatt = BluezGatt::start(&conn, &adapter, CAPS).await.unwrap();
    assert!(reg.lock().unwrap().app.is_some());
    drop(gatt);
    wait_until("the application is unregistered", || {
        reg.lock().unwrap().app.is_none()
    })
    .await;
}
