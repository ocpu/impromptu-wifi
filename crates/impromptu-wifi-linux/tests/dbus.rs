//! The NetworkManager and hostnamed backends over real D-Bus: a private
//! `dbus-daemon`, with stand-in services written from the public D-Bus API
//! documentation of NetworkManager and systemd-hostnamed.
//!
//! This checks that the backends speak those APIs as documented, and their
//! own logic (security choice, profile clean-up, error mapping, URLs). It
//! can't prove the real services behave exactly like these stand-ins; run
//! `impromptu-wifi-device` on a real machine for that.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use impromptu_wifi_core::traits::{
    ConnectError, DeviceInfoSource, HostnameError, HostnameProvider, NetworkFlags, NetworkStatus,
    WifiAuth, WifiConnect, WifiScanner,
};
use impromptu_wifi_linux::hostname::LinuxDevice;
use impromptu_wifi_linux::nm::NmWifi;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::{interface, Connection, ObjectServer};

mod common;
use common::{connect, private_bus, Bus};

// --- the stand-in world --------------------------------------------------------

const PSK: u32 = 0x100;
const SAE: u32 = 0x400;

#[derive(Clone)]
struct Ap {
    ssid: &'static str,
    strength: u8,
    flags: u32,
    rsn: u32,
}

#[derive(Default)]
struct World {
    aps: Vec<Ap>,
    /// SSID -> the passphrase that works ("" for open networks).
    networks: HashMap<String, String>,
    /// Saved profiles: path and SSID.
    profiles: Vec<(OwnedObjectPath, Vec<u8>)>,
    /// What each AddAndActivateConnection call asked for: (setting, key) -> value.
    requests: Vec<HashMap<(String, String), String>>,
    active_states: HashMap<u32, u32>,
    nm_state: u32,
    wireless_enabled: bool,
    primary: Option<OwnedObjectPath>,
    next_id: u32,
    last_scan: i64,
    hostname: String,
    pretty: String,
}

type Shared = Arc<Mutex<World>>;

fn path(p: &str) -> OwnedObjectPath {
    OwnedObjectPath::try_from(p.to_string()).unwrap()
}

/// A readable rendering of a settings value for assertions.
fn render(v: &OwnedValue) -> String {
    if let Ok(bytes) = Vec::<u8>::try_from(v.try_clone().unwrap()) {
        return String::from_utf8_lossy(&bytes).into_owned();
    }
    if let Ok(b) = bool::try_from(&**v) {
        return b.to_string();
    }
    String::try_from(v.try_clone().unwrap()).unwrap_or_else(|_| format!("{v:?}"))
}

struct Root(Shared);

#[interface(name = "org.freedesktop.NetworkManager")]
impl Root {
    fn get_devices(&self) -> Vec<OwnedObjectPath> {
        vec![
            path("/org/freedesktop/NetworkManager/Devices/1"),
            path("/org/freedesktop/NetworkManager/Devices/2"),
        ]
    }

