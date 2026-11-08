//! Wi-Fi through NetworkManager's D-Bus API.
//!
//! Written against NetworkManager's documented D-Bus interfaces. Behaviour
//! worth knowing:
//!
//! * **Profiles.** Activating creates a saved connection profile, which
//!   NetworkManager then reconnects to on its own, across reboots. Nothing
//!   else needs persisting. A *failed* attempt would also leave a profile
//!   behind, which NetworkManager keeps retrying with the wrong passphrase,
//!   so it is deleted. After a successful join, other profiles for the same
//!   SSID are deleted too, so stale credentials can't take over.
//! * **Security.** The key management is chosen from the access point's
//!   advertised flags: WPA3-only networks need `sae`, WPA2 and transition
//!   networks `wpa-psk`, open networks none (or `owe`). A network that isn't
//!   currently visible is treated as hidden, with `wpa-psk` if a passphrase
//!   is given. WEP and enterprise (802.1X) networks aren't supported: Improv
//!   carries only a passphrase.
//! * **Signal strength.** NetworkManager reports a percentage, not dBm.
//!   [`percent_to_dbm`] converts back with an approximation.
//! * **Permissions.** Changing connections needs root or a polkit rule; see
//!   the `dist/` directory of this crate.

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use futures::StreamExt;
use impromptu_wifi_core::traits::{
    AvailabilitySource, ConnectError, NetworkFlags, NetworkInfo, NetworkStatus, ScanError, Url,
    WifiAuth, WifiConnect, WifiScanner,
};
use zbus::proxy::CacheProperties;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::{proxy, Connection};

// Values from NetworkManager's D-Bus API documentation.
const DEVICE_TYPE_ETHERNET: u32 = 1;
const DEVICE_TYPE_WIFI: u32 = 2;
const DEVICE_TYPE_MODEM: u32 = 8;
const NM_STATE_CONNECTED_LOCAL: u32 = 50;
const ACTIVE_STATE_ACTIVATED: u32 = 2;
const ACTIVE_STATE_DEACTIVATED: u32 = 4;
const REASON_CONNECT_TIMEOUT: u32 = 6;
const REASON_NO_SECRETS: u32 = 9;
const REASON_LOGIN_FAILED: u32 = 10;
const AP_FLAGS_PRIVACY: u32 = 0x1;
const AP_SEC_KEY_MGMT_PSK: u32 = 0x100;
const AP_SEC_KEY_MGMT_802_1X: u32 = 0x200;
const AP_SEC_KEY_MGMT_SAE: u32 = 0x400;
const AP_SEC_KEY_MGMT_OWE: u32 = 0x800;
const AP_SEC_KEY_MGMT_EAP_SUITE_B_192: u32 = 0x2000;

#[proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManager {
    fn get_devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
    fn add_and_activate_connection(
        &self,
        connection: HashMap<&str, HashMap<&str, Value<'_>>>,
        device: &ObjectPath<'_>,
        specific_object: &ObjectPath<'_>,
    ) -> zbus::Result<(OwnedObjectPath, OwnedObjectPath)>;
    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn primary_connection(&self) -> zbus::Result<OwnedObjectPath>;
    #[zbus(property)]
    fn wireless_enabled(&self) -> zbus::Result<bool>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.Device",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Device {
    #[zbus(property)]
    fn device_type(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn interface(&self) -> zbus::Result<String>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.Device.Wireless",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Wireless {
    fn request_scan(&self, options: HashMap<&str, Value<'_>>) -> zbus::Result<()>;
    fn get_all_access_points(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
    #[zbus(property)]
    fn last_scan(&self) -> zbus::Result<i64>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.AccessPoint",
    default_service = "org.freedesktop.NetworkManager"
)]
trait AccessPoint {
    #[zbus(property)]
    fn ssid(&self) -> zbus::Result<Vec<u8>>;
    #[zbus(property)]
    fn strength(&self) -> zbus::Result<u8>;
    #[zbus(property)]
    fn flags(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn wpa_flags(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn rsn_flags(&self) -> zbus::Result<u32>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.Connection.Active",
    default_service = "org.freedesktop.NetworkManager"
)]
trait ActiveConnection {
    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn ip4_config(&self) -> zbus::Result<OwnedObjectPath>;
    // Named locally to avoid clashing with the `State` property's own
    // change stream, which the macro also calls `receive_state_changed`.
    #[zbus(signal, name = "StateChanged")]
    fn activation_changed(&self, state: u32, reason: u32) -> zbus::Result<()>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.IP4Config",
    default_service = "org.freedesktop.NetworkManager"
)]
trait Ip4Config {
    #[zbus(property)]
    fn address_data(&self) -> zbus::Result<Vec<HashMap<String, OwnedValue>>>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.Settings",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager/Settings"
)]
trait Settings {
    fn list_connections(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.Settings.Connection",
    default_service = "org.freedesktop.NetworkManager"
)]
trait SettingsConnection {
    fn get_settings(&self) -> zbus::Result<HashMap<String, HashMap<String, OwnedValue>>>;
    fn delete(&self) -> zbus::Result<()>;
}

