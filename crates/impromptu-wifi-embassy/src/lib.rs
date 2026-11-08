//! Improv Wi-Fi glue for embassy-based firmware.
//!
//! This is the most specialised layer of the stack: it binds the
//! stack-agnostic traits to concrete embassy-ecosystem crates.
//!
//! * [`ButtonAuth`]: an [`AuthSource`] for BLE authorization, from a button
//!   (any `embedded_hal_async::digital::Wait` pin) and an `embassy-time`
//!   revoke timeout.
//! * `trouble` (feature `trouble`): the Improv GATT service for
//!   `trouble-host` and a [`GattServer`](impromptu_wifi_ble::GattServer) on it.
//! * `trouble::central` (feature `trouble-central`): a
//!   [`GattClient`](impromptu_wifi_ble::GattClient) on `trouble-host`, for firmware
//!   that provisions other devices.
//!
//! * `net` (feature `embassy-net`): redirect URLs and connectivity flags
//!   from an `embassy-net` stack, for any Wi-Fi backend.
//! * `esp` (feature `esp`): an ESP32 Wi-Fi backend over `esp-radio`,
//!   implementing [`WifiConnect`], [`WifiScanner`] and [`NetworkStatus`].
//!
//! * `usb` (feature `usb`): serial over USB CDC-ACM with `embassy-usb`,
//!   including the zero-length packets USB bulk transfers need.
//!
//! UARTs need no glue beyond choosing the right type: see
//! [`impromptu_wifi_serial`] for which HAL types implement `embedded-io-async`, and
//! [`Duplex`] for joining split halves.
//!
//! On other chips, implement [`WifiConnect`] and [`WifiScanner`] over your
//! Wi-Fi driver; the `net` helpers still apply.
#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(async_fn_in_trait)]

mod auth;
#[cfg(feature = "esp")]
pub mod esp;
#[cfg(feature = "embassy-net")]
pub mod net;
pub mod shared;
#[cfg(any(feature = "trouble", feature = "trouble-central"))]
pub mod trouble;
#[cfg(feature = "usb")]
pub mod usb;

pub use auth::ButtonAuth;
pub use shared::SharedWifi;

pub use impromptu_wifi_ble::{BleClientLink, BleLink};
pub use impromptu_wifi_client::Client;
pub use impromptu_wifi_core::traits::*;
pub use impromptu_wifi_core::Driver;
pub use impromptu_wifi_serial::{Duplex, SerialClientLink, SerialLink};
