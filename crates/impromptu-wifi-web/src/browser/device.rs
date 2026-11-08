//! The JavaScript API: one connected device, with the CLI's commands.

use core::fmt::Debug;
use core::future::Future;

use impromptu_wifi_ble::BleClientLink;
use impromptu_wifi_client::proto::{ErrorCode, State};
use impromptu_wifi_client::{diagnose_join_failure, Client, ClientError, ClientLink};
use impromptu_wifi_serial::SerialClientLink;
use js_sys::Function;
use wasm_bindgen::prelude::*;

use super::ble::WebGatt;
use super::serial::WebSerialPort;
use super::time::with_timeout;
use crate::slot::{Slot, Unavailable};
use crate::text::{
    capability_names, error_message, interface_names, join_failure_view, state_label,
    JoinFailureView,
};
use crate::LinkFailure;

/// Baud rate for serial devices, as the CLI's default.
const BAUD: u32 = 115_200;
/// Per-call limit, as the CLI's default `--timeout`.
const CALL_MS: u32 = 10_000;
/// Limit for joining a network and for waiting for a button press.
const JOIN_MS: u32 = 60_000;
/// Limit for the scan after a failed join.
const SCAN_AFTER_FAILURE_MS: u32 = 20_000;
/// Networks kept from one scan.
const SCAN_CAPACITY: usize = 64;

type SerialClient = Client<SerialClientLink<WebSerialPort>>;
type BleClient = Client<BleClientLink<WebGatt>>;

enum Conn {
    Serial(Box<SerialClient>),
    Ble(Box<BleClient>),
}

/// Run `$body` with `$c` bound to the client of either transport.
macro_rules! with_client {
    ($conn:expr, |$c:ident| $body:expr) => {
        match &mut $conn {
            Conn::Serial($c) => $body,
            Conn::Ble($c) => $body,
        }
    };
}

/// A device's state, as `state()` returns it.
#[wasm_bindgen]
pub struct StateReport {
    /// `stopped`, `authorization-required`, `authorized`, `provisioning` or
    /// `provisioned`.
    #[wasm_bindgen(getter_with_clone)]
    pub code: String,
    /// One line for people.
    #[wasm_bindgen(getter_with_clone)]
    pub label: String,
    /// Optional commands the device supports (Bluetooth only).
    #[wasm_bindgen(getter_with_clone)]
    pub capabilities: Option<Vec<String>>,
}

/// Firmware and hardware information, as `info()` returns it.
#[wasm_bindgen]
pub struct InfoReport {
    /// Device name.
    #[wasm_bindgen(getter_with_clone)]
    pub name: String,
    /// Firmware name.
    #[wasm_bindgen(getter_with_clone)]
    pub firmware: String,
    /// Firmware version.
    #[wasm_bindgen(getter_with_clone)]
    pub version: String,
    /// Hardware or chip.
    #[wasm_bindgen(getter_with_clone)]
    pub hardware: String,
    /// Operating system name and version, if the device reports them.
    #[wasm_bindgen(getter_with_clone)]
    pub os: Option<String>,
}

/// One network from `scan()`.
#[wasm_bindgen]
pub struct Network {
    /// Network name.
    #[wasm_bindgen(getter_with_clone)]
    pub ssid: String,
    /// Signal strength in dBm.
    pub rssi: i8,
    /// Security as the device reports it: the authentication type over
    /// Bluetooth, `secured` or `open` over serial.
    #[wasm_bindgen(getter_with_clone)]
    pub security: String,
    /// Whether the network needs a passphrase.
    pub secured: bool,
}

/// Connectivity, as `network()` returns it (serial only).
#[wasm_bindgen]
pub struct NetworkReport {
    /// Whether the device has network connectivity.
    pub online: bool,
    /// Interfaces the device has.
    #[wasm_bindgen(getter_with_clone)]
    pub interfaces: Vec<String>,
    /// Where the device can be reached.
    #[wasm_bindgen(getter_with_clone)]
    pub urls: Vec<String>,
}

/// A connected Improv device. Create one with `connectSerial()` or
/// `connectBluetooth()`; every method returns a `Promise` and rejects with
/// an `Error` whose message can be shown as is.
#[wasm_bindgen]
pub struct Device {
    conn: Slot<Conn>,
    transport: &'static str,
    name: Option<String>,
}

