//! `impromptu-wifi-device`: make this Linux machine provisionable over Improv.
//!
//! Serves Improv on a serial port (a UART, or `/dev/ttyGS0` in USB gadget
//! mode), over Bluetooth LE, or both, with NetworkManager or wpa_supplicant
//! for Wi-Fi and systemd-hostnamed for names. Meant to run as a systemd
//! service; see `dist/` for the unit and a polkit rule.
//!
//! Bluetooth is only offered while the machine is offline, and stops once
//! it's online. Without a physical authorization step, anyone in radio range
//! could otherwise point a working machine at a network of their choosing.

use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use impromptu_wifi_ble::BleLink;
use impromptu_wifi_core::traits::{
    ConnectError, DeviceEvent, NetworkFlags, NetworkInfo, NetworkStatus, ScanError, Url,
    WifiConnect, WifiScanner,
};
use impromptu_wifi_core::{Driver, EventSource, NoAuth, NotifyOnJoin};
use impromptu_wifi_linux::bluez::{Adapter, BluezGatt};
use impromptu_wifi_linux::join::{join_channel, JoinSender};
use impromptu_wifi_linux::nm::{NmOnline, NmRadio, NmWifi};
use impromptu_wifi_linux::wpa::{WpaOnline, WpaWifi, DEFAULT_CTRL_DIR};
use impromptu_wifi_linux::{hostname::LinuxDevice, serial};
use impromptu_wifi_serial::SerialLink;

#[derive(Parser)]
#[command(
    name = "impromptu-wifi-device",
    version,
    about = "Make this Linux machine provisionable over Improv Wi-Fi"
)]
#[command(group = clap::ArgGroup::new("transport").required(true).multiple(true).args(["serial", "ble"]))]
struct Args {
    /// Serial port to serve Improv on, for example /dev/ttyGS0 or /dev/serial0
    #[arg(long)]
    serial: Option<String>,

    /// Offer Improv over Bluetooth LE while the machine is offline
    #[arg(long)]
    ble: bool,

    /// Serial baud rate
    #[arg(long, default_value_t = 115_200)]
    baud: u32,

    /// Wi-Fi interface (default: the first one the backend manages)
    #[arg(long)]
    interface: Option<String>,

    /// What joins Wi-Fi networks: NetworkManager, or wpa_supplicant
    /// directly (for systems without NetworkManager, such as Raspberry Pi
    /// OS 11 with dhcpcd). `auto` uses NetworkManager if it is running.
    #[arg(long, value_enum, default_value_t = BackendKind::Auto)]
    backend: BackendKind,

    /// Directory of wpa_supplicant's control sockets (its `ctrl_interface`)
    #[arg(long, default_value = DEFAULT_CTRL_DIR)]
    wpa_ctrl_dir: String,

    /// Count the machine as offline while Wi-Fi is, even if it is online
    /// through another interface such as Ethernet. Bluetooth provisioning
    /// is then offered on a wired machine, to anyone in radio range.
    #[arg(long)]
    ignore_wired: bool,

    /// Firmware name reported to clients
    #[arg(long, default_value = "impromptu-wifi-linux")]
    firmware_name: String,

    /// Firmware version reported to clients
    #[arg(long, default_value = env!("CARGO_PKG_VERSION"))]
    firmware_version: String,
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum BackendKind {
    Auto,
    Networkmanager,
    WpaSupplicant,
}

/// The Wi-Fi backend in use.
#[derive(Clone)]
enum Wifi {
    Nm(NmWifi),
    Wpa(WpaWifi),
}

impl WifiConnect for Wifi {
    async fn connect(&mut self, ssid: &str, password: &str) -> Result<Option<Url>, ConnectError> {
        match self {
            Self::Nm(w) => w.connect(ssid, password).await,
            Self::Wpa(w) => w.connect(ssid, password).await,
        }
    }
}

impl WifiScanner for Wifi {
    async fn scan<const N: usize>(
        &mut self,
        out: &mut heapless::Vec<NetworkInfo, N>,
    ) -> Result<(), ScanError> {
        match self {
            Self::Nm(w) => w.scan(out).await,
            Self::Wpa(w) => w.scan(out).await,
        }
    }
}

impl NetworkStatus for Wifi {
    async fn network_flags(&self) -> NetworkFlags {
        match self {
            Self::Nm(w) => w.network_flags().await,
            Self::Wpa(w) => w.network_flags().await,
        }
    }
    async fn current_url(&self) -> Option<Url> {
        match self {
            Self::Nm(w) => w.current_url().await,
            Self::Wpa(w) => w.current_url().await,
        }
    }
}

/// Whether the machine is online, from the backend in use.
enum Online {
    Nm(NmOnline),
    Wpa(WpaOnline),
}

impl Online {
    fn online(&self) -> bool {
        match self {
            Self::Nm(o) => o.online(),
            Self::Wpa(o) => o.online(),
        }
    }

