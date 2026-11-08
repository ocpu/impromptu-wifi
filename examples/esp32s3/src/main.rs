//! Improv Wi-Fi on an ESP32-S3, over USB serial and Bluetooth LE at once,
//! with the rest of a device's lifecycle around it.
//!
//! * **Serial:** the chip's USB-Serial-JTAG port, as ESP Web Tools and the
//!   `impromptu-wifi` CLI expect. No authorization, as the serial spec
//!   defines it.
//! * **Bluetooth LE:** the Improv Wi-Fi GATT service through trouble-host.
//!   Writes need a short press of the BOOT button (GPIO0), valid for 60
//!   seconds. Advertising stops while the device is provisioned, as the spec
//!   asks.
//! * **Saved network:** each successful join is saved to the first sector of
//!   the `nvs` partition. At boot the device rejoins it.
//! * **Lost network:** the device rejoins with backoff. After three failed
//!   attempts it can be provisioned again over either transport, while it
//!   keeps trying in the background.
//! * **Factory reset:** hold BOOT for 5 seconds. The saved network is erased
//!   and the chip restarts unprovisioned.
//! * **Status LED:** the WS2812 on GPIO38 shows whether the device is waiting,
//!   joining or online, and blinks white when a BLE client sends Identify.
//!   See the `led` module for the patterns.
//! * **Web page:** `http://<address>/`, the redirect URL clients open after
//!   provisioning, answers with an "it works" page. See the `web` module.
//! * **Logs:** UART0, so they never interleave with serial frames.
//!
//! Both transports share one Wi-Fi controller through a [`WifiShare`], which
//! also tells each transport about joins made through the other, and about
//! a lost network.

#![no_std]
#![no_main]

mod led;
mod web;

use bt_hci::controller::ExternalController;
use embassy_executor::Spawner;
use embassy_futures::select::{select, select3, Either, Either3};
use embassy_net::{Runner, Stack, StackResources};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use esp_bootloader_esp_idf::partitions::{
    read_partition_table, DataPartitionSubType, PartitionType, PARTITION_TABLE_MAX_LEN,
};
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::rng::Rng;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb::usb_serial_jtag::UsbSerialJtag;
use esp_hal::Async;
use esp_radio::ble::controller::BleConnector;
use esp_radio::wifi::{Interface, WifiController};
use esp_storage::FlashStorage;
use impromptu_wifi::ble::{capabilities, BleLink};
use impromptu_wifi::device::{
    unsupported, AuthEvent, AuthSource, DeviceEvent, DeviceInfoSource, Driver, EventSource,
    Identify, NoAuth,
};
use impromptu_wifi::embassy::esp::EspWifi;
use impromptu_wifi::embassy::flash::FlashStore;
use impromptu_wifi::embassy::trouble::{
    add_gap_and_gatt, serve, ImprovWifiBridge, ImprovWifiService, TroubleGattServer,
    GAP_GATT_ATTRIBUTE_COUNT,
};
use impromptu_wifi::embassy::{
    keep_online, Button, ButtonAuth, JoinEvents, KeepOnline, PersistOnJoin, Press, SharedWifi,
    WifiShare,
};
use impromptu_wifi::proto::ble::{
    advertising_data, scan_response_data, Capabilities, ADVERTISING_DATA_LEN,
};
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

/// The Wi-Fi controller, saving each join to flash.
type Backend = PersistOnJoin<EspWifi<'static, 'static>, FlashStore<FlashStorage<'static>>>;
/// The backend, shared by the two transports (two event receivers).
type Radio = WifiShare<CriticalSectionRawMutex, Backend, 2>;
type Wifi = SharedWifi<'static, CriticalSectionRawMutex, Backend, 2>;
type Joins = JoinEvents<'static, CriticalSectionRawMutex, 2>;

/// How long a BOOT-button press authorizes BLE writes; the spec suggests
/// about a minute.
const AUTH_TIMEOUT: Duration = Duration::from_secs(60);

/// The Bluetooth name, sent in the scan response: the advertisement itself
/// is full.
const BLE_NAME: &str = "Impromptu";

/// How long BOOT must be held for a factory reset.
const RESET_HOLD: Duration = Duration::from_secs(5);

/// Short presses of BOOT, from the button task to the BLE authorization.
static SHORT_PRESS: Signal<CriticalSectionRawMutex, ()> = Signal::new();

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
        "ESP32-S3"
    }
    fn device_name(&self) -> &str {
        "Impromptu Wi-Fi demo"
    }
}

/// Identify blinks the status LED, in the LED task: the driver doesn't
/// wait for it.
impl Identify for Board {
    async fn identify(&mut self) {
        led::EVENTS.signal(led::Event::Identify);
    }
}

unsupported!(Board: HostnameProvider);

/// The device object, `static` so the web page can borrow its strings.
static BOARD: Board = Board;

