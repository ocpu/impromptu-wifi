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
pub mod queue;
pub mod slot;
pub mod text;
pub mod uuid;

#[cfg(all(target_arch = "wasm32", web_sys_unstable_apis))]
pub mod browser {
    //! The browser links: Web Serial, Web Bluetooth, and timers.
    pub mod ble;
    pub mod device;
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

/// Whether a link error means the device is gone, so the page should
/// return to its connect buttons.
pub trait LinkFailure {
    /// The connection ended.
    fn is_disconnect(&self) -> bool;
}

impl LinkFailure for WebError {
    fn is_disconnect(&self) -> bool {
        matches!(self, Self::Disconnected)
    }
}

impl LinkFailure for impromptu_wifi_serial::SerialError<WebError> {
    fn is_disconnect(&self) -> bool {
        use impromptu_wifi_serial::SerialError;
        match self {
            SerialError::Io(e) => e.is_disconnect(),
            SerialError::Eof => true,
            SerialError::Oversize => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use impromptu_wifi_serial::SerialError;

    #[test]
    fn disconnects_are_recognised() {
        assert!(WebError::Disconnected.is_disconnect());
        assert!(!WebError::Js("x".into()).is_disconnect());
        assert!(SerialError::Io(WebError::Disconnected).is_disconnect());
        assert!(SerialError::<WebError>::Eof.is_disconnect());
        assert!(!SerialError::<WebError>::Oversize.is_disconnect());
    }
}
