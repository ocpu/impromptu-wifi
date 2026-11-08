//! Both BlueZ sides end to end, over a private `dbus-daemon`: the host's
//! zbus central talks to `impromptu-wifi-linux`'s zbus peripheral through a
//! stand-in `org.bluez`.
//!
//! The stand-in plays both roles `bluetoothd` plays. Toward the device it
//! accepts the GATT application and the advertisement, reading them back as
//! `bluetoothd` does. Toward the central it shows the advertisement as an
//! `org.bluez.Device1`, and on `Connect` mirrors the application as remote
//! GATT objects, forwarding reads (as long reads in 22-byte pieces),
//! writes and notifications (cut to 20 bytes, as with a 23-byte MTU). It is
//! written from the BlueZ D-Bus API documentation; it can't prove
//! `bluetoothd` behaves identically.
#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use impromptu_wifi_ble::{capabilities, BleClientLink, BleLink};
use impromptu_wifi_client::{Client, ClientError};
use impromptu_wifi_core::proto::ble::Capabilities;
use impromptu_wifi_core::proto::State;
use impromptu_wifi_core::{
    unsupported, ConnectError, DeviceInfoSource, Driver, NoAuth, Url, WifiConnect,
};
use impromptu_wifi_host::ble::{self, BleAdapter, BleError, BleGattClient};
use impromptu_wifi_linux::bluez::{Adapter, BluezGatt};
use tokio::io::{AsyncBufReadExt, BufReader};
use zbus::fdo::{ManagedObjects, ObjectManager, ObjectManagerProxy, PropertiesProxy};
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::{interface, Connection};

const ADAPTER: &str = "/org/bluez/hci0";
const DEVICE: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_01";
const ADDRESS: &str = "AA:BB:CC:DD:EE:01";
const CHARACTERISTIC: &str = "org.bluez.GattCharacteristic1";
/// What a 23-byte ATT MTU leaves for a notification or a read piece.
const NOTIFY_LEN: usize = 20;
const READ_PIECE: usize = 22;

// --- a private bus -------------------------------------------------------------

struct Bus {
    address: String,
    _daemon: tokio::process::Child,
    _dir: std::path::PathBuf,
}

async fn private_bus() -> Bus {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "impromptu-wifi-host-dbus-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("bus.conf");
    std::fs::write(
        &config,
        format!(
            r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:dir={}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>"#,
            dir.display()
        ),
    )
    .unwrap();
    let mut daemon = tokio::process::Command::new("dbus-daemon")
        .arg(format!("--config-file={}", config.display()))
        .args(["--nofork", "--print-address"])
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("dbus-daemon must be installed to run these tests");
    let mut line = String::new();
    BufReader::new(daemon.stdout.take().unwrap())
        .read_line(&mut line)
        .await
        .unwrap();
    Bus {
        address: line.trim().to_string(),
        _daemon: daemon,
        _dir: dir,
    }
}

async fn connect(bus: &Bus) -> Connection {
    zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap()
}

// --- the stand-in bluetoothd -------------------------------------------------------

#[derive(Default)]
struct World {
    powered: bool,
    discovering: bool,
    filter: Option<HashMap<String, OwnedValue>>,
    /// Owner and objects of the device's GATT application.
    app: Option<(String, ManagedObjects)>,
    /// Path and properties of the device's advertisement.
    ad: Option<(OwnedObjectPath, HashMap<String, OwnedValue>)>,
    connected: bool,
    mirrors: Vec<OwnedObjectPath>,
}

type Shared = Arc<Mutex<World>>;

fn path(p: &str) -> OwnedObjectPath {
    OwnedObjectPath::try_from(p).unwrap()
}

fn get<T: TryFrom<OwnedValue>>(props: &HashMap<String, OwnedValue>, key: &str) -> Option<T> {
    T::try_from(props.get(key)?.try_clone().ok()?).ok()
}

struct FakeAdapter(Shared);

