//! Bindings for `trouble-host`.
#[cfg(feature = "trouble")]
mod peripheral;
#[cfg(feature = "trouble")]
pub use peripheral::*;
#[cfg(feature = "trouble")]
pub mod gatt;
#[cfg(feature = "trouble")]
pub use gatt::{add_gap_and_gatt, DatabaseHash, GAP_GATT_ATTRIBUTE_COUNT};

#[cfg(feature = "trouble-central")]
pub mod central;