#[wasm_bindgen]
impl Device {
    /// Ask the user for a serial port and connect at 115200 baud.
    ///
    /// Rejects with an error named `NotFoundError` if the user closes the
    /// chooser without picking a port.
    #[wasm_bindgen(js_name = connectSerial)]
    pub async fn connect_serial() -> Result<Device, JsValue> {
        let mut port = WebSerialPort::request(BAUD).await?;
        // USB-Serial-JTAG devices keep output sent while the port was closed.
        port.discard_pending().await;
        Ok(Self {
            conn: Slot::new(Conn::Serial(Box::new(Client::new(SerialClientLink::new(
                port,
            ))))),
            transport: "serial",
            name: None,
        })
    }

    /// Ask the user for a Bluetooth device offering Improv and connect.
    ///
    /// Rejects with an error named `NotFoundError` if the user closes the
    /// chooser without picking a device.
    #[wasm_bindgen(js_name = connectBluetooth)]
    pub async fn connect_bluetooth() -> Result<Device, JsValue> {
        let gatt = WebGatt::request().await?;
        let name = gatt.name();
        Ok(Self {
            conn: Slot::new(Conn::Ble(Box::new(Client::new(BleClientLink::new(gatt))))),
            transport: "bluetooth",
            name,
        })
    }

    /// `serial` or `bluetooth`.
    #[wasm_bindgen(getter)]
    pub fn transport(&self) -> String {
        self.transport.into()
    }

    /// The Bluetooth device's advertised name; undefined on serial.
    #[wasm_bindgen(getter)]
    pub fn name(&self) -> Option<String> {
        self.name.clone()
    }

    /// The device's state, and on Bluetooth its optional commands.
    pub async fn state(&self) -> Result<StateReport, JsValue> {
        self.run(|mut conn| async move {
            let ble = matches!(conn, Conn::Ble(_));
            let result = with_client!(conn, |c| {
                let state = timed(CALL_MS, c.current_state()).await;
                let caps = match state {
                    Ok(_) if ble => c.capabilities().await.ok(),
                    _ => None,
                };
                state.map(|s| (s, caps))
            });
            let report = result.map(|(s, caps)| StateReport {
                code: state_code(s).into(),
                label: state_label(s).into(),
                capabilities: caps
                    .map(|c| capability_names(c).into_iter().map(String::from).collect()),
            });
            (conn, report)
        })
        .await
    }

    /// Firmware and hardware information.
    pub async fn info(&self) -> Result<InfoReport, JsValue> {
        self.run(|mut conn| async move {
            let result = with_client!(conn, |c| timed(CALL_MS, c.device_info()).await);
            let report = result.map(|i| InfoReport {
                name: i.device_name.as_str().into(),
                firmware: i.firmware_name.as_str().into(),
                version: i.firmware_version.as_str().into(),
                hardware: i.chip.as_str().into(),
                os: i.os.map(|(n, v)| format!("{n} {v}")),
            });
            (conn, report)
        })
        .await
    }

    /// The Wi-Fi networks the device can see, strongest first as the device
    /// orders them.
    pub async fn scan(&self) -> Result<Vec<Network>, JsValue> {
        self.run(|mut conn| async move {
            let result = with_client!(conn, |c| timed(CALL_MS, c.scan::<SCAN_CAPACITY>()).await);
            let report = result.map(|networks| {
                networks
                    .iter()
                    .map(|n| Network {
                        ssid: n.ssid.as_str().into(),
                        rssi: n.rssi_dbm,
                        security: match (&n.auth, n.requires_passphrase) {
                            (Some(a), _) => a.as_str().into(),
                            (None, true) => "secured".into(),
                            (None, false) => "open".into(),
                        },
                        secured: n.requires_passphrase,
                    })
                    .collect()
            });
            (conn, report)
        })
        .await
    }

