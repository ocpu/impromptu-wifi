//! Improv Wi-Fi over Bluetooth LE, for both ends of the connection.
//!
//! This crate does not depend on a BLE stack. Instead it defines the small
//! surface each role needs, for platform crates to implement on their stack:
//!
//! * Device: [`GattServer`] (trouble, nrf-softdevice, ESP-IDF, BlueZ…),
//!   wrapped by [`BleLink`] for [`impromptu_wifi_core::Driver`].
//! * Client: [`GattClient`] (btleplug, bluer, Web Bluetooth, trouble in
//!   central mode…), wrapped by [`BleClientLink`] for
//!   [`impromptu_wifi_client::Client`].
//!
//! ```ignore
//! // Device
//! const CAPS: Capabilities = impromptu_wifi_ble::capabilities::<MyWifi, MyDevice>();
//! let mut driver = Driver::new(BleLink::new(gatt_server, CAPS), wifi, device, true);
//! let url = driver.provision(&mut button_auth).await?;
//!
//! // Client
//! let mut client = Client::new(BleClientLink::new(gatt_client));
//! if client.current_state().await? == State::AuthorizationRequired {
//!     // ask the user to press the device's button
//!     client.wait_for_state(State::Authorized).await?;
//! }
//! let url = client.provision("home", "hunter22").await?;
//! ```
#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(async_fn_in_trait)]

mod client;
mod device;

pub use client::{BleClientLink, GattClient, Notification};
pub use device::{capabilities, BleLink, GattServer};

pub use impromptu_wifi_client;
pub use impromptu_wifi_core;