const CAPS: Capabilities = capabilities::<Wifi, Board>();

/// The GAP and GATT services come from `add_gap_and_gatt`, whose GATT service
/// carries a Database Hash, so clients that cache the layout notice when a
/// firmware update changes it.
#[gatt_server(attribute_table_size = GAP_GATT_ATTRIBUTE_COUNT + ImprovWifiService::ATTRIBUTE_COUNT)]
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

    // The saved network lives in the first sector of the `nvs` partition,
    // which nothing else in this firmware uses.
    let mut flash = FlashStorage::new(peripherals.FLASH);
    let offset = nvs_offset(&mut flash).expect("an nvs partition in the partition table");
    let store = FlashStore::new(flash, offset).expect("the nvs partition starts on a sector");

    // Wi-Fi and the IP stack. EspWifi waits for a DHCP address after a join
    // and reports http://<address> as the redirect URL.
    let controller = WifiController::new(peripherals.WIFI, Default::default())
        .expect("starting the Wi-Fi controller");
    // One socket for DHCP, one per web server task.
    static NET: StaticCell<StackResources<{ 1 + web::SOCKETS }>> = StaticCell::new();
    let seed = u64::from(Rng::new().random()) << 32 | u64::from(Rng::new().random());
    let (stack, runner) = embassy_net::new(
        Interface::station(),
        embassy_net::Config::dhcpv4(Default::default()),
        NET.init(StackResources::new()),
        seed,
    );
    spawner.spawn(net_task(runner).expect("spawning the network task"));
    for _ in 0..web::SOCKETS {
        spawner.spawn(web_task(stack).expect("spawning a web task"));
    }

    static RADIO: StaticCell<Radio> = StaticCell::new();
    let radio: &'static Radio = RADIO.init(WifiShare::new(PersistOnJoin::new(
        EspWifi::new(controller).with_stack(stack),
        store,
    )));
    let joins = || radio.events().expect("one receiver per transport");

    // Rejoin the saved network now and whenever it drops.
    spawner.spawn(keep_online_task(radio).expect("spawning the keep-online task"));

    // The status LED.
    // The LED on the board this was tested on takes red first; with
    // `Grb`, green shows as red.
    let ws2812 = led::Ws2812::new(peripherals.RMT, peripherals.GPIO38, led::ColorOrder::Rgb);
    spawner.spawn(led_task(ws2812, radio).expect("spawning the LED task"));

    // BOOT: a short press authorizes BLE, a long one resets.
    let button = Input::new(
        peripherals.GPIO0,
        InputConfig::default().with_pull(Pull::Up),
    );
    spawner.spawn(
        button_task(Button::active_low(button, RESET_HOLD), radio)
            .expect("spawning the button task"),
    );

    // Serial over USB-Serial-JTAG.
    let jtag = UsbSerialJtag::new(peripherals.USB_DEVICE).into_async();
    spawner.spawn(serial_task(jtag, radio.handle(), joins()).expect("spawning the serial task"));

    // Bluetooth LE.
    let transport = BleConnector::new(peripherals.BT, Default::default())
        .expect("starting the Bluetooth controller");
    ble(ExternalController::<_, 1>::new(transport), radio, joins()).await;

    // The Bluetooth host stopped; serial keeps running.
    loop {
        Timer::after(Duration::from_secs(3600)).await;
    }
}

/// A random static Bluetooth address derived from the chip's factory MAC:
/// the same on every boot, and different from the address other firmware on
/// this chip used.
///
/// Clients cache a device's GATT layout by address, and this firmware has no
/// Service Changed or Database Hash characteristic to invalidate such a
/// cache. With the chip's public address, an ESPHome Bluetooth proxy that
/// had seen earlier firmware on the chip kept using that firmware's layout.
fn ble_address() -> Address {
    let mac = esp_hal::efuse::base_mac_address();
    // Least significant byte first, as the controller takes it.
    let mut addr = [0u8; 6];
    for (dst, src) in addr.iter_mut().zip(mac.as_bytes().iter().rev()) {
        *dst = *src;
    }
    // A static random address has its two most significant bits set
    // (Bluetooth Core Specification, Vol 6, Part B, 1.3.2.1).
    addr[5] |= 0xC0;
    Address::random(addr)
}

/// Where the `nvs` partition starts, from the partition table in flash.
fn nvs_offset(flash: &mut FlashStorage<'_>) -> Option<u32> {
    let mut buf = [0u8; PARTITION_TABLE_MAX_LEN];
    let table = read_partition_table(flash, &mut buf).ok()?;
    let nvs = table
        .find_partition(PartitionType::Data(DataPartitionSubType::Nvs))
        .ok()??;
    Some(nvs.offset())
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Interface>) -> ! {
    runner.run().await
}

#[embassy_executor::task(pool_size = web::SOCKETS)]
async fn web_task(stack: Stack<'static>) -> ! {
    web::serve(stack, web::About::of(&BOARD)).await
}

