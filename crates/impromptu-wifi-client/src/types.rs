use impromptu_wifi_traits::{NetworkFlags, Ssid, Url};

/// Longest string the client keeps from a response (device-info fields,
/// hostnames, device names). Longer strings fail with
/// [`ClientError::TooLong`](crate::ClientError::TooLong).
pub const MAX_TEXT: usize = 128;

/// An owned string from a device response.
pub type Text = heapless::String<MAX_TEXT>;

/// What the device-info command returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Firmware name.
    pub firmware_name: Text,
    /// Firmware version.
    pub firmware_version: Text,
    /// Chip identifier or board/model name.
    pub chip: Text,
    /// User-facing device name.
    pub device_name: Text,
    /// OS name and version, if the device reports them.
    pub os: Option<(Text, Text)>,
}

/// A network the device can see.
///
/// Serial reports only whether a passphrase is needed; BLE also reports the
/// authentication type as a string such as `"WPA2"` or `"WPA/WPA2"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedNetwork {
    /// The network's SSID.
    pub ssid: Ssid,
    /// Signal strength in dBm.
    pub rssi_dbm: i8,
    /// Whether the user has to enter a passphrase.
    pub requires_passphrase: bool,
    /// Authentication type as reported over BLE; `None` on serial.
    pub auth: Option<heapless::String<16>>,
}

/// What the serial network-state command returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkState {
    /// Connectivity flags.
    pub flags: NetworkFlags,
    /// Where the device can be reached, when online.
    pub urls: heapless::Vec<Url, 4>,
}