    async fn url(&self) -> Option<Url> {
        match self {
            Self::Nm(o) => o.url().await,
            Self::Wpa(o) => o.url().await,
        }
    }
}

impl EventSource for Online {
    async fn next(&mut self) -> DeviceEvent {
        match self {
            Self::Nm(o) => o.next().await,
            Self::Wpa(o) => o.next().await,
        }
    }
}

/// The backend, and how to watch whether the machine is online.
#[derive(Clone)]
struct Backend {
    bus: zbus::Connection,
    wifi: Wifi,
    ignore_wired: bool,
}

impl Backend {
    async fn new(bus: &zbus::Connection, args: &Args) -> Result<Self> {
        let kind = match args.backend {
            BackendKind::Auto if networkmanager_running(bus).await => BackendKind::Networkmanager,
            BackendKind::Auto => BackendKind::WpaSupplicant,
            k => k,
        };
        let wifi = match kind {
            BackendKind::Networkmanager => Wifi::Nm(
                NmWifi::new(bus.clone(), args.interface.as_deref())
                    .await
                    .context("using NetworkManager")?,
            ),
            _ => {
                let interface = match &args.interface {
                    Some(i) => i.clone(),
                    None => first_ctrl_socket(&args.wpa_ctrl_dir).with_context(|| {
                        format!("no wpa_supplicant control socket in {}", args.wpa_ctrl_dir)
                    })?,
                };
                Wifi::Wpa(
                    WpaWifi::new(&args.wpa_ctrl_dir, &interface)
                        .await
                        .with_context(|| format!("using wpa_supplicant on {interface}"))?,
                )
            }
        };
        log::info!(
            "Wi-Fi through {}",
            match &wifi {
                Wifi::Nm(_) => "NetworkManager".to_owned(),
                Wifi::Wpa(w) => format!("wpa_supplicant on {}", w.interface()),
            }
        );
        Ok(Self {
            bus: bus.clone(),
            wifi,
            ignore_wired: args.ignore_wired,
        })
    }

    /// Watch whether the machine is online; the first event corrects
    /// `assume_online` if it is wrong.
    async fn watch(&self, assume_online: bool) -> Result<Online> {
        Ok(match &self.wifi {
            Wifi::Nm(w) if self.ignore_wired => Online::Nm(
                NmOnline::wifi_only(&self.bus, w, assume_online)
                    .await
                    .context("watching the Wi-Fi device")?,
            ),
            Wifi::Nm(_) => Online::Nm(
                NmOnline::new(&self.bus, assume_online)
                    .await
                    .context("watching NetworkManager's state")?,
            ),
            Wifi::Wpa(w) => {
                Online::Wpa(WpaOnline::new(w.clone(), !self.ignore_wired, assume_online).await)
            }
        })
    }
}

/// Whether NetworkManager holds its name on the system bus.
async fn networkmanager_running(bus: &zbus::Connection) -> bool {
    let Ok(dbus) = zbus::fdo::DBusProxy::new(bus).await else {
        return false;
    };
    let Ok(name) = zbus::names::BusName::try_from("org.freedesktop.NetworkManager") else {
        return false;
    };
    dbus.name_has_owner(name).await.unwrap_or(false)
}

/// The first interface with a control socket, skipping P2P devices.
fn first_ctrl_socket(dir: &str) -> Option<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| !n.starts_with("p2p-"))
        .collect();
    names.sort();
    names.into_iter().next()
}

/// Minimal logger: one line per record on stderr, which the journal keeps.
struct Stderr;

impl log::Log for Stderr {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            eprintln!("{}: {}", r.level().as_str().to_lowercase(), r.args());
        }
    }
    fn flush(&self) {}
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let _ = log::set_logger(&Stderr).map(|()| log::set_max_level(log::LevelFilter::Info));
    let args = Args::parse();
    tokio::select! {
        result = serve(args) => match result {
            Ok(never) => match never {},
            Err(e) => {
                log::error!("{e:#}");
                std::process::ExitCode::FAILURE
            }
        },
        _ = shutdown() => std::process::ExitCode::SUCCESS,
    }
}

