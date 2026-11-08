//! Conformance: the real `Client` against the real `Driver`, over an
//! in-memory serial line with log noise on the device side.

// Each test drives one client from a single task on a single thread, so a
// `RefCell` borrow held across `.await` can never be contended.
#![allow(clippy::await_holding_refcell_ref)]

extern crate std;
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use embassy_futures::{
    block_on,
    select::{select, Either},
    yield_now,
};
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use impromptu_wifi_client::{Client, ClientError};
use impromptu_wifi_core::proto::{ErrorCode, State};
use impromptu_wifi_core::{
    unsupported, ConnectError, DeviceInfoSource, Driver, HostnameError, HostnameProvider,
    NetworkFlags, NetworkInfo, NetworkStatus, NoAuth, ScanError, Url, WifiAuth, WifiConnect,
    WifiScanner,
};
use impromptu_wifi_serial::{SerialClientLink, SerialLink};

// --- an in-memory serial line ----------------------------------------------

type Queue = Rc<RefCell<VecDeque<u8>>>;

/// One end of the line. Reads poll until data arrives; if nothing arrives
/// for a long time the test has deadlocked, so the read fails instead.
struct End {
    rx: Queue,
    tx: Queue,
    noise: &'static [u8],
}

#[derive(Debug)]
struct Stalled;

impl core::fmt::Display for Stalled {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("test deadlocked")
    }
}

impl core::error::Error for Stalled {}

impl embedded_io_async::Error for Stalled {
    fn kind(&self) -> ErrorKind {
        ErrorKind::TimedOut
    }
}

impl ErrorType for End {
    type Error = Stalled;
}

impl Read for End {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Stalled> {
        for _ in 0..10_000 {
            {
                let mut q = self.rx.borrow_mut();
                if !q.is_empty() {
                    let n = buf.len().min(q.len());
                    for b in &mut buf[..n] {
                        *b = q.pop_front().unwrap();
                    }
                    return Ok(n);
                }
            }
            yield_now().await;
        }
        Err(Stalled)
    }
}

impl Write for End {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Stalled> {
        let mut q = self.tx.borrow_mut();
        q.extend(self.noise);
        q.extend(buf);
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Stalled> {
        Ok(())
    }
}

/// A serial line: (device end, client end). The device end prefixes every
/// write with `noise`, like firmware logging on the same UART.
fn line(noise: &'static [u8]) -> (End, End) {
    let to_device = Queue::default();
    let to_client = Queue::default();
    (
        End {
            rx: to_device.clone(),
            tx: to_client.clone(),
            noise,
        },
        End {
            rx: to_client,
            tx: to_device,
            noise: b"",
        },
    )
}

// --- a device that supports everything serial can do ----------------------

#[derive(Default)]
struct Wifi {
    connected: bool,
}

impl WifiConnect for Wifi {
    async fn connect(&mut self, ssid: &str, password: &str) -> Result<Option<Url>, ConnectError> {
        if (ssid, password) == ("home", "hunter22") {
            self.connected = true;
            Ok(Some(Url::try_from("http://192.168.1.50").unwrap()))
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
            ("home", -40, WifiAuth::Wpa2Personal),
            ("cafe", -71, WifiAuth::Open),
            ("lab", -80, WifiAuth::Owe),
        ];
        for (ssid, rssi, auth) in found {
            let _ = out.push(NetworkInfo::new(ssid, rssi, auth).unwrap());
        }
        Ok(())
    }
}

impl NetworkStatus for Wifi {
    async fn network_flags(&self) -> NetworkFlags {
        let online = if self.connected {
            NetworkFlags::ONLINE
        } else {
            0
        };
        NetworkFlags(NetworkFlags::WIFI | online)
    }
}

struct Lamp {
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
        "esp32-c6"
    }
    fn device_name(&self) -> &str {
        "Desk Lamp"
    }
    fn os_name(&self) -> Option<&str> {
        Some("ESPHome")
    }
    fn os_version(&self) -> Option<&str> {
        Some("2025.12.2")
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

unsupported!(Lamp: Identify);

// --- harness ----------------------------------------------------------------

type TestClient = Client<SerialClientLink<End>>;

/// Run `script` as the client while the driver serves it; stop when the
/// script finishes.
fn session<F: core::future::Future<Output = ()>>(
    noise: &'static [u8],
    script: impl FnOnce(&'static RefCell<TestClient>) -> F,
) {
    let (device_end, client_end) = line(noise);
    let lamp = Lamp {
        hostname: heapless::String::try_from("lamp").unwrap(),
    };
    let mut driver = Driver::new(SerialLink::new(device_end), Wifi::default(), lamp, false);
    // Leaked so the script's future can borrow it for 'static; fine in a test.
    let client: &'static RefCell<TestClient> = Box::leak(Box::new(RefCell::new(Client::new(
        SerialClientLink::new(client_end),
    ))));

    match block_on(select(driver.run(&mut NoAuth), script(client))) {
        Either::First(Err(e)) => panic!("driver stopped: {e:?}"),
        Either::First(Ok(never)) => match never {},
        Either::Second(()) => {}
    }
}

