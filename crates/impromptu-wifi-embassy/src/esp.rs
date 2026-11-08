//! An ESP32 Wi-Fi backend over `esp-radio` (1.0.0-beta.1, for esp-hal 1.2).
//!
//! Built in CI through `examples/esp32s3` only, as it needs a chip
//! feature. Written against the esp-radio 1.0.0-beta.1 sources.
//!
//! ```ignore
//! let (controller, interfaces) = /* esp-radio setup */;
//! let (stack, runner) = embassy_net::new(interfaces.station, net_config, resources, seed);
//! spawner.spawn(net_task(runner))?;
//!
//! let mut wifi = EspWifi::new(controller).with_stack(stack);
//! let mut driver = Driver::new(link, &mut wifi, &mut device, true);
//! let url = driver.provision(&mut auth).await?;
//! if let Some(creds) = wifi.credentials() {
//!     // persist creds.ssid / creds.password to flash
//! }
//! ```
//!
//! To persist them, wrap the backend in
//! [`PersistOnJoin`](impromptu_wifi_core::PersistOnJoin) with a
//! `FlashStore` (feature `flash`) instead, as `examples/esp32s3` does.
//!
//! # Requirements in the application
//!
//! * Your chip's esp-radio feature, for example `esp-radio/esp32c6`.
//! * `esp-hal` with its `unstable` feature: esp-radio uses unstable esp-hal
//!   APIs, and esp-hal's build script refuses to build otherwise.
//! * A global allocator (for example `esp-alloc`), which esp-radio requires.
//!
//! # WPA3
//!
//! esp-radio 1.0.0-beta.1 does not support WPA3 (its module documentation
//! says so, and its station configuration has no WPA3 method). A network in
//! WPA3-Personal-only mode can't be joined; `connect` fails. WPA2/WPA3
//! transition networks are joined over WPA2. Observed on an ESP32-S3 with a
//! phone hotspot, 2026-11-08.
//!
//! Without a stack, `connect` succeeds on association and returns no URL.
//! With one, it waits for DHCP and returns `http://<address>`, failing with
//! [`ConnectError::Timeout`] if no address arrives: a device without an
//! address isn't usefully provisioned.

use embassy_net::Stack;
use embassy_time::{with_timeout, Duration};
use esp_radio::wifi::scan::ScanConfig;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{
    AuthenticationMethod, AuthenticationMethodConfig, Config, ConnectionError, DisconnectReason,
    Password, Ssid as EspSsid, WifiController,
};
use impromptu_wifi_core::traits::{
    ConnectError, NetworkFlags, NetworkInfo, NetworkStatus, ScanError, Url, WifiAuth, WifiConnect,
    WifiScanner,
};

use crate::net::{wait_for_url, wifi_flags, DEFAULT_IP_TIMEOUT};

/// How long [`EspWifi`] waits for association by default.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

pub use impromptu_wifi_core::traits::Credentials;

/// ESP32 station-mode Wi-Fi for Improv.
pub struct EspWifi<'d, 'n> {
    controller: WifiController<'d>,
    stack: Option<Stack<'n>>,
    connect_timeout: Duration,
    ip_timeout: Duration,
    allow_legacy_wpa: bool,
    credentials: Option<Credentials>,
}

impl<'d, 'n> EspWifi<'d, 'n> {
    /// Wrap a station-capable controller.
    pub fn new(controller: WifiController<'d>) -> Self {
        Self {
            controller,
            stack: None,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            ip_timeout: DEFAULT_IP_TIMEOUT,
            allow_legacy_wpa: false,
            credentials: None,
        }
    }

    /// Wait for an address on `stack` after joining, and report its URL.
    pub fn with_stack(mut self, stack: Stack<'n>) -> Self {
        self.stack = Some(stack);
        self
    }

    /// How long to wait for association (default 30 s).
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// How long to wait for DHCP after association (default 20 s).
    pub fn ip_timeout(mut self, timeout: Duration) -> Self {
        self.ip_timeout = timeout;
        self
    }

    /// Also accept WPA (version 1) networks.
    ///
    /// ESP-IDF treats the configured security as a minimum. The default
    /// minimum is WPA2, which joins WPA2 networks and WPA2/WPA3 transition
    /// networks (over WPA2) but refuses WPA-only ones; this lowers it to WPA.
    pub fn allow_legacy_wpa(mut self, allow: bool) -> Self {
        self.allow_legacy_wpa = allow;
        self
    }

    /// Credentials of the last successful connection.
    pub fn credentials(&self) -> Option<&Credentials> {
        self.credentials.as_ref()
    }

    /// The controller, for anything Improv doesn't cover.
    pub fn controller_mut(&mut self) -> &mut WifiController<'d> {
        &mut self.controller
    }

    /// Give back the controller.
    pub fn into_inner(self) -> WifiController<'d> {
        self.controller
    }
}

