//! The Improv Wi-Fi device side on Linux: provisioning a Raspberry Pi or
//! similar machine the way an ESP32 is provisioned.
//!
//! * [`nm::NmWifi`] (feature `networkmanager`): Wi-Fi through
//!   NetworkManager, implementing [`WifiConnect`], [`WifiScanner`] and
//!   [`NetworkStatus`].
//! * [`wpa::WpaWifi`] (feature `wpa-supplicant`): Wi-Fi through
//!   wpa_supplicant's control socket, for systems without NetworkManager.
//! * [`hostname::LinuxDevice`] (feature `hostnamed`): device info, hostname
//!   and device name through systemd-hostnamed.
//! * [`serial`] (feature `serial`): UARTs and USB gadget ports.
//! * [`join`]: keeps the serial and Bluetooth drivers in agreement once
//!   either of them provisions the machine.
//! * [`bluez::BluezGatt`] (feature `bluez`): Bluetooth LE through BlueZ.
//! * The `impromptu-wifi-device` binary (feature `daemon`) ties these together as a
//!   systemd service; unit and polkit files are in `dist/`.
//!
//! The client side for Linux, macOS and Windows lives in `impromptu-wifi-host` and
//! the `impromptu-wifi` CLI.

#[cfg(feature = "bluez")]
pub mod bluez;
#[cfg(feature = "hostnamed")]
pub mod hostname;
pub mod join;
#[cfg(feature = "networkmanager")]
pub mod nm;
#[cfg(feature = "serial")]
pub mod serial;
#[cfg(feature = "wpa-supplicant")]
pub mod wpa;

pub use impromptu_wifi_ble::{BleLink, GattServer};
pub use impromptu_wifi_core::traits::*;
pub use impromptu_wifi_core::Driver;
pub use impromptu_wifi_serial::SerialLink;
