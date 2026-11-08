//! `impromptu-wifi`: provision Wi-Fi devices over Improv, by serial port or Bluetooth LE.

use std::fmt::Debug;
use std::future::Future;
use std::io::{BufRead, IsTerminal};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use impromptu_wifi_client::proto::ble::Capabilities;
use impromptu_wifi_client::proto::{ErrorCode, State};
use impromptu_wifi_client::{Client, ClientError, ClientLink};
use impromptu_wifi_host::{ble, serial, BleClientLink, SerialClientLink};

#[derive(Parser)]
#[command(
    name = "impromptu-wifi",
    version,
    about = "Provision Wi-Fi devices over Improv, by serial port or Bluetooth LE"
)]
struct Cli {
    /// Serial port of the device, for example /dev/ttyUSB0 (see `impromptu-wifi ports`)
    #[arg(long, short, global = true, conflicts_with = "ble")]
    port: Option<String>,

    /// Bluetooth device: its address, or part of its name (see `impromptu-wifi discover`)
    #[arg(long, short, global = true)]
    ble: Option<String>,

    /// Serial baud rate
    #[arg(long, global = true, default_value_t = serial::DEFAULT_BAUD)]
    baud: u32,

    /// Reset the device first (serial; ESP32-style DTR/RTS wiring)
    #[arg(long, global = true)]
    reset: bool,

    /// Seconds to wait for an answer (joining a network allows at least 60)
    #[arg(long, global = true, default_value_t = 10)]
    timeout: u64,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List serial ports
    Ports,
    /// Find Improv devices over Bluetooth LE
    Discover {
        /// How long to scan, in seconds
        #[arg(long, default_value_t = 5)]
        seconds: u64,
    },
    /// Show the device's state
    State,
    /// Show firmware and hardware information
    Info,
    /// List the Wi-Fi networks the device can see
    Scan,
    /// Send Wi-Fi credentials to the device
    Provision {
        /// Network name
        #[arg(long)]
        ssid: String,
        /// Passphrase. Prompted for if omitted; prefer the prompt, since
        /// command lines end up in shell history and process listings
        #[arg(long, conflicts_with = "open")]
        password: Option<String>,
        /// The network has no passphrase
        #[arg(long)]
        open: bool,
    },
    /// Make the device blink or beep (Bluetooth only)
    Identify,
    /// Show the hostname, or set it
    Hostname { name: Option<String> },
    /// Show the device name, or set it
    Name { name: Option<String> },
    /// Show network connectivity (serial only)
    Network,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    // Print errors ourselves, as one readable line: returning them from
    // `main` would include a backtrace whenever RUST_BACKTRACE is set.
    match run(Cli::parse()).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    match &cli.command {
        Command::Ports => return list_ports(),
        Command::Discover { seconds } => return discover(Duration::from_secs(*seconds)).await,
        _ => {}
    }

    // Read the passphrase before connecting, so a prompt isn't raced by the
    // device's answers.
    let password = match &cli.command {
        Command::Provision {
            ssid,
            password,
            open,
        } => Some(passphrase(ssid, password.as_deref(), *open)?),
        _ => None,
    };

    if let Some(path) = &cli.port {
        let mut port = serial::open(path, cli.baud).with_context(|| format!("opening {path}"))?;
        if cli.reset {
            serial::hard_reset(&mut port)
                .await
                .context("resetting the device")?;
            // Give the firmware time to boot and start its Improv service.
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }
        let mut client = Client::new(SerialClientLink::new(port));
        execute(&mut client, &cli, password).await
    } else if let Some(query) = &cli.ble {
        let adapter = bluetooth().await?;
        eprintln!("Looking for {query}...");
        let device = ble::find(&adapter, query, Duration::from_secs(10))
            .await?
            .ok_or_else(|| {
                anyhow!("no Improv device matching {query:?} found; try `impromptu-wifi discover`")
            })?;
        let gatt = ble::BtleGattClient::connect(device.peripheral).await?;
        let mut client = Client::new(BleClientLink::new(gatt));
        let result = execute(&mut client, &cli, password).await;
        // Disconnect explicitly: many devices stop advertising while
        // connected, and the OS may otherwise hold the link open for a while.
        let _ = client.into_inner().into_inner().disconnect().await;
        result
    } else {
        bail!("choose a device with --port (see `impromptu-wifi ports`) or --ble (see `impromptu-wifi discover`)")
    }
}

