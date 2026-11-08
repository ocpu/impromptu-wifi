//! `impromptu-wifi-device`: make this Linux machine provisionable over Improv.
//!
//! Serves Improv on a serial port (a UART, or `/dev/ttyGS0` in USB gadget
//! mode), over Bluetooth LE, or both, with NetworkManager for Wi-Fi and
//! systemd-hostnamed for names. Meant to run as a systemd service; see
//! `dist/` for the unit and a polkit rule.
//!
//! Bluetooth is only offered while the machine is offline, and stops once
//! it's provisioned, as the Improv spec describes. Without a physical
//! authorization step, anyone in radio range could otherwise point a
//! working machine at a network of their choosing.

use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use impromptu_wifi_ble::BleLink;
use impromptu_wifi_core::{Driver, EventSource, NoAuth, NotifyOnJoin};
use impromptu_wifi_linux::bluez::{Adapter, BluezGatt};
use impromptu_wifi_linux::join::{join_channel, JoinSender};
use impromptu_wifi_linux::nm::{NmOnline, NmRadio, NmWifi};
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

    /// Wi-Fi interface (default: the first one NetworkManager manages)
    #[arg(long)]
    interface: Option<String>,

    /// Firmware name reported to clients
    #[arg(long, default_value = "impromptu-wifi-linux")]
    firmware_name: String,

    /// Firmware version reported to clients
    #[arg(long, default_value = env!("CARGO_PKG_VERSION"))]
    firmware_version: String,
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
    let wifi = NmWifi::new(bus.clone(), args.interface.as_deref())
        .await
        .context("using NetworkManager")?;
    let device = LinuxDevice::new(&bus, &args.firmware_name, &args.firmware_version)
        .await
        .context("using systemd-hostnamed")?;

    // Each transport owns clones of the backends. Joins made through either
    // are announced to the other, so both report the same state.
    let (joined, _) = join_channel();
    let ble = args.ble.then(|| {
        tokio::spawn(serve_ble(
            bus.clone(),
            wifi.clone(),
            device.clone(),
            joined.clone(),
        ))
    });
    match args.serial {
        Some(path) => {
            let radio = NmRadio::new(&bus)
                .await
                .context("watching the Wi-Fi radio switch")?;
            serve_serial(&bus, &path, args.baud, wifi, device, radio, joined).await
        }
        None => match ble {
            Some(task) => task.await.context("Bluetooth task")?,
            None => unreachable!("clap requires a transport"),
        },
    }
}

async fn serve_serial(
    bus: &zbus::Connection,
    path: &str,
    baud: u32,
    mut wifi: NmWifi,
    mut device: LinuxDevice,
    mut radio: NmRadio,
    joined: JoinSender,
) -> Result<std::convert::Infallible> {
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

        let online = wifi.online_url().await;
        let mut nm_online = NmOnline::new(bus, online.is_some())
            .await
            .context("watching NetworkManager's state")?;
        let mut events = joined.subscribe();
        let notifying = NotifyOnJoin::new(&mut wifi, joined.notifier());
        let mut driver = Driver::new(SerialLink::new(port), notifying, &mut device, false);
        if let Some(url) = online {
            driver.mark_provisioned(url);
        }
        // While the radio is off, serial clients see the Stopped state.
        driver.machine_mut().set_available(radio.enabled());
        // `run_with` only returns when the link fails. Radio changes, the
        // machine going on or offline, and joins made over Bluetooth all
        // reach the serial client.
        let Err(e) = driver
            .run_with(
                &mut NoAuth,
                &mut (&mut radio, (&mut nm_online, &mut events)),
            )
            .await;
        log::warn!("serial link ended: {e:?}; reopening");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn serve_ble(
    bus: zbus::Connection,
    mut wifi: NmWifi,
    mut device: LinuxDevice,
    joined: JoinSender,
) -> Result<std::convert::Infallible> {
    let adapter = Adapter::first(&bus)
        .await
        .context("finding a Bluetooth adapter")?;
    let caps = impromptu_wifi_ble::capabilities::<NmWifi, LinuxDevice>();
    // Assume online: if the machine is offline, the first event says so.
    let mut online = NmOnline::new(&bus, true)
        .await
        .context("watching NetworkManager's state")?;
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
        // by a serial client, or by NetworkManager rejoining a saved
        // network (at boot, say). Either way Bluetooth should stop.
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
            }
            Err(e) => log::warn!("Bluetooth session ended: {e}"),
        }
        gatt.stop().await;
        log::info!("stopped offering Improv over Bluetooth");
    }
}
