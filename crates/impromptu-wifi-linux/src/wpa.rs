//! Wi-Fi through wpa_supplicant's control interface, for systems without
//! NetworkManager: Raspberry Pi OS 11 and older (dhcpcd with
//! wpa_supplicant), Buildroot, Yocto.
//!
//! [`WpaWifi`] talks to the UNIX socket `wpa_cli` uses,
//! `/var/run/wpa_supplicant/<interface>`, enabled by `ctrl_interface=` in
//! `wpa_supplicant.conf`. With `GROUP=netdev` there, members of `netdev`
//! may use it. The commands are those of wpa_supplicant's
//! `wpa_supplicant/README` and `ctrl_iface.c` (2.9 and later).
//!
//! * **Join:** `ADD_NETWORK` and `SET_NETWORK`, then `SELECT_NETWORK`, and
//!   wait for `CTRL-EVENT-CONNECTED`, or a sign of failure, on an
//!   `ATTACH`ed socket. The address comes from whatever runs DHCP (dhcpcd,
//!   systemd-networkd); `STATUS` reports it as `ip_address`.
//! * **Keep:** on success, older networks with the same SSID are removed and
//!   `SAVE_CONFIG` writes the configuration, which needs `update_config=1`.
//!   On failure the network is removed again and the others re-enabled.
//! * **Scan:** `SCAN`, then `SCAN_RESULTS` after `CTRL-EVENT-SCAN-RESULTS`.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram as StdDatagram};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use impromptu_wifi_core::traits::{
    ConnectError, NetworkFlags, NetworkInfo, NetworkStatus, ScanError, Url, WifiAuth, WifiConnect,
    WifiScanner,
};
use tokio::net::UnixDatagram;

/// Where wpa_supplicant's control sockets usually are.
pub const DEFAULT_CTRL_DIR: &str = "/var/run/wpa_supplicant";

/// How long to wait for an answer to one command.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// Failed `CTRL-EVENT-NETWORK-NOT-FOUND` scans before giving up on a join.
const NOT_FOUND_SCANS: u32 = 3;

/// Why talking to wpa_supplicant failed.
#[derive(Debug)]
pub enum WpaError {
    /// The control socket could not be reached or answered with an error.
    Io(io::Error),
    /// wpa_supplicant answered `FAIL` or something unexpected.
    Command(String),
}

impl fmt::Display for WpaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "wpa_supplicant control socket: {e}"),
            Self::Command(c) => write!(f, "wpa_supplicant: {c}"),
        }
    }
}

impl std::error::Error for WpaError {}

impl From<io::Error> for WpaError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// One connection to a control socket, from an abstract-namespace socket of
/// our own, so the service needs no writable directory.
struct Ctrl {
    sock: UnixDatagram,
}

impl Ctrl {
    fn open(path: &Path) -> io::Result<Self> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let name = format!(
            "impromptu-wifi-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let local = SocketAddr::from_abstract_name(name.as_bytes())?;
        let sock = StdDatagram::bind_addr(&local)?;
        sock.connect(path)?;
        sock.set_nonblocking(true)?;
        Ok(Self {
            sock: UnixDatagram::from_std(sock)?,
        })
    }

    /// Send a command and return its reply, skipping event messages that a
    /// monitor socket may receive in between.
    async fn request(&self, cmd: &str) -> io::Result<String> {
        self.sock.send(cmd.as_bytes()).await?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = tokio::time::timeout(REPLY_TIMEOUT, self.sock.recv(&mut buf))
                .await
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("no reply to {}", redact(cmd)),
                    )
                })??;
            let reply = String::from_utf8_lossy(&buf[..n]).into_owned();
            if !reply.starts_with('<') {
                return Ok(reply);
            }
        }
    }

    /// A command whose reply must be `OK`.
    async fn ok(&self, cmd: &str) -> Result<(), WpaError> {
        let reply = self.request(cmd).await?;
        if reply.trim_end() == "OK" {
            Ok(())
        } else {
            Err(WpaError::Command(format!(
                "{} -> {}",
                redact(cmd),
                reply.trim_end()
            )))
        }
    }

    /// Wait for the next event, without its `<level>` prefix.
    async fn event(&self) -> io::Result<String> {
        let mut buf = vec![0u8; 4096];
        loop {
            let n = self.sock.recv(&mut buf).await?;
            let msg = String::from_utf8_lossy(&buf[..n]);
            if let Some(rest) = msg.strip_prefix('<') {
                if let Some((_, event)) = rest.split_once('>') {
                    return Ok(event.to_owned());
                }
            }
        }
    }
}

