//! `ButtonAuth` timing, against a real clock (embassy-time's std driver).

use std::{cell::Cell, rc::Rc};

use embassy_futures::{
    block_on,
    select::{select, Either},
};
use embassy_time::{Duration, Instant, Timer};
use embedded_hal::digital::ErrorType;
use embedded_hal_async::digital::Wait;
use impromptu_wifi_core::traits::{AuthEvent, AuthSource};
use impromptu_wifi_embassy::ButtonAuth;

/// A pin whose falling edges are queued by the test.
#[derive(Clone, Default)]
struct FakePin(Rc<Cell<u32>>);

impl FakePin {
    fn press(&self) {
        self.0.set(self.0.get() + 1);
    }
}

impl ErrorType for FakePin {
    type Error = core::convert::Infallible;
}

impl Wait for FakePin {
    async fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error> {
        while self.0.get() == 0 {
            Timer::after(Duration::from_millis(1)).await;
        }
        self.0.set(self.0.get() - 1);
        Ok(())
    }
    async fn wait_for_high(&mut self) -> Result<(), Self::Error> {
        unimplemented!()
    }
    async fn wait_for_low(&mut self) -> Result<(), Self::Error> {
        unimplemented!()
    }
    async fn wait_for_rising_edge(&mut self) -> Result<(), Self::Error> {
        unimplemented!()
    }
    async fn wait_for_any_edge(&mut self) -> Result<(), Self::Error> {
        unimplemented!()
    }
}

const TIMEOUT: Duration = Duration::from_millis(150);

#[test]
fn press_authorizes_then_timeout_revokes() {
    let pin = FakePin::default();
    let mut auth = ButtonAuth::active_low(pin.clone(), TIMEOUT);
    pin.press();
    assert_eq!(block_on(auth.next()), AuthEvent::Authorize);

    let start = Instant::now();
    assert_eq!(block_on(auth.next()), AuthEvent::Revoke);
    assert!(
        start.elapsed() >= Duration::from_millis(140),
        "revoked after {:?}",
        start.elapsed()
    );
    assert!(auth.deadline().is_none());
}

#[test]
fn deadline_survives_cancellation() {
    // The driver drops `next()` whenever a command arrives. The revoke must
    // still happen at the original deadline, not TIMEOUT after the last drop.
    let pin = FakePin::default();
    let mut auth = ButtonAuth::active_low(pin.clone(), TIMEOUT);
    pin.press();
    block_on(auth.next());
    let deadline = auth.deadline().unwrap();

    for _ in 0..5 {
        let cancelled = block_on(select(auth.next(), Timer::after(Duration::from_millis(20))));
        assert!(matches!(cancelled, Either::Second(())));
    }
    assert_eq!(block_on(auth.next()), AuthEvent::Revoke);
    let late = Instant::now().saturating_duration_since(deadline);
    assert!(
        late < Duration::from_millis(50),
        "revoked {late:?} after the deadline"
    );
}

#[test]
fn pressing_again_extends_the_window() {
    let pin = FakePin::default();
    let mut auth = ButtonAuth::active_low(pin.clone(), TIMEOUT);
    pin.press();
    block_on(auth.next());
    let first = auth.deadline().unwrap();

    block_on(Timer::after(Duration::from_millis(50)));
    pin.press();
    assert_eq!(block_on(auth.next()), AuthEvent::Authorize);
    assert!(auth.deadline().unwrap() > first);
}

#[test]
fn restart_timeout_pushes_the_deadline_back() {
    let pin = FakePin::default();
    let mut auth = ButtonAuth::active_low(pin.clone(), TIMEOUT);
    pin.press();
    block_on(auth.next());
    let first = auth.deadline().unwrap();

    block_on(Timer::after(Duration::from_millis(100)));
    auth.restart_timeout();
    let restarted = auth.deadline().unwrap();
    assert!(
        restarted >= first + Duration::from_millis(90),
        "deadline moved from {first:?} to {restarted:?}"
    );

    // The revoke now comes TIMEOUT after the restart, not after the press.
    let start = Instant::now();
    assert_eq!(block_on(auth.next()), AuthEvent::Revoke);
    assert!(
        start.elapsed() >= Duration::from_millis(140),
        "revoked after {:?}",
        start.elapsed()
    );
}

#[test]
fn restart_timeout_does_not_authorize() {
    let pin = FakePin::default();
    let mut auth = ButtonAuth::active_low(pin, TIMEOUT);
    auth.restart_timeout();
    assert!(auth.deadline().is_none(), "no press, no authorization");
}