impl WifiConnect for EspWifi<'_, '_> {
    async fn connect(&mut self, ssid: &str, password: &str) -> Result<Option<Url>, ConnectError> {
        if self.controller.is_connected() {
            // Re-provisioning: leave the current network first. Returns
            // `NotConnected` at once if it already dropped, which is fine.
            let _ = self.controller.disconnect_async().await;
        }

        let esp_ssid = EspSsid::try_from(ssid).map_err(|_| ConnectError::Failed)?;
        let authentication = if password.is_empty() {
            AuthenticationMethodConfig::Open
        } else {
            let pw = Password::try_from(password).map_err(|_| ConnectError::InvalidCredentials)?;
            if self.allow_legacy_wpa {
                AuthenticationMethodConfig::Wpa(pw)
            } else {
                AuthenticationMethodConfig::Wpa2Personal(pw)
            }
        };
        let config = Config::Station(
            StationConfig::default()
                .with_ssid(esp_ssid)
                .with_authentication(authentication),
        );
        self.controller
            .set_config(&config)
            .map_err(|_| ConnectError::Unavailable)?;

        match with_timeout(self.connect_timeout, self.controller.connect_async()).await {
            Err(_) => return Err(ConnectError::Timeout),
            Ok(Err(e)) => return Err(connect_error(&e)),
            Ok(Ok(_)) => {}
        }

        let url = match self.stack {
            None => None,
            Some(stack) => Some(
                wait_for_url(stack, self.ip_timeout)
                    .await
                    .ok_or(ConnectError::Timeout)?,
            ),
        };

        self.credentials =
            Some(Credentials::new(ssid, password).ok_or(ConnectError::InvalidCredentials)?);
        Ok(url)
    }
}

impl WifiScanner for EspWifi<'_, '_> {
    async fn scan<const N: usize>(
        &mut self,
        out: &mut heapless::Vec<NetworkInfo, N>,
    ) -> Result<(), ScanError> {
        let mut found = self
            .controller
            .scan_async(&ScanConfig::default())
            .await
            .map_err(|_| ScanError::Failed)?;

        // Strongest first, one entry per SSID (mesh systems advertise the
        // same name from several access points), hidden networks skipped.
        found.sort_unstable_by(|a, b| b.signal_strength.cmp(&a.signal_strength));
        for ap in &found {
            let name = ap.ssid.as_str();
            if name.is_empty() || out.iter().any(|n| n.ssid.as_str() == name) {
                continue;
            }
            let auth = ap.auth_method.map(wifi_auth).unwrap_or(WifiAuth::Unknown);
            if let Some(info) = NetworkInfo::new(name, ap.signal_strength, auth) {
                if out.push(info).is_err() {
                    break;
                }
            }
        }
        Ok(())
    }
}

impl NetworkStatus for EspWifi<'_, '_> {
    async fn current_url(&self) -> Option<Url> {
        crate::net::url_for(self.stack?)
    }

    async fn network_flags(&self) -> NetworkFlags {
        match self.stack {
            Some(stack) => wifi_flags(stack),
            None => {
                let online = if self.controller.is_connected() {
                    NetworkFlags::ONLINE
                } else {
                    0
                };
                NetworkFlags(NetworkFlags::WIFI | online)
            }
        }
    }
}

/// Map ESP-IDF's disconnect reasons onto Improv's connect errors.
fn connect_error(e: &ConnectionError) -> ConnectError {
    match e {
        ConnectionError::Failed(info) => match info.reason {
            DisconnectReason::AuthenticationFailed
            | DisconnectReason::AuthenticationExpired
            | DisconnectReason::FourWayHandshakeTimeout
            | DisconnectReason::HandshakeTimeout
            | DisconnectReason::MicFailure
            | DisconnectReason::InvalidPmkid => ConnectError::InvalidCredentials,
            DisconnectReason::NoAccessPointFound
            | DisconnectReason::NoAccessPointFoundWithCompatibleSecurity
            | DisconnectReason::NoAccessPointFoundInAuthmodeThreshold
            | DisconnectReason::NoAccessPointFoundInRssiThreshold => ConnectError::NetworkNotFound,
            DisconnectReason::Timeout | DisconnectReason::BeaconTimeout => ConnectError::Timeout,
            _ => ConnectError::Failed,
        },
        #[allow(unreachable_patterns)]
        _ => ConnectError::Failed,
    }
}

/// Map ESP-IDF's authentication methods onto Improv's.
fn wifi_auth(m: AuthenticationMethod) -> WifiAuth {
    match m {
        AuthenticationMethod::None => WifiAuth::Open,
        AuthenticationMethod::Wep => WifiAuth::Wep,
        AuthenticationMethod::Wpa => WifiAuth::Wpa,
        AuthenticationMethod::Wpa2Personal => WifiAuth::Wpa2Personal,
        AuthenticationMethod::WpaWpa2Personal => WifiAuth::WpaWpa2Personal,
        AuthenticationMethod::Wpa2Enterprise => WifiAuth::Wpa2Enterprise,
        AuthenticationMethod::Wpa3Personal
        | AuthenticationMethod::Wpa3ExtPsk
        | AuthenticationMethod::Wpa3ExtPskMixed => WifiAuth::Wpa3Personal,
        AuthenticationMethod::Wpa2Wpa3Personal => WifiAuth::Wpa2Wpa3Personal,
        AuthenticationMethod::WapiPersonal => WifiAuth::WapiPersonal,
        AuthenticationMethod::Owe => WifiAuth::Owe,
        AuthenticationMethod::Wpa3EntSuiteB192Bit | AuthenticationMethod::Wpa3Enterprise => {
            WifiAuth::Wpa3Enterprise
        }
        AuthenticationMethod::Wpa2Wpa3Enterprise => WifiAuth::Wpa2Wpa3Enterprise,
        AuthenticationMethod::WpaEnterprise => WifiAuth::WpaEnterprise,
        _ => WifiAuth::Unknown,
    }
}
