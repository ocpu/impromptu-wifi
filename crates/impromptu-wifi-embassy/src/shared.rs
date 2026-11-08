//! One Wi-Fi backend shared by several drivers.
//!
//! A device that offers Improv Wi-Fi over serial and BLE at the same time
//! runs two [`Driver`](impromptu_wifi_core::Driver)s, but has one Wi-Fi
//! controller, which can't be cloned. Put the backend in a
//! [`Mutex`] and give each driver a [`SharedWifi`]:
//!
//! ```ignore
//! static WIFI: StaticCell<Mutex<CriticalSectionRawMutex, EspWifi>> = StaticCell::new();
//! let wifi = WIFI.init(Mutex::new(EspWifi::new(controller).with_stack(stack)));
//!
//! let serial = Driver::new(serial_link, SharedWifi::new(wifi), device, false);
//! let ble = Driver::new(ble_link, SharedWifi::new(wifi), device, true);
//! ```
//!
//! Each call holds the lock until it finishes, so a join started over one
//! transport makes a scan from the other wait for it. That is what a single
//! radio would do anyway.

use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::mutex::Mutex;
use impromptu_wifi_core::traits::{
    ConnectError, NetworkFlags, NetworkInfo, NetworkStatus, ScanError, Url, WifiConnect,
    WifiScanner,
};

/// A handle to a Wi-Fi backend `W` behind a mutex. Implements the Wi-Fi
/// traits `W` implements, including whether the optional ones are
/// supported.
pub struct SharedWifi<'a, M: RawMutex, W> {
    wifi: &'a Mutex<M, W>,
}

impl<'a, M: RawMutex, W> SharedWifi<'a, M, W> {
    /// A handle to `wifi`.
    pub const fn new(wifi: &'a Mutex<M, W>) -> Self {
        Self { wifi }
    }

    /// The shared backend.
    pub fn inner(&self) -> &'a Mutex<M, W> {
        self.wifi
    }
}

impl<M: RawMutex, W> Clone for SharedWifi<'_, M, W> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M: RawMutex, W> Copy for SharedWifi<'_, M, W> {}

impl<M: RawMutex, W: WifiConnect> WifiConnect for SharedWifi<'_, M, W> {
    async fn connect(&mut self, ssid: &str, password: &str) -> Result<Option<Url>, ConnectError> {
        self.wifi.lock().await.connect(ssid, password).await
    }
}

impl<M: RawMutex, W: WifiScanner> WifiScanner for SharedWifi<'_, M, W> {
    const SUPPORTED: bool = W::SUPPORTED;

    async fn scan<const N: usize>(
        &mut self,
        out: &mut heapless::Vec<NetworkInfo, N>,
    ) -> Result<(), ScanError> {
        self.wifi.lock().await.scan(out).await
    }
}

impl<M: RawMutex, W: NetworkStatus> NetworkStatus for SharedWifi<'_, M, W> {
    const SUPPORTED: bool = W::SUPPORTED;

    async fn network_flags(&self) -> NetworkFlags {
        self.wifi.lock().await.network_flags().await
    }

    async fn current_url(&self) -> Option<Url> {
        self.wifi.lock().await.current_url().await
    }
}
