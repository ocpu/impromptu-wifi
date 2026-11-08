//! One Wi-Fi backend shared by several drivers, which agree on being
//! provisioned.
//!
//! A device that offers Improv Wi-Fi over serial and BLE at the same time
//! runs two [`Driver`](impromptu_wifi_core::Driver)s, but has one Wi-Fi
//! controller, which can't be cloned. Put the backend in a [`WifiShare`],
//! give each driver a [`SharedWifi`] handle, and pass each driver the
//! share's [`JoinEvents`]. When one driver provisions the device, the others
//! report `Provisioned` too, and a BLE driver's `provision_with` returns.
//! [`WifiShare::announce_unprovisioned`] moves them all back, so the device
//! can be provisioned again:
//!
//! ```ignore
//! static SHARE: StaticCell<WifiShare<CriticalSectionRawMutex, EspWifi, 2>> = StaticCell::new();
//! let share = SHARE.init(WifiShare::new(EspWifi::new(controller).with_stack(stack)));
//!
//! let mut serial = Driver::new(serial_link, share.handle(), device, false);
//! serial.run_with(&mut NoAuth, &mut share.events().unwrap()).await;
//! ```
//!
//! Each call holds the lock until it finishes, so a join started over one
//! transport makes a scan from the other wait for it. That is what a single
//! radio would do anyway.

use core::cell::Cell;

use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::watch::{Receiver, Watch};
use impromptu_wifi_core::traits::{
    ConnectError, DeviceEvent, EventSource, NetworkFlags, NetworkInfo, NetworkStatus, ScanError,
    Url, WifiConnect, WifiScanner,
};

/// A Wi-Fi backend `W` behind a mutex, and the last announcement of whether
/// the device is provisioned. `RECEIVERS` is how many [`JoinEvents`] can
/// exist, one per driver.
pub struct WifiShare<M: RawMutex, W, const RECEIVERS: usize> {
    wifi: Mutex<M, W>,
    joined: Watch<M, DeviceEvent, RECEIVERS>,
    /// Joins in progress through a handle.
    joining: BlockingMutex<M, Cell<u8>>,
}

/// What a device is doing with its network, for a status indicator; see
/// [`WifiShare::status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum WifiStatus {
    /// Not on a network: waiting for a client to provision it.
    Unprovisioned,
    /// Joining a network, for a client or to rejoin a saved one.
    Joining,
    /// On a network.
    Provisioned,
}

impl<M: RawMutex, W, const RECEIVERS: usize> WifiShare<M, W, RECEIVERS> {
    /// Share `wifi`.
    pub const fn new(wifi: W) -> Self {
        Self {
            wifi: Mutex::new(wifi),
            joined: Watch::new(),
            joining: BlockingMutex::new(Cell::new(0)),
        }
    }

    /// A handle for one driver. Handles are `Copy`.
    pub fn handle(&self) -> SharedWifi<'_, M, W, RECEIVERS> {
        SharedWifi { share: self }
    }

    /// The joins made through any handle, as [`DeviceEvent::Provisioned`],
    /// for one driver's [`EventSource`]. `None` once `RECEIVERS` exist.
    ///
    /// A receiver taken after a join still reports that join, so a driver
    /// started later learns that the device is already provisioned.
    pub fn events(&self) -> Option<JoinEvents<'_, M, RECEIVERS>> {
        self.joined.receiver().map(JoinEvents)
    }

    /// Tell every driver that the device left its network and can be
    /// provisioned again ([`DeviceEvent::Unprovisioned`]), for example after
    /// it could not rejoin. A successful join through any handle announces
    /// [`DeviceEvent::Provisioned`] again.
    pub fn announce_unprovisioned(&self) {
        self.joined.sender().send(DeviceEvent::Unprovisioned);
    }

    /// Whether the last announcement was [`DeviceEvent::Provisioned`].
    pub fn is_provisioned(&self) -> bool {
        matches!(self.joined.try_get(), Some(DeviceEvent::Provisioned(_)))
    }

    /// What the device is doing with its network: joining while any handle
    /// is in [`connect`](WifiConnect::connect), otherwise whether the last
    /// announcement was `Provisioned`. Doesn't wait for the lock, so an LED
    /// task can read it on every frame.
    pub fn status(&self) -> WifiStatus {
        if self.joining.lock(Cell::get) > 0 {
            WifiStatus::Joining
        } else if self.is_provisioned() {
            WifiStatus::Provisioned
        } else {
            WifiStatus::Unprovisioned
        }
    }

    /// The backend, for the application: to read the credentials after a
    /// join, for example.
    pub fn wifi(&self) -> &Mutex<M, W> {
        &self.wifi
    }
}