async fn execute<L>(client: &mut Client<L>, cli: &Cli, password: Option<String>) -> Result<()>
where
    L: ClientLink,
    L::Error: Debug,
{
    let t = Duration::from_secs(cli.timeout);
    match &cli.command {
        Command::Ports | Command::Discover { .. } => unreachable!(),

        Command::State => {
            let state = timed(t, client.current_state()).await?;
            println!("{}", describe_state(state));
            if let Ok(caps) = client.capabilities().await {
                println!("Supports: {}", describe_caps(caps));
            }
        }

        Command::Info => {
            let info = timed(t, client.device_info()).await?;
            println!("Name:     {}", info.device_name);
            println!("Firmware: {} {}", info.firmware_name, info.firmware_version);
            println!("Hardware: {}", info.chip);
            if let Some((name, version)) = &info.os {
                println!("OS:       {name} {version}");
            }
        }

        Command::Scan => {
            let networks = timed(t, client.scan::<64>()).await?;
            if networks.is_empty() {
                println!("No networks found.");
            }
            for n in &networks {
                let security = match (&n.auth, n.requires_passphrase) {
                    (Some(auth), _) => auth.as_str(),
                    (None, true) => "secured",
                    (None, false) => "open",
                };
                println!("{:>4} dBm  {:<10} {}", n.rssi_dbm, security, n.ssid);
            }
        }

        Command::Provision { ssid, .. } => {
            let password = password.unwrap_or_default();
            if timed(t, client.current_state()).await? == State::AuthorizationRequired {
                eprintln!("Press the button on the device to authorize it...");
                let wait = t.max(Duration::from_secs(60));
                timed(wait, client.wait_for_state(State::Authorized)).await?;
            }
            eprintln!("Sending credentials for {ssid}...");
            let join = t.max(Duration::from_secs(60));
            match timed(join, client.provision(ssid, &password)).await? {
                Some(url) => println!("Connected. The device is at {url}"),
                None => println!("Connected."),
            }
        }

        Command::Identify => timed(t, client.identify()).await?,

        Command::Hostname { name } => {
            let result = match name {
                Some(n) => timed(t, client.set_hostname(n)).await?,
                None => timed(t, client.hostname()).await?,
            };
            println!("{result}");
        }

        Command::Name { name } => {
            let result = match name {
                Some(n) => timed(t, client.set_device_name(n)).await?,
                None => timed(t, client.device_name()).await?,
            };
            println!("{result}");
        }

        Command::Network => {
            let net = timed(t, client.network_state()).await?;
            let f = net.flags;
            println!("Online:   {}", if f.is_online() { "yes" } else { "no" });
            let ifaces: Vec<&str> = [
                (0x02, "Wi-Fi"),
                (0x04, "Ethernet"),
                (0x08, "Thread"),
                (0x10, "cellular"),
            ]
            .into_iter()
            .filter(|(bit, _)| f.0 & bit != 0)
            .map(|(_, name)| name)
            .collect();
            println!(
                "Supports: {}",
                if ifaces.is_empty() {
                    "-".into()
                } else {
                    ifaces.join(", ")
                }
            );
            for url in &net.urls {
                println!("URL:      {url}");
            }
        }
    }
    Ok(())
}

/// Run `fut` with a deadline, turning client errors into readable ones.
async fn timed<T, E: Debug>(
    limit: Duration,
    fut: impl Future<Output = Result<T, ClientError<E>>>,
) -> Result<T> {
    match tokio::time::timeout(limit, fut).await {
        Err(_) => bail!("no answer from the device within {}s", limit.as_secs()),
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(explain(e)),
    }
}

