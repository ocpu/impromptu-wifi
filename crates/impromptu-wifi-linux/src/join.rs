//! Keep several drivers in agreement about being provisioned.
//!
//! The device daemon serves serial and Bluetooth LE with one driver each.
//! When one of them provisions the machine, the other should report
//! `Provisioned` too, and a Bluetooth driver should stop. [`join_channel`]
//! connects them:
//!
//! ```ignore
//! let (joined, events) = join_channel();
//! let serial_wifi = NotifyOnJoin::new(nm.clone(), joined.notifier());
//! let ble_wifi = NotifyOnJoin::new(nm.clone(), joined.notifier());
//! serial_driver.run_with(&mut NoAuth, &mut events.clone()).await;
//! ```

use impromptu_wifi_core::traits::{DeviceEvent, EventSource, Url};
use tokio::sync::watch;

/// Announces successful joins. Cheap to clone.
#[derive(Clone)]
pub struct JoinSender(watch::Sender<Option<Option<Url>>>);

/// Receives joins as [`DeviceEvent::Provisioned`]. Each clone sees every
/// join made after the clone was taken.
#[derive(Clone)]
pub struct JoinEvents(watch::Receiver<Option<Option<Url>>>);

/// A connected sender and receiver.
pub fn join_channel() -> (JoinSender, JoinEvents) {
    let (tx, rx) = watch::channel(None);
    (JoinSender(tx), JoinEvents(rx))
}

impl JoinSender {
    /// Announce that the device joined a network, reachable at `url`.
    pub fn send(&self, url: &Option<Url>) {
        self.0.send_replace(Some(url.clone()));
    }

    /// A callback for
    /// [`NotifyOnJoin`](impromptu_wifi_core::NotifyOnJoin).
    pub fn notifier(&self) -> impl FnMut(&Option<Url>) + Send + Sync + 'static {
        let this = self.clone();
        move |url| this.send(url)
    }

    /// The last announced join, if any.
    pub fn last(&self) -> Option<Option<Url>> {
        self.0.borrow().clone()
    }

    /// A new receiver.
    pub fn subscribe(&self) -> JoinEvents {
        JoinEvents(self.0.subscribe())
    }
}

impl EventSource for JoinEvents {
    async fn next(&mut self) -> DeviceEvent {
        loop {
            // Cancel-safe: `changed` only marks the value seen when it
            // returns, so an interrupted wait loses nothing.
            if self.0.changed().await.is_err() {
                // Every sender is gone: no more joins can happen.
                core::future::pending::<()>().await;
            }
            if let Some(url) = self.0.borrow_and_update().clone() {
                return DeviceEvent::Provisioned(url);
            }
        }
    }
}
