//! Improv Wi-Fi client for web browsers.
//!
//! Provisions devices from a web page over Web Serial and Web Bluetooth,
//! reusing [`impromptu_wifi_client::Client`] with the serial and BLE links of
//! `impromptu-wifi-serial` and `impromptu-wifi-ble`. The browser code is
//! compiled only for `wasm32` with `RUSTFLAGS=--cfg=web_sys_unstable_apis`,
//! which `web-sys` requires for its Web Serial and Web Bluetooth bindings;
//! on other targets this crate contains only the page text, so the workspace
//! builds and tests everywhere.

pub mod text;