    async fn add_and_activate_connection(
        &self,
        connection: HashMap<String, HashMap<String, OwnedValue>>,
        _device: ObjectPath<'_>,
        _specific_object: ObjectPath<'_>,
        #[zbus(object_server)] server: &ObjectServer,
        #[zbus(connection)] conn: &Connection,
    ) -> zbus::fdo::Result<(OwnedObjectPath, OwnedObjectPath)> {
        let request: HashMap<(String, String), String> = connection
            .iter()
            .flat_map(|(setting, kv)| {
                kv.iter()
                    .map(move |(k, v)| ((setting.clone(), k.clone()), render(v)))
            })
            .collect();
        let ssid = request
            .get(&("802-11-wireless".into(), "ssid".into()))
            .cloned()
            .unwrap_or_default();
        let psk = request
            .get(&("802-11-wireless-security".into(), "psk".into()))
            .cloned()
            .unwrap_or_default();

        let (id, ok) = {
            let mut w = self.0.lock().unwrap();
            w.next_id += 1;
            let ok = w.networks.get(&ssid) == Some(&psk);
            w.requests.push(request);
            (w.next_id, ok)
        };
        let profile = path(&format!("/org/freedesktop/NetworkManager/Settings/{id}"));
        let active = path(&format!(
            "/org/freedesktop/NetworkManager/ActiveConnection/{id}"
        ));
        self.0
            .lock()
            .unwrap()
            .profiles
            .push((profile.clone(), ssid.clone().into_bytes()));
        self.0.lock().unwrap().active_states.insert(id, 1); // activating
        server
            .at(
                &profile,
                Profile {
                    world: self.0.clone(),
                    path: profile.clone(),
                },
            )
            .await?;
        server
            .at(
                &active,
                Active {
                    world: self.0.clone(),
                    id,
                    ok,
                },
            )
            .await?;
        if ok {
            server
                .at(
                    format!("/org/freedesktop/NetworkManager/IP4Config/{id}"),
                    Ip4,
                )
                .await?;
        }

        // Activation finishes a moment later, as it would after DHCP.
        let world = self.0.clone();
        let conn = conn.clone();
        let active_path = active.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            let (state, reason) = if ok { (2, 1) } else { (4, 9) }; // activated / no-secrets
            {
                let mut w = world.lock().unwrap();
                w.active_states.insert(id, state);
                if ok {
                    w.nm_state = 70;
                    w.primary = Some(active_path.clone());
                }
            }
            let emitter = SignalEmitter::new(&conn, active_path).unwrap();
            Active::activation_changed(&emitter, state, reason)
                .await
                .unwrap();
        });
        Ok((profile, active))
    }

    #[zbus(property)]
    fn state(&self) -> u32 {
        self.0.lock().unwrap().nm_state
    }

    #[zbus(property)]
    fn wireless_enabled(&self) -> bool {
        self.0.lock().unwrap().wireless_enabled
    }

    #[zbus(property)]
    fn primary_connection(&self) -> OwnedObjectPath {
        self.0
            .lock()
            .unwrap()
            .primary
            .clone()
            .unwrap_or_else(|| path("/"))
    }
}

struct Device(u32, &'static str);

#[interface(name = "org.freedesktop.NetworkManager.Device")]
impl Device {
    #[zbus(property)]
    fn device_type(&self) -> u32 {
        self.0
    }
    #[zbus(property)]
    fn interface(&self) -> String {
        self.1.into()
    }
}

struct Wireless(Shared);

#[interface(name = "org.freedesktop.NetworkManager.Device.Wireless")]
impl Wireless {
    fn request_scan(&self, _options: HashMap<String, OwnedValue>) {
        let world = self.0.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            world.lock().unwrap().last_scan += 1;
        });
    }
    fn get_all_access_points(&self) -> Vec<OwnedObjectPath> {
        let n = self.0.lock().unwrap().aps.len();
        (0..n)
            .map(|i| path(&format!("/org/freedesktop/NetworkManager/AccessPoint/{i}")))
            .collect()
    }
    #[zbus(property)]
    fn last_scan(&self) -> i64 {
        self.0.lock().unwrap().last_scan
    }
}

struct AccessPoint(Ap);

#[interface(name = "org.freedesktop.NetworkManager.AccessPoint")]
impl AccessPoint {
    #[zbus(property)]
    fn ssid(&self) -> Vec<u8> {
        self.0.ssid.as_bytes().to_vec()
    }
    #[zbus(property)]
    fn strength(&self) -> u8 {
        self.0.strength
    }
    #[zbus(property)]
    fn flags(&self) -> u32 {
        self.0.flags
    }
    #[zbus(property)]
    fn wpa_flags(&self) -> u32 {
        0
    }
    #[zbus(property)]
    fn rsn_flags(&self) -> u32 {
        self.0.rsn
    }
}

struct Active {
    world: Shared,
    id: u32,
    ok: bool,
}

#[interface(name = "org.freedesktop.NetworkManager.Connection.Active")]
impl Active {
    #[zbus(property)]
    fn state(&self) -> u32 {
        self.world.lock().unwrap().active_states[&self.id]
    }
    #[zbus(property)]
    fn ip4_config(&self) -> OwnedObjectPath {
        if self.ok {
            path(&format!(
                "/org/freedesktop/NetworkManager/IP4Config/{}",
                self.id
            ))
        } else {
            path("/")
        }
    }
    #[zbus(signal, name = "StateChanged")]
    async fn activation_changed(
        emitter: &SignalEmitter<'_>,
        state: u32,
        reason: u32,
    ) -> zbus::Result<()>;
}

struct Ip4;