/// Keep passphrases out of error messages and logs. Every command that is
/// formatted into a message goes through this. The value of any
/// `SET_NETWORK` field other than the SSID is hidden. Splits at the first
/// two spaces only, so a value with spaces can't move the field name.
fn redact(cmd: &str) -> String {
    let mut parts = cmd.splitn(4, ' ');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("SET_NETWORK"), Some(id), Some(field)) if field != "ssid" => {
            format!("SET_NETWORK {id} {field} <redacted>")
        }
        _ => cmd.to_owned(),
    }
}

/// Wi-Fi through wpa_supplicant on one interface.
#[derive(Clone)]
pub struct WpaWifi {
    path: PathBuf,
    interface: String,
    connect_timeout: Duration,
    ip_timeout: Duration,
    scan_timeout: Duration,
    last_error: Option<String>,
}

impl WpaWifi {
    /// Use `interface` (for example `wlan0`) through its control socket in
    /// `ctrl_dir` ([`DEFAULT_CTRL_DIR`] usually). Checks that wpa_supplicant
    /// answers.
    pub async fn new(ctrl_dir: impl AsRef<Path>, interface: &str) -> Result<Self, WpaError> {
        let wifi = Self {
            path: ctrl_dir.as_ref().join(interface),
            interface: interface.to_owned(),
            connect_timeout: Duration::from_secs(30),
            ip_timeout: Duration::from_secs(20),
            scan_timeout: Duration::from_secs(10),
            last_error: None,
        };
        let pong = wifi.ctrl()?.request("PING").await?;
        if pong.trim_end() != "PONG" {
            return Err(WpaError::Command(format!("PING -> {}", pong.trim_end())));
        }
        Ok(wifi)
    }

    /// How long to wait for association (default 30 s).
    pub fn connect_timeout(mut self, d: Duration) -> Self {
        self.connect_timeout = d;
        self
    }

    /// How long to wait for an address after associating (default 20 s).
    pub fn ip_timeout(mut self, d: Duration) -> Self {
        self.ip_timeout = d;
        self
    }

    /// The interface name.
    pub fn interface(&self) -> &str {
        &self.interface
    }

