//! Learn about every successful join, whichever driver made it.

use impromptu_wifi_traits::{
    ConnectError, NetworkFlags, NetworkInfo, NetworkStatus, ScanError, Url, WifiConnect,
    WifiScanner,
};

/// A Wi-Fi backend that calls `on_join` after each successful
/// [`connect`](WifiConnect::connect), with the redirect URL.
///
/// A device that serves several transports runs one driver per transport.
/// When one of them provisions the device, the others should report
/// `Provisioned` too. Wrap each driver's Wi-Fi object in `NotifyOnJoin`,
/// send the URL to the other drivers from `on_join`, and deliver it to them
/// as a [`DeviceEvent::Provisioned`](impromptu_wifi_traits::DeviceEvent)
/// through their [`EventSource`](impromptu_wifi_traits::EventSource).
///
/// ```ignore
/// let (tx, rx) = tokio::sync::watch::channel(None);
/// let wifi = NotifyOnJoin::new(nm.clone(), move |url| {
///     tx.send_replace(Some(url.clone()));
/// });
/// ```
///
/// The optional traits are forwarded, including whether they are supported.
pub struct NotifyOnJoin<W, F> {
    wifi: W,
    on_join: F,
}

impl<W, F: FnMut(&Option<Url>)> NotifyOnJoin<W, F> {
    /// Wrap `wifi`.
    pub fn new(wifi: W, on_join: F) -> Self {
        Self { wifi, on_join }
    }

    /// The wrapped backend.
    pub fn inner(&self) -> &W {
        &self.wifi
    }

    /// The wrapped backend, mutably.
    pub fn inner_mut(&mut self) -> &mut W {
        &mut self.wifi
    }

    /// Unwrap the backend.
    pub fn into_inner(self) -> W {
        self.wifi
    }
}

impl<W: WifiConnect, F: FnMut(&Option<Url>)> WifiConnect for NotifyOnJoin<W, F> {
    async fn connect(&mut self, ssid: &str, password: &str) -> Result<Option<Url>, ConnectError> {
        let result = self.wifi.connect(ssid, password).await;
        if let Ok(url) = &result {
            (self.on_join)(url);
        }
        result
    }
}

impl<W: WifiScanner, F> WifiScanner for NotifyOnJoin<W, F> {
    const SUPPORTED: bool = W::SUPPORTED;

    async fn scan<const N: usize>(
        &mut self,
        out: &mut heapless::Vec<NetworkInfo, N>,
    ) -> Result<(), ScanError> {
        self.wifi.scan(out).await
    }
}

impl<W: NetworkStatus, F> NetworkStatus for NotifyOnJoin<W, F> {
    const SUPPORTED: bool = W::SUPPORTED;

    async fn network_flags(&self) -> NetworkFlags {
        self.wifi.network_flags().await
    }

    async fn current_url(&self) -> Option<Url> {
        self.wifi.current_url().await
    }
}
