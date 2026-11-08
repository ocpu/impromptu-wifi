//! What a device must provide to be provisioned over Improv Wi-Fi.
//!
//! This crate is deliberately small and independent of the wire format and
//! the driver, so that platform crates can implement it without depending on
//! anything else in the stack.
//!
//! # Required and optional capabilities
//!
//! Every device needs [`WifiConnect`] and [`DeviceInfoSource`]. The rest are
//! optional, and each optional trait has a `SUPPORTED` constant. The driver
//! reads these constants to decide which commands to answer and, on BLE,
//! which capability bits to advertise. Support is therefore fixed by the
//! types and can't drift out of sync with the implementation.
//!
//! To opt out of optional traits, use [`unsupported!`]:
//!
//! ```
//! use impromptu_wifi_traits::{unsupported, ConnectError, DeviceInfoSource, Url, WifiConnect};
//!
//! struct MyWifi;
//! impl WifiConnect for MyWifi {
//!     async fn connect(&mut self, _ssid: &str, _pw: &str) -> Result<Option<Url>, ConnectError> {
//!         Ok(None)
//!     }
//! }
//! unsupported!(MyWifi: WifiScanner, NetworkStatus);
//!
//! struct MyDevice;
//! impl DeviceInfoSource for MyDevice {
//!     fn firmware_name(&self) -> &str { "my-fw" }
//!     fn firmware_version(&self) -> &str { "1.0.0" }
//!     fn chip(&self) -> &str { "esp32-c6" }
//!     fn device_name(&self) -> &str { "Kitchen Sensor" }
//! }
//! unsupported!(MyDevice: Identify, HostnameProvider);
//! ```
//!
//! # Why one object per role
//!
//! The driver takes one Wi-Fi object and one device object, each
//! implementing several traits, rather than one object per trait. On real
//! hardware the same Wi-Fi controller both connects and scans, and often
//! owns the hostname too, and it can't be handed out twice.
#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
// Used with whatever executor the platform provides, so `Send` is not required.
#![allow(async_fn_in_trait)]

#[doc(hidden)]
pub use heapless;

/// Longest SSID, in bytes (the 802.11 limit).
pub const MAX_SSID_LEN: usize = 32;

/// Longest redirect URL, in bytes.
pub const MAX_URL_LEN: usize = 128;

/// A network name. Owned, so scanners can fill it from a temporary buffer.
pub type Ssid = heapless::String<MAX_SSID_LEN>;

/// A URL the client should open after provisioning, such as the device's web UI.
pub type Url = heapless::String<MAX_URL_LEN>;

/// Longest passphrase, in bytes: a WPA2 passphrase is 8 to 63 characters,
/// and a raw pre-shared key 64 hexadecimal digits (IEEE 802.11-2020,
/// Annex J.4).
pub const MAX_PASSPHRASE_LEN: usize = 64;

/// A Wi-Fi passphrase. Empty for open networks.
pub type Passphrase = heapless::String<MAX_PASSPHRASE_LEN>;

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

/// Authentication method of a scanned network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum WifiAuth {
    /// No authentication.
    Open,
    /// Wired Equivalent Privacy (WEP).
    Wep,
    /// WPA Personal.
    Wpa,
    /// WPA2 Personal.
    Wpa2Personal,
    /// WPA/WPA2 Personal transition mode.
    WpaWpa2Personal,
    /// WPA2 Enterprise.
    Wpa2Enterprise,
    /// WPA3 Personal.
    Wpa3Personal,
    /// WPA2/WPA3 Personal transition mode.
    Wpa2Wpa3Personal,
    /// WAPI Personal.
    WapiPersonal,
    /// Opportunistic Wireless Encryption: encrypted, but no passphrase.
    Owe,
    /// WPA3 Enterprise, including Suite B 192-bit mode.
    Wpa3Enterprise,
    /// WPA2/WPA3 Enterprise transition mode.
    Wpa2Wpa3Enterprise,
    /// WPA Enterprise.
    WpaEnterprise,
    /// The scan could not identify the method.
    Unknown,
}

impl WifiAuth {
    /// Whether the user has to enter a passphrase. OWE doesn't need one;
    /// an unknown method is assumed to.
    pub const fn requires_passphrase(self) -> bool {
        !matches!(self, WifiAuth::Open | WifiAuth::Owe)
    }
}

/// A network found by a scan.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct NetworkInfo {
    /// The network's SSID.
    pub ssid: Ssid,
    /// Received signal strength, in dBm.
    pub rssi_dbm: i8,
    /// Authentication method.
    pub auth: WifiAuth,
}

