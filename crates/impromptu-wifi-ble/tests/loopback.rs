//! Conformance: the real BLE `Client` against the real `Driver`, over an
//! in-memory GATT connection.

// Each test drives one client from a single task on a single thread, so a
// `RefCell` borrow held across `.await` can never be contended.
#![allow(clippy::await_holding_refcell_ref)]

extern crate std;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
    vec::Vec,
};

use embassy_futures::{
    block_on,
    select::{select, Either},
    yield_now,
};
use impromptu_wifi_ble::{
    capabilities, BleClientLink, BleLink, GattClient, GattServer, Notification,
};
use impromptu_wifi_client::{Client, ClientError};
use impromptu_wifi_core::proto::ble::Capabilities;
use impromptu_wifi_core::proto::{ErrorCode, State};
use impromptu_wifi_core::{
    unsupported, AuthEvent, AuthSource, ConnectError, DeviceInfoSource, Driver, HostnameError,
    HostnameProvider, Identify, NetworkInfo, ScanError, Url, WifiAuth, WifiConnect, WifiScanner,
};

// --- an in-memory GATT connection --------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Note {
    State(u8),
    Error(u8),
    Result(Vec<u8>),
}

/// Everything both ends can see: characteristic values, queued writes and
/// notifications, the advertisement, and the device's button.
#[derive(Default)]
struct Air {
    writes: VecDeque<Vec<u8>>,
    write_sizes: Vec<usize>,
    notes: VecDeque<Note>,
    state: u8,
    caps: u8,
    service_data: Option<[u8; 6]>,
    button: bool,
}

type Shared = Rc<RefCell<Air>>;

#[derive(Debug, PartialEq)]
struct Stalled;

/// Poll `f` until it yields a value; give up if the test has deadlocked.
async fn wait<T>(mut f: impl FnMut() -> Option<T>) -> Result<T, Stalled> {
    for _ in 0..10_000 {
        if let Some(v) = f() {
            return Ok(v);
        }
        yield_now().await;
    }
    Err(Stalled)
}

struct Peripheral(Shared);

impl GattServer for Peripheral {
    type Error = Stalled;
    async fn rpc_command_written(&mut self, buf: &mut [u8]) -> Result<usize, Stalled> {
        let w = wait(|| self.0.borrow_mut().writes.pop_front()).await?;
        buf[..w.len()].copy_from_slice(&w);
        Ok(w.len())
    }
    async fn set_state(&mut self, v: u8) -> Result<(), Stalled> {
        let mut air = self.0.borrow_mut();
        air.state = v;
        air.notes.push_back(Note::State(v));
        Ok(())
    }
    async fn set_error(&mut self, v: u8) -> Result<(), Stalled> {
        self.0.borrow_mut().notes.push_back(Note::Error(v));
        Ok(())
    }
    async fn set_rpc_result(&mut self, v: &[u8]) -> Result<(), Stalled> {
        self.0
            .borrow_mut()
            .notes
            .push_back(Note::Result(v.to_vec()));
        Ok(())
    }
    async fn set_service_data(&mut self, d: [u8; 6]) -> Result<(), Stalled> {
        self.0.borrow_mut().service_data = Some(d);
        Ok(())
    }
}

struct Central {
    air: Shared,
    max_write_len: usize,
}

impl GattClient for Central {
    type Error = Stalled;
    fn max_write_len(&self) -> usize {
        self.max_write_len
    }
    async fn write_rpc_command(&mut self, chunk: &[u8]) -> Result<(), Stalled> {
        let mut air = self.air.borrow_mut();
        air.write_sizes.push(chunk.len());
        air.writes.push_back(chunk.to_vec());
        Ok(())
    }
    async fn notification(&mut self, buf: &mut [u8]) -> Result<Notification, Stalled> {
        Ok(
            match wait(|| self.air.borrow_mut().notes.pop_front()).await? {
                Note::State(v) => Notification::State(v),
                Note::Error(v) => Notification::Error(v),
                Note::Result(v) => {
                    buf[..v.len()].copy_from_slice(&v);
                    Notification::RpcResult(v.len())
                }
            },
        )
    }
    async fn read_state(&mut self) -> Result<u8, Stalled> {
        Ok(self.air.borrow().state)
    }
    async fn read_capabilities(&mut self) -> Result<u8, Stalled> {
        Ok(self.air.borrow().caps)
    }
}

/// The device's button: authorizes when the test "presses" it.
struct Button(Shared);

impl AuthSource for Button {
    async fn next(&mut self) -> AuthEvent {
        loop {
            if core::mem::take(&mut self.0.borrow_mut().button) {
                return AuthEvent::Authorize;
            }
            yield_now().await;
        }
    }
}

// --- the device ------------------------------------------------------------------

struct Wifi;

impl WifiConnect for Wifi {
    async fn connect(&mut self, ssid: &str, password: &str) -> Result<Option<Url>, ConnectError> {
        if ssid == "home" && password == "correct-horse-battery-staple" {
            Ok(Some(Url::try_from("http://lamp.local").unwrap()))
        } else {
            Err(ConnectError::InvalidCredentials)
        }
    }
}

impl WifiScanner for Wifi {
    async fn scan<const N: usize>(
        &mut self,
        out: &mut heapless::Vec<NetworkInfo, N>,
    ) -> Result<(), ScanError> {
        let found = [
            ("home", -40, WifiAuth::Wpa2Wpa3Personal),
            ("cafe", -71, WifiAuth::Open),
            ("lab", -80, WifiAuth::Owe),
        ];
        for (ssid, rssi, auth) in found {
            let _ = out.push(NetworkInfo::new(ssid, rssi, auth).unwrap());
        }
        Ok(())
    }
}

