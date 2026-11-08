//! Improv Wi-Fi client for web browsers.
//!
//! Provisions devices from a web page over Web Serial and Web Bluetooth,
//! reusing [`impromptu_wifi_client::Client`] with the serial and BLE links of
//! `impromptu-wifi-serial` and `impromptu-wifi-ble`. The browser code is
//! compiled only for `wasm32` with `RUSTFLAGS=--cfg=web_sys_unstable_apis`,
//! which `web-sys` requires for its Web Serial and Web Bluetooth bindings;
//! on other targets this crate contains only the page text, so the workspace
//! builds and tests everywhere.

pub mod chunks;
pub mod text;

#[cfg(all(target_arch = "wasm32", web_sys_unstable_apis))]
pub mod browser {
    //! The browser links: Web Serial, Web Bluetooth, and timers.
    pub mod serial;
    pub mod time;
}

/// Why a browser link failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebError {
    /// The device went away: unplugged, out of range, or the port closed.
    Disconnected,
    /// The browser reported this error.
    Js(String),
}

impl core::fmt::Display for WebError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Disconnected => f.write_str("the device disconnected"),
            Self::Js(e) => f.write_str(e),
        }
    }
}

impl core::error::Error for WebError {}

impl embedded_io_async::Error for WebError {
    fn kind(&self) -> embedded_io_async::ErrorKind {
        match self {
            Self::Disconnected => embedded_io_async::ErrorKind::BrokenPipe,
            Self::Js(_) => embedded_io_async::ErrorKind::Other,
        }
    }
}
