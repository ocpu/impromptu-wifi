//! `WpaWifi` against a stand-in wpa_supplicant: a UNIX datagram socket that
//! answers the control commands and sends the events a real one would.
#![cfg(feature = "wpa-supplicant")]

use std::collections::BTreeMap;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use impromptu_wifi_linux::wpa::WpaWifi;
use impromptu_wifi_linux::{ConnectError, NetworkStatus, WifiAuth, WifiConnect, WifiScanner};

/// What the stand-in knows.
#[derive(Default)]
struct State {
    /// id -> (ssid as set, psk as set, disabled)
    networks: BTreeMap<u32, (String, String, bool)>,
    next_id: u32,
    connected: Option<u32>,
    saved: Vec<String>,
    log: Vec<String>,
}

/// The access points in range: ssid -> passphrase.
const RANGE: &[(&str, &str)] = &[("home", "hunter22")];

fn unhex(s: &str) -> String {
    let bytes: Vec<u8> = (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect();
    String::from_utf8(bytes).unwrap()
}

/// Start the stand-in in `dir`; returns its state.
fn wpa_supplicant(dir: &std::path::Path) -> Arc<Mutex<State>> {
    let sock = UnixDatagram::bind(dir.join("wlan0")).unwrap();
    let state = Arc::new(Mutex::new(State::default()));
    let shared = state.clone();
    thread::spawn(move || {
        let mut monitors: Vec<SocketAddr> = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let (n, from) = sock.recv_from(&mut buf).unwrap();
            let cmd = String::from_utf8_lossy(&buf[..n]).into_owned();
            let mut s = shared.lock().unwrap();
            s.log.push(cmd.clone());
            let mut events: Vec<String> = Vec::new();
            let reply = match cmd.split(' ').collect::<Vec<_>>().as_slice() {
                ["PING"] => "PONG\n".into(),
                ["ATTACH"] => {
                    monitors.push(from.clone());
                    "OK\n".into()
                }
                ["LIST_NETWORKS"] => {
                    let mut out = String::from("network id / ssid / bssid / flags\n");
                    for (id, (ssid, _, disabled)) in &s.networks {
                        let flags = if *disabled { "[DISABLED]" } else { "" };
                        out += &format!("{id}\t{}\tany\t{flags}\n", unhex(ssid));
                    }
                    out
                }
                ["ADD_NETWORK"] => {
                    let id = s.next_id;
                    s.next_id += 1;
                    s.networks.insert(id, (String::new(), String::new(), true));
                    format!("{id}\n")
                }
                ["SET_NETWORK", id, field, value @ ..] => {
                    let id: u32 = id.parse().unwrap();
                    let value = value.join(" ");
                    let n = s.networks.get_mut(&id).unwrap();
                    match *field {
                        "ssid" => n.0 = value,
                        "psk" => n.1 = value.trim_matches('"').into(),
                        _ => {}
                    }
                    "OK\n".into()
                }
                ["SELECT_NETWORK", id] => {
                    let id: u32 = id.parse().unwrap();
                    for (other, n) in s.networks.iter_mut() {
                        n.2 = *other != id;
                    }
                    let (ssid, psk, _) = s.networks[&id].clone();
                    let ssid = unhex(&ssid);
                    match RANGE.iter().find(|(name, _)| *name == ssid) {
                        None => events.extend(
                            (0..3).map(|_| "<3>CTRL-EVENT-NETWORK-NOT-FOUND".to_owned()),
                        ),
                        Some((_, pass)) if *pass != psk => events.push(format!(
                            "<3>CTRL-EVENT-SSID-TEMP-DISABLED id={id} ssid=\"{ssid}\" auth_failures=1 duration=10 reason=WRONG_KEY"
                        )),
                        Some(_) => {
                            s.connected = Some(id);
                            events.push(format!(
                                "<3>CTRL-EVENT-CONNECTED - Connection to aa:bb:cc:dd:ee:ff completed [id={id} id_str=]"
                            ));
                        }
                    }
                    "OK\n".into()
                }
                ["REMOVE_NETWORK", id] => {
                    s.networks.remove(&id.parse().unwrap());
                    "OK\n".into()
                }
                ["ENABLE_NETWORK", id] => {
                    if let Some(n) = s.networks.get_mut(&id.parse().unwrap()) {
                        n.2 = false;
                    }
                    "OK\n".into()
                }
                ["SAVE_CONFIG"] => {
                    s.saved = s.networks.values().map(|n| unhex(&n.0)).collect();
                    "OK\n".into()
                }
                ["STATUS"] => match s.connected {
                    Some(_) => "wpa_state=COMPLETED\nssid=home\nip_address=10.0.0.7\n".into(),
                    None => "wpa_state=SCANNING\n".into(),
                },
                ["SCAN"] => {
                    events.push("<2>CTRL-EVENT-SCAN-RESULTS ".into());
                    "OK\n".into()
                }
                ["SCAN_RESULTS"] => "bssid / frequency / signal level / flags / ssid\n\
                     aa:bb:cc:dd:ee:01\t2412\t-48\t[WPA2-PSK-CCMP][ESS]\thome\n\
                     aa:bb:cc:dd:ee:02\t2437\t-70\t[RSN-SAE-CCMP][ESS]\tlab\n"
                    .into(),
                _ => "UNKNOWN COMMAND\n".into(),
            };
            sock.send_to_addr(reply.as_bytes(), &from).unwrap();
            drop(s);
            for event in events {
                for m in &monitors {
                    let _ = sock.send_to_addr(event.as_bytes(), m);
                }
            }
        }
    });
    state
}