    /// A description of the last failure, for logs.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    fn ctrl(&self) -> io::Result<Ctrl> {
        Ctrl::open(&self.path)
    }

    async fn monitor(&self) -> Result<Ctrl, WpaError> {
        let ctrl = self.ctrl()?;
        ctrl.ok("ATTACH").await?;
        Ok(ctrl)
    }

    /// `STATUS`, as key/value pairs.
    pub async fn status(&self) -> Result<HashMap<String, String>, WpaError> {
        Ok(parse_status(&self.ctrl()?.request("STATUS").await?))
    }

    /// `http://<address>` if associated and addressed.
    pub async fn url(&self) -> Option<Url> {
        let status = self.status().await.ok()?;
        url_from_status(&status)
    }

    async fn try_connect(
        &self,
        ssid: &str,
        password: &str,
    ) -> Result<Option<Url>, (ConnectError, String)> {
        let fail = |e: WpaError| (ConnectError::Unavailable, e.to_string());
        let ctrl = self.ctrl().map_err(|e| fail(e.into()))?;
        let before = parse_networks(
            &ctrl
                .request("LIST_NETWORKS")
                .await
                .map_err(|e| fail(e.into()))?,
        );
        let monitor = self.monitor().await.map_err(fail)?;

        let id = ctrl
            .request("ADD_NETWORK")
            .await
            .map_err(|e| fail(e.into()))?;
        let id: u32 = id.trim().parse().map_err(|_| {
            (
                ConnectError::Failed,
                format!("ADD_NETWORK -> {}", id.trim()),
            )
        })?;

        let result = self.join(&ctrl, &monitor, id, ssid, password).await;
        match result {
            Ok(url) => {
                // Keep one network per SSID, as NetworkManager does, and
                // let the others take over again if this one goes away.
                for n in &before {
                    if n.ssid == ssid {
                        let _ = ctrl.ok(&format!("REMOVE_NETWORK {}", n.id)).await;
                    } else if !n.disabled {
                        let _ = ctrl.ok(&format!("ENABLE_NETWORK {}", n.id)).await;
                    }
                }
                if let Err(e) = ctrl.ok("SAVE_CONFIG").await {
                    log::warn!(
                        "the network is joined but not saved (update_config=1 missing?): {e}"
                    );
                }
                Ok(url)
            }
            Err(e) => {
                let _ = ctrl.ok(&format!("REMOVE_NETWORK {id}")).await;
                for n in before.iter().filter(|n| !n.disabled) {
                    let _ = ctrl.ok(&format!("ENABLE_NETWORK {}", n.id)).await;
                }
                Err(e)
            }
        }
    }

    async fn join(
        &self,
        ctrl: &Ctrl,
        monitor: &Ctrl,
        id: u32,
        ssid: &str,
        password: &str,
    ) -> Result<Option<Url>, (ConnectError, String)> {
        let set = |field: &str, value: String| format!("SET_NETWORK {id} {field} {value}");
        let failed = |e: WpaError| (ConnectError::Failed, e.to_string());

        // Unquoted hex: any bytes, no escaping.
        ctrl.ok(&set("ssid", hex(ssid.as_bytes())))
            .await
            .map_err(failed)?;
        // Hidden networks answer only to a probe for their name.
        ctrl.ok(&set("scan_ssid", "1".into()))
            .await
            .map_err(failed)?;
        if password.is_empty() {
            ctrl.ok(&set("key_mgmt", "NONE".into()))
                .await
                .map_err(failed)?;
        } else {
            let psk = if password.len() == 64 && password.bytes().all(|b| b.is_ascii_hexdigit()) {
                password.to_owned() // a raw pre-shared key
            } else {
                format!("\"{password}\"")
            };
            ctrl.ok(&set("psk", psk))
                .await
                .map_err(|e| (ConnectError::InvalidCredentials, e.to_string()))?;
            // WPA3 (SAE) where wpa_supplicant and the driver support it;
            // WPA2 otherwise. PMF is optional, as SAE requires.
            let wpa3 = ctrl
                .ok(&set("key_mgmt", "\"WPA-PSK WPA-PSK-SHA256 SAE\"".into()))
                .await;
            if wpa3.is_ok() {
                let _ = ctrl.ok(&set("ieee80211w", "1".into())).await;
            } else {
                ctrl.ok(&set("key_mgmt", "WPA-PSK".into()))
                    .await
                    .map_err(failed)?;
            }
        }
        ctrl.ok(&format!("SELECT_NETWORK {id}"))
            .await
            .map_err(failed)?;

        let outcome = tokio::time::timeout(self.connect_timeout, async {
            let mut not_found = 0;
            loop {
                let event = monitor
                    .event()
                    .await
                    .map_err(|e| (ConnectError::Failed, e.to_string()))?;
                match classify(&event, id) {
                    Event::Connected => return Ok(()),
                    Event::WrongKey => return Err((ConnectError::InvalidCredentials, event)),
                    Event::NotFound => {
                        not_found += 1;
                        if not_found >= NOT_FOUND_SCANS {
                            return Err((ConnectError::NetworkNotFound, event));
                        }
                    }
                    Event::Other => {}
                }
            }
        })
        .await;
        match outcome {
            Err(_) => {
                return Err((
                    ConnectError::Timeout,
                    "no connection within the timeout".into(),
                ))
            }
            Ok(r) => r?,
        }

        // Associated; wait for whatever runs DHCP to assign an address.
        let deadline = tokio::time::Instant::now() + self.ip_timeout;
        loop {
            if let Some(url) = self.url().await {
                return Ok(Some(url));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err((
                    ConnectError::Timeout,
                    "associated, but no IPv4 address".into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}

impl WifiConnect for WpaWifi {
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

impl WifiScanner for WpaWifi {
    async fn scan<const N: usize>(
        &mut self,
        out: &mut heapless::Vec<NetworkInfo, N>,
    ) -> Result<(), ScanError> {
        let monitor = self.monitor().await.map_err(|_| ScanError::Unavailable)?;
        let reply = monitor
            .request("SCAN")
            .await
            .map_err(|_| ScanError::Unavailable)?;
        // FAIL-BUSY: a scan is already running; its results will do.
        if matches!(reply.trim_end(), "OK" | "FAIL-BUSY") {
            let _ = tokio::time::timeout(self.scan_timeout, async {
                while let Ok(event) = monitor.event().await {
                    if event.starts_with("CTRL-EVENT-SCAN-RESULTS") {
                        break;
                    }
                }
            })
            .await;
        }
        let results = monitor
            .request("SCAN_RESULTS")
            .await
            .map_err(|_| ScanError::Failed)?;
        for network in parse_scan_results(&results) {
            if out.push(network).is_err() {
                break;
            }
        }
        Ok(())
    }
}

impl NetworkStatus for WpaWifi {
    async fn network_flags(&self) -> NetworkFlags {
        let online = if self.url().await.is_some() {
            NetworkFlags::ONLINE
        } else {
            0
        };
        NetworkFlags(NetworkFlags::WIFI | online)
    }

    async fn current_url(&self) -> Option<Url> {
        self.url().await
    }
}

/// Whether the machine is online, checked every 2 seconds, as an
/// [`EventSource`](impromptu_wifi_core::traits::EventSource): the
/// counterpart of `nm::NmOnline` for systems without NetworkManager.
///
/// Online means wpa_supplicant is associated and the interface has an
/// address, or, unless only Wi-Fi counts, that the machine has an IPv4
/// default route through any interface (`/proc/net/route`), Ethernet
/// included.
pub struct WpaOnline {
    wifi: WpaWifi,
    wired_counts: bool,
    reported: bool,
    current: bool,
}

/// How often [`WpaOnline`] checks.
const ONLINE_POLL: Duration = Duration::from_secs(2);

impl WpaOnline {
    /// Start watching. `wired_counts`: whether a default route through
    /// another interface counts as online. The first event is the current
    /// state, if it differs from `assume_online`.
    pub async fn new(wifi: WpaWifi, wired_counts: bool, assume_online: bool) -> Self {
        let mut watch = Self {
            wifi,
            wired_counts,
            reported: assume_online,
            current: assume_online,
        };
        watch.current = watch.check().await;
        watch
    }

    /// Whether the machine is online, as last checked.
    pub fn online(&self) -> bool {
        self.current
    }

    /// `http://<address>` of the Wi-Fi interface, if it has one.
    pub async fn url(&self) -> Option<Url> {
        self.wifi.url().await
    }

    async fn check(&self) -> bool {
        self.wifi.url().await.is_some() || (self.wired_counts && has_default_route())
    }
}

impl impromptu_wifi_core::traits::EventSource for WpaOnline {
    async fn next(&mut self) -> impromptu_wifi_core::traits::DeviceEvent {
        use impromptu_wifi_core::traits::DeviceEvent;
        // Cancel-safe: state only changes right before returning.
        loop {
            let now = self.check().await;
            self.current = now;
            if now != self.reported {
                if now {
                    let url = self.wifi.url().await;
                    self.reported = true;
                    return DeviceEvent::Provisioned(url);
                }
                self.reported = false;
                return DeviceEvent::Unprovisioned;
            }
            tokio::time::sleep(ONLINE_POLL).await;
        }
    }
}

/// Whether `/proc/net/route` has an IPv4 default route that is up.
fn has_default_route() -> bool {
    std::fs::read_to_string("/proc/net/route").is_ok_and(|t| default_route_in(&t))
}

/// `/proc/net/route`: a header, then `Iface Destination Gateway Flags ...
/// Mask ...`, hexadecimal; flag 0x1 is RTF_UP.
fn default_route_in(table: &str) -> bool {
    table.lines().skip(1).any(|line| {
        let cols: Vec<&str> = line.split_whitespace().collect();
        cols.len() > 7
            && cols[1] == "00000000"
            && cols[7] == "00000000"
            && u32::from_str_radix(cols[3], 16).is_ok_and(|f| f & 0x1 != 0)
    })
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// What an event means for a join of network `id`.
#[derive(Debug, PartialEq, Eq)]
enum Event {
    Connected,
    WrongKey,
    NotFound,
    Other,
}

fn classify(event: &str, id: u32) -> Event {
    let ours = || event.split_whitespace().any(|w| w == format!("id={id}"));
    if event.starts_with("CTRL-EVENT-CONNECTED") {
        Event::Connected
    } else if event.starts_with("CTRL-EVENT-SSID-TEMP-DISABLED")
        && ours()
        && (event.contains("reason=WRONG_KEY") || event.contains("reason=AUTH_FAILED"))
    {
        Event::WrongKey
    } else if event.starts_with("CTRL-EVENT-NETWORK-NOT-FOUND") {
        Event::NotFound
    } else {
        Event::Other
    }
}

/// `key=value` lines.
fn parse_status(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

fn url_from_status(status: &HashMap<String, String>) -> Option<Url> {
    if status.get("wpa_state").map(String::as_str) != Some("COMPLETED") {
        return None;
    }
    let ip = status.get("ip_address")?;
    // A link-local address means DHCP failed.
    if ip.starts_with("169.254.") {
        return None;
    }
    let mut url = Url::new();
    url.push_str("http://").ok()?;
    url.push_str(ip).ok()?;
    Some(url)
}

/// One line of `LIST_NETWORKS`.
#[derive(Debug, PartialEq, Eq)]
struct Saved {
    id: u32,
    ssid: String,
    disabled: bool,
}

/// `LIST_NETWORKS`: a header, then `id \t ssid \t bssid \t flags`.
fn parse_networks(text: &str) -> Vec<Saved> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let mut cols = line.split('\t');
            let id = cols.next()?.parse().ok()?;
            let ssid = decode_ssid(cols.next()?);
            let flags = cols.nth(1).unwrap_or("");
            Some(Saved {
                id,
                ssid,
                disabled: flags.contains("[DISABLED]"),
            })
        })
        .collect()
}

/// `SCAN_RESULTS`: a header, then `bssid \t frequency \t signal \t flags \t
/// ssid`. Strongest first, one entry per SSID, hidden networks left out.
fn parse_scan_results(text: &str) -> Vec<NetworkInfo> {
    let mut found: Vec<NetworkInfo> = Vec::new();
    for line in text.lines().skip(1) {
        let cols: Vec<&str> = line.split('\t').collect();
        let [_, _, signal, flags, ssid] = cols.as_slice() else {
            continue;
        };
        let ssid = decode_ssid(ssid);
        let Ok(rssi) = signal.parse::<i32>() else {
            continue;
        };
        if ssid.is_empty() || ssid.chars().all(|c| c == '\0') {
            continue;
        }
        let rssi = rssi.clamp(i8::MIN.into(), i8::MAX.into()) as i8;
        match found.iter_mut().find(|n| n.ssid.as_str() == ssid) {
            Some(n) if n.rssi_dbm < rssi => n.rssi_dbm = rssi,
            Some(_) => {}
            None => {
                if let Some(n) = NetworkInfo::new(&ssid, rssi, auth_from_flags(flags)) {
                    found.push(n);
                }
            }
        }
    }
    found.sort_by_key(|n| std::cmp::Reverse(n.rssi_dbm));
    found
}

/// The security of a network from its scan flags, such as
/// `[WPA2-PSK-CCMP][ESS]` or `[RSN-SAE+PSK-CCMP][ESS]`.
fn auth_from_flags(flags: &str) -> WifiAuth {
    let sae = flags.contains("SAE");
    let psk = flags.contains("PSK");
    let wpa2 = flags.contains("WPA2-") || flags.contains("RSN-");
    let wpa1 = flags.contains("[WPA-");
    if flags.contains("EAP") {
        WifiAuth::Wpa2Enterprise
    } else if sae && psk {
        WifiAuth::Wpa2Wpa3Personal
    } else if sae {
        WifiAuth::Wpa3Personal
    } else if wpa1 && wpa2 {
        WifiAuth::WpaWpa2Personal
    } else if wpa2 {
        WifiAuth::Wpa2Personal
    } else if wpa1 {
        WifiAuth::Wpa
    } else if flags.contains("OWE") {
        WifiAuth::Owe
    } else if flags.contains("WEP") {
        WifiAuth::Wep
    } else {
        WifiAuth::Open
    }
}

/// Undo wpa_supplicant's `printf_encode`: `\\`, `\"`, `\e`, `\n`, `\r`,
/// `\t` and `\xNN`.
fn decode_ssid(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            let (byte, len) = match b[i + 1] {
                b'\\' => (b'\\', 2),
                b'"' => (b'"', 2),
                b'e' => (0x1b, 2),
                b'n' => (b'\n', 2),
                b'r' => (b'\r', 2),
                b't' => (b'\t', 2),
                b'x' if i + 3 < b.len() => match u8::from_str_radix(&s[i + 2..i + 4], 16) {
                    Ok(v) => (v, 4),
                    Err(_) => (b'\\', 1),
                },
                _ => (b'\\', 1),
            };
            out.push(byte);
            i += len;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_results_are_parsed_sorted_and_deduplicated() {
        let text = "bssid / frequency / signal level / flags / ssid\n\
            aa:aa:aa:aa:aa:01\t2412\t-60\t[WPA2-PSK-CCMP][ESS]\thome\n\
            aa:aa:aa:aa:aa:02\t5180\t-40\t[WPA2-PSK-CCMP][ESS]\thome\n\
            aa:aa:aa:aa:aa:03\t2437\t-70\t[RSN-SAE-CCMP][ESS]\tlab\n\
            aa:aa:aa:aa:aa:04\t2437\t-50\t[ESS]\tcaf\\xc3\\xa9\n\
            aa:aa:aa:aa:aa:05\t2437\t-30\t[WPA2-PSK-CCMP][ESS]\t\n\
            aa:aa:aa:aa:aa:06\t2437\t-80\t[WPA-PSK-TKIP][WPA2-PSK-CCMP][ESS]\told\n";
        let found = parse_scan_results(text);
        let summary: Vec<_> = found
            .iter()
            .map(|n| (n.ssid.as_str(), n.rssi_dbm, n.auth))
            .collect();
        assert_eq!(
            summary,
            [
                ("home", -40, WifiAuth::Wpa2Personal),
                ("café", -50, WifiAuth::Open),
                ("lab", -70, WifiAuth::Wpa3Personal),
                ("old", -80, WifiAuth::WpaWpa2Personal),
            ]
        );
    }

    #[test]
    fn security_flags_map_to_auth_types() {
        assert_eq!(
            auth_from_flags("[RSN-SAE+PSK-CCMP][ESS]"),
            WifiAuth::Wpa2Wpa3Personal
        );
        assert_eq!(
            auth_from_flags("[WPA2-EAP-CCMP][ESS]"),
            WifiAuth::Wpa2Enterprise
        );
        assert_eq!(auth_from_flags("[WEP][ESS]"), WifiAuth::Wep);
        assert_eq!(auth_from_flags("[WPA-PSK-TKIP][ESS]"), WifiAuth::Wpa);
        assert_eq!(auth_from_flags("[ESS]"), WifiAuth::Open);
    }

    #[test]
    fn ssid_escapes_are_decoded() {
        assert_eq!(decode_ssid(r#"a\\b\"c\x41\t"#), "a\\b\"cA\t");
        assert_eq!(decode_ssid(r"trailing\"), "trailing\\");
    }

    #[test]
    fn saved_networks_are_parsed() {
        let text = "network id / ssid / bssid / flags\n\
            0\thome\tany\t[CURRENT]\n\
            1\tlab\tany\t[DISABLED]\n";
        assert_eq!(
            parse_networks(text),
            [
                Saved {
                    id: 0,
                    ssid: "home".into(),
                    disabled: false
                },
                Saved {
                    id: 1,
                    ssid: "lab".into(),
                    disabled: true
                },
            ]
        );
    }

    #[test]
    fn status_gives_a_url_only_when_associated_and_addressed() {
        let status = |s: &str| url_from_status(&parse_status(s));
        assert_eq!(
            status("wpa_state=COMPLETED\nip_address=192.168.0.200\n").as_deref(),
            Some("http://192.168.0.200")
        );
        assert_eq!(
            status("wpa_state=COMPLETED\nip_address=169.254.3.4\n"),
            None
        );
        assert_eq!(
            status("wpa_state=SCANNING\nip_address=192.168.0.200\n"),
            None
        );
        assert_eq!(status("wpa_state=COMPLETED\n"), None);
    }

    #[test]
    fn events_are_classified_for_our_network() {
        assert_eq!(
            classify(
                "CTRL-EVENT-CONNECTED - Connection to aa:bb:cc:dd:ee:ff completed [id=3 id_str=]",
                3
            ),
            Event::Connected
        );
        let wrong = "CTRL-EVENT-SSID-TEMP-DISABLED id=3 ssid=\"home\" auth_failures=1 duration=10 reason=WRONG_KEY";
        assert_eq!(classify(wrong, 3), Event::WrongKey);
        assert_eq!(
            classify(wrong, 4),
            Event::Other,
            "another network's failure"
        );
        assert_eq!(classify("CTRL-EVENT-NETWORK-NOT-FOUND", 3), Event::NotFound);
        assert_eq!(classify("CTRL-EVENT-SCAN-STARTED", 3), Event::Other);
    }

    #[test]
    fn default_routes_are_found() {
        let header =
            "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n";
        let default = "eth0\t00000000\t0100A8C0\t0003\t0\t0\t202\t00000000\t0\t0\t0\n";
        let subnet = "eth0\t0000A8C0\t00000000\t0001\t0\t0\t202\t00FFFFFF\t0\t0\t0\n";
        assert!(default_route_in(&format!("{header}{subnet}{default}")));
        assert!(!default_route_in(&format!("{header}{subnet}")));
        assert!(!default_route_in(header));
    }

    #[test]
    fn passphrases_are_redacted() {
        assert_eq!(
            redact("SET_NETWORK 3 psk \"hunter22\""),
            "SET_NETWORK 3 psk <redacted>"
        );
        assert_eq!(
            redact("SET_NETWORK 3 ssid 686f6d65"),
            "SET_NETWORK 3 ssid 686f6d65"
        );
        assert_eq!(
            redact("SET_NETWORK 3 psk \"  pass with spaces \""),
            "SET_NETWORK 3 psk <redacted>"
        );
    }
}
