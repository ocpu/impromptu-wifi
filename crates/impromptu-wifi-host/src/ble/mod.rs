//! Bluetooth LE client side.
//!
//! [`discover`] lists nearby Improv Wi-Fi devices with their state and
//! capabilities, read from their advertisements without connecting.
//! [`find`] scans until one device matches. [`BleGattClient`] connects to a
//! device and implements [`GattClient`](impromptu_wifi_ble::GattClient) for
//! [`BleClientLink`](impromptu_wifi_ble::BleClientLink).
//!
//! On Linux this talks to BlueZ over D-Bus with zbus, so it needs no C
//! library. On other systems it uses `btleplug` (CoreBluetooth on macOS,
//! WinRT on Windows).

use std::fmt;

use impromptu_wifi_client::proto::ble::Capabilities;
use impromptu_wifi_client::proto::State;

#[cfg(target_os = "linux")]
mod bluez;
#[cfg(target_os = "linux")]
use bluez as platform;

#[cfg(not(target_os = "linux"))]
mod btle;
#[cfg(not(target_os = "linux"))]
use btle as platform;

pub use platform::{discover, find, first_adapter, BleAdapter, BleGattClient, BlePeripheral};

/// Why a BLE operation failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum BleError {
    /// A D-Bus call to BlueZ failed, or BlueZ refused it.
    #[cfg(target_os = "linux")]
    Dbus(zbus::Error),
    /// The platform BLE stack failed.
    #[cfg(not(target_os = "linux"))]
    Btle(btleplug::Error),
    /// The system has no Bluetooth adapter.
    NoAdapter,
    /// The peripheral lacks this Improv Wi-Fi characteristic.
    MissingCharacteristic(&'static str),
    /// The device disconnected.
    Disconnected,
    /// This step took too long.
    Timeout(&'static str),
}

impl fmt::Display for BleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(target_os = "linux")]
            Self::Dbus(e) => write!(f, "BlueZ: {e}"),
            #[cfg(not(target_os = "linux"))]
            Self::Btle(e) => write!(f, "Bluetooth error: {e}"),
            Self::NoAdapter => f.write_str("no Bluetooth adapter found"),
            Self::MissingCharacteristic(c) => {
                write!(f, "device has no Improv Wi-Fi {c} characteristic")
            }
            Self::Disconnected => f.write_str("device disconnected"),
            Self::Timeout(what) => write!(f, "timed out {what}"),
        }
    }
}

// No `source()`: the message already includes the stack's own, whose text
// repeats its cause, so exposing it again would print it a third time.
impl std::error::Error for BleError {}

/// An Improv Wi-Fi device seen while scanning.
#[derive(Debug, Clone)]
pub struct Discovered {
    /// The device, for [`BleGattClient::connect`].
    pub peripheral: BlePeripheral,
    /// Its address, or an opaque identifier on macOS.
    pub address: String,
    /// Its advertised name, if any.
    pub name: Option<String>,
    /// Signal strength of the last advertisement, in dBm.
    pub rssi: Option<i16>,
    /// State from the advertised service data (spec 2.0 and later).
    pub state: Option<State>,
    /// Capabilities from the advertised service data.
    pub capabilities: Option<Capabilities>,
}

impl Discovered {
    /// Whether `query` names this device: its address (case-insensitive) or
    /// part of its name.
    pub fn matches(&self, query: &str) -> bool {
        self.address.eq_ignore_ascii_case(query)
            || self
                .name
                .as_deref()
                .is_some_and(|n| n.to_lowercase().contains(&query.to_lowercase()))
    }
}

/// Strongest signal first; devices without a reading last.
fn sort_by_signal(found: &mut [Discovered]) {
    found.sort_by_key(|d| std::cmp::Reverse(d.rssi.unwrap_or(i16::MIN)));
}
