use embassy_futures::select::{select, Either};
use embassy_time::{Duration, Instant, Timer};
use embedded_hal_async::digital::Wait;
use impromptu_wifi_core::traits::{AuthEvent, AuthSource};

use crate::button::{Edges, PressSource};

/// Authorization from a physical button, revoked after a timeout.
///
/// A press yields [`AuthEvent::Authorize`] and (re)starts the timeout; when
/// it expires, [`AuthEvent::Revoke`] follows. The spec suggests about one
/// minute. Pressing again while authorized extends the window, and so do
/// the events the spec names (a failed Wi-Fi join, a hostname or
/// device-name write), through [`AuthSource::restart_timeout`].
///
/// The presses come from a [`PressSource`]: a pin directly
/// ([`active_low`](Self::active_low), [`active_high`](Self::active_high)),
/// or the short presses of a [`Button`](crate::Button) task through a
/// [`Signal`](embassy_sync::signal::Signal), when the same button also does
/// something else, such as a factory reset on a long press:
///
/// ```ignore
/// static SHORT_PRESS: Signal<CriticalSectionRawMutex, ()> = Signal::new();
/// let mut auth = ButtonAuth::new(&SHORT_PRESS, Duration::from_secs(60));
/// ```
///
/// The deadline is stored in the struct rather than in a timer future,
/// because the driver cancels [`next`](AuthSource::next) whenever a command
/// arrives. A plain `Timer::after` would restart on every command and never
/// expire.
///
/// A pin used directly is not debounced: repeated edges only re-authorize,
/// which is harmless. Presses that happen while `next` isn't being polled
/// (during a Wi-Fi join, say) depend on the HAL latching the edge.
pub struct ButtonAuth<P> {
    presses: P,
    timeout: Duration,
    deadline: Option<Instant>,
}

impl<B: Wait> ButtonAuth<Edges<B>> {
    /// A button that pulls the pin low when pressed (the usual wiring, with
    /// a pull-up), with the given revoke timeout.
    pub fn active_low(button: B, timeout: Duration) -> Self {
        Self::new(Edges::active_low(button), timeout)
    }

    /// A button that drives the pin high when pressed.
    pub fn active_high(button: B, timeout: Duration) -> Self {
        Self::new(Edges::active_high(button), timeout)
    }
}

impl<P: PressSource> ButtonAuth<P> {
    /// Authorize on each press from `presses`, for `timeout`.
    pub fn new(presses: P, timeout: Duration) -> Self {
        Self {
            presses,
            timeout,
            deadline: None,
        }
    }

    /// When authorization will be revoked, if currently authorized.
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Whether a press is still in effect. A BLE application that creates a
    /// driver per connection uses this to carry an authorization from before
    /// the connection into the new driver's
    /// [`Machine::authorize`](impromptu_wifi_core::Machine::authorize).
    pub fn is_authorized(&self) -> bool {
        self.deadline.is_some_and(|d| d > Instant::now())
    }
}

impl<P: PressSource> AuthSource for ButtonAuth<P> {
    async fn next(&mut self) -> AuthEvent {
        let pressed = match self.deadline {
            None => {
                self.presses.pressed().await;
                true
            }
            Some(deadline) => {
                let timer = Timer::at(deadline);
                matches!(
                    select(self.presses.pressed(), timer).await,
                    Either::First(())
                )
            }
        };
        if pressed {
            self.deadline = Some(Instant::now() + self.timeout);
            AuthEvent::Authorize
        } else {
            self.deadline = None;
            AuthEvent::Revoke
        }
    }

    fn restart_timeout(&mut self) {
        if self.deadline.is_some() {
            self.deadline = Some(Instant::now() + self.timeout);
        }
    }
}
