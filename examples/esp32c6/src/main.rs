//! Improv Wi-Fi on an ESP32-C6, over USB serial and Bluetooth LE at once.
//!
//! * **Serial:** the chip's USB-Serial-JTAG port, as ESP Web Tools and the
//!   `impromptu-wifi` CLI expect. No authorization, as the serial spec
//!   defines it.
//! * **Bluetooth LE:** the Improv Wi-Fi GATT service through trouble-host.
//!   Writes need a press of the BOOT button (GPIO9), valid for 60 seconds.
//!   Advertising stops once the device is provisioned, as the spec asks.
//! * **Logs:** UART0, so they never interleave with serial frames.
//!
//! Both transports share one Wi-Fi controller through
//! [`SharedWifi`]. Credentials are not saved: after a reset the device
//! asks to be provisioned again.

#![no_std]
#![no_main]

use bt_hci::controller::ExternalController;
use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_net::{Runner, StackResources};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::rng::Rng;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb::usb_serial_jtag::UsbSerialJtag;
use esp_hal::Async;
use esp_radio::ble::controller::BleConnector;
use esp_radio::wifi::{Interface, WifiController};
use impromptu_wifi::ble::{capabilities, BleLink};
use impromptu_wifi::device::{
    unsupported, AuthEvent, AuthSource, DeviceInfoSource, Driver, NoAuth,
};
use impromptu_wifi::embassy::esp::EspWifi;
use impromptu_wifi::embassy::trouble::{
    serve, ImprovWifiBridge, ImprovWifiService, TroubleGattServer,
};
use impromptu_wifi::embassy::{ButtonAuth, SharedWifi};
use impromptu_wifi::proto::ble::{advertising_data, Capabilities};
use impromptu_wifi::proto::State;
use impromptu_wifi::serial::SerialLink;
use log::{info, warn};
use static_cell::StaticCell;
use trouble_host::prelude::*;

esp_bootloader_esp_idf::esp_app_desc!();

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    log::error!("{info}");
    loop {}
}

extern crate alloc;

type Radio = Mutex<CriticalSectionRawMutex, EspWifi<'static, 'static>>;
type Wifi = SharedWifi<'static, CriticalSectionRawMutex, EspWifi<'static, 'static>>;

/// How long a BOOT-button press authorizes BLE writes; the spec suggests
/// about a minute.
const AUTH_TIMEOUT: Duration = Duration::from_secs(60);

/// What this firmware tells clients about itself.
#[derive(Clone, Copy)]
struct Board;

impl DeviceInfoSource for Board {
    fn firmware_name(&self) -> &str {
        env!("CARGO_PKG_NAME")
    }
    fn firmware_version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }
    fn chip(&self) -> &str {
        "ESP32-C6"
    }
    fn device_name(&self) -> &str {
        "Impromptu Wi-Fi demo"
    }
}

unsupported!(Board: Identify, HostnameProvider);

const CAPS: Capabilities = capabilities::<Wifi, Board>();

#[gatt_server]
struct Server {
    improv_wifi: ImprovWifiService,
}

