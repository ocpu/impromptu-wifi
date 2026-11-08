//! Improv Wi-Fi provisioning for Rust.
//!
//! This crate implements the [Improv Wi-Fi](https://www.improv-wifi.com)
//! protocol by re-exporting the `impromptu-wifi-*` crates as modules. Cargo
//! features select which modules exist. The crate is `no_std`; the `host*`
//! and `linux*` features add `std` and tokio.
//!
//! | Feature | Module | Contents |
//! |---|---|---|
//! | (always) | [`proto`] | Wire format, both transports |
//! | (always) | [`traits`] | Backend traits a device implements |
//! | `device` | `device` | State machine and async `Driver` |
//! | `client` | `client` | Session machine and async `Client` |
//! | `serial` | `serial` | Serial transport, both roles |
//! | `ble` | `ble` | BLE transport over GATT traits, both roles |
//! | `embassy` | `embassy` | `ButtonAuth`, `Button`, `WifiShare`, `keep_online`; `embassy-trouble`, `embassy-trouble-central`, `embassy-usb`, `embassy-net`, `embassy-flash` and `esp` add submodules |
//! | `host` | `host` | Desktop client side; `host-serial` and `host-ble` select transports |
//! | `linux` | `linux` | Linux device side; `linux-networkmanager`, `linux-hostnamed`, `linux-serial` and `linux-bluez` select backends |
//! | `defmt` | | `defmt::Format` for every enabled `no_std` layer |
//!
//! Features enable the layers they build on: `serial` and `ble` enable
//! `device` and `client`, and each platform feature enables its transports.
//!
//! Without features the minimum supported Rust version is 1.87. The
//! `embassy*`, `esp`, `host*` and `linux*` features need 1.91.
//!
//! ```
//! use impromptu_wifi::proto::{State, Transport};
//!
//! // Authorization states exist only on BLE.
//! assert!(State::AuthorizationRequired.valid_for(Transport::Ble));
//! assert!(!State::AuthorizationRequired.valid_for(Transport::Serial));
//! ```
#![no_std]
#![cfg_attr(docsrs, feature(doc_cfg))]

#[doc(inline)]
pub use impromptu_wifi_proto as proto;
#[doc(inline)]
pub use impromptu_wifi_traits as traits;

#[cfg(feature = "device")]
#[doc(inline)]
pub use impromptu_wifi_core as device;

#[cfg(feature = "client")]
#[doc(inline)]
pub use impromptu_wifi_client as client;

#[cfg(feature = "serial")]
#[doc(inline)]
pub use impromptu_wifi_serial as serial;

#[cfg(feature = "ble")]
#[doc(inline)]
pub use impromptu_wifi_ble as ble;

#[cfg(feature = "embassy")]
#[doc(inline)]
pub use impromptu_wifi_embassy as embassy;

#[cfg(feature = "host")]
#[doc(inline)]
pub use impromptu_wifi_host as host;

#[cfg(feature = "linux")]
#[doc(inline)]
pub use impromptu_wifi_linux as linux;
