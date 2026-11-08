use embassy_futures::select::{select, Either};
use embassy_time::{Duration, Instant, Timer};
use embedded_hal_async::digital::Wait;
use impromptu_wifi_core::traits::{AuthEvent, AuthSource};

/// Authorization from a physical button, revoked after a timeout.
///
/// A press yields [`AuthEvent::Authorize`] and (re)starts the timeout; when
/// it expires, [`AuthEvent::Revoke`] follows. The spec suggests about one
/// minute. Pressing again while authorized extends the window, and so do
/// the events the spec names (a failed Wi-Fi join, a hostname or
/// device-name write), through [`AuthSource::restart_timeout`].
///
/// The deadline is stored in the struct rather than in a timer future,
/// because the driver cancels [`next`](AuthSource::next) whenever a command
/// arrives. A plain `Timer::after` would restart on every command and never
/// expire.
///
/// No debouncing is done: repeated edges only re-authorize, which is
/// harmless. Presses that happen while `next` isn't being polled (during a
/// Wi-Fi join, say) depend on the HAL latching the edge.
pub struct ButtonAuth<B> {
    button: B,
    timeout: Duration,
    active_low: bool,
    deadline: Option<Instant>,
}

impl<B: Wait> ButtonAuth<B> {
    /// A button that pulls the pin low when pressed (the usual wiring, with
    /// a pull-up), with the given revoke timeout.
    pub fn active_low(button: B, timeout: Duration) -> Self {
        Self {
            button,
            timeout,
            active_low: true,
            deadline: None,
        }
    }

    /// A button that drives the pin high when pressed.
    pub fn active_high(button: B, timeout: Duration) -> Self {
        Self {
            button,
            timeout,
            active_low: false,
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

    async fn press(&mut self) {
        loop {
            let edge = if self.active_low {
                self.button.wait_for_falling_edge().await
            } else {
                self.button.wait_for_rising_edge().await
            };
            if edge.is_ok() {
                return;
            }
            // A pin error isn't a press. Back off and retry instead of spinning.
            Timer::after(Duration::from_millis(100)).await;
        }
    }
}

impl<B: Wait> AuthSource for ButtonAuth<B> {
    async fn next(&mut self) -> AuthEvent {
        let pressed = match self.deadline {
            None => {
                self.press().await;
                true
            }
            Some(deadline) => {
                let timer = Timer::at(deadline);
                matches!(select(self.press(), timer).await, Either::First(()))
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