/// Why NetworkManager couldn't be used.
#[derive(Debug)]
pub enum NmError {
    /// D-Bus failed (NetworkManager not running, permission denied…).
    DBus(zbus::Error),
    /// NetworkManager manages no Wi-Fi device (or not the one asked for).
    NoWifiDevice,
}

impl fmt::Display for NmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DBus(e) => write!(f, "NetworkManager: {e}"),
            Self::NoWifiDevice => f.write_str("NetworkManager has no matching Wi-Fi device"),
        }
    }
}

impl std::error::Error for NmError {}

impl From<zbus::Error> for NmError {
    fn from(e: zbus::Error) -> Self {
        Self::DBus(e)
    }
}

/// Wi-Fi on one NetworkManager-managed device.
///
/// Cheap to clone (the D-Bus connection is shared), so each transport can
/// own one.
#[derive(Clone)]
pub struct NmWifi {
    conn: Connection,
    device: OwnedObjectPath,
    connect_timeout: Duration,
    scan_timeout: Duration,
    last_error: Option<String>,
}

/// Build a property-uncached proxy for the object at `path`: NetworkManager
/// state changes underneath us, so always ask.
async fn at<P>(conn: &Connection, path: &OwnedObjectPath) -> zbus::Result<P>
where
    P: From<zbus::Proxy<'static>> + zbus::proxy::Defaults,
{
    zbus::proxy::Builder::<P>::new(conn)
        .path(path.clone())?
        .cache_properties(CacheProperties::No)
        .build()
        .await
}

async fn root(conn: &Connection) -> zbus::Result<NetworkManagerProxy<'static>> {
    NetworkManagerProxy::builder(conn)
        .cache_properties(CacheProperties::No)
        .build()
        .await
}

/// Whether NetworkManager's Wi-Fi radio is switched on (its
/// `WirelessEnabled` property), as an [`AvailabilitySource`].
///
/// Pass it to [`Driver::run_with`](impromptu_wifi_core::Driver::run_with)
/// so a serial client sees the `Stopped` state while Wi-Fi is off (for
/// example after `nmcli radio wifi off` or airplane mode), and set the
/// initial value from [`enabled`](Self::enabled) first.
pub struct NmRadio {
    proxy: NetworkManagerProxy<'static>,
    changes: zbus::proxy::PropertyStream<'static, bool>,
    reported: bool,
}

impl NmRadio {
    /// Start following `WirelessEnabled`.
    pub async fn new(conn: &Connection) -> Result<Self, NmError> {
        // Property streams need the proxy's cache; it is kept up to date
        // from NetworkManager's change signals.
        let proxy = NetworkManagerProxy::builder(conn)
            .cache_properties(CacheProperties::Yes)
            .build()
            .await?;
        let changes = proxy.receive_wireless_enabled_changed().await;
        let reported = proxy.wireless_enabled().await?;
        Ok(Self {
            proxy,
            changes,
            reported,
        })
    }

    /// Whether Wi-Fi is switched on, as last known.
    pub fn enabled(&self) -> bool {
        self.proxy
            .cached_wireless_enabled()
            .ok()
            .flatten()
            .unwrap_or(self.reported)
    }
}

impl AvailabilitySource for NmRadio {
    async fn next(&mut self) -> bool {
        // Cancel-safe: the only await is the stream's, and a change that
        // arrived while this future wasn't polled is still in the cache.
        loop {
            let now = self.enabled();
            if now != self.reported {
                self.reported = now;
                return now;
            }
            if self.changes.next().await.is_none() {
                // NetworkManager went away; report no further changes.
                core::future::pending::<()>().await;
            }
        }
    }
}

impl NmWifi {
    /// Use the first Wi-Fi device NetworkManager manages, on the system bus.
    pub async fn system() -> Result<Self, NmError> {
        Self::new(Connection::system().await?, None).await
    }

