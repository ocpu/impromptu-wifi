# ESP32-C6 example

Firmware that makes an ESP32-C6 provisionable over Improv Wi-Fi, through the
`impromptu-wifi` crate with the `esp`, `embassy-trouble` and `embassy-flash`
features. [`../esp32s3`](../esp32s3) is the same firmware for the ESP32-S3.

| Transport | Where | Authorization |
|---|---|---|
| Serial | USB-Serial-JTAG (the chip's own USB port) | None, as the serial spec defines |
| Bluetooth LE | Improv Wi-Fi GATT service, advertised as `Impromptu` | Short press of BOOT (GPIO9); valid for 60 s |

Logs go to UART0 (the USB-to-UART port on development boards such as the
ESP32-C6-DevKitC-1), so they never mix with Improv Wi-Fi frames on the
USB-Serial-JTAG port.

Both transports share one Wi-Fi controller through `WifiShare`. After a join,
the device waits for a DHCP address and reports `http://<address>` as the
redirect URL. Bluetooth advertising stops while the device is provisioned;
serial keeps answering.

## Lifecycle

| Event | What the firmware does |
|---|---|
| Successful join, over either transport | Saves SSID and passphrase to flash (`PersistOnJoin` with `FlashStore`). A join with the network already saved writes nothing. |
| Boot with a saved network | Joins it (`keep_online`). On success both transports report `Provisioned` and Bluetooth stops advertising. Bluetooth advertises during the attempt, up to 50 s (30 s association, 20 s DHCP). |
| Connection lost | Rejoins after 2, 4, 8 s, and so on, up to every 5 min. Connectivity is checked every 10 s. |
| Three failed rejoins in a row | Reports `Authorized` over serial and advertises over Bluetooth again, so a client can provision another network. Rejoins continue in the background. |
| BOOT held for 5 s | Erases the saved network and restarts the chip, unprovisioned. |

Returning from `Provisioned` to `Authorized` is a choice of this crate; the
Improv Wi-Fi specification does not define that transition. Clients that
stop polling after `Provisioned` see the change only when they reconnect.

### Where the network is saved

In the first 4 KiB sector of the `nvs` data partition from the partition
table, as a 128-byte record with a CRC-32. esp-radio 1.0.0-beta.1 starts
the Wi-Fi driver with ESP-IDF's NVS disabled (`nvs_enable: 0`), so nothing
else in this firmware uses that partition. The default espflash partition
table has one at `0x9000`.

* The passphrase is stored in plain text. Anyone with access to the flash
  can read it, unless flash encryption is enabled.
* A save erases the sector before writing it. If power fails between the
  two, the device boots unprovisioned.

## Bluetooth address

The firmware advertises from a random static address derived from the
chip's factory MAC (the two most significant bits set, as the Bluetooth
Core Specification, Vol 6, Part B, 1.3.2.1 requires). It is the same on
every boot and differs from the chip's public address.

Clients such as ESPHome Bluetooth proxies cache a device's GATT layout by
address, and this firmware has no Service Changed or Database Hash
characteristic that would tell them it changed. With the public address, a
proxy that had seen earlier firmware on the same chip used that firmware's
layout, and Home Assistant failed to subscribe with "Write not permitted".
A future firmware that changes the GATT layout can hit the same problem
under this address.

## Status LED

The WS2812 RGB LED on GPIO8, driven by the RMT peripheral at 80 MHz.

| Pattern | Meaning |
|---|---|
| Blue, blinking once a second | Unprovisioned: waiting for a client |
| Yellow, blinking four times a second | Joining a network, for a client or to rejoin the saved one |
| Green, steady | On a network |
| White, blinking fast for 5 s | A BLE client sent Identify |
| Cyan flash | BOOT pressed briefly: BLE writes authorized for 60 s |
| Red, steady | BOOT held for 5 s: factory reset in progress |

The status comes from `WifiShare::status`. Because the device implements
`Identify`, BLE advertisements and the capabilities characteristic include
the identify bit.

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

Over Bluetooth LE: press BOOT briefly, then within 60 seconds:

```sh
impromptu-wifi --ble Impromptu provision --ssid home
```

ESP Web Tools and Home Assistant's Improv Wi-Fi integration can also
provision it, over serial and BLE respectively.

## Limitations

* **No hostname.** Clients can't read or set the hostname.
* **No WPA3-only networks.** esp-radio 1.0.0-beta.1 does not support WPA3,
  so a network in WPA3-Personal-only mode fails to join. WPA2 and WPA2/WPA3
  transition networks work.
* **The LED is assumed on GPIO8**, as on the ESP32-C6-DevKitC-1. Other
  boards need a different pin, or none.
* **Not yet run on hardware.** The firmware builds and links in CI; it has not
  been flashed and tested against a real client.
