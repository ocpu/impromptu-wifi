//! `keep_online`, `Button` and the `Unprovisioned` announcement, against a
//! real clock (embassy-time's std driver).

use std::cell::Cell;
use std::rc::Rc;

use embassy_futures::block_on;
use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use embedded_hal::digital::ErrorType;
use embedded_hal_async::digital::Wait;
use impromptu_wifi_core::traits::{
    unsupported, AuthEvent, AuthSource, ConnectError, CredentialStore, Credentials, DeviceEvent,
    EventSource, NetworkFlags, NetworkStatus, Url, WifiConnect,
};
use impromptu_wifi_embassy::{
    keep_online, Button, ButtonAuth, KeepOnline, PersistOnJoin, Press, WifiShare,
};

// ---------------------------------------------------------------------------
// keep_online
// ---------------------------------------------------------------------------

/// What the test controls: whether the access point is there, and whether
/// the device is online.
#[derive(Clone, Default)]
struct World {
    ap_up: Rc<Cell<bool>>,
    online: Rc<Cell<bool>>,
    joins: Rc<Cell<u32>>,
}

struct Radio(World);

impl WifiConnect for Radio {
    async fn connect(&mut self, _: &str, _: &str) -> Result<Option<Url>, ConnectError> {
        self.0.joins.set(self.0.joins.get() + 1);
        if self.0.ap_up.get() {
            self.0.online.set(true);
            Ok(Some(Url::try_from("http://10.0.0.2").unwrap()))
        } else {
            Err(ConnectError::NetworkNotFound)
        }
    }
}

impl NetworkStatus for Radio {
    async fn network_flags(&self) -> NetworkFlags {
        NetworkFlags(if self.0.online.get() {
            NetworkFlags::ONLINE
        } else {
            0
        })
    }
}

unsupported!(Radio: WifiScanner);

#[derive(Default)]
struct Store {
    held: Option<Credentials>,
    saves: Rc<Cell<u32>>,
}

impl CredentialStore for Store {
    type Error = ();
    async fn load(&mut self) -> Result<Option<Credentials>, ()> {
        Ok(self.held.clone())
    }
    async fn save(&mut self, c: &Credentials) -> Result<(), ()> {
        self.saves.set(self.saves.get() + 1);
        self.held = Some(c.clone());
        Ok(())
    }
    async fn clear(&mut self) -> Result<(), ()> {
        self.held = None;
        Ok(())
    }
}

type Share = WifiShare<NoopRawMutex, PersistOnJoin<Radio, Store>, 1>;

fn fast() -> KeepOnline {
    KeepOnline::default()
        .check_every(Duration::from_millis(5))
        .first_retry_delay(Duration::from_millis(5))
        .max_retry_delay(Duration::from_millis(10))
        .failures_before_reprovision(2)
}

/// Run `keep_online` until `test` finishes, failing after a second.
fn with_keep_online<T>(share: &Share, test: impl core::future::Future<Output = T>) -> T {
    block_on(async {
        let deadline = Timer::after(Duration::from_secs(1));
        match select(keep_online(share, fast()), select(test, deadline)).await {
            Either::Second(Either::First(t)) => t,
            Either::Second(Either::Second(())) => panic!("timed out"),
        }
    })
}

#[test]
fn boot_rejoins_the_saved_network_without_rewriting_it() {
    let world = World::default();
    world.ap_up.set(true);
    let saves = Rc::new(Cell::new(0));
    let store = Store {
        held: Credentials::new("home", "pw"),
        saves: saves.clone(),
    };
    let share = Share::new(PersistOnJoin::new(Radio(world.clone()), store));
    let mut events = share.events().unwrap();

    let event = with_keep_online(&share, events.next());
    assert_eq!(
        event,
        DeviceEvent::Provisioned(Some(Url::try_from("http://10.0.0.2").unwrap()))
    );
    assert!(share.is_provisioned());
    assert_eq!(saves.get(), 0);
}

#[test]
fn nothing_saved_means_no_joins() {
    let world = World::default();
    world.ap_up.set(true);
    let share = Share::new(PersistOnJoin::new(Radio(world.clone()), Store::default()));
    with_keep_online(&share, Timer::after(Duration::from_millis(50)));
    assert_eq!(world.joins.get(), 0);
    assert!(!share.is_provisioned());
}

