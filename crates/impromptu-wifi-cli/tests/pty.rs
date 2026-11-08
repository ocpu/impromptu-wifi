//! End to end: the real `impromptu-wifi` binary, in its own process, talks to the
//! device-side `Driver` through a kernel pseudo-terminal. This exercises
//! argument parsing, opening the port with `serialport`, the TTY layer,
//! passphrase input, exit codes and error messages.

// Pseudo-terminals are a Unix facility.
#![cfg(unix)]

use std::io::Write as _;
use std::os::fd::OwnedFd;
use std::process::{Command, Output, Stdio};

use embedded_io_adapters::tokio_1::FromTokio;
use impromptu_wifi_core::{
    unsupported, ConnectError, DeviceInfoSource, Driver, HostnameError, HostnameProvider,
    NetworkFlags, NetworkInfo, NetworkStatus, NoAuth, ScanError, Url, WifiAuth, WifiConnect,
    WifiScanner,
};
use impromptu_wifi_serial::SerialLink;
use nix::pty::openpty;
use nix::sys::termios::{cfmakeraw, tcgetattr, tcsetattr, SetArg};

#[derive(Default)]
struct Wifi {
    online: bool,
}

impl WifiConnect for Wifi {
    async fn connect(&mut self, ssid: &str, password: &str) -> Result<Option<Url>, ConnectError> {
        if (ssid, password) == ("home", "hunter22") {
            self.online = true;
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
        for (ssid, rssi, auth) in [
            ("home", -40, WifiAuth::Wpa2Personal),
            ("cafe", -71, WifiAuth::Open),
        ] {
            let _ = out.push(NetworkInfo::new(ssid, rssi, auth).unwrap());
        }
        Ok(())
    }
}

impl NetworkStatus for Wifi {
    async fn network_flags(&self) -> NetworkFlags {
        NetworkFlags(NetworkFlags::WIFI | if self.online { NetworkFlags::ONLINE } else { 0 })
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

/// Start a device on a fresh pseudo-terminal. Returns the path a client
/// opens, and the slave end, which must stay open to keep the pair alive.
fn start_device() -> (String, OwnedFd) {
    let pty = openpty(None, None).expect("openpty");
    let mut termios = tcgetattr(&pty.slave).unwrap();
    cfmakeraw(&mut termios);
    tcsetattr(&pty.slave, SetArg::TCSANOW, &termios).unwrap();
    let path = nix::unistd::ttyname(&pty.slave)
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let master = std::fs::File::from(pty.master);
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let io = FromTokio::new(tokio::fs::File::from_std(master));
            let lamp = Lamp {
                hostname: heapless::String::try_from("lamp").unwrap(),
            };
            let mut driver = Driver::new(SerialLink::new(io), Wifi::default(), lamp, false);
            let _ = driver.run(&mut NoAuth).await;
        });
    });
    (path, pty.slave)
}

fn run_cli(port: &str, args: &[&str], stdin: Option<&str>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_impromptu-wifi"))
        .args(["--port", port, "--timeout", "5"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run impromptu-wifi");
    let mut input = child.stdin.take().unwrap();
    if let Some(text) = stdin {
        input.write_all(text.as_bytes()).unwrap();
    }
    drop(input);
    child.wait_with_output().unwrap()
}

fn ok(out: &Output) -> String {
    assert!(
        out.status.success(),
        "impromptu-wifi failed: {}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).unwrap()
}

fn failure(out: &Output) -> String {
    assert!(
        !out.status.success(),
        "impromptu-wifi unexpectedly succeeded"
    );
    String::from_utf8(out.stderr.clone()).unwrap()
}

#[test]
fn cli_against_a_device_over_a_pseudo_terminal() {
    let (port, _keep) = start_device();

    let info = ok(&run_cli(&port, &["info"], None));
    assert!(info.contains("Name:     Desk Lamp"), "{info}");
    assert!(info.contains("Firmware: lamp-fw 2.0.0"), "{info}");

    let scan = ok(&run_cli(&port, &["scan"], None));
    assert!(scan.contains(" -40 dBm  secured    home"), "{scan}");
    assert!(scan.contains(" -71 dBm  open       cafe"), "{scan}");

    assert!(ok(&run_cli(&port, &["state"], None)).contains("Ready for Wi-Fi credentials"));

    let offline = ok(&run_cli(&port, &["network"], None));
    assert!(
        offline.contains("Online:   no") && offline.contains("Supports: Wi-Fi"),
        "{offline}"
    );

    // A wrong passphrase on the command line: a readable error and a failing exit code.
    let wrong = failure(&run_cli(
        &port,
        &["provision", "--ssid", "home", "--password", "nope"],
        None,
    ));
    // The device can see the network, so the passphrase is the suspect.
    assert!(
        wrong.contains("could not join \"home\" (seen at")
            && wrong.contains("Check the passphrase"),
        "{wrong}"
    );

    // The right one, piped on stdin as a script would.
    let joined = ok(&run_cli(
        &port,
        &["provision", "--ssid", "home"],
        Some("hunter22\n"),
    ));
    assert!(
        joined.contains("The device is at http://192.168.1.50"),
        "{joined}"
    );

    assert!(ok(&run_cli(&port, &["state"], None)).contains("Provisioned"));
    let online = ok(&run_cli(&port, &["network"], None));
    assert!(
        online.contains("Online:   yes") && online.contains("URL:      http://192.168.1.50"),
        "{online}"
    );

    assert_eq!(ok(&run_cli(&port, &["hostname"], None)).trim(), "lamp");
    assert_eq!(
        ok(&run_cli(&port, &["hostname", "lamp-2"], None)).trim(),
        "lamp-2"
    );
    assert!(failure(&run_cli(&port, &["hostname", "no spaces"], None))
        .contains("rejected that hostname"));

    // Identify is BLE-only: refused locally, and the port is still usable after.
    assert!(failure(&run_cli(&port, &["identify"], None))
        .contains("isn't available over this connection"));
    assert_eq!(ok(&run_cli(&port, &["hostname"], None)).trim(), "lamp-2");
}

#[test]
fn missing_device_choice_is_explained() {
    let out = Command::new(env!("CARGO_BIN_EXE_impromptu-wifi"))
        .arg("info")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("choose a device with --port"));
}

#[test]
fn reset_without_modem_lines_fails_clearly() {
    // A pseudo-terminal has no DTR/RTS lines, like an adapter without them
    // wired: --reset must fail with an explanation, not hang or misbehave.
    let (port, _keep) = start_device();
    let err = failure(&run_cli(&port, &["--reset", "info"], None));
    assert!(err.contains("resetting the device"), "{err}");
    // The device itself is unaffected.
    assert!(ok(&run_cli(&port, &["info"], None)).contains("Desk Lamp"));
}
