//! Messages shown on the page. They follow the CLI's wording, with the
//! page's controls in place of its options.

use core::fmt::Debug;

use impromptu_wifi_client::proto::ble::Capabilities;
use impromptu_wifi_client::proto::{ErrorCode, State};
#[cfg(test)]
use impromptu_wifi_client::ScannedNetwork;
use impromptu_wifi_client::{ClientError, JoinFailure, LONG_SCAN};
use impromptu_wifi_traits::NetworkFlags;

/// One line describing a device state.
pub fn state_label(s: State) -> &'static str {
    match s {
        State::Stopped => "Stopped: provisioning is unavailable (Wi-Fi may be off)",
        State::AuthorizationRequired => "Waiting for authorization: press the device's button",
        State::Authorized => "Ready for Wi-Fi credentials",
        State::Provisioning => "Connecting to a network",
        State::Provisioned => "Provisioned",
    }
}

/// Names of the optional BLE commands a device supports.
pub fn capability_names(c: Capabilities) -> Vec<&'static str> {
    [
        (Capabilities::IDENTIFY, "identify"),
        (Capabilities::DEVICE_INFO, "device info"),
        (Capabilities::SCAN, "scan"),
        (Capabilities::HOSTNAME, "hostname"),
    ]
    .into_iter()
    .filter(|(cap, _)| c.contains(*cap))
    .map(|(_, name)| name)
    .collect()
}

/// Names of the network interfaces in `flags`.
pub fn interface_names(flags: NetworkFlags) -> Vec<&'static str> {
    [
        (NetworkFlags::WIFI, "Wi-Fi"),
        (NetworkFlags::ETHERNET, "Ethernet"),
        (NetworkFlags::THREAD, "Thread"),
        (NetworkFlags::CELLULAR, "cellular"),
    ]
    .into_iter()
    .filter(|(bit, _)| flags.0 & bit != 0)
    .map(|(_, name)| name)
    .collect()
}

/// A readable message for a failed call.
pub fn error_message<E: Debug>(e: &ClientError<E>) -> String {
    match e {
        ClientError::Device(ErrorCode::UnableToConnect) => {
            "The device could not join the network; check the network name and passphrase.".into()
        }
        ClientError::Device(ErrorCode::UnknownCommand) => {
            "The device doesn't support this command.".into()
        }
        ClientError::Device(ErrorCode::NotAuthorized) => {
            "The device needs authorization; press its button and try again.".into()
        }
        ClientError::Device(ErrorCode::BadHostname) => {
            "The device rejected that hostname (letters, digits and hyphens only).".into()
        }
        ClientError::Device(code) => format!("The device reported an error: {code:?}."),
        ClientError::Unsupported => "This command isn't available over this connection.".into(),
        ClientError::Protocol => {
            "The device sent an unexpected or damaged answer; try again.".into()
        }
        ClientError::TooLong => "A value is too long for the protocol.".into(),
        ClientError::Link(e) => format!("Connection error: {e:?}."),
    }
}

/// A failed join, split for display: what happened and what to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinFailureView {
    /// `not-seen`, `open-network`, `passphrase-missing`, `wpa3-only`,
    /// `rejected` or `unknown`.
    pub kind: &'static str,
    /// One line: what happened.
    pub title: String,
    /// What to check or change.
    pub advice: String,
    /// Signal strength the device saw the network at, if it saw it.
    pub rssi: Option<i8>,
    /// A network whose name differs only in case or spaces.
    pub similar: Option<String>,
}

