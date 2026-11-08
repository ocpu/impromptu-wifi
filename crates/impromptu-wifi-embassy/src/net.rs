//! Helpers for Wi-Fi backends whose IP layer is `embassy-net`.
//!
//! Joining a network (association) and being reachable (an IP address) are
//! different moments. Improv reports "provisioned" with a URL, so a backend
//! should finish `connect` only once DHCP has assigned an address.

use core::fmt::Write as _;

use embassy_net::Stack;
use embassy_time::{with_timeout, Duration};
use impromptu_wifi_core::traits::{NetworkFlags, Url};

/// How long [`wait_for_url`] waits for DHCP by default.
pub const DEFAULT_IP_TIMEOUT: Duration = Duration::from_secs(20);

/// Wait until `stack` has an IPv4 address, then return `http://<address>`.
/// Returns `None` if no address arrives within `timeout`.
pub async fn wait_for_url(stack: Stack<'_>, timeout: Duration) -> Option<Url> {
    with_timeout(timeout, stack.wait_config_up()).await.ok()?;
    url_for(stack)
}

/// `http://<address>` for the stack's current IPv4 address, if it has one.
pub fn url_for(stack: Stack<'_>) -> Option<Url> {
    let config = stack.config_v4()?;
    let mut url = Url::new();
    write!(url, "http://{}", config.address.address()).ok()?;
    Some(url)
}

/// Network-state flags for a device whose only interface is Wi-Fi on
/// `stack`: Wi-Fi present, and online once the stack has an address.
pub fn wifi_flags(stack: Stack<'_>) -> NetworkFlags {
    let online = if stack.is_config_up() {
        NetworkFlags::ONLINE
    } else {
        0
    };
    NetworkFlags(NetworkFlags::WIFI | online)
}
