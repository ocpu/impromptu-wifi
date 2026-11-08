//! The facade exposes the paths the README examples use.

use impromptu_wifi::ble::{capabilities, BleClientLink, BleLink};
use impromptu_wifi::client::Client;
use impromptu_wifi::device::{
    unsupported, ConnectError, DeviceInfoSource, Driver, NoAuth, Url, WifiConnect,
};
use impromptu_wifi::proto::ble::Capabilities;
use impromptu_wifi::serial::{SerialClientLink, SerialLink};

struct Wifi;
impl WifiConnect for Wifi {
    async fn connect(&mut self, _ssid: &str, _password: &str) -> Result<Option<Url>, ConnectError> {
        Ok(None)
    }
}
unsupported!(Wifi: WifiScanner, NetworkStatus);

struct Device;
impl DeviceInfoSource for Device {
    fn firmware_name(&self) -> &str {
        "fw"
    }
    fn firmware_version(&self) -> &str {
        "1.0"
    }
    fn chip(&self) -> &str {
        "test"
    }
    fn device_name(&self) -> &str {
        "Test"
    }
}
unsupported!(Device: Identify, HostnameProvider);

#[test]
fn readme_paths_resolve() {
    // Nothing optional is supported, so only device info is advertised.
    const CAPS: Capabilities = capabilities::<Wifi, Device>();
    assert_eq!(CAPS, Capabilities::DEVICE_INFO);

    // The types exist under the facade's module paths.
    let _ = (
        core::any::type_name::<Driver<SerialLink<()>, Wifi, Device>>(),
        core::any::type_name::<NoAuth>(),
        core::any::type_name::<Client<SerialClientLink<()>>>(),
        core::any::type_name::<BleLink<()>>(),
        core::any::type_name::<BleClientLink<()>>(),
    );
}