#[embassy_executor::task]
async fn led_task(ws2812: led::Ws2812, radio: &'static Radio) -> ! {
    led::run(ws2812, || radio.status()).await
}

#[embassy_executor::task]
async fn keep_online_task(radio: &'static Radio) -> ! {
    keep_online(radio, KeepOnline::default()).await
}

#[embassy_executor::task]
async fn button_task(mut button: Button<Input<'static>>, radio: &'static Radio) -> ! {
    loop {
        match button.next().await {
            Press::Short => {
                SHORT_PRESS.signal(());
                led::EVENTS.signal(led::Event::Acknowledge);
            }
            Press::Long => {
                info!("factory reset: erasing the saved network");
                led::EVENTS.signal(led::Event::Reset);
                // Let the LED task show it before the flash erase blocks.
                Timer::after(Duration::from_millis(100)).await;
                // Waits for a join in progress to finish.
                let mut wifi = radio.wifi().lock().await;
                if let Err(e) = wifi.forget().await {
                    warn!("erasing the saved network: {e:?}");
                }
                esp_hal::system::software_reset();
            }
        }
    }
}

#[embassy_executor::task]
async fn serial_task(jtag: UsbSerialJtag<'static, Async>, wifi: Wifi, mut joins: Joins) -> ! {
    let mut driver = Driver::new(SerialLink::new(jtag), wifi, Board, false);
    loop {
        // `run_with` only returns when the link fails, for example when the
        // USB host goes away; the driver keeps its state for the next
        // session. Joins made over BLE or at boot reach serial clients as
        // Provisioned, a lost network as Authorized.
        let Err(e) = driver.run_with(&mut NoAuth, &mut joins).await;
        warn!("serial link: {e:?}");
        Timer::after(Duration::from_millis(500)).await;
    }
}

/// Advertise and serve Improv Wi-Fi over BLE while the device is not
/// provisioned. Returns only if the Bluetooth host fails.
async fn ble<C: Controller>(controller: C, radio: &'static Radio, mut joins: Joins) {
    let mut wifi: Wifi = radio.handle();
    let mut resources: HostResources<C, DefaultPacketPool, 1, 1> = HostResources::new();
    let stack = trouble_host::new(controller, &mut resources)
        .set_random_address(ble_address())
        .build();
    let mut peripheral = stack.peripheral();
    let mut runner = stack.runner();
    let mut table = AttributeTable::new();
    let hash = add_gap_and_gatt(&mut table, BLE_NAME, &appearance::UNKNOWN);
    let server = Server::new(table);
    hash.update(server.table())
        .expect("storing the GATT database hash");
    let mut auth = ButtonAuth::new(&SHORT_PRESS, AUTH_TIMEOUT);
    let mut scan_buf = [0; ADVERTISING_DATA_LEN];
    let scan_data = scan_response_data(BLE_NAME, &mut scan_buf);

    let session = async {
        let mut provisioned = false;
        loop {
            if provisioned {
                // Quiet until the device loses its network for good.
                while joins.next().await != DeviceEvent::Unprovisioned {}
                info!("network lost; advertising again");
                provisioned = false;
                // A press made while provisioned was meant for nothing here.
                SHORT_PRESS.reset();
            }

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
                        scan_data,
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
            let conn = match select3(advertiser.accept(), auth.next(), joins.next()).await {
                Either3::First(Ok(conn)) => conn,
                Either3::First(Err(e)) => {
                    warn!("accepting a connection: {e:?}");
                    continue;
                }
                Either3::Second(AuthEvent::Authorize | AuthEvent::Revoke) => continue,
                Either3::Third(DeviceEvent::Provisioned(_)) => {
                    info!("provisioned over serial or from flash; stopping advertising");
                    provisioned = true;
                    continue;
                }
                Either3::Third(_) => continue,
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
            let outcome = match select(
                serve(&conn, &server.improv_wifi, &BRIDGE),
                driver.provision_with(&mut auth, &mut joins),
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
            let Some(url) = outcome else {
                info!("Bluetooth client disconnected");
                continue;
            };
            info!("provisioned; redirect URL {url:?}");
            if let Some(e) = radio.wifi().lock().await.take_save_error() {
                warn!("saving the network: {e:?}");
            }
            // Let the client read the result, then disconnect.
            let _ = select(
                serve(&conn, &server.improv_wifi, &BRIDGE),
                Timer::after(Duration::from_secs(5)),
            )
            .await;
            conn.raw().disconnect();
            provisioned = true;
        }
    };
    match select(runner.run(), session).await {
        Either::First(Err(e)) => warn!("Bluetooth host stopped: {e:?}"),
        Either::First(Ok(())) => warn!("Bluetooth host stopped"),
        Either::Second(_) => {}
    }
}
