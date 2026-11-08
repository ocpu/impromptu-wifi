//! The client side of Improv Wi-Fi: discover what a device can do and send
//! it Wi-Fi credentials.
//!
//! The layering mirrors `impromptu-wifi-core`:
//!
//! * [`session::Session`]: a synchronous, sans-IO machine that decides which
//!   incoming state, error or result answers the outstanding command. The
//!   protocol has no request ids, so this is where the subtle rules live.
//! * [`ClientLink`]: what a transport must provide. `impromptu-wifi-serial` and
//!   `impromptu-wifi-ble` implement it.
//! * [`Client`]: an executor-agnostic async API on top.
//!
//! ```ignore
//! let mut client = Client::new(SerialClientLink::new(port));
//! let info = client.device_info().await?;
//! let networks = client.scan::<16>().await?;
//! let url = client.provision("home", "hunter22").await?;
//! ```
//!
//! # Timeouts
//!
//! The client never times out on its own: a Wi-Fi join can legitimately be
//! silent for tens of seconds, and a `no_std` crate can't pick a timer for
//! you. Race calls against your executor's timer. Dropping a call midway is
//! safe; the next call starts a fresh exchange.
#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(async_fn_in_trait)]

mod client;
mod diagnose;
mod link;
pub mod session;
mod types;

pub use client::{Client, ClientError};
pub use diagnose::{diagnose_join_failure, JoinFailure, LONG_SCAN};
pub use link::{ClientLink, Incoming};
pub use types::{DeviceInfo, NetworkState, ScannedNetwork, Text, MAX_TEXT};

pub use impromptu_wifi_proto as proto;