/// A fresh directory for the control socket, removed when dropped.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "impromptu-wifi-wpa-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn setup() -> (TempDir, Arc<Mutex<State>>, WpaWifi) {
    let dir = TempDir::new();
    let state = wpa_supplicant(&dir.0);
    let wifi = WpaWifi::new(&dir.0, "wlan0")
        .await
        .unwrap()
        .connect_timeout(Duration::from_secs(5))
        .ip_timeout(Duration::from_secs(2));
    (dir, state, wifi)
}

#[tokio::test]
async fn a_join_is_saved_and_replaces_older_networks_for_the_ssid() {
    let (_dir, state, mut wifi) = setup().await;
    // An older, disabled network for the same SSID, as after a reset.
    state
        .lock()
        .unwrap()
        .networks
        .insert(0, ("686f6d65".into(), "old".into(), true));
    state.lock().unwrap().next_id = 1;

    let url = wifi.connect("home", "hunter22").await.unwrap();
    assert_eq!(url.as_deref(), Some("http://10.0.0.7"));
    {
        let s = state.lock().unwrap();
        assert_eq!(s.saved, ["home"], "one network for the SSID, saved");
        assert!(
            s.log
                .iter()
                .all(|c| !c.contains("hunter22") || c.starts_with("SET_NETWORK 1 psk")),
            "the passphrase only goes into its own command"
        );
    }
    assert!(wifi.network_flags().await.is_online());
}

#[tokio::test]
async fn failed_joins_are_classified_and_cleaned_up() {
    let (_dir, state, mut wifi) = setup().await;
    assert_eq!(
        wifi.connect("home", "wrong-pass").await,
        Err(ConnectError::InvalidCredentials)
    );
    assert_eq!(
        wifi.connect("elsewhere", "whatever1").await,
        Err(ConnectError::NetworkNotFound)
    );
    let s = state.lock().unwrap();
    assert!(s.networks.is_empty(), "failed networks are removed");
    assert!(s.saved.is_empty(), "nothing is saved");
    assert!(!s.log.iter().any(|c| c == "SAVE_CONFIG"));
}

#[tokio::test]
async fn scans_report_security_from_the_flags() {
    let (_dir, _state, mut wifi) = setup().await;
    let mut found = heapless::Vec::<_, 8>::new();
    wifi.scan(&mut found).await.unwrap();
    let summary: Vec<_> = found.iter().map(|n| (n.ssid.as_str(), n.auth)).collect();
    assert_eq!(
        summary,
        [
            ("home", WifiAuth::Wpa2Personal),
            ("lab", WifiAuth::Wpa3Personal)
        ]
    );
}