// --- tests ------------------------------------------------------------------

#[test]
fn full_provisioning_conversation() {
    session(b"[I][app]: log line\r\n", |client| async move {
        let mut c = client.borrow_mut();

        assert_eq!(c.current_state().await.unwrap(), State::Authorized);

        let info = c.device_info().await.unwrap();
        assert_eq!(info.firmware_name, "lamp-fw");
        assert_eq!(info.device_name, "Desk Lamp");
        assert_eq!(
            info.os.as_ref().map(|(n, v)| (n.as_str(), v.as_str())),
            Some(("ESPHome", "2025.12.2"))
        );

        let networks = c.scan::<8>().await.unwrap();
        let summary: std::vec::Vec<_> = networks
            .iter()
            .map(|n| (n.ssid.as_str(), n.rssi_dbm, n.requires_passphrase))
            .collect();
        assert_eq!(
            summary,
            [
                ("home", -40, true),
                ("cafe", -71, false),
                ("lab", -80, false)
            ]
        );
        assert!(
            networks.iter().all(|n| n.auth.is_none()),
            "serial reports no auth type"
        );

        let offline = c.network_state().await.unwrap();
        assert!(!offline.flags.is_online());
        assert!(offline.urls.is_empty());

        assert!(matches!(
            c.provision("home", "wrong").await,
            Err(ClientError::Device(ErrorCode::UnableToConnect))
        ));
        // The device reports the error before it falls back to "authorized",
        // so the cached state can lag; asking is authoritative.
        assert_eq!(c.current_state().await.unwrap(), State::Authorized);

        let url = c.provision("home", "hunter22").await.unwrap();
        assert_eq!(url.as_deref(), Some("http://192.168.1.50"));
        assert_eq!(c.current_state().await.unwrap(), State::Provisioned);

        let online = c.network_state().await.unwrap();
        assert!(online.flags.is_online());
        assert_eq!(
            online
                .urls
                .iter()
                .map(|u| u.as_str())
                .collect::<std::vec::Vec<_>>(),
            ["http://192.168.1.50"]
        );
    });
}

#[test]
fn hostname_and_device_name() {
    session(b"", |client| async move {
        let mut c = client.borrow_mut();
        assert_eq!(c.hostname().await.unwrap(), "lamp");
        assert_eq!(c.set_hostname("lamp-2").await.unwrap(), "lamp-2");
        assert!(matches!(
            c.set_hostname("not valid").await,
            Err(ClientError::Device(ErrorCode::BadHostname))
        ));
        assert_eq!(c.hostname().await.unwrap(), "lamp-2");

        assert_eq!(c.device_name().await.unwrap(), "Desk Lamp");
        // Lamp keeps the default `set_device_name`, which is unavailable.
        assert!(matches!(
            c.set_device_name("Bedroom").await,
            Err(ClientError::Device(ErrorCode::UnknownCommand))
        ));
    });
}

#[test]
fn ble_only_commands_are_refused_locally() {
    session(b"", |client| async move {
        let mut c = client.borrow_mut();
        // Never reaches the wire: on serial, 0x02 would mean "get state".
        assert!(matches!(c.identify().await, Err(ClientError::Unsupported)));
        // The connection is still in step afterwards.
        assert_eq!(c.current_state().await.unwrap(), State::Authorized);
    });
}

#[test]
fn heavy_noise_between_every_packet() {
    // Noise containing a fake header and a near-miss of one.
    session(
        b"IMPRO boot IMPROVE x IMPROV\x02\r\n",
        |client| async move {
            let mut c = client.borrow_mut();
            assert_eq!(c.device_info().await.unwrap().chip, "esp32-c6");
            assert_eq!(
                c.scan::<2>().await.unwrap().len(),
                2,
                "capacity limits results"
            );
            assert!(c.provision("home", "hunter22").await.unwrap().is_some());
        },
    );
}