impl NetworkInfo {
    /// Build an entry, or `None` if `ssid` is longer than [`MAX_SSID_LEN`].
    pub fn new(ssid: &str, rssi_dbm: i8, auth: WifiAuth) -> Option<Self> {
        Some(Self {
            ssid: Ssid::try_from(ssid).ok()?,
            rssi_dbm,
            auth,
        })
    }
}

/// The settings of a Wi-Fi network the device joined, for a
/// [`CredentialStore`].
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    /// Network name.
    pub ssid: Ssid,
    /// Passphrase (empty for open networks).
    pub password: Passphrase,
}

impl Credentials {
    /// Build credentials, or `None` if `ssid` is longer than
    /// [`MAX_SSID_LEN`] or `password` longer than [`MAX_PASSPHRASE_LEN`].
    pub fn new(ssid: &str, password: &str) -> Option<Self> {
        Some(Self {
            ssid: Ssid::try_from(ssid).ok()?,
            password: Passphrase::try_from(password).ok()?,
        })
    }
}

/// Shows the SSID only, so the passphrase stays out of logs.
impl core::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Credentials")
            .field("ssid", &self.ssid)
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for Credentials {
    fn format(&self, f: defmt::Formatter<'_>) {
        defmt::write!(f, "Credentials {{ ssid: {}, .. }}", self.ssid.as_str())
    }
}

/// Network connectivity flags, reported by the serial network-state command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct NetworkFlags(pub u8);

impl NetworkFlags {
    /// The device currently has network connectivity.
    pub const ONLINE: u8 = 1 << 0;
    /// A Wi-Fi interface is present (it may be disabled).
    pub const WIFI: u8 = 1 << 1;
    /// An Ethernet interface is present.
    pub const ETHERNET: u8 = 1 << 2;
    /// A Thread interface is present.
    pub const THREAD: u8 = 1 << 3;
    /// A cellular modem is present.
    pub const CELLULAR: u8 = 1 << 4;

    /// Whether [`ONLINE`](Self::ONLINE) is set.
    pub const fn is_online(self) -> bool {
        self.0 & Self::ONLINE != 0
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a Wi-Fi connection attempt failed.
///
/// The protocol reports every variant as "unable to connect"; the detail is
/// for logs and for the application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum ConnectError {
    /// The access point rejected the credentials.
    InvalidCredentials,
    /// No network with the requested SSID was found.
    NetworkNotFound,
    /// The attempt timed out.
    Timeout,
    /// Wi-Fi is not available on the platform.
    Unavailable,
    /// The attempt failed for another reason.
    Failed,
}

/// Why a scan failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum ScanError {
    /// Scanning is not available right now (for example, the radio is busy).
    Unavailable,
    /// The scan failed for another reason.
    Failed,
}

/// Why a hostname could not be set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum HostnameError {
    /// The hostname is not acceptable. Reported as "bad hostname".
    InvalidName,
    /// The platform does not support changing the hostname.
    Unavailable,
    /// The platform failed to apply it for another reason.
    Failed,
}

/// Why a device name could not be set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum DeviceNameError {
    /// The name is not acceptable.
    InvalidName,
    /// The device does not support renaming.
    Unavailable,
    /// The device failed to apply it for another reason.
    Failed,
}

// ---------------------------------------------------------------------------
// Required traits
// ---------------------------------------------------------------------------

/// Connects the device to a Wi-Fi network.
pub trait WifiConnect {
    /// Try to join `ssid`, returning once the outcome is known.
    ///
    /// On success, optionally return a URL the client should redirect the
    /// user to. Persisting the credentials is up to the implementation.
    async fn connect(&mut self, ssid: &str, password: &str) -> Result<Option<Url>, ConnectError>;
}

/// Firmware and hardware metadata, returned by the device info command.
///
/// Strings borrow from `self`, so they can change at runtime: the device
/// name in particular can be set by the client.
pub trait DeviceInfoSource {
    /// Firmware name.
    fn firmware_name(&self) -> &str;
    /// Firmware version.
    fn firmware_version(&self) -> &str;
    /// Chip identifier or board/model name.
    fn chip(&self) -> &str;
    /// User-facing device name.
    fn device_name(&self) -> &str;
    /// OS name, if different from the firmware.
    fn os_name(&self) -> Option<&str> {
        None
    }
    /// OS version, if different from the firmware.
    fn os_version(&self) -> Option<&str> {
        None
    }
    /// Rename the device. Not a capability bit in the protocol, so this is
    /// an overridable default rather than a separate trait. Async because
    /// applying a name usually means I/O: flash on a microcontroller, a
    /// system service on Linux.
    async fn set_device_name(&mut self, _name: &str) -> Result<(), DeviceNameError> {
        Err(DeviceNameError::Unavailable)
    }
}

