//! Rejoin the saved network at boot, and keep the device on it.
//!
//! [`keep_online`] is the part of a device's lifecycle that happens outside
//! any Improv Wi-Fi session:
//!
//! 1. **Boot:** load the saved credentials and join. On success every driver
//!    on the [`WifiShare`] reports `Provisioned`, and a BLE task stops
//!    advertising.
//! 2. **Online:** check connectivity every [`KeepOnline::check_every`].
//! 3. **Connection lost:** rejoin with the saved credentials, waiting
//!    longer after each failure, up to [`KeepOnline::max_retry_delay`].
//! 4. **Still offline** after [`KeepOnline::failures_before_reprovision`]
//!    failed rejoins: announce [`DeviceEvent::Unprovisioned`], so the
//!    drivers accept new settings and a BLE task advertises again. Rejoining
//!    continues in the background; whichever succeeds first, a rejoin or a
//!    client, provisions the device again.
//!
//! Joins happen through a [`SharedWifi`](crate::SharedWifi) handle and so
//! hold the share's lock, up to the backend's connect timeout. A client
//! scanning in the meantime waits for the attempt to finish.
//!
//! [`DeviceEvent::Unprovisioned`]: impromptu_wifi_core::traits::DeviceEvent::Unprovisioned

use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_time::{Duration, Timer};
use impromptu_wifi_core::traits::{CredentialStore, NetworkStatus, WifiConnect};
use impromptu_wifi_core::PersistOnJoin;

use crate::WifiShare;

/// When [`keep_online`] checks and rejoins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct KeepOnline {
    /// How often to check connectivity while online. Default 10 s.
    pub check_every: Duration,
    /// Delay before the second rejoin attempt; it doubles after each
    /// failure. Default 2 s.
    pub first_retry_delay: Duration,
    /// Longest delay between rejoin attempts. Default 5 min.
    pub max_retry_delay: Duration,
    /// Failed rejoins, in a row, before the device can be provisioned
    /// again. Default 3.
    pub failures_before_reprovision: u32,
}

impl Default for KeepOnline {
    fn default() -> Self {
        Self {
            check_every: Duration::from_secs(10),
            first_retry_delay: Duration::from_secs(2),
            max_retry_delay: Duration::from_secs(300),
            failures_before_reprovision: 3,
        }
    }
}

impl KeepOnline {
    /// Set [`check_every`](Self::check_every).
    pub fn check_every(mut self, d: Duration) -> Self {
        self.check_every = d;
        self
    }

    /// Set [`first_retry_delay`](Self::first_retry_delay).
    pub fn first_retry_delay(mut self, d: Duration) -> Self {
        self.first_retry_delay = d;
        self
    }

    /// Set [`max_retry_delay`](Self::max_retry_delay).
    pub fn max_retry_delay(mut self, d: Duration) -> Self {
        self.max_retry_delay = d;
        self
    }

    /// Set [`failures_before_reprovision`](Self::failures_before_reprovision).
    pub fn failures_before_reprovision(mut self, n: u32) -> Self {
        self.failures_before_reprovision = n;
        self
    }

    fn retry_delay(&self, failures: u32) -> Duration {
        let factor = 1u32 << failures.saturating_sub(1).min(16);
        let delay = self.first_retry_delay * factor;
        if delay > self.max_retry_delay {
            self.max_retry_delay
        } else {
            delay
        }
    }
}

/// Join the saved network at boot, then keep the device on it, as the
/// [module documentation](self) describes. Never returns; run it in a task
/// of its own.
///
/// `W` must report connectivity through [`NetworkStatus`]; a backend that
/// doesn't is treated as always offline once provisioned, so prefer one
/// that does (such as `EspWifi` with a network stack).
///
/// Store errors at boot count as no saved credentials.
pub async fn keep_online<M, W, S, const R: usize>(
    share: &WifiShare<M, PersistOnJoin<W, S>, R>,
    policy: KeepOnline,
) -> !
where
    M: RawMutex,
    W: WifiConnect + NetworkStatus,
    S: CredentialStore,
{
    let restored = share.wifi().lock().await.restore().await;
    // Pretend the device was online, so the first pass tries to join at once.
    let mut failures = 0;
    let mut first = matches!(restored, Ok(Some(_)));
    loop {
        if !first {
            let wait = if failures == 0 {
                policy.check_every
            } else {
                policy.retry_delay(failures)
            };
            Timer::after(wait).await;
        }
        first = false;

        let saved = {
            let wifi = share.wifi().lock().await;
            if wifi.network_flags().await.is_online() {
                failures = 0;
                continue;
            }
            wifi.saved().cloned()
        };
        // Nothing saved: the device waits for a client.
        let Some(saved) = saved else {
            failures = 0;
            continue;
        };
        // On success the handle announces `Provisioned` to every driver.
        if share
            .handle()
            .connect(&saved.ssid, &saved.password)
            .await
            .is_ok()
        {
            failures = 0;
            continue;
        }
        failures = failures.saturating_add(1);
        if failures == policy.failures_before_reprovision {
            share.announce_unprovisioned();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_doubles_up_to_the_maximum() {
        let p = KeepOnline::default()
            .first_retry_delay(Duration::from_secs(2))
            .max_retry_delay(Duration::from_secs(20));
        let delays: [u64; 5] = core::array::from_fn(|i| p.retry_delay(i as u32 + 1).as_secs());
        assert_eq!(delays, [2, 4, 8, 16, 20]);
        assert_eq!(p.retry_delay(u32::MAX).as_secs(), 20);
    }
}
