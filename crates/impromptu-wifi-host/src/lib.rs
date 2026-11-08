//! The Improv Wi-Fi client side on desktop operating systems, for tools
//! that provision devices: serial ports through `tokio-serial`, and
//! Bluetooth LE through BlueZ's D-Bus API on Linux (zbus, no C library) or
//! `btleplug` elsewhere (CoreBluetooth on macOS, WinRT on Windows).
//!
//! Both produce links for [`impromptu_wifi_client::Client`]:
//!
//! ```ignore
//! // Serial
//! let mut client = Client::new(SerialClientLink::new(impromptu_wifi_host::serial::open("/dev/ttyUSB0", 115_200)?));
//!
//! // BLE
//! let adapter = impromptu_wifi_host::ble::first_adapter().await?;
//! let devices = impromptu_wifi_host::ble::discover(&adapter, Duration::from_secs(5)).await?;
//! let gatt = impromptu_wifi_host::ble::BleGattClient::connect(devices[0].peripheral.clone()).await?;
//! let mut client = Client::new(BleClientLink::new(gatt));
//! ```
//!
//! All futures are `Send`, so they can be spawned on a multi-threaded tokio
//! runtime.

#[cfg(feature = "ble")]
pub mod ble;
#[cfg(feature = "serial")]
pub mod serial;

pub use impromptu_wifi_ble::BleClientLink;
pub use impromptu_wifi_client::{Client, ClientError};
pub use impromptu_wifi_serial::SerialClientLink;