// ---------------------------------------------------------------------------
// Optional traits
// ---------------------------------------------------------------------------

/// Makes the device recognizable to the user, for example by flashing an
/// LED or playing a sound. Used over BLE.
pub trait Identify {
    /// Whether identify is supported. Opt out with [`unsupported!`].
    const SUPPORTED: bool = true;

    /// Start the identification action. Should return promptly; long
    /// patterns belong in a separate task.
    async fn identify(&mut self);
}

/// Scans for nearby Wi-Fi networks.
pub trait WifiScanner {
    /// Whether scanning is supported. Opt out with [`unsupported!`].
    const SUPPORTED: bool = true;

    /// Scan and fill `out`, strongest first. Entries that don't fit may be
    /// dropped.
    async fn scan<const N: usize>(
        &mut self,
        out: &mut heapless::Vec<NetworkInfo, N>,
    ) -> Result<(), ScanError>;
}

/// Reads and updates the device's hostname.
pub trait HostnameProvider {
    /// Whether hostname get/set is supported. Opt out with [`unsupported!`].
    const SUPPORTED: bool = true;

    /// The current hostname. Synchronous, so implementations keep a cached
    /// copy and refresh it in [`set_hostname`](Self::set_hostname).
    fn hostname(&self) -> &str;

    /// Apply a new hostname. The driver has already checked the spec's
    /// rules (letters, digits and hyphens, at most 255 characters). Async
    /// because applying it usually means I/O (on Linux, a D-Bus call to
    /// systemd-hostnamed).
    async fn set_hostname(&mut self, name: &str) -> Result<(), HostnameError>;
}

/// Reports general connectivity, for the serial network-state command.
pub trait NetworkStatus {
    /// Whether network state is supported. Opt out with [`unsupported!`].
    const SUPPORTED: bool = true;

    /// Current flags. Async because connectivity changes outside the
    /// driver's control (a cable pulled, a router rebooted), so it's worth
    /// asking the system rather than caching.
    async fn network_flags(&self) -> NetworkFlags;

    /// Where the device can be reached right now, if online, for example
    /// `http://192.168.1.50`. Lets "network state" report a URL even when
    /// this run of the driver didn't do the provisioning (a device that was
    /// already online when it started). Defaults to none.
    async fn current_url(&self) -> Option<Url> {
        None
    }
}

/// Keeps the credentials of the last network joined across restarts, so a
/// device can rejoin at boot instead of asking to be provisioned again.
///
/// Implementations write to flash, a file, or a platform service. The
/// driver doesn't use this trait itself; wrap the Wi-Fi object in
/// `impromptu_wifi_core::PersistOnJoin` to save after each successful join.
pub trait CredentialStore {
    /// Why an operation failed.
    type Error: core::fmt::Debug;

    /// The saved credentials, or `None` if nothing valid is saved.
    async fn load(&mut self) -> Result<Option<Credentials>, Self::Error>;

    /// Replace the saved credentials.
    async fn save(&mut self, credentials: &Credentials) -> Result<(), Self::Error>;

    /// Remove the saved credentials, for a factory reset.
    async fn clear(&mut self) -> Result<(), Self::Error>;
}

impl<T: CredentialStore + ?Sized> CredentialStore for &mut T {
    type Error = T::Error;
    async fn load(&mut self) -> Result<Option<Credentials>, Self::Error> {
        (**self).load().await
    }
    async fn save(&mut self, credentials: &Credentials) -> Result<(), Self::Error> {
        (**self).save(credentials).await
    }
    async fn clear(&mut self) -> Result<(), Self::Error> {
        (**self).clear().await
    }
}