static BRIDGE: ImprovWifiBridge<CriticalSectionRawMutex> = ImprovWifiBridge::new();

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);
    // Wi-Fi and BLE together (coex) need more heap.
    esp_alloc::heap_allocator!(size: 64 * 1024);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    // Wi-Fi and the IP stack. EspWifi waits for a DHCP address after a join
    // and reports http://<address> as the redirect URL.
    let controller = WifiController::new(peripherals.WIFI, Default::default())
        .expect("starting the Wi-Fi controller");
    static NET: StaticCell<StackResources<3>> = StaticCell::new();
    let seed = u64::from(Rng::new().random()) << 32 | u64::from(Rng::new().random());
    let (stack, runner) = embassy_net::new(
        Interface::station(),
        embassy_net::Config::dhcpv4(Default::default()),
        NET.init(StackResources::new()),
        seed,
    );
    spawner.spawn(net_task(runner).expect("spawning the network task"));

    static RADIO: StaticCell<Radio> = StaticCell::new();
    let radio: &'static Radio = RADIO.init(Mutex::new(EspWifi::new(controller).with_stack(stack)));

    // Serial over USB-Serial-JTAG.
    let jtag = UsbSerialJtag::new(peripherals.USB_DEVICE).into_async();
    spawner.spawn(serial_task(jtag, SharedWifi::new(radio)).expect("spawning the serial task"));

    // Bluetooth LE.
    let transport = BleConnector::new(peripherals.BT, Default::default())
        .expect("starting the Bluetooth controller");
    let button = Input::new(
        peripherals.GPIO9,
        InputConfig::default().with_pull(Pull::Up),
    );
    ble(
        ExternalController::<_, 1>::new(transport),
        button,
        SharedWifi::new(radio),
    )
    .await;

    // Provisioned over BLE: Bluetooth is off now, serial keeps running.
    loop {
        Timer::after(Duration::from_secs(3600)).await;
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Interface>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn serial_task(jtag: UsbSerialJtag<'static, Async>, wifi: Wifi) -> ! {
    let mut driver = Driver::new(SerialLink::new(jtag), wifi, Board, false);
    loop {
        // `run` only returns when the link fails, for example when the USB
        // host goes away; the driver keeps its state for the next session.
        let Err(e) = driver.run(&mut NoAuth).await;
        warn!("serial link: {e:?}");
        Timer::after(Duration::from_millis(500)).await;
    }
}

/// Advertise and serve Improv Wi-Fi over BLE until the device is
/// provisioned.
async fn ble<C: Controller>(controller: C, button: Input<'static>, mut wifi: Wifi) {
    let mut resources: HostResources<C, DefaultPacketPool, 1, 1> = HostResources::new();
    let stack = trouble_host::new(controller, &mut resources).build();
    let mut peripheral = stack.peripheral();
    let mut runner = stack.runner();
    let server = Server::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name: "Impromptu",
        appearance: &appearance::UNKNOWN,
    }))
    .expect("building the GATT server");
    let mut auth = ButtonAuth::active_low(button, AUTH_TIMEOUT);

    let session = async {
        loop {
            // Advertise the current state, and advertise again whenever the
            // button changes it, as the spec asks.
            let state = if auth.is_authorized() {
                State::Authorized
            } else {
                State::AuthorizationRequired
            };
            let adv = advertising_data(state, CAPS);
            let advertiser = match peripheral
                .advertise(
                    &AdvertisementParameters::default(),
                    Advertisement::ConnectableScannableUndirected {
                        adv_data: &adv,
                        scan_data: &[],
                    },
                )
                .await
            {
                Ok(a) => a,
                Err(e) => {
                    warn!("advertising: {e:?}");
                    Timer::after(Duration::from_secs(1)).await;
                    continue;
                }
            };
            let conn = match select(advertiser.accept(), auth.next()).await {
                Either::First(Ok(conn)) => conn,
                Either::First(Err(e)) => {
                    warn!("accepting a connection: {e:?}");
                    continue;
                }
                Either::Second(AuthEvent::Authorize | AuthEvent::Revoke) => continue,
            };
            let Ok(conn) = conn.with_attribute_server(&server) else {
                continue;
            };
            info!("Bluetooth client connected");

            let Ok(gatt) = TroubleGattServer::new(&conn, &server.improv_wifi, &BRIDGE, CAPS) else {
                continue;
            };
            let mut driver = Driver::new(BleLink::new(gatt, CAPS), &mut wifi, Board, true);
            if auth.is_authorized() {
                // The button was pressed before the client connected.
                driver.machine_mut().authorize();
            }
            let provisioned = match select(
                serve(&conn, &server.improv_wifi, &BRIDGE),
                driver.provision(&mut auth),
            )
            .await
            {
                Either::First(()) => None,
                Either::Second(Ok(url)) => Some(url),
                Either::Second(Err(e)) => {
                    warn!("Bluetooth session: {e:?}");
                    None
                }
            };
            drop(driver);
            if let Some(url) = provisioned {
                info!("provisioned over Bluetooth; redirect URL {url:?}");
                // Let the client read the result, then stop.
                let _ = select(
                    serve(&conn, &server.improv_wifi, &BRIDGE),
                    Timer::after(Duration::from_secs(5)),
                )
                .await;
                return;
            }
            info!("Bluetooth client disconnected");
        }
    };
    match select(runner.run(), session).await {
        Either::First(Err(e)) => warn!("Bluetooth host stopped: {e:?}"),
        Either::First(Ok(())) | Either::Second(()) => {}
    }
}
