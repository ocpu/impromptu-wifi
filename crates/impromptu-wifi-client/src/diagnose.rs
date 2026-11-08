//! Why a join failed, judged from a scan made right after it.
//!
//! The protocol reports only `Unable to connect`. What the device can see
//! narrows the cause down; clients turn the result into their own wording.

use crate::types::ScannedNetwork;

/// A scan with this many networks may have been cut short by the device:
/// `impromptu-wifi-core`'s driver reports at most 16, the strongest.
pub const LONG_SCAN: usize = 16;

/// The likely reason a join failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinFailure<'a> {
    /// The device couldn't scan, so there is nothing to go on.
    NoScan,
    /// The network isn't in the scan.
    NotSeen {
        /// A network whose name differs only in case or surrounding spaces.
        similar: Option<&'a ScannedNetwork>,
        /// How many networks the scan listed; see [`LONG_SCAN`].
        scanned: usize,
    },
    /// The network is open, but a passphrase was sent.
    OpenButPassphraseSent {
        /// Signal strength the device saw.
        rssi_dbm: i8,
    },
    /// The network needs a passphrase, and none was sent.
    PassphraseMissing {
        /// Signal strength the device saw.
        rssi_dbm: i8,
    },
    /// The network reports WPA3 without WPA2, which some devices can't join.
    Wpa3Only {
        /// Signal strength the device saw.
        rssi_dbm: i8,
    },
    /// The network is visible and the passphrase presence fits; most likely
    /// a wrong passphrase or an unsupported security mode.
    Rejected {
        /// Signal strength the device saw.
        rssi_dbm: i8,
    },
}

/// Diagnose a failed join to `ssid` from `scan` (`None` if the device
/// couldn't scan). `passphrase_sent` is whether a non-empty passphrase was
/// sent.
pub fn diagnose_join_failure<'a>(
    ssid: &str,
    passphrase_sent: bool,
    scan: Option<&'a [ScannedNetwork]>,
) -> JoinFailure<'a> {
    let Some(networks) = scan else {
        return JoinFailure::NoScan;
    };
    let Some(network) = networks.iter().find(|n| n.ssid.as_str() == ssid) else {
        return JoinFailure::NotSeen {
            similar: networks
                .iter()
                .find(|n| n.ssid.trim().eq_ignore_ascii_case(ssid.trim())),
            scanned: networks.len(),
        };
    };
    let rssi_dbm = network.rssi_dbm;
    if !network.requires_passphrase && passphrase_sent {
        return JoinFailure::OpenButPassphraseSent { rssi_dbm };
    }
    if network.requires_passphrase && !passphrase_sent {
        return JoinFailure::PassphraseMissing { rssi_dbm };
    }
    let wpa3_only = network
        .auth
        .as_deref()
        .is_some_and(|a| a.contains("WPA3") && !a.contains("WPA2"));
    if wpa3_only {
        JoinFailure::Wpa3Only { rssi_dbm }
    } else {
        JoinFailure::Rejected { rssi_dbm }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(ssid: &str, secured: bool, auth: Option<&str>) -> ScannedNetwork {
        ScannedNetwork {
            ssid: ssid.try_into().unwrap(),
            rssi_dbm: -50,
            requires_passphrase: secured,
            auth: auth.map(|a| a.try_into().unwrap()),
        }
    }

    #[test]
    fn classifies_like_the_cli() {
        let scan = [
            net("OnePlus Nord", true, None),
            net("Cafe", false, None),
            net("Lab", true, Some("WPA3")),
        ];
        let s = Some(&scan[..]);
        assert_eq!(
            diagnose_join_failure("Garage", true, s),
            JoinFailure::NotSeen {
                similar: None,
                scanned: 3
            }
        );
        match diagnose_join_failure("oneplus nord", true, s) {
            JoinFailure::NotSeen {
                similar: Some(n),
                scanned: 3,
            } => {
                assert_eq!(n.ssid.as_str(), "OnePlus Nord")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            diagnose_join_failure("OnePlus Nord", true, s),
            JoinFailure::Rejected { rssi_dbm: -50 }
        );
        assert_eq!(
            diagnose_join_failure("Cafe", true, s),
            JoinFailure::OpenButPassphraseSent { rssi_dbm: -50 }
        );
        assert_eq!(
            diagnose_join_failure("OnePlus Nord", false, s),
            JoinFailure::PassphraseMissing { rssi_dbm: -50 }
        );
        assert_eq!(
            diagnose_join_failure("Lab", true, s),
            JoinFailure::Wpa3Only { rssi_dbm: -50 }
        );
        assert_eq!(
            diagnose_join_failure("Lab", true, None),
            JoinFailure::NoScan
        );
    }
}