    /// Use the Wi-Fi device named `interface` (for example `wlan0`), or the
    /// first one if `None`, over `conn`.
    pub async fn new(conn: Connection, interface: Option<&str>) -> Result<Self, NmError> {
        let mut chosen = None;
        for path in root(&conn).await?.get_devices().await? {
            let dev: DeviceProxy = at(&conn, &path).await?;
            if dev.device_type().await? != DEVICE_TYPE_WIFI {
                continue;
            }
            let wanted = match interface {
                None => true,
                Some(want) => dev.interface().await.is_ok_and(|have| have == want),
            };
            if wanted {
                chosen = Some(path);
                break;
            }
        }
        let device = chosen.ok_or(NmError::NoWifiDevice)?;
        Ok(Self {
            conn,
            device,
            connect_timeout: Duration::from_secs(45),
            scan_timeout: Duration::from_secs(10),
            last_error: None,
        })
    }

    /// How long to wait for a join, including DHCP (default 45 s).
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// How long to wait for a fresh scan (default 10 s).
    pub fn scan_timeout(mut self, timeout: Duration) -> Self {
        self.scan_timeout = timeout;
        self
    }

    /// A description of the last failure, for logs. The client only ever
    /// sees Improv's coarse error codes.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Whether the system is online, and its URL: for
    /// [`Driver::mark_provisioned`](impromptu_wifi_core::Driver::mark_provisioned)
    /// at startup.
    pub async fn online_url(&self) -> Option<Option<Url>> {
        let online = root(&self.conn).await.ok()?.state().await.ok()? >= NM_STATE_CONNECTED_LOCAL;
        online.then_some(self.primary_url().await)
    }

    async fn primary_url(&self) -> Option<Url> {
        let active = root(&self.conn)
            .await
            .ok()?
            .primary_connection()
            .await
            .ok()?;
        if active.as_str() == "/" {
            return None;
        }
        self.url_of(&active).await
    }

    async fn url_of(&self, active: &OwnedObjectPath) -> Option<Url> {
        let ac: ActiveConnectionProxy = at(&self.conn, active).await.ok()?;
        let ip4 = ac.ip4_config().await.ok()?;
        if ip4.as_str() == "/" {
            return None;
        }
        let cfg: Ip4ConfigProxy = at(&self.conn, &ip4).await.ok()?;
        let data = cfg.address_data().await.ok()?;
        let address = String::try_from(data.first()?.get("address")?.try_clone().ok()?).ok()?;
        let mut url = Url::new();
        url.push_str("http://").ok()?;
        url.push_str(&address).ok()?;
        Some(url)
    }

