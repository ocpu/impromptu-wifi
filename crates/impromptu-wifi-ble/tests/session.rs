//! End-to-end: a scripted BLE client talks to `Driver` + `BleLink`.

extern crate std;
use std::{collections::VecDeque, string::String, vec::Vec};

use embassy_futures::{block_on, yield_now};
use impromptu_wifi_ble::{capabilities, BleLink, GattServer};
use impromptu_wifi_core::proto::ble::{Capabilities, MAX_RPC_FRAME};
use impromptu_wifi_core::proto::{Command, ErrorCode, RawRpc, State, Transport};
use impromptu_wifi_core::{
    unsupported, AuthEvent, AuthSource, ConnectError, DeviceInfoSource, Driver, HostnameError,
    HostnameProvider, Identify, NetworkInfo, ScanError, Url, WifiAuth, WifiConnect, WifiScanner,
};

#[derive(Debug, PartialEq)]
enum Seen {
    State(State),
    Error(ErrorCode),
    Result(u8, Vec<String>),
    ServiceData([u8; 6]),
}

#[derive(Default)]
struct FakeGatt {
    writes: VecDeque<Vec<u8>>,
    seen: Vec<Seen>,
}

impl FakeGatt {
    /// Queue a command, split into 20-byte writes like a phone would.
    fn client_writes(&mut self, cmd: Command<'_>) {
        let mut buf = [0u8; MAX_RPC_FRAME];
        let n = cmd.encode(Transport::Ble, &mut buf).unwrap();
        self.writes.extend(buf[..n].chunks(20).map(<[u8]>::to_vec));
    }
}

impl GattServer for FakeGatt {
    type Error = &'static str;

    async fn rpc_command_written(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        yield_now().await; // let a ready AuthSource win the race first
        let w = self.writes.pop_front().ok_or("disconnected")?;
        buf[..w.len()].copy_from_slice(&w);
        Ok(w.len())
    }
    async fn set_state(&mut self, v: u8) -> Result<(), Self::Error> {
        self.seen.push(Seen::State(State::try_from(v).unwrap()));
        Ok(())
    }
    async fn set_error(&mut self, v: u8) -> Result<(), Self::Error> {
        self.seen.push(Seen::Error(ErrorCode::try_from(v).unwrap()));
        Ok(())
    }
    async fn set_rpc_result(&mut self, v: &[u8]) -> Result<(), Self::Error> {
        let raw = RawRpc::parse(v, Transport::Ble).expect("result checksum must be valid");
        let strings = raw.strings().map(|s| String::from(s.unwrap())).collect();
        self.seen.push(Seen::Result(raw.command, strings));
        Ok(())
    }
    async fn set_service_data(&mut self, d: [u8; 6]) -> Result<(), Self::Error> {
        self.seen.push(Seen::ServiceData(d));
        Ok(())
    }
}

/// Authorizes once, immediately, then never again.
struct PressedOnce(bool);

impl AuthSource for PressedOnce {
    async fn next(&mut self) -> AuthEvent {
        if core::mem::take(&mut self.0) {
            AuthEvent::Authorize
        } else {
            core::future::pending().await
        }
    }
}

struct FakeWifi;

impl WifiConnect for FakeWifi {
    async fn connect(&mut self, _ssid: &str, _pw: &str) -> Result<Option<Url>, ConnectError> {
        Ok(Some(Url::try_from("http://lamp.local").unwrap()))
    }
}

impl WifiScanner for FakeWifi {
    async fn scan<const N: usize>(
        &mut self,
        out: &mut heapless::Vec<NetworkInfo, N>,
    ) -> Result<(), ScanError> {
        let found = [
            ("home", -40, WifiAuth::Wpa2Wpa3Personal),
            ("cafe", -71, WifiAuth::Open),
        ];
        for (ssid, rssi, auth) in found {
            let _ = out.push(NetworkInfo::new(ssid, rssi, auth).unwrap());
        }
        Ok(())
    }
}

unsupported!(FakeWifi: NetworkStatus);

/// A device that supports identify and hostname.
struct Lamp {
    hostname: heapless::String<64>,
    identified: bool,
}

