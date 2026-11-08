//! Compile-time check: futures over the host links are `Send`, so they can be
//! spawned on a multi-threaded tokio runtime. The traits don't require
//! `Send` (embedded executors don't need it), so this holds only because
//! Rust sees through to the concrete futures. If a change breaks that, this
//! file stops compiling.

use impromptu_wifi_core::proto::ble::Capabilities;
use impromptu_wifi_core::{
    unsupported, ConnectError, DeviceInfoSource, Driver, NoAuth, Url, WifiConnect,
};
use impromptu_wifi_host::ble::BtleGattClient;
use impromptu_wifi_host::serial::Port;
use impromptu_wifi_host::{BleClientLink, Client, SerialClientLink};
use impromptu_wifi_serial::SerialLink;

fn assert_send<T: Send>(_: &T) {}

#[allow(dead_code)]
fn client_futures(
    serial: &mut Client<SerialClientLink<Port>>,
    ble: &mut Client<BleClientLink<BtleGattClient>>,
) {
    assert_send(&serial.provision("ssid", "password"));
    assert_send(&serial.scan::<8>());
    assert_send(&ble.provision("ssid", "password"));
    assert_send(&ble.wait_for_state(impromptu_wifi_core::proto::State::Authorized));
    assert_send(&ble.capabilities());
}

struct Wifi;
impl WifiConnect for Wifi {
    async fn connect(&mut self, _: &str, _: &str) -> Result<Option<Url>, ConnectError> {
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
        "1"
    }
    fn chip(&self) -> &str {
        "linux"
    }
    fn device_name(&self) -> &str {
        "dev"
    }
}
unsupported!(Device: Identify, HostnameProvider);

#[allow(dead_code)]
fn device_futures(driver: &mut Driver<SerialLink<Port>, Wifi, Device>) {
    assert_send(&driver.run(&mut NoAuth));
}

#[test]
fn futures_are_send() {
    // The real check happened at compile time.
    let _ = Capabilities::NONE;
}
