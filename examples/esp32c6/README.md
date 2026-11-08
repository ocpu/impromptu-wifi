# ESP32-C6 example

Firmware that makes an ESP32-C6 provisionable over Improv Wi-Fi, through the
`impromptu-wifi` crate with the `esp` and `embassy-trouble` features.

| Transport | Where | Authorization |
|---|---|---|
| Serial | USB-Serial-JTAG (the chip's own USB port) | None, as the serial spec defines |
| Bluetooth LE | Improv Wi-Fi GATT service, advertised as `Impromptu` | Press BOOT (GPIO9); valid for 60 s |

Logs go to UART0 (the USB-to-UART port on development boards such as the
ESP32-C6-DevKitC-1), so they never mix with Improv Wi-Fi frames on the
USB-Serial-JTAG port.

Both transports share one Wi-Fi controller through `SharedWifi`. After a join,
the device waits for a DHCP address and reports `http://<address>` as the
redirect URL. Bluetooth advertising stops once the device is provisioned;
serial keeps answering.

## Prerequisites

* Rust 1.91 or later with the `riscv32imac-unknown-none-elf` target:
  `rustup target add riscv32imac-unknown-none-elf`.
* [`espflash`](https://github.com/esp-rs/espflash) 4 or later to flash.

## Build and flash

From this directory, with the board connected:

```sh
cargo run --release
```

`.cargo/config.toml` selects the target and uses `espflash flash --monitor`
as the runner. To build without a board, use `cargo build --release`.

## Provision it

Over serial, with the CLI from this repository:

```sh
impromptu-wifi --port /dev/ttyACM0 provision --ssid home
```

Over Bluetooth LE: press BOOT, then within 60 seconds:

```sh
impromptu-wifi --ble Impromptu provision --ssid home
```

ESP Web Tools and Home Assistant's Improv Wi-Fi integration can also
provision it, over serial and BLE respectively.

## Limitations

* **Credentials are not saved.** After a reset, the device asks to be
  provisioned again. `EspWifi::credentials()` returns the last working
  credentials for the application to store in flash.
* **No identify and no hostname.** The board's RGB LED needs an RMT driver,
  which this example leaves out.
* **Not yet run on hardware.** The firmware builds and links in CI; it has not
  been flashed and tested against a real client.