    async fn wireless(&self) -> zbus::Result<WirelessProxy<'static>> {
        at(&self.conn, &self.device).await
    }

    /// The visible access point for `ssid` with the strongest signal.
    async fn find_ap(&self, ssid: &str) -> zbus::Result<Option<(OwnedObjectPath, ApSecurity)>> {
        let mut best: Option<(OwnedObjectPath, ApSecurity, u8)> = None;
        for path in self.wireless().await?.get_all_access_points().await? {
            let ap: AccessPointProxy = at(&self.conn, &path).await?;
            if ap.ssid().await? != ssid.as_bytes() {
                continue;
            }
            let strength = ap.strength().await?;
            if best.as_ref().is_none_or(|(_, _, s)| strength > *s) {
                best = Some((path, ApSecurity::of(&ap).await?, strength));
            }
        }
        Ok(best.map(|(p, s, _)| (p, s)))
    }

    async fn try_connect(
        &mut self,
        ssid: &str,
        password: &str,
    ) -> Result<Option<Url>, (ConnectError, String)> {
        let dbus = |e: zbus::Error| (ConnectError::Unavailable, format!("NetworkManager: {e}"));
        let ap = self.find_ap(ssid).await.map_err(dbus)?;
        let security = ap.as_ref().map(|(_, s)| *s);
        let key_mgmt = key_mgmt(security, password)?;

        let mut wifi: HashMap<&str, Value> = HashMap::new();
        wifi.insert("ssid", Value::from(ssid.as_bytes().to_vec()));
        wifi.insert("mode", Value::from("infrastructure"));
        if ap.is_none() {
            wifi.insert("hidden", Value::from(true));
        }
        let mut connection: HashMap<&str, Value> = HashMap::new();
        connection.insert("id", Value::from(ssid));
        connection.insert("type", Value::from("802-11-wireless"));
        let mut settings = HashMap::new();
        settings.insert("connection", connection);
        settings.insert("802-11-wireless", wifi);
        if let Some(km) = key_mgmt {
            let mut sec: HashMap<&str, Value> = HashMap::new();
            sec.insert("key-mgmt", Value::from(km));
            if km != "owe" {
                sec.insert("psk", Value::from(password));
            }
            settings.insert("802-11-wireless-security", sec);
        }

        let specific = ap.as_ref().map(|(p, _)| p.clone());
        let none = ObjectPath::from_static_str_unchecked("/");
        let nm = root(&self.conn).await.map_err(dbus)?;
        let (profile, active) = nm
            .add_and_activate_connection(
                settings,
                &self.device,
                specific.as_deref().unwrap_or(&none),
            )
            .await
            .map_err(dbus)?;

        match tokio::time::timeout(self.connect_timeout, self.wait_activated(&active)).await {
            Ok(Ok(())) => {
                let url = self.url_of(&active).await;
                // Best effort: a leftover profile is untidy, not fatal.
                let _ = self.remove_other_profiles(ssid.as_bytes(), &profile).await;
                Ok(url)
            }
            outcome => {
                let _ = self.delete_profile(&profile).await;
                Err(match outcome {
                    Err(_) => (
                        ConnectError::Timeout,
                        "timed out joining the network".into(),
                    ),
                    Ok(Err(reason)) => (
                        connect_error(reason),
                        format!("activation failed (reason {reason})"),
                    ),
                    Ok(Ok(())) => unreachable!(),
                })
            }
        }
    }

    /// Wait for the active connection to activate. `Err` carries
    /// NetworkManager's reason code (0 if unknown).
    async fn wait_activated(&self, active: &OwnedObjectPath) -> Result<(), u32> {
        let ac: ActiveConnectionProxy = at(&self.conn, active).await.map_err(|_| 0u32)?;
        // Subscribe before reading the state, so a change in between isn't missed.
        let mut changes = ac.receive_activation_changed().await.map_err(|_| 0u32)?;
        match ac.state().await {
            Ok(ACTIVE_STATE_ACTIVATED) => return Ok(()),
            Ok(ACTIVE_STATE_DEACTIVATED) | Err(_) => return Err(0),
            Ok(_) => {}
        }
        while let Some(signal) = changes.next().await {
            let Ok(args) = signal.args() else { continue };
            match args.state {
                ACTIVE_STATE_ACTIVATED => return Ok(()),
                ACTIVE_STATE_DEACTIVATED => return Err(args.reason),
                _ => {}
            }
        }
        Err(0)
    }

    async fn delete_profile(&self, profile: &OwnedObjectPath) -> zbus::Result<()> {
        let c: SettingsConnectionProxy = at(&self.conn, profile).await?;
        c.delete().await
    }

    async fn remove_other_profiles(&self, ssid: &[u8], keep: &OwnedObjectPath) -> zbus::Result<()> {
        let settings = SettingsProxy::builder(&self.conn)
            .cache_properties(CacheProperties::No)
            .build()
            .await?;
        for path in settings.list_connections().await? {
            if &path == keep {
                continue;
            }
            let c: SettingsConnectionProxy = at(&self.conn, &path).await?;
            let Ok(s) = c.get_settings().await else {
                continue;
            };
            let same = s
                .get("802-11-wireless")
                .and_then(|w| w.get("ssid"))
                .and_then(|v| v.try_clone().ok())
                .and_then(|v| Vec::<u8>::try_from(v).ok())
                .is_some_and(|bytes| bytes == ssid);
            if same {
                let _ = c.delete().await;
            }
        }
        Ok(())
    }
}

/// An access point's security flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ApSecurity {
    flags: u32,
    wpa: u32,
    rsn: u32,
}

impl ApSecurity {
    async fn of(ap: &AccessPointProxy<'_>) -> zbus::Result<Self> {
        Ok(Self {
            flags: ap.flags().await?,
            wpa: ap.wpa_flags().await?,
            rsn: ap.rsn_flags().await?,
        })
    }