fn explain<E: Debug>(e: ClientError<E>) -> anyhow::Error {
    match e {
        ClientError::Device(ErrorCode::UnableToConnect) => {
            anyhow!("the device could not join the network; check the network name and passphrase")
        }
        ClientError::Device(ErrorCode::UnknownCommand) => {
            anyhow!("the device doesn't support this command")
        }
        ClientError::Device(ErrorCode::NotAuthorized) => {
            anyhow!("the device needs authorization; press its button and try again")
        }
        ClientError::Device(ErrorCode::BadHostname) => {
            anyhow!("the device rejected that hostname (letters, digits and hyphens only)")
        }
        ClientError::Device(code) => anyhow!("the device reported an error: {code:?}"),
        ClientError::Unsupported => anyhow!("this command isn't available over this connection"),
        ClientError::Protocol => {
            anyhow!("the device sent an unexpected or damaged answer; try again")
        }
        ClientError::TooLong => anyhow!("a value is too long for the protocol"),
        ClientError::Link(e) => anyhow!("connection error: {e:?}"),
    }
}

fn passphrase(ssid: &str, given: Option<&str>, open: bool) -> Result<String> {
    if open {
        return Ok(String::new());
    }
    if let Some(p) = given {
        return Ok(p.to_owned());
    }
    if std::io::stdin().is_terminal() {
        return rpassword::prompt_password(format!("Passphrase for {ssid}: "))
            .context("reading passphrase");
    }
    // Piped input: the first line is the passphrase.
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .context("reading passphrase from stdin")?;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

fn describe_state(state: State) -> &'static str {
    match state {
        State::Stopped => "Stopped: provisioning is unavailable (Wi-Fi may be off)",
        State::AuthorizationRequired => "Waiting for authorization: press the device's button",
        State::Authorized => "Ready for Wi-Fi credentials",
        State::Provisioning => "Connecting to a network",
        State::Provisioned => "Provisioned",
    }
}

fn describe_caps(caps: Capabilities) -> String {
    let names: Vec<&str> = [
        (Capabilities::IDENTIFY, "identify"),
        (Capabilities::DEVICE_INFO, "device info"),
        (Capabilities::SCAN, "scan"),
        (Capabilities::HOSTNAME, "hostname"),
    ]
    .into_iter()
    .filter(|(c, _)| caps.contains(*c))
    .map(|(_, n)| n)
    .collect();
    if names.is_empty() {
        "-".into()
    } else {
        names.join(", ")
    }
}

fn list_ports() -> Result<()> {
    let ports = serial::available_ports().context("listing serial ports")?;
    if ports.is_empty() {
        println!("No serial ports found.");
    }
    for p in &ports {
        println!("{}", serial::describe(p));
    }
    Ok(())
}

/// The first Bluetooth adapter, with a hint when the stack isn't there.
async fn bluetooth() -> Result<ble::BleAdapter> {
    ble::first_adapter().await.context(if cfg!(target_os = "linux") {
        "starting Bluetooth; check that the bluetooth service (BlueZ) is running and an adapter is present"
    } else {
        "starting Bluetooth; check that Bluetooth is on and this program may use it"
    })
}

async fn discover(duration: Duration) -> Result<()> {
    let adapter = bluetooth().await?;
    eprintln!("Scanning for {}s...", duration.as_secs());
    let found = ble::discover(&adapter, duration).await?;
    if found.is_empty() {
        println!("No Improv devices found.");
    }
    for d in &found {
        let rssi = d
            .rssi
            .map(|r| format!("{r} dBm"))
            .unwrap_or_else(|| "-".into());
        let state = match d.state {
            Some(State::AuthorizationRequired) => "needs authorization",
            Some(State::Authorized) => "ready",
            Some(State::Provisioning) => "connecting",
            Some(State::Provisioned) => "provisioned",
            Some(State::Stopped) => "stopped",
            None => "unknown (pre-2.0 firmware)",
        };
        let name = d.name.as_deref().unwrap_or("(unnamed)");
        println!("{:<18} {:>8}  {:<26} {name}", d.address, rssi, state);
    }
    Ok(())
}