impl Lamp {
    fn new() -> Self {
        Self {
            hostname: heapless::String::try_from("lamp").unwrap(),
            identified: false,
        }
    }
}

impl DeviceInfoSource for Lamp {
    fn firmware_name(&self) -> &str {
        "lamp-fw"
    }
    fn firmware_version(&self) -> &str {
        "2.0.0"
    }
    fn chip(&self) -> &str {
        "nrf52840"
    }
    fn device_name(&self) -> &str {
        "Lamp"
    }
}

impl Identify for Lamp {
    async fn identify(&mut self) {
        self.identified = true;
    }
}

impl HostnameProvider for Lamp {
    fn hostname(&self) -> &str {
        &self.hostname
    }
    async fn set_hostname(&mut self, name: &str) -> Result<(), HostnameError> {
        self.hostname = heapless::String::try_from(name).map_err(|_| HostnameError::InvalidName)?;
        Ok(())
    }
}

/// A device with only the required trait.
struct Bare;

impl DeviceInfoSource for Bare {
    fn firmware_name(&self) -> &str {
        "bare"
    }
    fn firmware_version(&self) -> &str {
        "0.0.1"
    }
    fn chip(&self) -> &str {
        "rp2350"
    }
    fn device_name(&self) -> &str {
        "Bare"
    }
}

unsupported!(Bare: Identify, HostnameProvider);

fn strings(s: &[&str]) -> Vec<String> {
    s.iter().map(|s| String::from(*s)).collect()
}

// Computed at compile time from the traits' `SUPPORTED` constants.
const LAMP_CAPS: Capabilities = capabilities::<FakeWifi, Lamp>();
const BARE_CAPS: Capabilities = capabilities::<FakeWifi, Bare>();

#[test]
fn capabilities_follow_the_impls() {
    let all = Capabilities::IDENTIFY
        | Capabilities::SCAN
        | Capabilities::DEVICE_INFO
        | Capabilities::HOSTNAME;
    assert_eq!(LAMP_CAPS, all);
    assert_eq!(BARE_CAPS, Capabilities::DEVICE_INFO | Capabilities::SCAN);
}

#[test]
fn authorized_provisioning_over_chunked_writes() {
    let mut gatt = FakeGatt::default();
    gatt.client_writes(Command::WifiSettings {
        ssid: "home",
        password: "a-much-longer-passphrase",
    });

    let caps = LAMP_CAPS;
    let link = BleLink::new(gatt, caps);
    let mut driver = Driver::new(link, FakeWifi, Lamp::new(), true);

    let url = block_on(driver.provision(&mut PressedOnce(true))).unwrap();
    assert_eq!(url.as_deref(), Some("http://lamp.local"));

    let (link, _, _) = driver.into_parts();
    let c = caps.bits();
    assert_eq!(
        link.into_inner().seen,
        [
            Seen::Error(ErrorCode::None),
            Seen::State(State::AuthorizationRequired),
            Seen::ServiceData([0x01, c, 0, 0, 0, 0]),
            Seen::State(State::Authorized),
            Seen::ServiceData([0x02, c, 0, 0, 0, 0]),
            Seen::Error(ErrorCode::None),
            Seen::State(State::Provisioning),
            Seen::ServiceData([0x03, c, 0, 0, 0, 0]),
            Seen::State(State::Provisioned),
            Seen::ServiceData([0x04, c, 0, 0, 0, 0]),
            Seen::Result(0x01, strings(&["http://lamp.local"])),
        ]
    );
}

#[test]
fn credentials_before_authorization_are_refused() {
    let mut gatt = FakeGatt::default();
    gatt.client_writes(Command::WifiSettings {
        ssid: "home",
        password: "pw",
    });
    let mut driver = Driver::new(BleLink::new(gatt, LAMP_CAPS), FakeWifi, Lamp::new(), true);

    // No authorization ever arrives; the link ends after the one command.
    let err = block_on(driver.provision(&mut impromptu_wifi_core::NoAuth)).unwrap_err();
    assert_eq!(err, "disconnected");
    let (link, _, _) = driver.into_parts();
    assert!(link
        .into_inner()
        .seen
        .contains(&Seen::Error(ErrorCode::NotAuthorized)));
}

