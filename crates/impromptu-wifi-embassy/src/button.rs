//! Button presses: short and long, and as a source for [`ButtonAuth`].
//!
//! A device with one button often uses it twice: a short press authorizes
//! BLE provisioning, a long press clears the saved network. Only one task
//! can own the pin, so a [`Button`] task tells the presses apart and passes
//! the short ones on through a [`Signal`]:
//!
//! ```ignore
//! static SHORT_PRESS: Signal<CriticalSectionRawMutex, ()> = Signal::new();
//!
//! #[embassy_executor::task]
//! async fn button_task(mut button: Button<Input<'static>>) -> ! {
//!     loop {
//!         match button.next().await {
//!             Press::Short => SHORT_PRESS.signal(()),
//!             Press::Long => factory_reset().await,
//!         }
//!     }
//! }
//!
//! let auth = ButtonAuth::new(&SHORT_PRESS, Duration::from_secs(60));
//! ```
//!
//! [`ButtonAuth`]: crate::ButtonAuth

use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use embedded_hal_async::digital::Wait;

/// Presses shorter than this are contact bounce, not presses.
pub const DEBOUNCE: Duration = Duration::from_millis(20);

/// How long a press lasted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Press {
    /// Released before the long-press threshold.
    Short,
    /// Held until the long-press threshold. Reported when the threshold is
    /// reached, before the release, so the device can react while the
    /// button is still down.
    Long,
}

/// A push button on a pin, telling short presses from long ones.
pub struct Button<B> {
    pin: B,
    active_low: bool,
    long: Duration,
    /// A long press was reported and the button may still be down.
    held: bool,
}

impl<B: Wait> Button<B> {
    /// A button that pulls the pin low when pressed (the usual wiring, with
    /// a pull-up). A press held for `long` is a [`Press::Long`].
    pub fn active_low(pin: B, long: Duration) -> Self {
        Self {
            pin,
            active_low: true,
            long,
            held: false,
        }
    }

    /// A button that drives the pin high when pressed.
    pub fn active_high(pin: B, long: Duration) -> Self {
        Self {
            pin,
            active_low: false,
            long,
            held: false,
        }
    }

    /// Wait for the next press.
    ///
    /// Not cancel-safe: a press in progress when the future is dropped is
    /// lost. Run it in a task of its own, as in the [module
    /// example](self).
    pub async fn next(&mut self) -> Press {
        loop {
            if core::mem::take(&mut self.held) {
                self.released().await;
            }
            self.pushed().await;
            let start = Instant::now();
            let long = self.long;
            match select(self.released(), Timer::after(long)).await {
                Either::First(()) if start.elapsed() >= DEBOUNCE => return Press::Short,
                Either::First(()) => continue,
                Either::Second(()) => {
                    self.held = true;
                    return Press::Long;
                }
            }
        }
    }

    async fn pushed(&mut self) {
        loop {
            let edge = if self.active_low {
                self.pin.wait_for_falling_edge().await
            } else {
                self.pin.wait_for_rising_edge().await
            };
            if edge.is_ok() {
                return;
            }
            pin_error_backoff().await;
        }
    }

    async fn released(&mut self) {
        loop {
            let level = if self.active_low {
                self.pin.wait_for_high().await
            } else {
                self.pin.wait_for_low().await
            };
            if level.is_ok() {
                return;
            }
            pin_error_backoff().await;
        }
    }
}

/// A pin error isn't a press. Back off and retry instead of spinning.
async fn pin_error_backoff() {
    Timer::after(Duration::from_millis(100)).await;
}

/// Where [`ButtonAuth`](crate::ButtonAuth) gets its presses.
pub trait PressSource {
    /// Wait for the next press. Must be cancel-safe: the driver races it
    /// against incoming commands.
    async fn pressed(&mut self);
}

/// The edges of a pin, as presses; what
/// [`ButtonAuth::active_low`](crate::ButtonAuth::active_low) and
/// [`active_high`](crate::ButtonAuth::active_high) use.
pub struct Edges<B> {
    pin: B,
    active_low: bool,
}

impl<B: Wait> Edges<B> {
    /// Falling edges are presses.
    pub fn active_low(pin: B) -> Self {
        Self {
            pin,
            active_low: true,
        }
    }

    /// Rising edges are presses.
    pub fn active_high(pin: B) -> Self {
        Self {
            pin,
            active_low: false,
        }
    }
}

impl<B: Wait> PressSource for Edges<B> {
    async fn pressed(&mut self) {
        loop {
            let edge = if self.active_low {
                self.pin.wait_for_falling_edge().await
            } else {
                self.pin.wait_for_rising_edge().await
            };
            if edge.is_ok() {
                return;
            }
            pin_error_backoff().await;
        }
    }
}

/// Each [`signal`](Signal::signal) is a press. Presses signalled while no
/// one waits collapse into one.
impl<M: RawMutex> PressSource for &Signal<M, ()> {
    async fn pressed(&mut self) {
        self.wait().await
    }
}