    /// Send Wi-Fi credentials; resolves to the URL the device reports, if
    /// any. An empty `passphrase` joins an open network. `progress(stage)`
    /// is called with `"authorize"` (press the device's button, up to
    /// 60 s), `"send"` (credentials sent; joining, up to 60 s) and
    /// `"diagnose"` (the join failed; scanning to explain why).
    ///
    /// A failed join rejects with `kind` set to `not-seen`, `open-network`,
    /// `passphrase-missing`, `wpa3-only`, `rejected` or `unknown`, plus
    /// `title`, `advice`, and `rssi` and `similar` where known.
    pub async fn provision(
        &self,
        ssid: String,
        passphrase: String,
        progress: Function,
    ) -> Result<Option<String>, JsValue> {
        let say = move |stage: &str| {
            let _ = progress.call1(&JsValue::NULL, &JsValue::from_str(stage));
        };
        self.run(|mut conn| async move {
            let result = with_client!(conn, |c| provision(c, &ssid, &passphrase, &say).await);
            (conn, result)
        })
        .await
    }

    /// Make the device show itself, for example by blinking (Bluetooth
    /// only).
    pub async fn identify(&self) -> Result<(), JsValue> {
        self.run(|mut conn| async move {
            let result = with_client!(conn, |c| timed(CALL_MS, c.identify()).await);
            (conn, result)
        })
        .await
    }

    /// Read the hostname, or set it when `set` is given; resolves to the
    /// hostname the device reports.
    pub async fn hostname(&self, set: Option<String>) -> Result<String, JsValue> {
        self.run(|mut conn| async move {
            let result = with_client!(conn, |c| match &set {
                Some(n) => timed(CALL_MS, c.set_hostname(n)).await,
                None => timed(CALL_MS, c.hostname()).await,
            });
            (conn, result.map(|t| t.as_str().into()))
        })
        .await
    }

    /// Read the device name, or set it when `set` is given; resolves to the
    /// name the device reports.
    #[wasm_bindgen(js_name = deviceName)]
    pub async fn device_name(&self, set: Option<String>) -> Result<String, JsValue> {
        self.run(|mut conn| async move {
            let result = with_client!(conn, |c| match &set {
                Some(n) => timed(CALL_MS, c.set_device_name(n)).await,
                None => timed(CALL_MS, c.device_name()).await,
            });
            (conn, result.map(|t| t.as_str().into()))
        })
        .await
    }

    /// Connectivity and URLs (serial only).
    pub async fn network(&self) -> Result<NetworkReport, JsValue> {
        self.run(|mut conn| async move {
            let result = with_client!(conn, |c| timed(CALL_MS, c.network_state()).await);
            let report = result.map(|n| NetworkReport {
                online: n.flags.is_online(),
                interfaces: interface_names(n.flags)
                    .into_iter()
                    .map(String::from)
                    .collect(),
                urls: n.urls.iter().map(|u| u.as_str().into()).collect(),
            });
            (conn, report)
        })
        .await
    }

    /// Close the connection. Later calls reject.
    pub async fn disconnect(&self) {
        match self.conn.clear() {
            Some(Conn::Serial(c)) => c.into_inner().into_inner().close().await,
            Some(Conn::Ble(c)) => c.into_inner().gatt_mut().disconnect(),
            None => {}
        }
    }
}

impl Device {
    /// Take the connection, run `f`, and put the connection back, unless
    /// the device disconnected.
    async fn run<T, F, Fut>(&self, f: F) -> Result<T, JsValue>
    where
        F: FnOnce(Conn) -> Fut,
        Fut: Future<Output = (Conn, Result<T, Failure>)>,
    {
        let conn = self.conn.take().map_err(|u| match u {
            Unavailable::Busy => js_error("busy", "another command is still running", None),
            Unavailable::Gone => js_error("disconnected", "the device disconnected", None),
        })?;
        let (conn, result) = f(conn).await;
        let disconnected = matches!(&result, Err(f) if f.disconnected);
        // Close the link if the device went away, or if `disconnect()` was
        // called while this command ran.
        let gone = if disconnected {
            Some(conn)
        } else {
            self.conn.put(conn).err()
        };
        if let Some(conn) = gone {
            self.conn.clear();
            drop_conn(conn).await;
        }
        result.map_err(|f| {
            if f.disconnected {
                js_error("disconnected", "the device disconnected", None)
            } else {
                js_error(f.kind, &f.message, f.join.as_ref())
            }
        })
    }
}