    fn auth(self) -> WifiAuth {
        let Self { flags, wpa, rsn } = self;
        let has = |set: u32, bit: u32| set & bit != 0;
        if has(rsn, AP_SEC_KEY_MGMT_EAP_SUITE_B_192) {
            WifiAuth::Wpa3Enterprise
        } else if has(rsn, AP_SEC_KEY_MGMT_802_1X) {
            WifiAuth::Wpa2Enterprise
        } else if has(wpa, AP_SEC_KEY_MGMT_802_1X) {
            WifiAuth::WpaEnterprise
        } else if has(rsn, AP_SEC_KEY_MGMT_SAE) && has(rsn, AP_SEC_KEY_MGMT_PSK) {
            WifiAuth::Wpa2Wpa3Personal
        } else if has(rsn, AP_SEC_KEY_MGMT_SAE) {
            WifiAuth::Wpa3Personal
        } else if has(rsn, AP_SEC_KEY_MGMT_OWE) {
            WifiAuth::Owe
        } else if has(rsn, AP_SEC_KEY_MGMT_PSK) && has(wpa, AP_SEC_KEY_MGMT_PSK) {
            WifiAuth::WpaWpa2Personal
        } else if has(rsn, AP_SEC_KEY_MGMT_PSK) {
            WifiAuth::Wpa2Personal
        } else if has(wpa, AP_SEC_KEY_MGMT_PSK) {
            WifiAuth::Wpa
        } else if has(flags, AP_FLAGS_PRIVACY) {
            WifiAuth::Wep
        } else {
            WifiAuth::Open
        }
    }
}

/// The `key-mgmt` setting for a network, or `None` for an open one.
fn key_mgmt(
    ap: Option<ApSecurity>,
    password: &str,
) -> Result<Option<&'static str>, (ConnectError, String)> {
    let Some(ap) = ap else {
        // Not visible: assume a hidden WPA2-style network if given a passphrase.
        return Ok((!password.is_empty()).then_some("wpa-psk"));
    };
    match ap.auth() {
        WifiAuth::Open => Ok(None),
        WifiAuth::Owe => Ok(Some("owe")),
        WifiAuth::Wpa3Personal => Ok(Some("sae")),
        WifiAuth::Wpa
        | WifiAuth::Wpa2Personal
        | WifiAuth::WpaWpa2Personal
        | WifiAuth::Wpa2Wpa3Personal => Ok(Some("wpa-psk")),
        other => Err((
            ConnectError::Failed,
            format!("{other:?} networks aren't supported over Improv"),
        )),
    }
}

fn connect_error(reason: u32) -> ConnectError {
    match reason {
        REASON_NO_SECRETS | REASON_LOGIN_FAILED => ConnectError::InvalidCredentials,
        REASON_CONNECT_TIMEOUT => ConnectError::Timeout,
        _ => ConnectError::Failed,
    }
}

/// Approximate dBm for NetworkManager's signal percentage.
///
/// NetworkManager derives the percentage from dBm and doesn't expose the
/// original. This inverts the linear mapping it uses for nl80211 drivers
/// (about -90 dBm at 30 % up to -20 dBm at 100 %); other drivers report
/// differently, so treat the result as an estimate for sorting and display.
pub fn percent_to_dbm(percent: u8) -> i8 {
    (i16::from(percent.min(100)) - 120).clamp(-100, -20) as i8
}

impl WifiConnect for NmWifi {
    async fn connect(&mut self, ssid: &str, password: &str) -> Result<Option<Url>, ConnectError> {
        log::info!("joining {ssid:?}");
        match self.try_connect(ssid, password).await {
            Ok(url) => {
                log::info!(
                    "joined {ssid:?}{}",
                    url.as_deref()
                        .map(|u| format!(", reachable at {u}"))
                        .unwrap_or_default()
                );
                self.last_error = None;
                Ok(url)
            }
            Err((code, why)) => {
                log::warn!("joining {ssid:?} failed: {why}");
                self.last_error = Some(why);
                Err(code)
            }
        }
    }
}