/// Implement optional traits as unsupported for a type.
///
/// ```
/// # struct Lamp;
/// impromptu_wifi_traits::unsupported!(Lamp: Identify, HostnameProvider);
/// ```
///
/// Accepts `Identify`, `WifiScanner`, `HostnameProvider` and `NetworkStatus`.
#[macro_export]
macro_rules! unsupported {
    ($ty:ty : $($tr:ident),+ $(,)?) => {
        $( $crate::__unsupported_impl!($tr for $ty); )+
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __unsupported_impl {
    (Identify for $ty:ty) => {
        impl $crate::Identify for $ty {
            const SUPPORTED: bool = false;
            async fn identify(&mut self) {}
        }
    };
    (WifiScanner for $ty:ty) => {
        impl $crate::WifiScanner for $ty {
            const SUPPORTED: bool = false;
            async fn scan<const N: usize>(
                &mut self,
                _out: &mut $crate::heapless::Vec<$crate::NetworkInfo, N>,
            ) -> ::core::result::Result<(), $crate::ScanError> {
                ::core::result::Result::Err($crate::ScanError::Unavailable)
            }
        }
    };
    (HostnameProvider for $ty:ty) => {
        impl $crate::HostnameProvider for $ty {
            const SUPPORTED: bool = false;
            fn hostname(&self) -> &str {
                ""
            }
            async fn set_hostname(
                &mut self,
                _name: &str,
            ) -> ::core::result::Result<(), $crate::HostnameError> {
                ::core::result::Result::Err($crate::HostnameError::Unavailable)
            }
        }
    };
    (NetworkStatus for $ty:ty) => {
        impl $crate::NetworkStatus for $ty {
            const SUPPORTED: bool = false;
            async fn network_flags(&self) -> $crate::NetworkFlags {
                $crate::NetworkFlags::default()
            }
        }
    };
    ($other:ident for $ty:ty) => {
        ::core::compile_error!(::core::concat!(
            "unsupported!: `",
            ::core::stringify!($other),
            "` is not optional (expected Identify, WifiScanner, HostnameProvider or NetworkStatus)"
        ));
    };
}

// ---------------------------------------------------------------------------
// Forwarding through `&mut T`
// ---------------------------------------------------------------------------
//
// A driver lives for one connection (one BLE link, say), while the Wi-Fi and
// device objects live for the whole program. These impls let each driver
// borrow them: `Driver::new(link, &mut wifi, &mut device, true)`.

impl<T: WifiConnect + ?Sized> WifiConnect for &mut T {
    async fn connect(&mut self, ssid: &str, password: &str) -> Result<Option<Url>, ConnectError> {
        (**self).connect(ssid, password).await
    }
}

impl<T: DeviceInfoSource + ?Sized> DeviceInfoSource for &mut T {
    fn firmware_name(&self) -> &str {
        (**self).firmware_name()
    }
    fn firmware_version(&self) -> &str {
        (**self).firmware_version()
    }
    fn chip(&self) -> &str {
        (**self).chip()
    }
    fn device_name(&self) -> &str {
        (**self).device_name()
    }
    fn os_name(&self) -> Option<&str> {
        (**self).os_name()
    }
    fn os_version(&self) -> Option<&str> {
        (**self).os_version()
    }
    async fn set_device_name(&mut self, name: &str) -> Result<(), DeviceNameError> {
        (**self).set_device_name(name).await
    }
}

impl<T: Identify + ?Sized> Identify for &mut T {
    const SUPPORTED: bool = T::SUPPORTED;
    async fn identify(&mut self) {
        (**self).identify().await
    }
}

impl<T: WifiScanner + ?Sized> WifiScanner for &mut T {
    const SUPPORTED: bool = T::SUPPORTED;
    async fn scan<const N: usize>(
        &mut self,
        out: &mut heapless::Vec<NetworkInfo, N>,
    ) -> Result<(), ScanError> {
        (**self).scan(out).await
    }
}

impl<T: HostnameProvider + ?Sized> HostnameProvider for &mut T {
    const SUPPORTED: bool = T::SUPPORTED;
    fn hostname(&self) -> &str {
        (**self).hostname()
    }
    async fn set_hostname(&mut self, name: &str) -> Result<(), HostnameError> {
        (**self).set_hostname(name).await
    }
}

impl<T: NetworkStatus + ?Sized> NetworkStatus for &mut T {
    const SUPPORTED: bool = T::SUPPORTED;
    async fn network_flags(&self) -> NetworkFlags {
        (**self).network_flags().await
    }
    async fn current_url(&self) -> Option<Url> {
        (**self).current_url().await
    }
}

impl<T: AuthSource + ?Sized> AuthSource for &mut T {
    async fn next(&mut self) -> AuthEvent {
        (**self).next().await
    }
}

// ---------------------------------------------------------------------------
// Authorization
// ---------------------------------------------------------------------------

/// An authorization event (BLE).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum AuthEvent {
    /// The user performed the physical authorization.
    Authorize,
    /// The authorization timeout expired.
    Revoke,
}

/// Source of authorization events: a button, a timer, or both.
pub trait AuthSource {
    /// Wait for the next event. Must be cancel-safe: the driver races it
    /// against incoming commands.
    async fn next(&mut self) -> AuthEvent;

    /// Restart the authorization timeout, if one is running.
    ///
    /// The driver calls this when the BLE spec says the timeout restarts:
    /// after a failed Wi-Fi join, and after a hostname or device-name write.
    /// A source without a timeout ignores it, which is the default.
    fn restart_timeout(&mut self) {}
}

/// No authorization: for serial, and for BLE devices that start authorized.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoAuth;

impl AuthSource for NoAuth {
    async fn next(&mut self) -> AuthEvent {
        core::future::pending().await
    }
}

/// Something that happened outside this driver and changes what it
/// reports.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum DeviceEvent {
    /// Wi-Fi became available (`true`) or unavailable (`false`), for
    /// example because the radio was switched off. While unavailable a
    /// serial device reports the `Stopped` state.
    Available(bool),
    /// The device joined a network some other way, typically through
    /// another driver on another transport, and can be reached at this URL.
    /// An idle driver then reports `Provisioned`, so all transports agree.
    Provisioned(Option<Url>),
    /// The device is no longer on a network and can be provisioned again,
    /// for example after it lost its network and could not rejoin. A
    /// provisioned driver returns to `Authorized` or
    /// `AuthorizationRequired`.
    ///
    /// The Improv Wi-Fi specification does not define a transition out of
    /// `Provisioned`; reporting one is this crate's choice, so that a client
    /// can provision the device again without a restart.
    Unprovisioned,
}

