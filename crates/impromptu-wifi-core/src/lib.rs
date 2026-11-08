//! The protocol logic of the `impromptu-wifi-*` stack, independent of transport.
//!
//! Layers, from pure to effectful:
//!
//! * [`machine::Machine`]: a synchronous, sans-IO state machine. It decides
//!   what each command means in the current state, and tracks which state
//!   and error updates the client still needs to see. Fully unit-testable.
//! * [`traits`] (re-exported from `impromptu-wifi-traits`): what the device must
//!   provide. One Wi-Fi object ([`WifiConnect`], [`WifiScanner`],
//!   [`NetworkStatus`]) and one device object ([`DeviceInfoSource`],
//!   [`Identify`], [`HostnameProvider`]).
//! * [`link::Link`]: what a transport must provide: receive RPC frames,
//!   and send states, errors and results. `impromptu-wifi-serial` and `impromptu-wifi-ble`
//!   implement it.
//! * [`Driver`]: an executor-agnostic async loop gluing them together.
#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
// The traits are used with whatever executor the platform provides, so we
// deliberately don't require `Send` futures.
#![allow(async_fn_in_trait)]

pub mod driver;
pub mod link;
pub mod machine;

pub use driver::{ble_auth_name, Driver};
pub use link::Link;
pub use machine::{Decision, Machine};

/// Re-export of the wire-format crate.
pub use impromptu_wifi_proto as proto;
/// Re-export of the backend traits crate.
pub use impromptu_wifi_traits as traits;
pub use impromptu_wifi_traits::{
    unsupported, AlwaysAvailable, AuthEvent, AuthSource, AvailabilitySource, ConnectError,
    DeviceInfoSource, DeviceNameError, HostnameError, HostnameProvider, Identify, NetworkFlags,
    NetworkInfo, NetworkStatus, NoAuth, ScanError, Url, WifiAuth, WifiConnect, WifiScanner,
};