async fn shutdown() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("installing SIGTERM handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}

async fn serve(args: Args) -> Result<std::convert::Infallible> {
    let bus = zbus::Connection::system()
        .await
        .context("connecting to the system D-Bus")?;
    let backend = Backend::new(&bus, &args).await?;
    let device = LinuxDevice::new(&bus, &args.firmware_name, &args.firmware_version)
        .await
        .context("using systemd-hostnamed")?;

    // Each transport owns clones of the backends. Joins made through either
    // are announced to the other, so both report the same state.
    let (joined, _) = join_channel();
    let ble = args.ble.then(|| {
        tokio::spawn(serve_ble(
            bus.clone(),
            backend.clone(),
            device.clone(),
            joined.clone(),
        ))
    });
    match args.serial {
        Some(path) => {
            // wpa_supplicant has no radio switch of its own to follow.
            let radio = match backend.wifi {
                Wifi::Nm(_) => Some(
                    NmRadio::new(&bus)
                        .await
                        .context("watching the Wi-Fi radio switch")?,
                ),
                Wifi::Wpa(_) => None,
            };
            serve_serial(&path, args.baud, backend, device, radio, joined).await
        }
        None => match ble {
            Some(task) => task.await.context("Bluetooth task")?,
            None => unreachable!("clap requires a transport"),
        },
    }
}

/// Radio switch changes, if the backend has a switch.
struct Radio(Option<NmRadio>);

impl Radio {
    fn enabled(&self) -> bool {
        self.0.as_ref().is_none_or(NmRadio::enabled)
    }
}

impl EventSource for Radio {
    async fn next(&mut self) -> DeviceEvent {
        match &mut self.0 {
            Some(r) => r.next().await,
            None => std::future::pending().await,
        }
    }
}

async fn serve_serial(
    path: &str,
    baud: u32,
    backend: Backend,
    mut device: LinuxDevice,
    radio: Option<NmRadio>,
    joined: JoinSender,
) -> Result<std::convert::Infallible> {
    let mut radio = Radio(radio);
    let mut wifi = backend.wifi.clone();
    loop {
        let port = match serial::open(path, baud) {
            Ok(p) => p,
            Err(e) => {
                // A gadget port appears only once the USB host enumerates it.
                log::warn!("opening {path}: {e}; retrying");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        log::info!("serving Improv on {path}");

        // Assume offline: if the machine is online, the first event says so
        // and the client hears Provisioned.
        let mut online = backend.watch(false).await?;
        let mut events = joined.subscribe();
        let notifying = NotifyOnJoin::new(&mut wifi, joined.notifier());
        let mut driver = Driver::new(SerialLink::new(port), notifying, &mut device, false);
        if online.online() {
            driver.mark_provisioned(online.url().await);
        }
        // While the radio is off, serial clients see the Stopped state.
        driver.machine_mut().set_available(radio.enabled());
        // `run_with` only returns when the link fails. Radio changes, the
        // machine going on or offline, and joins made over Bluetooth all
        // reach the serial client.
        let Err(e) = driver
            .run_with(&mut NoAuth, &mut (&mut radio, (&mut online, &mut events)))
            .await;
        log::warn!("serial link ended: {e:?}; reopening");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn serve_ble(
    bus: zbus::Connection,
    backend: Backend,
    mut device: LinuxDevice,
    joined: JoinSender,
) -> Result<std::convert::Infallible> {
    let mut wifi = backend.wifi.clone();
    let adapter = Adapter::first(&bus)
        .await
        .context("finding a Bluetooth adapter")?;
    let caps = impromptu_wifi_ble::capabilities::<Wifi, LinuxDevice>();
    // Assume online: if the machine is offline, the first event says so.
    let mut online = backend.watch(true).await?;
    loop {
        // Only offer provisioning while offline.
        while online.online() {
            online.next().await;
        }
        log::info!(
            "offline: offering Improv over Bluetooth on {}",
            adapter.name()
        );
        let gatt = BluezGatt::start(&bus, &adapter, caps)
            .await
            .context("registering the Improv service")?;
        let mut joins = joined.subscribe();
        let notifying = NotifyOnJoin::new(&mut wifi, joined.notifier());
        let mut driver = Driver::new(BleLink::new(gatt, caps), notifying, &mut device, false);
        // Returns once the machine is online: provisioned over Bluetooth,
        // by a serial client, or by the backend rejoining a saved network
        // (at boot, say). Either way Bluetooth should stop.
        let outcome = driver
            .provision_with(&mut NoAuth, &mut (&mut joins, &mut online))
            .await;
        // Stop advertising at once, so no new client finds a machine that is
        // online. The service stays up a little longer: a connected client
        // reads the full result after being notified, from the exported
        // characteristics.
        let mut gatt = driver.into_parts().0.into_inner();
        gatt.stop_advertising().await;
        match outcome {
            Ok(url) => {
                log::info!(
                    "online{}",
                    url.map(|u| format!("; reachable at {u}"))
                        .unwrap_or_default()
                );
                tokio::time::sleep(Duration::from_secs(5)).await;
                gatt.stop().await;
                log::info!("stopped offering Improv over Bluetooth");
                // A join made here ends the session before the watcher has
                // seen the machine come online (wpa_supplicant's is polled);
                // let it catch up, or the loop would advertise again.
                let _ = tokio::time::timeout(Duration::from_secs(30), async {
                    while !online.online() {
                        online.next().await;
                    }
                })
                .await;
                continue;
            }
            Err(e) => log::warn!("Bluetooth session ended: {e}"),
        }
        gatt.stop().await;
        log::info!("stopped offering Improv over Bluetooth");
    }
}