#[interface(name = "org.bluez.Adapter1")]
impl FakeAdapter {
    fn set_discovery_filter(&self, filter: HashMap<String, OwnedValue>) {
        self.0.lock().unwrap().filter = Some(filter);
    }

    fn start_discovery(&self) {
        self.0.lock().unwrap().discovering = true;
    }

    fn stop_discovery(&self) -> zbus::fdo::Result<()> {
        let mut w = self.0.lock().unwrap();
        if !w.discovering {
            return Err(zbus::fdo::Error::Failed("No discovery started".into()));
        }
        w.discovering = false;
        Ok(())
    }

    #[zbus(property)]
    fn powered(&self) -> bool {
        self.0.lock().unwrap().powered
    }

    #[zbus(property)]
    fn set_powered(&mut self, value: bool) {
        self.0.lock().unwrap().powered = value;
    }

    #[zbus(property)]
    fn discovering(&self) -> bool {
        self.0.lock().unwrap().discovering
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
        let owner = header.sender().unwrap().to_string();
        let objects = ObjectManagerProxy::builder(conn)
            .destination(owner.clone())?
            .path(application)?
            .build()
            .await?
            .get_managed_objects()
            .await?;
        self.0.lock().unwrap().app = Some((owner, objects));
        Ok(())
    }

    fn unregister_application(&self, _application: OwnedObjectPath) {
        self.0.lock().unwrap().app = None;
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
        let owner = header.sender().unwrap().to_string();
        let props = PropertiesProxy::builder(conn)
            .destination(owner)?
            .path(advertisement.clone())?
            .build()
            .await?
            .get_all("org.bluez.LEAdvertisement1".try_into().unwrap())
            .await?;
        self.0.lock().unwrap().ad = Some((advertisement, props));
        // The central sees a device once it advertises.
        let server = conn.object_server();
        if server.interface::<_, FakeDevice>(DEVICE).await.is_err() {
            server.at(DEVICE, FakeDevice(self.0.clone())).await?;
        } else {
            let device = server.interface::<_, FakeDevice>(DEVICE).await?;
            device
                .get()
                .await
                .service_data_changed(device.signal_emitter())
                .await?;
        }
        Ok(())
    }

    fn unregister_advertisement(&self, advertisement: OwnedObjectPath) {
        let mut w = self.0.lock().unwrap();
        if w.ad.as_ref().is_some_and(|(p, _)| *p == advertisement) {
            w.ad = None;
        }
    }
}

/// The device as the central sees it.
struct FakeDevice(Shared);

#[interface(name = "org.bluez.Device1")]
impl FakeDevice {
    async fn connect(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let (owner, objects) = self
            .0
            .lock()
            .unwrap()
            .app
            .clone()
            .ok_or_else(|| zbus::fdo::Error::Failed("not connectable".into()))?;
        let server = conn.object_server();
        let mut mirrors = Vec::new();
        let mut services = HashMap::new();
        for (p, ifaces) in &objects {
            let Some((_, props)) = ifaces
                .iter()
                .find(|(n, _)| n.as_str() == "org.bluez.GattService1")
            else {
                continue;
            };
            let m = path(&format!("{DEVICE}/service{:04x}", services.len() + 10));
            server
                .at(
                    &m,
                    MirrorService {
                        uuid: get(props, "UUID").unwrap(),
                    },
                )
                .await?;
            services.insert(p.clone(), m.clone());
            mirrors.push(m);
        }
        for (p, ifaces) in &objects {
            let Some((_, props)) = ifaces.iter().find(|(n, _)| n.as_str() == CHARACTERISTIC) else {
                continue;
            };
            let parent: OwnedObjectPath = get(props, "Service").unwrap();
            let service = services[&parent].clone();
            let m = path(&format!("{service}/char{:04x}", mirrors.len()));
            server
                .at(
                    &m,
                    MirrorCharacteristic {
                        owner: owner.clone(),
                        target: p.clone(),
                        uuid: get(props, "UUID").unwrap(),
                        flags: get(props, "Flags").unwrap(),
                        service,
                        value: Vec::new(),
                        notifying: false,
                    },
                )
                .await?;
            mirrors.push(m);
        }
        {
            let mut w = self.0.lock().unwrap();
            w.mirrors = mirrors;
            w.connected = true;
        }
        self.connected_changed(&emitter).await?;
        self.services_resolved_changed(&emitter).await?;
        Ok(())
    }

