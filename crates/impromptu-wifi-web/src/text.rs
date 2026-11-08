//! Messages shown on the page. They follow the CLI's wording, with the
//! page's controls in place of its options.

use core::fmt::Debug;

use impromptu_wifi_client::proto::ble::Capabilities;
use impromptu_wifi_client::proto::{ErrorCode, State};
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

/// A readable message for a failed join to `ssid`.
pub fn join_failure_message(ssid: &str, f: &JoinFailure<'_>) -> String {
    match f {
        JoinFailure::NoScan => format!(
            "The device could not join \"{ssid}\". Check the network name (it is \
             case-sensitive) and the passphrase. Many devices, including most ESP32 chips, \
             need a 2.4 GHz network, and some can't join WPA3-only networks."
        ),
        JoinFailure::NotSeen { similar, scanned } => {
            let mut m = format!(
                "The device could not join \"{ssid}\": it can't see that network. Check \
                 that the name is exact (it is case-sensitive), that the network is in \
                 range, and that it offers 2.4 GHz: many devices, including most ESP32 \
                 chips, have no 5 GHz radio."
            );
            if let Some(n) = similar {
                m.push_str(&format!(" It does see \"{}\": is that the one?", n.ssid));
            }
            if *scanned >= LONG_SCAN {
                m.push_str(&format!(
                    " The scan listed {scanned} networks; devices often report only their \
                     strongest ones, so a weak network can be missing from it."
                ));
            }
            m
        }
        JoinFailure::OpenButPassphraseSent { rssi_dbm } => format!(
            "The device could not join \"{ssid}\" (seen at {rssi_dbm} dBm). The network \
             is open, but a passphrase was sent. Tick \u{201c}Open network\u{201d} and try again."
        ),
        JoinFailure::PassphraseMissing { rssi_dbm } => format!(
            "The device could not join \"{ssid}\" (seen at {rssi_dbm} dBm). The network \
             needs a passphrase, and none was sent."
        ),
        JoinFailure::Wpa3Only { rssi_dbm } => format!(
            "The device could not join \"{ssid}\" (seen at {rssi_dbm} dBm, WPA3 only). \
             Check the passphrase. Some devices can't join WPA3-only networks; if the \
             passphrase is right, switch the network to WPA2/WPA3 or WPA2."
        ),
        JoinFailure::Rejected { rssi_dbm } => format!(
            "The device could not join \"{ssid}\" (seen at {rssi_dbm} dBm). Check the \
             passphrase. If it is right, check the network's security mode: some devices, \
             including ESP32 firmware built on esp-radio, can't join WPA3-only networks, \
             so try WPA2/WPA3 or WPA2."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_network_hint_names_the_checkbox() {
        let m = join_failure_message(
            "Cafe",
            &JoinFailure::OpenButPassphraseSent { rssi_dbm: -50 },
        );
        assert!(m.contains("Open network") && !m.contains("--open"), "{m}");
    }

    #[test]
    fn long_scan_is_mentioned() {
        let m = join_failure_message(
            "Garage",
            &JoinFailure::NotSeen {
                similar: None,
                scanned: 16,
            },
        );
        assert!(m.contains("listed 16 networks"), "{m}");
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