#[interface(name = "org.freedesktop.NetworkManager.IP4Config")]
impl Ip4 {
    #[zbus(property)]
    fn address_data(&self) -> Vec<HashMap<String, OwnedValue>> {
        let mut a = HashMap::new();
        a.insert(
            "address".to_string(),
            OwnedValue::try_from(Value::from("192.168.1.50")).unwrap(),
        );
        a.insert("prefix".to_string(), OwnedValue::from(24u32));
        vec![a]
    }
}

struct Settings(Shared);

#[interface(name = "org.freedesktop.NetworkManager.Settings")]
impl Settings {
    fn list_connections(&self) -> Vec<OwnedObjectPath> {
        self.0
            .lock()
            .unwrap()
            .profiles
            .iter()
            .map(|(p, _)| p.clone())
            .collect()
    }
}

struct Profile {
    world: Shared,
    path: OwnedObjectPath,
}

#[interface(name = "org.freedesktop.NetworkManager.Settings.Connection")]
impl Profile {
    fn get_settings(&self) -> HashMap<String, HashMap<String, OwnedValue>> {
        let w = self.world.lock().unwrap();
        let ssid = w
            .profiles
            .iter()
            .find(|(p, _)| *p == self.path)
            .map(|(_, s)| s.clone())
            .unwrap_or_default();
        let mut wifi = HashMap::new();
        wifi.insert(
            "ssid".to_string(),
            OwnedValue::try_from(Value::from(ssid)).unwrap(),
        );
        let mut all = HashMap::new();
        all.insert("802-11-wireless".to_string(), wifi);
        all
    }

    async fn delete(&self, #[zbus(object_server)] server: &ObjectServer) -> zbus::fdo::Result<()> {
        self.world
            .lock()
            .unwrap()
            .profiles
            .retain(|(p, _)| *p != self.path);
        server.remove::<Profile, _>(&self.path).await?;
        Ok(())
    }
}

struct Hostnamed(Shared);

#[interface(name = "org.freedesktop.hostname1")]
impl Hostnamed {
    fn set_static_hostname(&self, hostname: String, _interactive: bool) -> zbus::fdo::Result<()> {
        if hostname.contains(' ') {
            return Err(zbus::fdo::Error::InvalidArgs(format!(
                "Invalid static hostname '{hostname}'"
            )));
        }
        self.0.lock().unwrap().hostname = hostname;
        Ok(())
    }
    fn set_pretty_hostname(&self, hostname: String, _interactive: bool) {
        self.0.lock().unwrap().pretty = hostname;
    }
    #[zbus(property)]
    fn hostname(&self) -> String {
        self.0.lock().unwrap().hostname.clone()
    }
    #[zbus(property)]
    fn static_hostname(&self) -> String {
        self.0.lock().unwrap().hostname.clone()
    }
    #[zbus(property)]
    fn pretty_hostname(&self) -> String {
        self.0.lock().unwrap().pretty.clone()
    }
}

/// Start the stand-in services on `bus`; return the shared world and the
/// service connection (keep it alive).
async fn services(bus: &Bus, world: World) -> (Shared, Connection) {
    let world: Shared = Arc::new(Mutex::new(world));
    let conn = connect(bus).await;
    let s = conn.object_server();
    s.at("/org/freedesktop/NetworkManager", Root(world.clone()))
        .await
        .unwrap();
    s.at(
        "/org/freedesktop/NetworkManager/Devices/1",
        Device(2, "wlan0"),
    )
    .await
    .unwrap();
    s.at(
        "/org/freedesktop/NetworkManager/Devices/1",
        Wireless(world.clone()),
    )
    .await
    .unwrap();
    s.at(
        "/org/freedesktop/NetworkManager/Devices/2",
        Device(1, "eth0"),
    )
    .await
    .unwrap();
    s.at(
        "/org/freedesktop/NetworkManager/Settings",
        Settings(world.clone()),
    )
    .await
    .unwrap();
    let aps = world.lock().unwrap().aps.clone();
    for (i, ap) in aps.into_iter().enumerate() {
        s.at(
            format!("/org/freedesktop/NetworkManager/AccessPoint/{i}"),
            AccessPoint(ap),
        )
        .await
        .unwrap();
    }
    let existing = world.lock().unwrap().profiles.clone();
    for (p, _) in existing {
        s.at(
            &p,
            Profile {
                world: world.clone(),
                path: p.clone(),
            },
        )
        .await
        .unwrap();
    }
    s.at("/org/freedesktop/hostname1", Hostnamed(world.clone()))
        .await
        .unwrap();
    conn.request_name("org.freedesktop.NetworkManager")
        .await
        .unwrap();
    conn.request_name("org.freedesktop.hostname1")
        .await
        .unwrap();
    (world, conn)
}