/// Source of [`DeviceEvent`]s for a driver.
///
/// The driver races [`next`](Self::next) against incoming commands, so an
/// event is published to the client as soon as it happens. Combine two
/// sources with a tuple: `(a, b)` yields whichever has an event first.
pub trait EventSource {
    /// Wait for the next event. Must be cancel-safe.
    async fn next(&mut self) -> DeviceEvent;
}

/// No events: the default.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoEvents;

impl EventSource for NoEvents {
    async fn next(&mut self) -> DeviceEvent {
        core::future::pending().await
    }
}

impl<T: EventSource + ?Sized> EventSource for &mut T {
    async fn next(&mut self) -> DeviceEvent {
        (**self).next().await
    }
}

impl<A: EventSource, B: EventSource> EventSource for (A, B) {
    /// Whichever source has an event first; `A` wins a tie. Cancel-safe
    /// because both sources are.
    async fn next(&mut self) -> DeviceEvent {
        use core::future::Future as _;
        let (a, b) = self;
        let mut a = core::pin::pin!(a.next());
        let mut b = core::pin::pin!(b.next());
        core::future::poll_fn(|cx| {
            if let core::task::Poll::Ready(e) = a.as_mut().poll(cx) {
                return core::task::Poll::Ready(e);
            }
            b.as_mut().poll(cx)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Plain;
    unsupported!(Plain: Identify, WifiScanner, HostnameProvider, NetworkStatus);

    struct Led;
    impl Identify for Led {
        async fn identify(&mut self) {}
    }

    #[test]
    fn support_comes_from_the_impl() {
        const { assert!(!<Plain as Identify>::SUPPORTED) };
        const { assert!(!<Plain as WifiScanner>::SUPPORTED) };
        const { assert!(!<Plain as HostnameProvider>::SUPPORTED) };
        const { assert!(!<Plain as NetworkStatus>::SUPPORTED) };
        const { assert!(<Led as Identify>::SUPPORTED) };
    }

    #[test]
    fn support_is_forwarded_through_mut_refs() {
        const { assert!(!<&mut Plain as Identify>::SUPPORTED) };
        const { assert!(<&mut Led as Identify>::SUPPORTED) };
    }

    #[test]
    fn network_info_rejects_long_ssid() {
        assert!(NetworkInfo::new("home", -40, WifiAuth::Wpa2Personal).is_some());
        let too_long = "this-ssid-is-definitely-longer-than-32-bytes";
        assert!(NetworkInfo::new(too_long, -40, WifiAuth::Open).is_none());
    }

    #[test]
    fn credentials_check_lengths_and_hide_the_passphrase() {
        let c = Credentials::new("home", "secret-pw").unwrap();
        assert_eq!(c.ssid.as_str(), "home");
        assert_eq!(c.password.as_str(), "secret-pw");
        let mut shown = heapless::String::<64>::new();
        core::fmt::write(&mut shown, format_args!("{c:?}")).unwrap();
        assert!(!shown.contains("secret"), "{shown}");

        let long = core::str::from_utf8(&[b'x'; 65]).unwrap();
        assert!(Credentials::new(&long[..33], "").is_none());
        assert!(Credentials::new("home", long).is_none());
        assert!(Credentials::new("home", &long[..64]).is_some());
    }

    #[test]
    fn passphrase_requirement() {
        assert!(!WifiAuth::Open.requires_passphrase());
        assert!(!WifiAuth::Owe.requires_passphrase());
        assert!(WifiAuth::Unknown.requires_passphrase());
    }
}