    async fn disconnect(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let mirrors = {
            let mut w = self.0.lock().unwrap();
            w.connected = false;
            std::mem::take(&mut w.mirrors)
        };
        let server = conn.object_server();
        for m in mirrors.iter().rev() {
            let _ = server.remove::<MirrorCharacteristic, _>(m).await;
            let _ = server.remove::<MirrorService, _>(m).await;
        }
        self.connected_changed(&emitter).await?;
        self.services_resolved_changed(&emitter).await?;
        Ok(())
    }

    #[zbus(property)]
    fn address(&self) -> String {
        ADDRESS.into()
    }

    #[zbus(property)]
    fn name(&self) -> String {
        "kitchen-pi".into()
    }

    #[zbus(property)]
    fn adapter(&self) -> OwnedObjectPath {
        path(ADAPTER)
    }

    #[zbus(property, name = "RSSI")]
    fn rssi(&self) -> i16 {
        -42
    }

    #[zbus(property, name = "UUIDs")]
    fn uuids(&self) -> Vec<String> {
        let w = self.0.lock().unwrap();
        w.ad.as_ref()
            .and_then(|(_, props)| get(props, "ServiceUUIDs"))
            .unwrap_or_default()
    }

    #[zbus(property)]
    fn service_data(&self) -> HashMap<String, OwnedValue> {
        let w = self.0.lock().unwrap();
        w.ad.as_ref()
            .and_then(|(_, props)| get(props, "ServiceData"))
            .unwrap_or_default()
    }

    #[zbus(property)]
    fn connected(&self) -> bool {
        self.0.lock().unwrap().connected
    }

    #[zbus(property)]
    fn services_resolved(&self) -> bool {
        self.0.lock().unwrap().connected
    }
}

struct MirrorService {
    uuid: String,
}

#[interface(name = "org.bluez.GattService1")]
impl MirrorService {
    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> String {
        self.uuid.clone()
    }

    #[zbus(property)]
    fn device(&self) -> OwnedObjectPath {
        path(DEVICE)
    }

    #[zbus(property)]
    fn primary(&self) -> bool {
        true
    }
}

/// A remote characteristic, forwarded to the device's local one.
struct MirrorCharacteristic {
    owner: String,
    target: OwnedObjectPath,
    uuid: String,
    flags: Vec<String>,
    service: OwnedObjectPath,
    value: Vec<u8>,
    notifying: bool,
}

async fn call_device(
    conn: &Connection,
    owner: &str,
    target: &ObjectPath<'_>,
    method: &str,
    body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
) -> zbus::fdo::Result<zbus::Message> {
    conn.call_method(Some(owner), target, Some(CHARACTERISTIC), method, body)
        .await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
}