#[test]
fn a_lost_network_reopens_provisioning_until_it_returns() {
    let world = World::default();
    world.ap_up.set(true);
    let store = Store {
        held: Credentials::new("home", "pw"),
        ..Store::default()
    };
    let share = Share::new(PersistOnJoin::new(Radio(world.clone()), store));
    let mut events = share.events().unwrap();

    with_keep_online(&share, async {
        assert!(matches!(events.next().await, DeviceEvent::Provisioned(_)));

        // The access point goes away.
        world.ap_up.set(false);
        world.online.set(false);
        assert_eq!(events.next().await, DeviceEvent::Unprovisioned);
        assert!(world.joins.get() >= 3, "boot, then two failed rejoins");

        // It comes back; a background rejoin provisions the device again.
        world.ap_up.set(true);
        assert!(matches!(events.next().await, DeviceEvent::Provisioned(_)));
    });
}

// ---------------------------------------------------------------------------
// Button
// ---------------------------------------------------------------------------

/// An active-low button: `true` while pressed.
#[derive(Clone, Default)]
struct Pin {
    down: Rc<Cell<bool>>,
    edges: Rc<Cell<u32>>,
}

impl Pin {
    fn push(&self) {
        self.down.set(true);
        self.edges.set(self.edges.get() + 1);
    }
    fn release(&self) {
        self.down.set(false);
    }
}

impl ErrorType for Pin {
    type Error = core::convert::Infallible;
}

impl Wait for Pin {
    async fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error> {
        while self.edges.get() == 0 {
            Timer::after(Duration::from_millis(1)).await;
        }
        self.edges.set(self.edges.get() - 1);
        Ok(())
    }
    async fn wait_for_high(&mut self) -> Result<(), Self::Error> {
        while self.down.get() {
            Timer::after(Duration::from_millis(1)).await;
        }
        Ok(())
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

/// Hold the button for `ms`, from a concurrent future.
async fn hold(pin: &Pin, ms: u64) {
    pin.push();
    Timer::after(Duration::from_millis(ms)).await;
    pin.release();
}

#[test]
fn button_tells_short_from_long_and_ignores_bounce() {
    let pin = Pin::default();
    let mut button = Button::active_low(pin.clone(), Duration::from_millis(100));
    block_on(async {
        let (press, ()) = embassy_futures::join::join(button.next(), hold(&pin, 30)).await;
        assert_eq!(press, Press::Short);

        // A 2 ms bounce is not a press; the real one after it is.
        let script = async {
            hold(&pin, 2).await;
            Timer::after(Duration::from_millis(5)).await;
            hold(&pin, 30).await;
        };
        let (press, ()) = embassy_futures::join::join(button.next(), script).await;
        assert_eq!(press, Press::Short);

        // A long press is reported while the button is still down.
        pin.push();
        assert_eq!(button.next().await, Press::Long);
        assert!(pin.down.get());
        // Releasing it is not another press.
        let script = async {
            Timer::after(Duration::from_millis(20)).await;
            pin.release();
            Timer::after(Duration::from_millis(20)).await;
            hold(&pin, 30).await;
        };
        let (press, ()) = embassy_futures::join::join(button.next(), script).await;
        assert_eq!(press, Press::Short);
    });
}

#[test]
fn button_auth_takes_presses_from_a_signal() {
    let presses = Signal::<NoopRawMutex, ()>::new();
    let mut auth = ButtonAuth::new(&presses, Duration::from_millis(50));
    presses.signal(());
    assert_eq!(block_on(auth.next()), AuthEvent::Authorize);
    assert!(auth.is_authorized());
    assert_eq!(block_on(auth.next()), AuthEvent::Revoke);
}

#[test]
fn unprovisioned_is_announced_to_every_driver() {
    let world = World::default();
    let share = WifiShare::<NoopRawMutex, _, 2>::new(Radio(world.clone()));
    let mut a = share.events().unwrap();
    let mut b = share.events().unwrap();
    world.ap_up.set(true);
    block_on(share.handle().connect("home", "pw")).unwrap();
    assert!(share.is_provisioned());
    share.announce_unprovisioned();
    assert!(!share.is_provisioned());
    // Only the latest announcement is kept.
    assert_eq!(block_on(a.next()), DeviceEvent::Unprovisioned);
    assert_eq!(block_on(b.next()), DeviceEvent::Unprovisioned);
}
