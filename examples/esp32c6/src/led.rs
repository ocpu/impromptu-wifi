//! The status LED: a WS2812 RGB LED driven by the RMT peripheral.
//!
//! On the ESP32-C6-DevKitC-1 the LED is on GPIO8.
//!
//! | Pattern | Meaning |
//! |---|---|
//! | Blue, blinking once a second | Unprovisioned: waiting for a client |
//! | Yellow, blinking four times a second | Joining a network |
//! | Green, steady | On a network |
//! | White, blinking fast for 5 s | Identify, requested by a BLE client |
//! | Cyan flash | BOOT pressed briefly: BLE writes authorized for 60 s |
//! | Red, steady | BOOT held: factory reset |

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Ticker};
use esp_hal::gpio::interconnect::PeripheralOutput;
use esp_hal::gpio::Level;
use esp_hal::peripherals::RMT;
use esp_hal::rmt::{Channel, PulseCode, Rmt, Tx, TxChannelConfig, TxChannelCreator};
use esp_hal::time::Rate;
use esp_hal::Async;
use impromptu_wifi::embassy::WifiStatus;

/// Something to show on top of the network status.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A BLE client asked the device to identify itself.
    Identify,
    /// BOOT was pressed briefly.
    Acknowledge,
    /// BOOT was held for a factory reset.
    Reset,
}

/// Events for the LED task. Several in one frame collapse into the last.
pub static EVENTS: Signal<CriticalSectionRawMutex, Event> = Signal::new();

const FRAME: Duration = Duration::from_millis(50);
const IDENTIFY_FOR: Duration = Duration::from_secs(5);
const ACKNOWLEDGE_FOR: Duration = Duration::from_millis(300);

/// A colour, kept dim: a WS2812 at full brightness is uncomfortable to
/// look at.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Rgb(u8, u8, u8);

const OFF: Rgb = Rgb(0, 0, 0);
const BLUE: Rgb = Rgb(0, 0, 24);
const YELLOW: Rgb = Rgb(20, 14, 0);
const GREEN: Rgb = Rgb(0, 16, 0);
const WHITE: Rgb = Rgb(24, 24, 24);
const CYAN: Rgb = Rgb(0, 20, 20);
const RED: Rgb = Rgb(32, 0, 0);

/// The order in which the LED takes its colour bytes.
#[derive(Clone, Copy)]
#[allow(dead_code, reason = "each board uses one of them")]
pub enum ColorOrder {
    /// Green, red, blue: the WS2812B datasheet order.
    Grb,
    /// Red, green, blue: some compatible LEDs, such as the one on an
    /// ESP32-S3 board this example was tested on.
    Rgb,
}

/// One WS2812 on one RMT channel.
pub struct Ws2812 {
    channel: Channel<'static, Async, Tx>,
    order: ColorOrder,
    /// Pulse lengths in RMT ticks: (high, low) for a 0 bit and a 1 bit.
    zero: (u16, u16),
    one: (u16, u16),
}

impl Ws2812 {
    /// Drive the LED on `pin` from RMT channel 0.
    pub fn new(rmt: RMT<'static>, pin: impl PeripheralOutput<'static>, order: ColorOrder) -> Self {
        let rmt = Rmt::new(rmt, Rate::from_mhz(80))
            .expect("an 80 MHz RMT clock")
            .into_async();
        let mhz = rmt.frequency().as_mhz();
        let ticks = |ns: u32| ((ns * mhz).div_ceil(1000)) as u16;
        let channel = rmt
            .channel0
            .configure_tx(
                &TxChannelConfig::default()
                    .with_clk_divider(1)
                    .with_idle_output(true)
                    .with_idle_output_level(Level::Low),
            )
            .expect("configuring RMT channel 0")
            .with_pin(pin);
        // WS2812B datasheet: T0H 0.4 µs, T0L 0.85 µs, T1H 0.8 µs, T1L 0.45 µs.
        Self {
            channel,
            order,
            zero: (ticks(400), ticks(850)),
            one: (ticks(800), ticks(450)),
        }
    }

    /// Show `colour`. The LED latches it after 50 µs of low output, which
    /// the next frame is far behind.
    pub async fn set(&mut self, Rgb(r, g, b): Rgb) {
        let mut codes = [PulseCode::end_marker(); 25];
        // Most significant bit first.
        let (first, second) = match self.order {
            ColorOrder::Grb => (g, r),
            ColorOrder::Rgb => (r, g),
        };
        let bits = u32::from(first) << 16 | u32::from(second) << 8 | u32::from(b);
        for (i, code) in codes[..24].iter_mut().enumerate() {
            let (high, low) = if bits & (1 << (23 - i)) != 0 {
                self.one
            } else {
                self.zero
            };
            *code = PulseCode::new(Level::High, high, Level::Low, low);
        }
        if let Err(e) = self.channel.transmit(&codes).await {
            log::warn!("status LED: {e:?}");
        }
    }
}

/// Show `status()` and the [`EVENTS`] on `led`, forever.
pub async fn run(mut led: Ws2812, status: impl Fn() -> WifiStatus) -> ! {
    let mut ticker = Ticker::every(FRAME);
    let mut overlay: Option<(Event, Instant)> = None;
    let mut shown = None;
    loop {
        let now = Instant::now();
        if let Some(event) = EVENTS.try_take() {
            let until = match event {
                Event::Identify => now + IDENTIFY_FOR,
                Event::Acknowledge => now + ACKNOWLEDGE_FOR,
                // Until the chip restarts.
                Event::Reset => Instant::MAX,
            };
            overlay = Some((event, until));
        }
        if overlay.is_some_and(|(_, until)| now >= until) {
            overlay = None;
        }

        let ms = now.as_millis();
        let blink = |period_ms: u64, on: Rgb| {
            if ms % period_ms < period_ms / 2 {
                on
            } else {
                OFF
            }
        };
        let colour = match overlay {
            Some((Event::Reset, _)) => RED,
            Some((Event::Identify, _)) => blink(200, WHITE),
            Some((Event::Acknowledge, _)) => CYAN,
            None => match status() {
                WifiStatus::Unprovisioned => blink(1000, BLUE),
                WifiStatus::Joining => blink(250, YELLOW),
                WifiStatus::Provisioned => GREEN,
            },
        };
        if shown != Some(colour) {
            led.set(colour).await;
            shown = Some(colour);
        }
        ticker.next().await;
    }
}
