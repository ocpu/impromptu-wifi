//! Bindings for `trouble-host`.
#[cfg(feature = "trouble")]
mod peripheral;
#[cfg(feature = "trouble")]
pub use peripheral::*;

#[cfg(feature = "trouble-central")]
pub mod central;