fn home_world() -> World {
    World {
        aps: vec![
            Ap {
                ssid: "home",
                strength: 90,
                flags: 1,
                rsn: PSK,
            },
            Ap {
                ssid: "home",
                strength: 40,
                flags: 1,
                rsn: PSK,
            },
            Ap {
                ssid: "lab",
                strength: 50,
                flags: 1,
                rsn: SAE,
            },
            Ap {
                ssid: "cafe",
                strength: 30,
                flags: 0,
                rsn: 0,
            },
            Ap {
                ssid: "",
                strength: 80,
                flags: 1,
                rsn: PSK,
            },
        ],
        networks: [
            ("home", "hunter22"),
            ("lab", "lab-pass"),
            ("secret", "hidden-pass"),
        ]
        .into_iter()
        .map(|(a, b)| (a.into(), b.into()))
        .collect(),
        nm_state: 20, // disconnected
        wireless_enabled: true,
        hostname: "raspberrypi".into(),
        ..World::default()
    }
}

// --- tests -----------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn scan_sorts_dedupes_and_maps_security() {
    let bus = private_bus().await;
    let (_world, _svc) = services(&bus, home_world()).await;
    let mut wifi = NmWifi::new(connect(&bus).await, None).await.unwrap();

    let mut out = heapless::Vec::<_, 8>::new();
    wifi.scan(&mut out).await.unwrap();
    let got: Vec<_> = out
        .iter()
        .map(|n| (n.ssid.as_str(), n.rssi_dbm, n.auth))
        .collect();
    assert_eq!(
        got,
        [
            ("home", -30, WifiAuth::Wpa2Personal),
            ("lab", -70, WifiAuth::Wpa3Personal),
            ("cafe", -90, WifiAuth::Open),
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn successful_join_replaces_old_profiles_and_reports_url() {
    let bus = private_bus().await;
    let mut world = home_world();
    world.profiles.push((
        path("/org/freedesktop/NetworkManager/Settings/old"),
        b"home".to_vec(),
    ));
    world.profiles.push((
        path("/org/freedesktop/NetworkManager/Settings/other"),
        b"office".to_vec(),
    ));
    let (world, _svc) = services(&bus, world).await;
    let mut wifi = NmWifi::new(connect(&bus).await, Some("wlan0"))
        .await
        .unwrap();

    assert_eq!(wifi.online_url().await, None, "offline at first");
    let url = wifi.connect("home", "hunter22").await.unwrap();
    assert_eq!(url.as_deref(), Some("http://192.168.1.50"));

    {
        let w = world.lock().unwrap();
        let req = &w.requests[0];
        let get = |s: &str, k: &str| req.get(&(s.to_string(), k.to_string())).cloned();
        assert_eq!(get("802-11-wireless", "ssid").as_deref(), Some("home"));
        assert_eq!(
            get("802-11-wireless-security", "key-mgmt").as_deref(),
            Some("wpa-psk")
        );
        assert_eq!(
            get("802-11-wireless-security", "psk").as_deref(),
            Some("hunter22")
        );
        assert_eq!(get("802-11-wireless", "hidden"), None, "visible network");
        let left: Vec<_> = w
            .profiles
            .iter()
            .map(|(p, _)| p.as_str().rsplit('/').next().unwrap().to_string())
            .collect();
        assert_eq!(
            left,
            ["other", "1"],
            "the old 'home' profile is gone, other networks untouched"
        );
    }

    let flags = wifi.network_flags().await;
    assert!(flags.is_online());
    assert_eq!(
        flags.0 & NetworkFlags::ETHERNET,
        NetworkFlags::ETHERNET,
        "eth0 reported"
    );
    assert_eq!(
        wifi.current_url().await.as_deref(),
        Some("http://192.168.1.50")
    );
    assert_eq!(
        wifi.online_url().await.flatten().as_deref(),
        Some("http://192.168.1.50")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_passphrase_is_reported_and_its_profile_removed() {
    let bus = private_bus().await;
    let (world, _svc) = services(&bus, home_world()).await;
    let mut wifi = NmWifi::new(connect(&bus).await, None).await.unwrap();

    assert_eq!(
        wifi.connect("home", "wrong").await,
        Err(ConnectError::InvalidCredentials)
    );
    assert!(
        wifi.last_error().unwrap().contains("reason 9"),
        "{:?}",
        wifi.last_error()
    );
    assert!(
        world.lock().unwrap().profiles.is_empty(),
        "failed profile deleted"
    );
    assert!(!wifi.network_flags().await.is_online());
}

#[tokio::test(flavor = "multi_thread")]
async fn wpa3_and_hidden_networks_get_the_right_settings() {
    let bus = private_bus().await;
    let (world, _svc) = services(&bus, home_world()).await;
    let mut wifi = NmWifi::new(connect(&bus).await, None).await.unwrap();

    wifi.connect("lab", "lab-pass").await.unwrap();
    wifi.connect("secret", "hidden-pass").await.unwrap();

    let w = world.lock().unwrap();
    let get =
        |i: usize, s: &str, k: &str| w.requests[i].get(&(s.to_string(), k.to_string())).cloned();
    assert_eq!(
        get(0, "802-11-wireless-security", "key-mgmt").as_deref(),
        Some("sae")
    );
    assert_eq!(get(1, "802-11-wireless", "hidden").as_deref(), Some("true"));
    assert_eq!(
        get(1, "802-11-wireless-security", "key-mgmt").as_deref(),
        Some("wpa-psk")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn hostname_and_device_name_through_hostnamed() {
    let bus = private_bus().await;
    let (world, _svc) = services(&bus, home_world()).await;
    let mut dev = LinuxDevice::new(&connect(&bus).await, "my-fw", "1.2.3")
        .await
        .unwrap();

    assert_eq!(dev.firmware_name(), "my-fw");
    assert_eq!(dev.hostname(), "raspberrypi");
    assert_eq!(
        dev.device_name(),
        "raspberrypi",
        "no pretty name yet: falls back"
    );

    dev.set_hostname("kitchen-pi").await.unwrap();
    assert_eq!(dev.hostname(), "kitchen-pi");
    assert_eq!(world.lock().unwrap().hostname, "kitchen-pi");
    assert_eq!(
        dev.set_hostname("bad name").await,
        Err(HostnameError::InvalidName)
    );
    assert_eq!(dev.hostname(), "kitchen-pi", "unchanged after a rejection");

    dev.set_device_name("Kitchen Display").await.unwrap();
    assert_eq!(dev.device_name(), "Kitchen Display");
    assert_eq!(world.lock().unwrap().pretty, "Kitchen Display");
}

// --- the whole stack ----------------------------------------------------------------

use embedded_io_adapters::tokio_1::FromTokio;
use impromptu_wifi_client::proto::State;
use impromptu_wifi_client::Client;
use impromptu_wifi_core::{Driver, NoAuth};
use impromptu_wifi_serial::{SerialClientLink, SerialLink};

#[tokio::test(flavor = "multi_thread")]
async fn client_provisions_a_linux_device_end_to_end() {
    let bus = private_bus().await;
    let (world, _svc) = services(&bus, home_world()).await;
    let conn = connect(&bus).await;
    let wifi = NmWifi::new(conn.clone(), None).await.unwrap();
    let dev = LinuxDevice::new(&conn, "my-fw", "1.2.3").await.unwrap();

    let (device_end, client_end) = tokio::io::duplex(4096);
    // Spawned on a multi-threaded runtime: the device future must be Send.
    tokio::spawn(async move {
        let mut driver = Driver::new(
            SerialLink::new(FromTokio::new(device_end)),
            wifi,
            dev,
            false,
        );
        let _ = driver.run(&mut NoAuth).await;
    });
    let mut client = Client::new(SerialClientLink::new(FromTokio::new(client_end)));

    assert_eq!(client.current_state().await.unwrap(), State::Authorized);
    let info = client.device_info().await.unwrap();
    assert_eq!(
        (info.firmware_name.as_str(), info.device_name.as_str()),
        ("my-fw", "raspberrypi")
    );

    let networks = client.scan::<8>().await.unwrap();
    assert_eq!(
        networks.iter().map(|n| n.ssid.as_str()).collect::<Vec<_>>(),
        ["home", "lab", "cafe"]
    );

    let url = client.provision("home", "hunter22").await.unwrap();
    assert_eq!(url.as_deref(), Some("http://192.168.1.50"));
    assert_eq!(client.current_state().await.unwrap(), State::Provisioned);

    assert_eq!(
        client.set_hostname("kitchen-pi").await.unwrap(),
        "kitchen-pi"
    );
    assert_eq!(world.lock().unwrap().hostname, "kitchen-pi");

    let net = client.network_state().await.unwrap();
    assert!(net.flags.is_online());
    assert_eq!(
        net.urls.iter().map(|u| u.as_str()).collect::<Vec<_>>(),
        ["http://192.168.1.50"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_already_online_reports_provisioned_after_a_restart() {
    let bus = private_bus().await;
    let (_world, _svc) = services(&bus, home_world()).await;
    let conn = connect(&bus).await;

    // Provision once, as before a reboot.
    NmWifi::new(conn.clone(), None)
        .await
        .unwrap()
        .connect("home", "hunter22")
        .await
        .unwrap();

    // "After the reboot": a fresh driver, told what the system already knows.
    let mut wifi = NmWifi::new(conn.clone(), None).await.unwrap();
    let online = wifi.online_url().await;
    assert_eq!(
        online.clone().flatten().as_deref(),
        Some("http://192.168.1.50")
    );
    let dev = LinuxDevice::new(&conn, "my-fw", "1.2.3").await.unwrap();

    let (device_end, client_end) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        let mut driver = Driver::new(
            SerialLink::new(FromTokio::new(device_end)),
            &mut wifi,
            dev,
            false,
        );
        if let Some(url) = online {
            driver.mark_provisioned(url);
        }
        let _ = driver.run(&mut NoAuth).await;
    });
    let mut client = Client::new(SerialClientLink::new(FromTokio::new(client_end)));
    assert_eq!(client.current_state().await.unwrap(), State::Provisioned);
    let net = client.network_state().await.unwrap();
    assert_eq!(
        net.urls.iter().map(|u| u.as_str()).collect::<Vec<_>>(),
        ["http://192.168.1.50"]
    );
}

/// Switch the stand-in NetworkManager's radio and announce it.
async fn set_radio(world: &Shared, svc: &Connection, enabled: bool) {
    world.lock().unwrap().wireless_enabled = enabled;
    let root = svc
        .object_server()
        .interface::<_, Root>("/org/freedesktop/NetworkManager")
        .await
        .unwrap();
    root.get()
        .await
        .wireless_enabled_changed(root.signal_emitter())
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn radio_switch_is_reported_as_stopped_over_serial() {
    use impromptu_wifi_client::ClientError;
    use impromptu_wifi_linux::nm::NmRadio;

    let bus = private_bus().await;
    let (world, svc) = services(&bus, home_world()).await;
    let conn = connect(&bus).await;
    let wifi = NmWifi::new(conn.clone(), None).await.unwrap();
    let dev = LinuxDevice::new(&conn, "my-fw", "1.2.3").await.unwrap();
    let mut radio = NmRadio::new(&conn).await.unwrap();
    assert!(radio.enabled());

    let (device_end, client_end) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        let mut driver = Driver::new(
            SerialLink::new(FromTokio::new(device_end)),
            wifi,
            dev,
            false,
        );
        driver.machine_mut().set_available(radio.enabled());
        let _ = driver.run_with(&mut NoAuth, &mut radio).await;
    });
    let mut client = Client::new(SerialClientLink::new(FromTokio::new(client_end)));
    assert_eq!(client.current_state().await.unwrap(), State::Authorized);

    // `nmcli radio wifi off`: the client is told without asking.
    set_radio(&world, &svc, false).await;
    tokio::time::timeout(
        Duration::from_secs(2),
        client.wait_for_state(State::Stopped),
    )
    .await
    .expect("the Stopped state was pushed")
    .unwrap();
    let refused = client.provision("home", "hunter22").await.unwrap_err();
    assert!(matches!(refused, ClientError::Device(_)), "got {refused:?}");
    assert!(
        world.lock().unwrap().requests.is_empty(),
        "nothing was sent to NetworkManager"
    );

    // And back on.
    set_radio(&world, &svc, true).await;
    tokio::time::timeout(
        Duration::from_secs(2),
        client.wait_for_state(State::Authorized),
    )
    .await
    .expect("the Authorized state was pushed")
    .unwrap();
    let url = client.provision("home", "hunter22").await.unwrap();
    assert_eq!(url.as_deref(), Some("http://192.168.1.50"));
}