unsupported!(Wifi: NetworkStatus);

struct Lamp {
    blinks: Rc<Cell<u32>>,
    hostname: heapless::String<64>,
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
        self.blinks.set(self.blinks.get() + 1);
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

const CAPS: Capabilities = capabilities::<Wifi, Lamp>();

// --- harness ---------------------------------------------------------------------

type TestClient = Client<BleClientLink<Central>>;

/// Run `script` as the client while the driver serves it.
fn session<F: core::future::Future<Output = ()>>(
    max_write_len: usize,
    require_auth: bool,
    script: impl FnOnce(&'static RefCell<TestClient>, Shared, Rc<Cell<u32>>) -> F,
) {
    let air = Shared::default();
    air.borrow_mut().caps = CAPS.bits(); // the platform sets this once
    let blinks = Rc::new(Cell::new(0));
    let lamp = Lamp {
        blinks: blinks.clone(),
        hostname: heapless::String::try_from("lamp").unwrap(),
    };

    let mut driver = Driver::new(
        BleLink::new(Peripheral(air.clone()), CAPS),
        Wifi,
        lamp,
        require_auth,
    );
    let central = Central {
        air: air.clone(),
        max_write_len,
    };
    let client: &'static RefCell<TestClient> = Box::leak(Box::new(RefCell::new(Client::new(
        BleClientLink::new(central),
    ))));

    let mut button = Button(air.clone());
    match block_on(select(driver.run(&mut button), script(client, air, blinks))) {
        Either::First(Err(e)) => panic!("driver stopped: {e:?}"),
        Either::First(Ok(never)) => match never {},
        Either::Second(()) => {}
    }
}

// --- tests -------------------------------------------------------------------------

#[test]
fn authorization_flow_at_default_mtu() {
    session(20, true, |client, air, blinks| async move {
        let mut c = client.borrow_mut();

        // Discovery works before authorization.
        assert_eq!(c.capabilities().await.unwrap(), CAPS);
        assert_eq!(
            c.current_state().await.unwrap(),
            State::AuthorizationRequired
        );
        assert_eq!(
            air.borrow().service_data,
            Some([0x01, CAPS.bits(), 0, 0, 0, 0])
        );
        assert_eq!(c.device_info().await.unwrap().chip, "nrf52840");

        // Identify returns once written; the device blinks when it gets there.
        c.identify().await.unwrap();
        assert_eq!(c.device_info().await.unwrap().device_name, "Lamp");
        assert_eq!(blinks.get(), 1);

        // Credentials are refused until the button is pressed.
        assert!(matches!(
            c.provision("home", "correct-horse-battery-staple").await,
            Err(ClientError::Device(ErrorCode::NotAuthorized))
        ));
        air.borrow_mut().button = true;
        c.wait_for_state(State::Authorized).await.unwrap();

        // BLE scans carry the auth type.
        let networks = c.scan::<8>().await.unwrap();
        let summary: Vec<_> = networks
            .iter()
            .map(|n| (n.ssid.as_str(), n.requires_passphrase, n.auth.as_deref()))
            .collect();
        assert_eq!(
            summary,
            [
                ("home", true, Some("WPA2/WPA3")),
                ("cafe", false, Some("NO")),
                ("lab", false, Some("NO"))
            ]
        );

        // A wrong passphrase, then the right one, sent in 20-byte writes.
        assert!(matches!(
            c.provision("home", "wrong").await,
            Err(ClientError::Device(ErrorCode::UnableToConnect))
        ));
        air.borrow_mut().write_sizes.clear();
        let url = c
            .provision("home", "correct-horse-battery-staple")
            .await
            .unwrap();
        assert_eq!(url.as_deref(), Some("http://lamp.local"));
        let sizes = air.borrow().write_sizes.clone();
        assert!(
            sizes.len() > 1 && sizes.iter().all(|&n| n <= 20),
            "chunked: {sizes:?}"
        );

        assert_eq!(c.current_state().await.unwrap(), State::Provisioned);
        assert_eq!(
            air.borrow().service_data.unwrap()[0],
            State::Provisioned as u8
        );
    });
}

#[test]
fn larger_mtu_sends_one_write() {
    session(244, false, |client, air, _| async move {
        let mut c = client.borrow_mut();
        assert_eq!(c.current_state().await.unwrap(), State::Authorized);
        c.provision("home", "correct-horse-battery-staple")
            .await
            .unwrap();
        assert_eq!(air.borrow().write_sizes.len(), 1);
    });
}

#[test]
fn hostname_round_trip() {
    session(20, false, |client, _, _| async move {
        let mut c = client.borrow_mut();
        assert_eq!(c.hostname().await.unwrap(), "lamp");
        assert_eq!(c.set_hostname("lamp-2").await.unwrap(), "lamp-2");
        assert!(matches!(
            c.set_hostname("no spaces").await,
            Err(ClientError::Device(ErrorCode::BadHostname))
        ));
    });
}

#[test]
fn serial_only_commands_are_refused_locally() {
    session(20, false, |client, air, _| async move {
        let mut c = client.borrow_mut();
        assert!(matches!(
            c.network_state().await,
            Err(ClientError::Unsupported)
        ));
        assert!(air.borrow().write_sizes.is_empty(), "nothing was sent");
        // current_state reads the characteristic instead of sending 0x02.
        assert_eq!(c.current_state().await.unwrap(), State::Authorized);
        assert!(air.borrow().write_sizes.is_empty());
    });
}