/// A handle to a [`WifiShare`]. Implements the Wi-Fi traits `W` implements,
/// including whether the optional ones are supported.
pub struct SharedWifi<'a, M: RawMutex, W, const RECEIVERS: usize> {
    share: &'a WifiShare<M, W, RECEIVERS>,
}

impl<M: RawMutex, W, const R: usize> Clone for SharedWifi<'_, M, W, R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M: RawMutex, W, const R: usize> Copy for SharedWifi<'_, M, W, R> {}

impl<M: RawMutex, W: WifiConnect, const R: usize> WifiConnect for SharedWifi<'_, M, W, R> {
    async fn connect(&mut self, ssid: &str, password: &str) -> Result<Option<Url>, ConnectError> {
        let result = {
            let _joining = Joining::start(&self.share.joining);
            self.share.wifi.lock().await.connect(ssid, password).await
        };
        if let Ok(url) = &result {
            self.share
                .joined
                .sender()
                .send(DeviceEvent::Provisioned(url.clone()));
        }
        result
    }
}

impl<M: RawMutex, W: WifiScanner, const R: usize> WifiScanner for SharedWifi<'_, M, W, R> {
    const SUPPORTED: bool = W::SUPPORTED;

    async fn scan<const N: usize>(
        &mut self,
        out: &mut heapless::Vec<NetworkInfo, N>,
    ) -> Result<(), ScanError> {
        self.share.wifi.lock().await.scan(out).await
    }
}

impl<M: RawMutex, W: NetworkStatus, const R: usize> NetworkStatus for SharedWifi<'_, M, W, R> {
    const SUPPORTED: bool = W::SUPPORTED;

    async fn network_flags(&self) -> NetworkFlags {
        self.share.wifi.lock().await.network_flags().await
    }

    async fn current_url(&self) -> Option<Url> {
        self.share.wifi.lock().await.current_url().await
    }
}

/// Counts a join in progress, also when the join is cancelled.
struct Joining<'a, M: RawMutex>(&'a BlockingMutex<M, Cell<u8>>);

impl<'a, M: RawMutex> Joining<'a, M> {
    fn start(count: &'a BlockingMutex<M, Cell<u8>>) -> Self {
        count.lock(|c| c.set(c.get().saturating_add(1)));
        Self(count)
    }
}

impl<M: RawMutex> Drop for Joining<'_, M> {
    fn drop(&mut self) {
        self.0.lock(|c| c.set(c.get().saturating_sub(1)));
    }
}

/// Joins made through a [`WifiShare`], as [`DeviceEvent::Provisioned`], and
/// [`DeviceEvent::Unprovisioned`] from
/// [`announce_unprovisioned`](WifiShare::announce_unprovisioned).
///
/// Only the latest announcement is kept: a driver that is busy while two
/// happen sees the second.
pub struct JoinEvents<'a, M: RawMutex, const RECEIVERS: usize>(
    Receiver<'a, M, DeviceEvent, RECEIVERS>,
);

impl<M: RawMutex, const R: usize> EventSource for JoinEvents<'_, M, R> {
    async fn next(&mut self) -> DeviceEvent {
        // Cancel-safe: the receiver only marks a value seen when `changed`
        // returns it.
        self.0.changed().await
    }
}
