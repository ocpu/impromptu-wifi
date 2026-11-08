//! Save the credentials of each successful join, and get them back at boot.

use impromptu_wifi_traits::{
    ConnectError, CredentialStore, Credentials, NetworkFlags, NetworkInfo, NetworkStatus,
    ScanError, Url, WifiConnect, WifiScanner,
};

/// A Wi-Fi backend that saves the credentials to a [`CredentialStore`] after
/// each successful [`connect`](WifiConnect::connect), and rejoins with them
/// at boot.
///
/// ```ignore
/// let mut wifi = PersistOnJoin::new(EspWifi::new(controller), FlashStore::new(flash, offset)?);
/// if let Ok(Some(saved)) = wifi.restore().await {
///     if let Ok(url) = wifi.connect(&saved.ssid, &saved.password).await {
///         driver.mark_provisioned(url);
///     }
/// }
/// ```
///
/// A join with the credentials already saved writes nothing, so rejoining
/// at boot or after a dropped connection doesn't wear the flash.
///
/// A failed save doesn't fail the join: the device is on the network, and
/// the client should hear that. The error is kept for the application, in
/// [`take_save_error`](Self::take_save_error).
///
/// The optional traits are forwarded, including whether they are supported.
pub struct PersistOnJoin<W, S: CredentialStore> {
    wifi: W,
    store: S,
    /// What the store holds, as far as this wrapper knows.
    saved: Option<Credentials>,
    save_error: Option<S::Error>,
}

impl<W, S: CredentialStore> PersistOnJoin<W, S> {
    /// Wrap `wifi`, saving to `store`.
    pub fn new(wifi: W, store: S) -> Self {
        Self {
            wifi,
            store,
            saved: None,
            save_error: None,
        }
    }

    /// Load the saved credentials, to rejoin with at boot.
    pub async fn restore(&mut self) -> Result<Option<Credentials>, S::Error> {
        self.saved = self.store.load().await?;
        Ok(self.saved.clone())
    }

    /// The saved credentials, from the last [`restore`](Self::restore) or
    /// successful join. Doesn't read the store.
    pub fn saved(&self) -> Option<&Credentials> {
        self.saved.as_ref()
    }

    /// Clear the store, for a factory reset. Doesn't leave the current
    /// network; restart the device or the drivers after this.
    pub async fn forget(&mut self) -> Result<(), S::Error> {
        self.saved = None;
        self.store.clear().await
    }

    /// Why the last save failed, if it did since the last call.
    pub fn take_save_error(&mut self) -> Option<S::Error> {
        self.save_error.take()
    }

    /// The wrapped backend.
    pub fn inner(&self) -> &W {
        &self.wifi
    }

    /// The wrapped backend, mutably.
    pub fn inner_mut(&mut self) -> &mut W {
        &mut self.wifi
    }

    /// The store.
    pub fn store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    /// Unwrap the backend and the store.
    pub fn into_parts(self) -> (W, S) {
        (self.wifi, self.store)
    }
}

impl<W: WifiConnect, S: CredentialStore> WifiConnect for PersistOnJoin<W, S> {
    async fn connect(&mut self, ssid: &str, password: &str) -> Result<Option<Url>, ConnectError> {
        let url = self.wifi.connect(ssid, password).await?;
        let unchanged = self
            .saved
            .as_ref()
            .is_some_and(|c| c.ssid == ssid && c.password == password);
        if !unchanged {
            // Longer than the protocol allows only if the backend accepted
            // something the driver never sends; nothing to save then.
            if let Some(credentials) = Credentials::new(ssid, password) {
                match self.store.save(&credentials).await {
                    Ok(()) => self.saved = Some(credentials),
                    Err(e) => self.save_error = Some(e),
                }
            }
        }
        Ok(url)
    }
}

impl<W: WifiScanner, S: CredentialStore> WifiScanner for PersistOnJoin<W, S> {
    const SUPPORTED: bool = W::SUPPORTED;

    async fn scan<const N: usize>(
        &mut self,
        out: &mut heapless::Vec<NetworkInfo, N>,
    ) -> Result<(), ScanError> {
        self.wifi.scan(out).await
    }
}

impl<W: NetworkStatus, S: CredentialStore> NetworkStatus for PersistOnJoin<W, S> {
    const SUPPORTED: bool = W::SUPPORTED;

    async fn network_flags(&self) -> NetworkFlags {
        self.wifi.network_flags().await
    }

    async fn current_url(&self) -> Option<Url> {
        self.wifi.current_url().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use embassy_futures::block_on;
    use impromptu_wifi_traits::unsupported;

    #[derive(Default)]
    struct Radio;
    impl WifiConnect for Radio {
        async fn connect(&mut self, ssid: &str, _: &str) -> Result<Option<Url>, ConnectError> {
            match ssid {
                "home" | "cafe" => Ok(None),
                _ => Err(ConnectError::NetworkNotFound),
            }
        }
    }
    unsupported!(Radio: WifiScanner, NetworkStatus);

    #[derive(Default)]
    struct Store {
        held: Option<Credentials>,
        saves: u32,
        broken: bool,
    }
    impl CredentialStore for Store {
        type Error = ();
        async fn load(&mut self) -> Result<Option<Credentials>, ()> {
            Ok(self.held.clone())
        }
        async fn save(&mut self, c: &Credentials) -> Result<(), ()> {
            if self.broken {
                return Err(());
            }
            self.saves += 1;
            self.held = Some(c.clone());
            Ok(())
        }
        async fn clear(&mut self) -> Result<(), ()> {
            self.held = None;
            Ok(())
        }
    }

    #[test]
    fn saves_successful_joins_once() {
        let mut wifi = PersistOnJoin::new(Radio, Store::default());
        assert!(block_on(wifi.connect("nowhere", "pw")).is_err());
        assert_eq!(wifi.store_mut().saves, 0, "a failed join saves nothing");

        block_on(wifi.connect("home", "pw")).unwrap();
        block_on(wifi.connect("home", "pw")).unwrap();
        assert_eq!(
            wifi.store_mut().saves,
            1,
            "the same network again isn't rewritten"
        );
        assert_eq!(wifi.saved().unwrap().ssid.as_str(), "home");

        block_on(wifi.connect("cafe", "other")).unwrap();
        assert_eq!(wifi.store_mut().saves, 2);
        assert_eq!(wifi.saved().unwrap().ssid.as_str(), "cafe");
    }

    #[test]
    fn restore_then_rejoin_writes_nothing() {
        let store = Store {
            held: Credentials::new("home", "pw"),
            ..Store::default()
        };
        let mut wifi = PersistOnJoin::new(Radio, store);
        let saved = block_on(wifi.restore()).unwrap().unwrap();
        block_on(wifi.connect(&saved.ssid, &saved.password)).unwrap();
        assert_eq!(wifi.store_mut().saves, 0);
    }

    #[test]
    fn a_failed_save_keeps_the_join_and_the_error() {
        let store = Store {
            broken: true,
            ..Store::default()
        };
        let mut wifi = PersistOnJoin::new(Radio, store);
        assert!(block_on(wifi.connect("home", "pw")).is_ok());
        assert!(wifi.saved().is_none());
        assert_eq!(wifi.take_save_error(), Some(()));
        assert_eq!(wifi.take_save_error(), None);
    }

    #[test]
    fn forget_clears_the_store() {
        let mut wifi = PersistOnJoin::new(Radio, Store::default());
        block_on(wifi.connect("home", "pw")).unwrap();
        block_on(wifi.forget()).unwrap();
        assert!(wifi.saved().is_none());
        assert_eq!(block_on(wifi.restore()).unwrap(), None);
    }
}