async fn drop_conn(conn: Conn) {
    match conn {
        Conn::Serial(c) => c.into_inner().into_inner().close().await,
        Conn::Ble(c) => c.into_inner().gatt_mut().disconnect(),
    }
}

/// An `Error` for JavaScript with a `kind` the page can branch on, and for
/// a failed join the parts of its explanation.
fn js_error(kind: &str, message: &str, join: Option<&JoinFailureView>) -> JsValue {
    let e = js_sys::Error::new(message);
    let set = |k: &str, v: JsValue| {
        let _ = js_sys::Reflect::set(&e, &JsValue::from_str(k), &v);
    };
    set("kind", JsValue::from_str(kind));
    if let Some(j) = join {
        set("title", JsValue::from_str(&j.title));
        set("advice", JsValue::from_str(&j.advice));
        if let Some(rssi) = j.rssi {
            set("rssi", JsValue::from(rssi));
        }
        if let Some(s) = &j.similar {
            set("similar", JsValue::from_str(s));
        }
    }
    e.into()
}

/// A failed call: a message for people, a kind for the page (`device`,
/// `timeout`, `link`, or a join-failure kind), and whether the link is
/// gone.
struct Failure {
    kind: &'static str,
    message: String,
    disconnected: bool,
    join: Option<JoinFailureView>,
}

impl<E: Debug + LinkFailure> From<ClientError<E>> for Failure {
    fn from(e: ClientError<E>) -> Self {
        Self {
            kind: match &e {
                ClientError::Link(_) => "link",
                ClientError::Device(ErrorCode::NotAuthorized) => "not-authorized",
                ClientError::Device(ErrorCode::BadHostname) => "bad-hostname",
                ClientError::Device(ErrorCode::UnknownCommand) | ClientError::Unsupported => {
                    "unsupported"
                }
                _ => "device",
            },
            disconnected: matches!(&e, ClientError::Link(l) if l.is_disconnect()),
            message: error_message(&e),
            join: None,
        }
    }
}

fn timeout_failure(ms: u32) -> Failure {
    Failure {
        kind: "timeout",
        message: format!("No answer from the device within {} s.", ms / 1000),
        disconnected: false,
        join: None,
    }
}

/// Run a client call with a deadline.
async fn timed<T, E: Debug + LinkFailure>(
    ms: u32,
    f: impl Future<Output = Result<T, ClientError<E>>>,
) -> Result<T, Failure> {
    match with_timeout(ms, f).await {
        None => Err(timeout_failure(ms)),
        Some(r) => r.map_err(Failure::from),
    }
}

/// The CLI's provision flow: wait for authorization if needed, join, and
/// explain a failed join from a scan.
async fn provision<L>(
    c: &mut Client<L>,
    ssid: &str,
    passphrase: &str,
    say: &impl Fn(&str),
) -> Result<Option<String>, Failure>
where
    L: ClientLink,
    L::Error: LinkFailure,
{
    if timed(CALL_MS, c.current_state()).await? == State::AuthorizationRequired {
        say("authorize");
        timed(JOIN_MS, c.wait_for_state(State::Authorized)).await?;
    }
    say("send");
    match with_timeout(JOIN_MS, c.provision(ssid, passphrase)).await {
        None => Err(timeout_failure(JOIN_MS)),
        Some(Ok(url)) => Ok(url.map(|u| u.as_str().into())),
        Some(Err(ClientError::Device(ErrorCode::UnableToConnect))) => {
            say("diagnose");
            let seen = with_timeout(SCAN_AFTER_FAILURE_MS, c.scan::<SCAN_CAPACITY>())
                .await
                .and_then(Result::ok);
            let diagnosis = diagnose_join_failure(ssid, !passphrase.is_empty(), seen.as_deref());
            let view = join_failure_view(ssid, &diagnosis);
            Err(Failure {
                kind: view.kind,
                message: format!("{}. {}", view.title, view.advice),
                disconnected: false,
                join: Some(view),
            })
        }
        Some(Err(e)) => Err(e.into()),
    }
}

fn state_code(s: State) -> &'static str {
    match s {
        State::Stopped => "stopped",
        State::AuthorizationRequired => "authorization-required",
        State::Authorized => "authorized",
        State::Provisioning => "provisioning",
        State::Provisioned => "provisioned",
    }
}