#[test]
fn ble_scan_is_one_result_of_triples() {
    let mut gatt = FakeGatt::default();
    gatt.client_writes(Command::ScanNetworks);
    let mut driver = Driver::new(BleLink::new(gatt, LAMP_CAPS), FakeWifi, Lamp::new(), false);
    let _ = block_on(driver.provision(&mut impromptu_wifi_core::NoAuth));

    let (link, _, _) = driver.into_parts();
    let seen = link.into_inner().seen;
    assert_eq!(
        seen.last(),
        Some(&Seen::Result(
            0x04,
            strings(&["home", "-40", "WPA2/WPA3", "cafe", "-71", "NO"])
        ))
    );
}

#[test]
fn identify_and_hostname_on_a_capable_device() {
    let mut gatt = FakeGatt::default();
    gatt.client_writes(Command::Identify);
    gatt.client_writes(Command::Hostname(Some("lamp-2")));
    gatt.client_writes(Command::Hostname(Some("not valid")));
    gatt.client_writes(Command::Hostname(None));
    let mut driver = Driver::new(BleLink::new(gatt, LAMP_CAPS), FakeWifi, Lamp::new(), false);
    let _ = block_on(driver.provision(&mut impromptu_wifi_core::NoAuth));

    assert!(driver.device().identified);
    let (link, _, lamp) = driver.into_parts();
    assert_eq!(lamp.hostname.as_str(), "lamp-2");
    let seen = link.into_inner().seen;
    let tail: Vec<_> = seen
        .iter()
        .filter(|s| !matches!(s, Seen::Error(ErrorCode::None)))
        .skip(2)
        .collect();
    assert_eq!(
        tail,
        [
            &Seen::Result(0x05, strings(&["lamp-2"])),
            &Seen::Error(ErrorCode::BadHostname),
            &Seen::Result(0x05, strings(&["lamp-2"])),
        ]
    );
}

#[test]
fn hostname_on_a_bare_device_is_unknown_command() {
    let mut gatt = FakeGatt::default();
    gatt.client_writes(Command::Hostname(None));
    let mut driver = Driver::new(BleLink::new(gatt, BARE_CAPS), FakeWifi, Bare, false);
    let _ = block_on(driver.provision(&mut impromptu_wifi_core::NoAuth));
    let (link, _, _) = driver.into_parts();
    assert_eq!(
        link.into_inner().seen.last(),
        Some(&Seen::Error(ErrorCode::UnknownCommand))
    );
}

/// Authorizes once, immediately, and counts timeout restarts.
struct CountingAuth {
    pressed: bool,
    restarts: usize,
}

impl AuthSource for CountingAuth {
    async fn next(&mut self) -> AuthEvent {
        if core::mem::take(&mut self.pressed) {
            AuthEvent::Authorize
        } else {
            core::future::pending().await
        }
    }

    fn restart_timeout(&mut self) {
        self.restarts += 1;
    }
}

struct FailingWifi;

impl WifiConnect for FailingWifi {
    async fn connect(&mut self, _ssid: &str, _pw: &str) -> Result<Option<Url>, ConnectError> {
        Err(ConnectError::InvalidCredentials)
    }
}

unsupported!(FailingWifi: WifiScanner, NetworkStatus);

#[test]
fn driver_restarts_the_auth_timeout_where_the_spec_says() {
    let mut gatt = FakeGatt::default();
    gatt.client_writes(Command::Hostname(Some("kitchen-lamp")));
    gatt.client_writes(Command::Hostname(None));
    gatt.client_writes(Command::WifiSettings {
        ssid: "home",
        password: "wrong",
    });
    let caps = capabilities::<FailingWifi, Lamp>();
    let mut driver = Driver::new(BleLink::new(gatt, caps), FailingWifi, Lamp::new(), true);
    let mut auth = CountingAuth {
        pressed: true,
        restarts: 0,
    };

    let err = block_on(driver.provision(&mut auth)).unwrap_err();
    assert_eq!(err, "disconnected");
    assert_eq!(
        auth.restarts, 2,
        "the hostname write and the failed join; not the hostname read"
    );
}