/// Describe a failed join to `ssid`.
pub fn join_failure_view(ssid: &str, f: &JoinFailure<'_>) -> JoinFailureView {
    const WPA3: &str = "Some devices, including ESP32 firmware built on esp-radio, can\u{2019}t \
                        join WPA3-only networks; if the passphrase is right, switch the \
                        network to WPA2/WPA3 or WPA2.";
    let q = |s: &str| format!("\u{201c}{s}\u{201d}");
    let view = |kind, title: String, advice: String, rssi| JoinFailureView {
        kind,
        title,
        advice,
        rssi,
        similar: None,
    };
    match f {
        JoinFailure::NoScan => view(
            "unknown",
            format!("The device could not join {}", q(ssid)),
            format!(
                "Check the network name (it is case-sensitive) and the passphrase. Many \
                 devices, including most ESP32 chips, need a 2.4 GHz network. {WPA3}"
            ),
            None,
        ),
        JoinFailure::NotSeen { similar, scanned } => {
            let mut advice = String::from(
                "Check that the name is exact (it is case-sensitive), that the network is in \
                 range, and that it offers 2.4 GHz: many devices, including most ESP32 chips, \
                 have no 5 GHz radio.",
            );
            if *scanned >= LONG_SCAN {
                advice.push_str(&format!(
                    " The scan listed {scanned} networks; devices often report only their \
                     strongest ones, so a weak network can be missing from it."
                ));
            }
            JoinFailureView {
                similar: similar.map(|n| n.ssid.as_str().into()),
                ..view(
                    "not-seen",
                    format!("The device can\u{2019}t see {}", q(ssid)),
                    advice,
                    None,
                )
            }
        }
        JoinFailure::OpenButPassphraseSent { rssi_dbm } => view(
            "open-network",
            format!("{} is an open network", q(ssid)),
            "A passphrase was sent, but the network has none.".into(),
            Some(*rssi_dbm),
        ),
        JoinFailure::PassphraseMissing { rssi_dbm } => view(
            "passphrase-missing",
            format!("{} needs a passphrase", q(ssid)),
            "None was sent. Enter the network\u{2019}s passphrase.".into(),
            Some(*rssi_dbm),
        ),
        JoinFailure::Wpa3Only { rssi_dbm } => view(
            "wpa3-only",
            format!("{} is WPA3 only", q(ssid)),
            format!("Check the passphrase. {WPA3}"),
            Some(*rssi_dbm),
        ),
        JoinFailure::Rejected { rssi_dbm } => view(
            "rejected",
            format!("{} didn\u{2019}t accept the device", q(ssid)),
            format!(
                "Check the passphrase. If it is right, the security mode may be the cause. {WPA3}"
            ),
            Some(*rssi_dbm),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn similar() -> ScannedNetwork {
        ScannedNetwork {
            ssid: "garage".try_into().unwrap(),
            rssi_dbm: -60,
            requires_passphrase: true,
            auth: None,
        }
    }

    #[test]
    fn join_failures_have_a_kind_title_and_advice() {
        let open = join_failure_view(
            "Cafe",
            &JoinFailure::OpenButPassphraseSent { rssi_dbm: -50 },
        );
        assert_eq!(open.kind, "open-network");
        assert_eq!(open.title, "\u{201c}Cafe\u{201d} is an open network");
        assert_eq!(open.rssi, Some(-50));
        assert!(open.advice.contains("has none"), "{}", open.advice);

        let wpa3 = join_failure_view("Lab", &JoinFailure::Wpa3Only { rssi_dbm: -40 });
        assert_eq!(wpa3.kind, "wpa3-only");
        assert!(wpa3.advice.contains("WPA2/WPA3"), "{}", wpa3.advice);

        let rejected = join_failure_view("Home", &JoinFailure::Rejected { rssi_dbm: -52 });
        assert_eq!(rejected.kind, "rejected");
        assert_eq!(
            rejected.title,
            "\u{201c}Home\u{201d} didn\u{2019}t accept the device"
        );

        assert_eq!(
            join_failure_view("Home", &JoinFailure::PassphraseMissing { rssi_dbm: -52 }).kind,
            "passphrase-missing"
        );
        assert_eq!(
            join_failure_view("Home", &JoinFailure::NoScan).kind,
            "unknown"
        );
    }

    #[test]
    fn a_network_not_seen_offers_the_similar_one() {
        let n = similar();
        let v = join_failure_view(
            "Garage",
            &JoinFailure::NotSeen {
                similar: Some(&n),
                scanned: 16,
            },
        );
        assert_eq!(v.kind, "not-seen");
        assert_eq!(
            v.title,
            "The device can\u{2019}t see \u{201c}Garage\u{201d}"
        );
        assert_eq!(v.similar.as_deref(), Some("garage"));
        assert_eq!(v.rssi, None);
        assert!(v.advice.contains("listed 16 networks"), "{}", v.advice);
    }

    #[test]
    fn not_authorized_asks_for_the_button() {
        let m = error_message(&ClientError::<()>::Device(ErrorCode::NotAuthorized));
        assert!(m.contains("press its button"), "{m}");
    }

    #[test]
    fn interfaces_are_named() {
        assert_eq!(interface_names(NetworkFlags(0x06)), ["Wi-Fi", "Ethernet"]);
    }

    #[test]
    fn states_and_capabilities_are_named() {
        assert_eq!(
            state_label(State::Authorized),
            "Ready for Wi-Fi credentials"
        );
        assert_eq!(
            capability_names(Capabilities::IDENTIFY.union(Capabilities::SCAN)),
            ["identify", "scan"]
        );
    }
}
