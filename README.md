# impromptu-wifi

A layered Rust implementation of the [Improv Wi-Fi](https://www.improv-wifi.com) provisioning protocol: `no_std` and allocation-free at the bottom, increasingly concrete toward the top.

Depend on the `impromptu-wifi` crate and enable the features you need. Each feature makes one layer available as a module:

```toml
[dependencies]
# Firmware: serial and BLE device side, button authorization, trouble-host.
impromptu-wifi = { version = "0.1", features = ["embassy-trouble", "embassy-usb"] }
```

| Feature | Module | Contents |
|---|---|---|
| (always) | `proto`, `traits` | Wire format; backend traits a device implements |
| `device` | `device` | State machine (sans-IO) and async `Driver` |
| `client` | `client` | Session machine (sans-IO) and async `Client` |
| `serial` | `serial` | `SerialLink` (device), `SerialClientLink` (client) |
| `ble` | `ble` | `BleLink` (device), `BleClientLink` (client) over GATT traits |
| `embassy` | `embassy` | `ButtonAuth`, `Button`, `WifiShare`, `keep_online`; plus `embassy-trouble`, `embassy-trouble-central`, `embassy-usb`, `embassy-net`, `embassy-flash`, `esp` |
| `host` | `host` | Desktop client side, tokio; plus `host-serial`, `host-ble` |
| `linux` | `linux` | Linux device side, tokio; plus `linux-networkmanager`, `linux-hostnamed`, `linux-serial`, `linux-bluez` |
| `defmt` | | `defmt::Format` for every enabled `no_std` layer |

[`examples/esp32c6`](examples/esp32c6) is a complete firmware that offers Improv Wi-Fi over USB serial and Bluetooth LE at the same time. [`examples/esp32s3`](examples/esp32s3) is the same firmware for the ESP32-S3 (Xtensa, built with Espressif's `esp` toolchain).

Features enable the layers they build on. No feature is on by default. On Linux, `host-ble` and `linux-bluez` talk to BlueZ over D-Bus through zbus and need no C library; `host-serial` and `linux-serial` need `libudev`.

Each module is also a crate of its own:

```
impromptu-wifi-proto       wire format, shared by both roles               no_std, no deps
impromptu-wifi-traits      backend traits a device implements              no_std, heapless only
  ├── impromptu-wifi-core    device: state machine + async Driver          no_std
  └── impromptu-wifi-client  client: session machine + async Client        no_std
impromptu-wifi-serial      SerialLink + SerialClientLink                   no_std
impromptu-wifi-ble         BleLink + BleClientLink over GATT traits        no_std
impromptu-wifi-embassy     ButtonAuth, USB CDC-ACM, trouble-host GATT,     no_std
                           embassy-net, ESP32 Wi-Fi backend (esp-radio)
impromptu-wifi-host        client side on desktops: tokio-serial, BlueZ    std, tokio
                           (zbus) on Linux, btleplug elsewhere
impromptu-wifi-linux       Linux device side: NetworkManager, hostnamed,   std, tokio
                           serial, BlueZ; the impromptu-wifi-device daemon
impromptu-wifi-cli         the impromptu-wifi command-line tool            binary
impromptu-wifi             the features above, re-exported as modules      no_std
```

Each layer depends only on the ones above it. A platform crate that only implements backends needs nothing but `impromptu-wifi-traits`.

## Using it

A device provides two objects. The Wi-Fi object implements `WifiConnect` (required) plus `WifiScanner` and `NetworkStatus`. The device object implements `DeviceInfoSource` (required) plus `Identify` and `HostnameProvider`. Optional traits you don't support are declared with one line:

```rust
// features = ["serial"]
use impromptu_wifi::device::{unsupported, Driver, NoAuth};
use impromptu_wifi::serial::SerialLink;

impl WifiConnect for MyWifi { /* async fn connect(...) */ }
impl WifiScanner for MyWifi { /* async fn scan(...) */ }
unsupported!(MyWifi: NetworkStatus);

impl DeviceInfoSource for MyDevice { /* firmware name, version, chip, device name */ }
unsupported!(MyDevice: Identify, HostnameProvider);

let link = SerialLink::new(uart); // anything implementing embedded_io_async::{Read, Write}
let mut driver = Driver::new(link, my_wifi, my_device, false);
driver.run(&mut NoAuth).await?;
```

Each optional trait has a `SUPPORTED` constant, so the driver knows at compile time which commands to answer, and BLE capabilities are a constant too:

```rust
const CAPS: Capabilities = impromptu_wifi::ble::capabilities::<MyWifi, MyDevice>();
let link = BleLink::new(gatt, CAPS);
```

The driver takes one object per role rather than one per trait because on real hardware a single Wi-Fi controller connects, scans and often owns the hostname, and can't be handed out twice.

### Serial on embassy

`SerialLink` takes any stream implementing `embedded-io-async` 0.7 `Read` and `Write`, but not every HAL UART qualifies:

| Platform | Use |
|---|---|
| nRF, RP | `BufferedUarte` / `BufferedUart` (the plain async UARTs only implement `Write`) |
| STM32 | `BufferedUart`, or `RingBufferedUartRx` + `UartTx` joined with `impromptu_wifi_serial::Duplex` |
| ESP32 | async `Uart` or `UsbSerialJtag` (needs esp-hal's `unstable` feature) |
| Any chip with USB | `impromptu_wifi_embassy::usb::CdcAcmSerial` over `embassy-usb` (feature `usb`) |

`CdcAcmSerial` adds what raw `embassy-usb` lacks: a zero-length packet after any transfer that ends on a full 64-byte packet (otherwise some hosts hold that Improv packet back indefinitely), and a single object for the split class. Unplugging ends the driver loop with `NotConnected`; keep the port and give each session a link that borrows it, as shown in the `usb` module docs.

Logs on the same line are skipped between packets, but a log line written *during* a packet (from another task or an interrupt) corrupts it. Both sides detect that by checksum and recover at the next packet, at the cost of a retry.

### The `impromptu-wifi` tool

```sh
impromptu-wifi ports                                   # serial ports
impromptu-wifi discover                                # Bluetooth devices, with their state
impromptu-wifi --port /dev/ttyUSB0 info
impromptu-wifi --port /dev/ttyUSB0 scan
impromptu-wifi --port /dev/ttyUSB0 provision --ssid home    # prompts for the passphrase
impromptu-wifi --ble kitchen provision --ssid home          # waits for a button press if needed
```

Opening a serial port leaves DTR and RTS alone, so ESP32 boards with the usual auto-reset wiring aren't reset by connecting; `--reset` resets one on purpose. On Linux, BLE needs BlueZ running, and building needs the `libudev` development package for serial ports (`libudev-dev pkg-config` on Debian/Ubuntu).

### A Linux machine as the device

`impromptu-wifi-device` makes a Raspberry Pi or similar machine provisionable like an ESP32:

```sh
impromptu-wifi-device --serial /dev/ttyGS0          # USB gadget mode; /dev/serial0 for the UART
impromptu-wifi-device --ble                         # Bluetooth, offered only while offline
impromptu-wifi-device --serial /dev/ttyGS0 --ble    # both
```

Wi-Fi goes through NetworkManager, which keeps the connection across reboots; the hostname (static) and device name (pretty hostname) through systemd-hostnamed. A failed join removes the profile it created, and a successful one replaces older profiles for the same SSID. Bluetooth stops once the machine is online, as the spec describes: there's no button to authorize with, so this keeps passers-by from re-pointing a working machine. `crates/impromptu-wifi-linux/dist/` has a hardened systemd unit and a polkit rule that lets an unprivileged service user make exactly these changes.

### Client side

```rust
use impromptu_wifi_client::Client;
use impromptu_wifi_serial::SerialClientLink;

let mut client = Client::new(SerialClientLink::new(port)); // or BleClientLink::new(gatt)
let info = client.device_info().await?;
let networks = client.scan::<16>().await?;
let url = client.provision("home", "hunter22").await?;
```

The protocol has no request ids, so the client keeps one command outstanding and a sans-IO `Session` decides which state, error or result answers it (rules in `impromptu_wifi_client::session`). Commands that don't exist on the transport, such as identify over serial, are refused before anything is sent. `current_state()` works on both transports: it reads the state characteristic on BLE and sends a command on serial. On BLE, `capabilities()` reads what the device supports, and `wait_for_state(State::Authorized)` waits out the button press.

For BLE, implement `impromptu_wifi_ble::GattClient` on your stack: write a chunk, wait for a notification, read two characteristics. `BleClientLink` splits commands to the connection's write size. The client never times out on its own; race calls against your executor's timer.

Failures are typed (`ConnectError`, `ScanError`, `HostnameError`, `DeviceNameError`). The protocol collapses them into its few error codes, but the driver keeps the detail: `driver.last_connect_error()` tells you whether a join failed on credentials, a missing network or a timeout.

## Transport differences the types handle for you

The serial and BLE variants look alike but differ in ways that are easy to get wrong:

| | Serial | BLE |
|---|---|---|
| Command `0x02` | request current state | identify |
| Command `0x07` | network state | not defined |
| State `0x00` / `0x01` | Stopped / — | — / Authorization required |
| Error `0x04` (not authorized) | not defined | yes |
| RPC checksum | on the enclosing packet | on each RPC frame |
| Scan results | one result per network (`YES`/`NO`), then an empty result | one result of SSID/RSSI/auth-type triples |
| Max RPC data | 253 bytes (must fit in a packet) | 255 bytes |

Everything that parses or encodes takes a `Transport`, and the driver formats scans per transport.

## Status

| Crate | State |
|---|---|
| `impromptu-wifi-proto` | Complete for spec as of BLE 2.4 / serial v1, including hostname, device name and network state |
| `impromptu-wifi-traits` | Complete |
| `impromptu-wifi-core` | State machine and driver complete. The driver tells the `AuthSource` when the spec restarts the authorization timeout, and follows an optional `EventSource`: Wi-Fi availability (the serial `Stopped` state) and joins made by another driver, so all transports report `Provisioned` together (`NotifyOnJoin`, and `WifiShare` on embassy), and a lost network, which reopens provisioning (`Unprovisioned`, which the spec does not define). `PersistOnJoin` saves the credentials of each join to a `CredentialStore` |
| `impromptu-wifi-client` | Complete: session machine and async client for every command |
| `impromptu-wifi-serial` | Complete for both roles |
| `impromptu-wifi-ble` | Both roles complete against the `GattServer` / `GattClient` traits; no stack bindings yet |
| `impromptu-wifi-embassy` | `ButtonAuth`, `Button`, `WifiShare` (with `status()` for an indicator), `keep_online`, a NOR-flash `FlashStore`, `trouble-host` bindings for both roles, and `embassy-net` helpers: complete and built here. The ESP32 backend (feature `esp`) is written against esp-radio 1.0.0-beta.1 and builds into complete ESP32-C6 and ESP32-S3 firmware (`examples/esp32c6`, `examples/esp32s3`). The ESP32-S3 firmware has run on one board (see its README); the ESP32-C6 firmware has not run on hardware |
| `impromptu-wifi-host` | Serial complete and tested over a real pseudo-terminal; BLE through BlueZ (zbus) on Linux, tested against a stand-in BlueZ, and `btleplug` on macOS and Windows, type-checked only; neither run against hardware yet |
| `impromptu-wifi-cli` | Complete for serial (end-to-end tests run the real binary); BLE commands untested on hardware |
| `impromptu-wifi-linux` | NetworkManager, hostnamed and serial complete, tested over real D-Bus against stand-in services; the BlueZ peripheral is built and type-checked, not yet run against hardware. Verify both on a real machine |

## Development

```sh
cargo test --workspace
```

Three kinds of end-to-end tests run against the full driver: scripted byte sequences (serial and BLE, including 20-byte BLE writes), and loopback conformance tests where the real `Client` talks to the real `Driver`: over an in-memory serial line with log noise (`impromptu-wifi-serial/tests/loopback.rs`), over an in-memory GATT connection with button authorization and different MTUs (`impromptu-wifi-ble/tests/loopback.rs`), and through the real `impromptu-wifi` binary over a kernel pseudo-terminal (`impromptu-wifi-cli/tests/pty.rs`). The Linux backends run over a private `dbus-daemon` against stand-in NetworkManager and hostnamed services written from their public D-Bus documentation (`impromptu-wifi-linux/tests/dbus.rs`, needs `dbus-daemon` installed); that checks they follow the documented APIs, not that the real services behave identically. Because both ends share `impromptu-wifi-proto`, also test against ESPHome firmware and the official JS SDK; a shared codec can't catch a shared misreading of the spec. The proto crate is a good fuzzing target (`Framer::feed`, `RpcAssembler::push`, `Command::parse`).

MSRV is 1.87 (set by `heapless` 0.9), including `impromptu-wifi` without the `embassy*`, `esp`, `host*` and `linux*` features, except `impromptu-wifi-embassy`, `impromptu-wifi-host`, `impromptu-wifi-cli` and `impromptu-wifi-linux` at 1.91 (set by `smoltcp` 0.13 and the host dependencies). `embedded-io-async` is 0.7, the version current embassy HALs implement.

CI (`.github/workflows/ci.yml`) runs `rustfmt`, `clippy -D warnings`, the tests and rustdoc on Linux; builds the `no_std` crates for `thumbv6m-none-eabi`, `thumbv7em-none-eabihf` and `riscv32imac-unknown-none-elf`; builds and lints the `examples/esp32c6` and `examples/esp32s3` firmware (the latter with Espressif's Xtensa toolchain, through `esp-rs/xtensa-toolchain`); checks both MSRVs; runs `cargo deny check` (`deny.toml`); and runs `scripts/check-dep-age.py`, which fails if any locked crates.io version was published less than 14 days earlier. The `trouble` and `trouble-central` features do not build for `thumbv6m-none-eabi` (`static_cell` 2.1.1, through `trouble-host`, uses atomic compare-and-swap, which that target lacks). On Linux, building the serial support in `impromptu-wifi-host`, `impromptu-wifi-cli` and `impromptu-wifi-linux` needs `libudev-dev pkg-config` (Debian/Ubuntu names); nothing needs `libdbus`.

To check the ESP32 backend in your own firmware, build your firmware crate with `impromptu-wifi-embassy` features `esp` (plus `trouble` for BLE), your chip's esp-radio feature, and `esp-hal/unstable`, for example targeting `riscv32imac-unknown-none-elf` on an ESP32-C6. Before publishing, set `repository` in the workspace `Cargo.toml` and add `LICENSE-MIT` and `LICENSE-APACHE`.

## License

MIT OR Apache-2.0.
