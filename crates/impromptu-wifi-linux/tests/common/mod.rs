//! A private `dbus-daemon` for tests, so nothing touches the system bus.

use tokio::io::{AsyncBufReadExt, BufReader};
use zbus::Connection;

pub struct Bus {
    pub address: String,
    _daemon: tokio::process::Child,
    _dir: std::path::PathBuf,
}

pub async fn private_bus() -> Bus {
    // Plain characters only: the directory ends up in a D-Bus address.
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("impromptu-wifi-dbus-{}-{n}", std::process::id()));
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

pub async fn connect(bus: &Bus) -> Connection {
    zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap()
}
