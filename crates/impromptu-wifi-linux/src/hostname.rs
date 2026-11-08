//! Hostname and device name through systemd-hostnamed.
//!
//! The Improv hostname is the *static* hostname; the device name is the
//! *pretty* hostname (a free-form label), falling back to the hostname when
//! none is set. Both survive reboots. Setting them needs root or a polkit
//! rule; see the `dist/` directory of this crate.

use impromptu_wifi_core::traits::{
    DeviceInfoSource, DeviceNameError, HostnameError, HostnameProvider,
};
use zbus::proxy::CacheProperties;
use zbus::{proxy, Connection};

#[proxy(
    interface = "org.freedesktop.hostname1",
    default_service = "org.freedesktop.hostname1",
    default_path = "/org/freedesktop/hostname1"
)]
trait Hostname1 {
    fn set_static_hostname(&self, hostname: &str, interactive: bool) -> zbus::Result<()>;
    fn set_pretty_hostname(&self, hostname: &str, interactive: bool) -> zbus::Result<()>;
    #[zbus(property)]
    fn hostname(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn static_hostname(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn pretty_hostname(&self) -> zbus::Result<String>;
}

/// This Linux machine, as an Improv device.
///
/// Clone it to serve several transports. Each clone caches the names it
/// last read or set, so a name changed through one transport shows up on
/// the others after a restart.
#[derive(Clone)]
pub struct LinuxDevice {
    hostnamed: Hostname1Proxy<'static>,
    firmware_name: String,
    firmware_version: String,
    chip: String,
    os: Option<(String, String)>,
    hostname: String,
    pretty: String,
}

impl LinuxDevice {
    /// Describe this machine. `firmware_name` and `firmware_version`
    /// identify your software; the chip comes from the device tree or DMI,
    /// and the OS from `/etc/os-release`.
    pub async fn new(
        conn: &Connection,
        firmware_name: &str,
        firmware_version: &str,
    ) -> zbus::Result<Self> {
        let hostnamed = Hostname1Proxy::builder(conn)
            .cache_properties(CacheProperties::No)
            .build()
            .await?;
        let mut static_name = hostnamed.static_hostname().await.unwrap_or_default();
        if static_name.is_empty() {
            static_name = hostnamed.hostname().await?;
        }
        let pretty = hostnamed.pretty_hostname().await.unwrap_or_default();
        Ok(Self {
            hostnamed,
            firmware_name: firmware_name.into(),
            firmware_version: firmware_version.into(),
            chip: board_model().await,
            os: os_release().await,
            hostname: static_name,
            pretty,
        })
    }

    /// Override the chip/board string reported in device info.
    pub fn with_chip(mut self, chip: &str) -> Self {
        self.chip = chip.into();
        self
    }
}

/// The board model: the device tree on single-board computers (for example
/// "Raspberry Pi 4 Model B Rev 1.4"), DMI on PCs, else the CPU architecture.
async fn board_model() -> String {
    for path in ["/proc/device-tree/model", "/sys/class/dmi/id/product_name"] {
        if let Ok(text) = tokio::fs::read_to_string(path).await {
            let text = text.trim_matches(|c: char| c == '\0' || c.is_whitespace());
            if !text.is_empty() {
                return text.into();
            }
        }
    }
    std::env::consts::ARCH.into()
}

/// `NAME` and `VERSION_ID` from os-release.
async fn os_release() -> Option<(String, String)> {
    let text = match tokio::fs::read_to_string("/etc/os-release").await {
        Ok(t) => t,
        Err(_) => tokio::fs::read_to_string("/usr/lib/os-release")
            .await
            .ok()?,
    };
    Some(parse_os_release(&text))
}

fn parse_os_release(text: &str) -> (String, String) {
    let get = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
            .map(|v| v.trim().trim_matches('"').to_string())
            .unwrap_or_default()
    };
    let name = get("NAME");
    (
        if name.is_empty() {
            "Linux".into()
        } else {
            name
        },
        get("VERSION_ID"),
    )
}

/// hostnamed rejects invalid names with `InvalidArgs`; anything else
/// (typically a polkit refusal) is a failure to apply.
fn rejected(e: &zbus::Error) -> bool {
    matches!(e, zbus::Error::MethodError(name, _, _) if name.as_str() == "org.freedesktop.DBus.Error.InvalidArgs")
}

impl DeviceInfoSource for LinuxDevice {
    fn firmware_name(&self) -> &str {
        &self.firmware_name
    }
    fn firmware_version(&self) -> &str {
        &self.firmware_version
    }
    fn chip(&self) -> &str {
        &self.chip
    }
    fn device_name(&self) -> &str {
        if self.pretty.is_empty() {
            &self.hostname
        } else {
            &self.pretty
        }
    }
    fn os_name(&self) -> Option<&str> {
        self.os.as_ref().map(|(n, _)| n.as_str())
    }
    fn os_version(&self) -> Option<&str> {
        self.os.as_ref().map(|(_, v)| v.as_str())
    }
    async fn set_device_name(&mut self, name: &str) -> Result<(), DeviceNameError> {
        match self.hostnamed.set_pretty_hostname(name, false).await {
            Ok(()) => {
                self.pretty = name.into();
                Ok(())
            }
            Err(e) if rejected(&e) => Err(DeviceNameError::InvalidName),
            Err(_) => Err(DeviceNameError::Failed),
        }
    }
}

impl HostnameProvider for LinuxDevice {
    fn hostname(&self) -> &str {
        &self.hostname
    }
    async fn set_hostname(&mut self, name: &str) -> Result<(), HostnameError> {
        match self.hostnamed.set_static_hostname(name, false).await {
            Ok(()) => {
                self.hostname = name.into();
                Ok(())
            }
            Err(e) if rejected(&e) => Err(HostnameError::InvalidName),
            Err(_) => Err(HostnameError::Failed),
        }
    }
}

impromptu_wifi_core::unsupported!(LinuxDevice: Identify);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_release_fields() {
        let text = "PRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\nNAME=\"Debian GNU/Linux\"\nVERSION_ID=\"12\"\n";
        assert_eq!(
            parse_os_release(text),
            ("Debian GNU/Linux".into(), "12".into())
        );
        assert_eq!(parse_os_release(""), ("Linux".into(), String::new()));
    }
}
