//! Values shared by both transports.

/// Which flavour of Improv is being spoken.
///
/// The two transports share most of their vocabulary, but not all of it:
///
/// * RPC command `0x02` means *request current state* on serial and
///   *identify* on BLE.
/// * [`State::Stopped`] and command `0x07` exist only on serial;
///   [`State::AuthorizationRequired`] and [`ErrorCode::NotAuthorized`] only on BLE.
/// * BLE RPC frames end in their own checksum. On serial, the enclosing
///   packet carries the checksum instead.
///
/// Anything that parses or encodes RPC frames therefore takes a `Transport`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Transport {
    /// Improv via serial (USB CDC / UART).
    Serial,
    /// Improv via Bluetooth Low Energy.
    Ble,
}

impl Transport {
    /// Whether RPC command and result frames end with a checksum byte.
    pub const fn rpc_has_checksum(self) -> bool {
        matches!(self, Transport::Ble)
    }

    /// The largest RPC *data* section (the bytes after command and length)
    /// that can be carried on this transport.
    ///
    /// On BLE the one-byte length field is the only limit. On serial the
    /// whole RPC frame (command, length and data) must itself fit in a
    /// packet whose length field is one byte, which leaves 253.
    pub const fn max_rpc_data(self) -> usize {
        match self {
            Transport::Serial => 253,
            Transport::Ble => 255,
        }
    }
}

/// Provisioning state, reported to the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum State {
    /// Serial only: provisioning is unavailable, for example because the
    /// Wi-Fi radio is off.
    Stopped = 0x00,
    /// BLE only: waiting for a physical interaction such as a button press.
    AuthorizationRequired = 0x01,
    /// Ready to accept credentials ("Ready" in the serial spec).
    Authorized = 0x02,
    /// Credentials received; attempting to connect.
    Provisioning = 0x03,
    /// Connected successfully.
    Provisioned = 0x04,
}

impl State {
    /// Whether this state may be reported on `transport`.
    pub const fn valid_for(self, transport: Transport) -> bool {
        match self {
            State::Stopped => matches!(transport, Transport::Serial),
            State::AuthorizationRequired => matches!(transport, Transport::Ble),
            _ => true,
        }
    }
}

impl TryFrom<u8> for State {
    type Error = u8;
    fn try_from(v: u8) -> Result<Self, u8> {
        Ok(match v {
            0x00 => State::Stopped,
            0x01 => State::AuthorizationRequired,
            0x02 => State::Authorized,
            0x03 => State::Provisioning,
            0x04 => State::Provisioned,
            other => return Err(other),
        })
    }
}

/// Error state, reported to the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum ErrorCode {
    /// No current error. Sent whenever a new RPC command arrives.
    None = 0x00,
    /// The RPC frame was malformed.
    InvalidRpc = 0x01,
    /// The RPC command is not known (or not supported by this device).
    UnknownCommand = 0x02,
    /// Credentials were received but connecting failed.
    UnableToConnect = 0x03,
    /// BLE only: the command requires authorization.
    NotAuthorized = 0x04,
    /// The hostname was rejected.
    BadHostname = 0x05,
    /// Anything else.
    Unknown = 0xFF,
}

impl ErrorCode {
    /// Whether this error may be reported on `transport`.
    pub const fn valid_for(self, transport: Transport) -> bool {
        !matches!(
            (self, transport),
            (ErrorCode::NotAuthorized, Transport::Serial)
        )
    }
}

impl TryFrom<u8> for ErrorCode {
    type Error = u8;
    fn try_from(v: u8) -> Result<Self, u8> {
        Ok(match v {
            0x00 => ErrorCode::None,
            0x01 => ErrorCode::InvalidRpc,
            0x02 => ErrorCode::UnknownCommand,
            0x03 => ErrorCode::UnableToConnect,
            0x04 => ErrorCode::NotAuthorized,
            0x05 => ErrorCode::BadHostname,
            0xFF => ErrorCode::Unknown,
            other => return Err(other),
        })
    }
}

/// RPC command identifiers.
pub mod command_id {
    /// Send Wi-Fi settings (both transports).
    pub const WIFI_SETTINGS: u8 = 0x01;
    /// Serial: request current state. Same byte as [`IDENTIFY`].
    pub const GET_CURRENT_STATE: u8 = 0x02;
    /// BLE: identify. Same byte as [`GET_CURRENT_STATE`].
    pub const IDENTIFY: u8 = 0x02;
    /// Request device information (both transports).
    pub const DEVICE_INFO: u8 = 0x03;
    /// Request scanned Wi-Fi networks (both transports).
    pub const SCAN_NETWORKS: u8 = 0x04;
    /// Get or set hostname (both transports).
    pub const HOSTNAME: u8 = 0x05;
    /// Get or set device name (both transports).
    pub const DEVICE_NAME: u8 = 0x06;
    /// Serial only: get network state.
    pub const NETWORK_STATE: u8 = 0x07;
}

/// Longest SSID accepted, in bytes (the 802.11 limit).
pub const MAX_SSID_LEN: usize = 32;
/// Longest passphrase accepted, in bytes (64 covers a raw hex PSK).
pub const MAX_PASSWORD_LEN: usize = 64;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_states_and_errors() {
        for v in 0..=255u8 {
            if let Ok(s) = State::try_from(v) {
                assert_eq!(s as u8, v);
            }
            if let Ok(e) = ErrorCode::try_from(v) {
                assert_eq!(e as u8, v);
            }
        }
    }

    #[test]
    fn transport_specific_values() {
        assert!(!State::Stopped.valid_for(Transport::Ble));
        assert!(!State::AuthorizationRequired.valid_for(Transport::Serial));
        assert!(!ErrorCode::NotAuthorized.valid_for(Transport::Serial));
        assert!(State::Provisioned.valid_for(Transport::Serial));
    }
}