#[interface(name = "org.bluez.GattCharacteristic1")]
impl MirrorCharacteristic {
    /// A long read, as bluetoothd does it: pieces at increasing offsets.
    async fn read_value(
        &self,
        _options: HashMap<String, OwnedValue>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<Vec<u8>> {
        let mut value = Vec::new();
        loop {
            let options = HashMap::from([("offset", Value::from(value.len() as u16))]);
            let piece: Vec<u8> =
                call_device(conn, &self.owner, &self.target, "ReadValue", &(options,))
                    .await?
                    .body()
                    .deserialize()?;
            let done = piece.len() < READ_PIECE;
            value.extend(piece.into_iter().take(READ_PIECE));
            if done {
                return Ok(value);
            }
        }
    }

    async fn write_value(
        &self,
        value: Vec<u8>,
        options: HashMap<String, OwnedValue>,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<()> {
        assert!(value.len() <= NOTIFY_LEN, "writes fit one packet");
        let kind: Option<String> = get(&options, "type");
        assert_eq!(kind.as_deref(), Some("request"), "writes with response");
        call_device(
            conn,
            &self.owner,
            &self.target,
            "WriteValue",
            &(value, HashMap::<&str, Value>::new()),
        )
        .await
        .map(drop)
    }

    async fn start_notify(
        &mut self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let mut changes = PropertiesProxy::builder(conn)
            .destination(self.owner.clone())?
            .path(self.target.clone())?
            .build()
            .await?
            .receive_properties_changed()
            .await?;
        call_device(conn, &self.owner, &self.target, "StartNotify", &()).await?;
        self.notifying = true;
        let conn = conn.clone();
        let mirror = emitter.path().to_owned();
        tokio::spawn(async move {
            while let Some(signal) = changes.next().await {
                let Ok(args) = signal.args() else { continue };
                let Some(value) = args.changed_properties().get("Value") else {
                    continue;
                };
                let Ok(mut bytes) = Vec::<u8>::try_from(value.try_clone().unwrap()) else {
                    continue;
                };
                bytes.truncate(NOTIFY_LEN);
                let Ok(iface) = conn
                    .object_server()
                    .interface::<_, MirrorCharacteristic>(&mirror)
                    .await
                else {
                    return; // disconnected
                };
                iface.get_mut().await.value = bytes;
                let _ = iface
                    .get()
                    .await
                    .value_changed(iface.signal_emitter())
                    .await;
            }
        });
        Ok(())
    }

    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> String {
        self.uuid.clone()
    }

    #[zbus(property)]
    fn service(&self) -> OwnedObjectPath {
        self.service.clone()
    }

    #[zbus(property)]
    fn flags(&self) -> Vec<String> {
        self.flags.clone()
    }

    #[zbus(property)]
    fn value(&self) -> Vec<u8> {
        self.value.clone()
    }

    #[zbus(property)]
    fn notifying(&self) -> bool {
        self.notifying
    }
}

async fn fake_bluetoothd(bus: &Bus) -> (Shared, Connection) {
    let world = Shared::default();
    let conn = connect(bus).await;
    let server = conn.object_server();
    server.at("/", ObjectManager).await.unwrap();
    server
        .at(ADAPTER, FakeAdapter(world.clone()))
        .await
        .unwrap();
    server
        .at(ADAPTER, FakeGattManager(world.clone()))
        .await
        .unwrap();
    server
        .at(ADAPTER, FakeAdvertisingManager(world.clone()))
        .await
        .unwrap();
    conn.request_name("org.bluez").await.unwrap();
    (world, conn)
}

// --- the device ----------------------------------------------------------------------

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

/// Start the device side and run its driver until `stop` fires.
async fn device(
    bus: &Bus,
    stop: tokio::sync::oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<Option<Url>> {
    let conn = connect(bus).await;
    let adapter = Adapter::first(&conn).await.unwrap();
    let gatt = BluezGatt::start(&conn, &adapter, CAPS).await.unwrap();
    tokio::spawn(async move {
        let mut driver = Driver::new(BleLink::new(gatt, CAPS), Wifi, Pi, false);
        let url = driver.provision(&mut NoAuth).await.unwrap();
        let _ = stop.await;
        driver.into_parts().0.into_inner().stop().await;
        url
    })
}

async fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..300 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting until {what}");
}

// --- tests ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn cli_side_provisions_linux_device_through_bluez() {
    let bus = private_bus().await;
    let (world, _bluetoothd) = fake_bluetoothd(&bus).await;
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let device = device(&bus, stop_rx).await;
    wait_until("the device advertises", || {
        world.lock().unwrap().ad.is_some()
    })
    .await;

    let adapter = BleAdapter::first_on(connect(&bus).await).await.unwrap();
    assert_eq!(adapter.path().as_str(), ADAPTER);

    let found = ble::discover(&adapter, Duration::from_millis(100))
        .await
        .unwrap();
    {
        let w = world.lock().unwrap();
        assert!(!w.discovering, "discovery stopped again");
        let filter = w.filter.as_ref().expect("a discovery filter was set");
        assert_eq!(
            get::<Vec<String>>(filter, "UUIDs").unwrap(),
            ["00467768-6228-2272-4663-277478268000"]
        );
        assert_eq!(get::<String>(filter, "Transport").unwrap(), "le");
    }
    assert_eq!(found.len(), 1);
    let d = &found[0];
    assert_eq!(
        (d.address.as_str(), d.name.as_deref(), d.rssi),
        (ADDRESS, Some("kitchen-pi"), Some(-42))
    );
    assert_eq!(d.state, Some(State::Authorized));
    assert_eq!(d.capabilities, Some(CAPS));

    let d = ble::find(&adapter, "KITCHEN", Duration::from_secs(1))
        .await
        .unwrap()
        .expect("found by part of its name, any case");
    let gatt = BleGattClient::connect(d.peripheral).await.unwrap();
    assert_eq!(gatt.device().as_str(), DEVICE);
    let mut client = Client::new(BleClientLink::new(gatt));

    assert_eq!(client.current_state().await.unwrap(), State::Authorized);
    assert_eq!(client.capabilities().await.unwrap(), CAPS);
    let info = client.device_info().await.unwrap();
    assert_eq!(
        (info.firmware_name.as_str(), info.chip.as_str()),
        ("impromptu-wifi-linux", "Raspberry Pi")
    );

    // Longer than one packet in both directions: a 16-byte SSID and a
    // 40-byte passphrase make a multi-chunk command, and the result needs a
    // long read after the cut notification.
    let err = client
        .provision(
            "home-network-24g",
            "0123456789012345678901234567890123456789",
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::Device(_)), "got {err:?}");