impl WifiScanner for NmWifi {
    async fn scan<const N: usize>(
        &mut self,
        out: &mut heapless::Vec<NetworkInfo, N>,
    ) -> Result<(), ScanError> {
        let wireless = self.wireless().await.map_err(|_| ScanError::Unavailable)?;
        let before = wireless.last_scan().await.unwrap_or(-1);
        // NetworkManager refuses scans that come too soon after the last
        // one; its existing results are then fresh enough.
        if wireless.request_scan(HashMap::new()).await.is_ok() {
            let deadline = tokio::time::Instant::now() + self.scan_timeout;
            while wireless.last_scan().await.unwrap_or(before) == before
                && tokio::time::Instant::now() < deadline
            {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }

        let mut found: Vec<(String, u8, WifiAuth)> = Vec::new();
        for path in wireless
            .get_all_access_points()
            .await
            .map_err(|_| ScanError::Failed)?
        {
            let Ok(ap) = at::<AccessPointProxy>(&self.conn, &path).await else {
                continue;
            };
            let (Ok(ssid), Ok(strength), Ok(security)) = (
                ap.ssid().await,
                ap.strength().await,
                ApSecurity::of(&ap).await,
            ) else {
                continue;
            };
            let Ok(ssid) = String::from_utf8(ssid) else {
                continue;
            };
            if !ssid.is_empty() {
                found.push((ssid, strength, security.auth()));
            }
        }
        // Strongest first, one entry per SSID (several access points often
        // share a name), hidden networks skipped.
        found.sort_by_key(|n| core::cmp::Reverse(n.1));
        for (ssid, strength, auth) in &found {
            if out.iter().any(|n| n.ssid.as_str() == ssid) {
                continue;
            }
            if let Some(info) = NetworkInfo::new(ssid, percent_to_dbm(*strength), *auth) {
                if out.push(info).is_err() {
                    break;
                }
            }
        }
        Ok(())
    }
}

impl NetworkStatus for NmWifi {
    async fn network_flags(&self) -> NetworkFlags {
        let mut flags = NetworkFlags::WIFI;
        if let Ok(nm) = root(&self.conn).await {
            if nm
                .state()
                .await
                .is_ok_and(|s| s >= NM_STATE_CONNECTED_LOCAL)
            {
                flags |= NetworkFlags::ONLINE;
            }
            for path in nm.get_devices().await.unwrap_or_default() {
                if let Ok(dev) = at::<DeviceProxy>(&self.conn, &path).await {
                    match dev.device_type().await {
                        Ok(DEVICE_TYPE_ETHERNET) => flags |= NetworkFlags::ETHERNET,
                        Ok(DEVICE_TYPE_MODEM) => flags |= NetworkFlags::CELLULAR,
                        _ => {}
                    }
                }
            }
        }
        NetworkFlags(flags)
    }

    async fn current_url(&self) -> Option<Url> {
        self.primary_url().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sec(flags: u32, wpa: u32, rsn: u32) -> ApSecurity {
        ApSecurity { flags, wpa, rsn }
    }

    #[test]
    fn auth_from_flags() {
        assert_eq!(sec(0, 0, 0).auth(), WifiAuth::Open);
        assert_eq!(sec(1, 0, 0).auth(), WifiAuth::Wep);
        assert_eq!(
            sec(1, 0, AP_SEC_KEY_MGMT_PSK).auth(),
            WifiAuth::Wpa2Personal
        );
        assert_eq!(
            sec(1, 0, AP_SEC_KEY_MGMT_PSK | AP_SEC_KEY_MGMT_SAE).auth(),
            WifiAuth::Wpa2Wpa3Personal
        );
        assert_eq!(
            sec(1, 0, AP_SEC_KEY_MGMT_SAE).auth(),
            WifiAuth::Wpa3Personal
        );
        assert_eq!(sec(0, 0, AP_SEC_KEY_MGMT_OWE).auth(), WifiAuth::Owe);
        assert_eq!(
            sec(1, AP_SEC_KEY_MGMT_PSK, AP_SEC_KEY_MGMT_PSK).auth(),
            WifiAuth::WpaWpa2Personal
        );
        assert_eq!(
            sec(1, 0, AP_SEC_KEY_MGMT_802_1X).auth(),
            WifiAuth::Wpa2Enterprise
        );
    }

    #[test]
    fn key_management_choice() {
        let psk = sec(1, 0, AP_SEC_KEY_MGMT_PSK);
        assert_eq!(key_mgmt(Some(psk), "pw").unwrap(), Some("wpa-psk"));
        assert_eq!(
            key_mgmt(Some(sec(1, 0, AP_SEC_KEY_MGMT_SAE)), "pw").unwrap(),
            Some("sae")
        );
        assert_eq!(key_mgmt(Some(sec(0, 0, 0)), "").unwrap(), None);
        assert_eq!(
            key_mgmt(None, "pw").unwrap(),
            Some("wpa-psk"),
            "hidden network"
        );
        assert_eq!(key_mgmt(None, "").unwrap(), None);
        assert!(key_mgmt(Some(sec(1, 0, 0)), "pw").is_err(), "WEP refused");
    }

    #[test]
    fn signal_conversion() {
        assert_eq!(percent_to_dbm(100), -20);
        assert_eq!(percent_to_dbm(30), -90);
        assert_eq!(percent_to_dbm(0), -100);
    }
}