    let url = client.provision("home", "hunter22").await.unwrap();
    assert_eq!(url.as_deref(), Some("http://192.168.1.50"));
    assert_eq!(client.current_state().await.unwrap(), State::Provisioned);

    client.into_inner().into_inner().disconnect().await.unwrap();
    assert!(!world.lock().unwrap().connected);

    stop_tx.send(()).unwrap();
    assert_eq!(
        device.await.unwrap().as_deref(),
        Some("http://192.168.1.50")
    );
    wait_until("the device unregistered", || {
        let w = world.lock().unwrap();
        w.app.is_none() && w.ad.is_none()
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_link_ends_waiting_with_disconnected() {
    let bus = private_bus().await;
    let (world, bluetoothd) = fake_bluetoothd(&bus).await;
    let (_stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let _device = device(&bus, stop_rx).await;
    wait_until("the device advertises", || {
        world.lock().unwrap().ad.is_some()
    })
    .await;

    let adapter = BleAdapter::first_on(connect(&bus).await).await.unwrap();
    let d = ble::find(&adapter, ADDRESS, Duration::from_secs(1))
        .await
        .unwrap()
        .expect("found by address");
    let gatt = BleGattClient::connect(d.peripheral).await.unwrap();
    let mut client = Client::new(BleClientLink::new(gatt));
    assert_eq!(client.current_state().await.unwrap(), State::Authorized);

    let waiting = tokio::spawn(async move { client.wait_for_state(State::Provisioned).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    // The radio link drops: bluetoothd reports Connected = false.
    bluetoothd
        .call_method(
            Some("org.bluez"),
            DEVICE,
            Some("org.bluez.Device1"),
            "Disconnect",
            &(),
        )
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .expect("waiting ended")
        .unwrap();
    assert!(
        matches!(result, Err(ClientError::Link(BleError::Disconnected))),
        "got {result:?}"
    );
}
