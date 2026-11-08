# ESP32-S3 example

Firmware that makes an ESP32-S3 provisionable over Improv Wi-Fi, through the
`impromptu-wifi` crate with the `esp`, `embassy-trouble` and `embassy-flash`
features. It is the [ESP32-C6 example](../esp32c6) adapted to the S3; the
differences are listed under [Differences from the ESP32-C6
example](#differences-from-the-esp32-c6-example).

| Transport | Where | Authorization |
|---|---|---|
| Serial | USB-Serial-JTAG (the chip's own USB port, labelled USB on the ESP32-S3-DevKitC-1) | None, as the serial spec defines |
| Bluetooth LE | Improv Wi-Fi GATT service, advertised as `Impromptu` | Short press of BOOT (GPIO0); valid for 60 s |

Logs go to UART0 (the USB-to-UART port on development boards such as the
ESP32-S3-DevKitC-1, labelled UART), so they never mix with Improv Wi-Fi frames on the
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

## Status LED

The WS2812 RGB LED on GPIO38, driven by the RMT peripheral at 80 MHz.

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

* The Espressif Rust toolchain for Xtensa, installed with
  [`espup`](https://github.com/esp-rs/espup): `espup install`, then
  `. ~/export-esp.sh` in each shell. `rust-toolchain.toml` selects the
  `esp` channel; `.cargo/config.toml` builds `core` and `alloc` from source
  (`build-std`), as Xtensa targets ship without them.
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

## Differences from the ESP32-C6 example

| | ESP32-C6 | ESP32-S3 |
|---|---|---|
| Target | `riscv32imac-unknown-none-elf`, stable Rust | `xtensa-esp32s3-none-elf`, the `esp` toolchain with `build-std` |
| BOOT button | GPIO9 | GPIO0 |
| RGB LED (DevKitC-1) | GPIO8 | GPIO38 (v1.1), GPIO48 (v1.0) |
| Cores | 1 | 2; this firmware uses one |
| Release image | about 1.0 MB | about 0.77 MB |

The source is otherwise the same. Keep the two in step when changing
either.

## Limitations

* **No hostname.** Clients can't read or set the hostname.
* **No WPA3-only networks.** esp-radio 1.0.0-beta.1 does not support WPA3,
  so a network in WPA3-Personal-only mode fails to join. WPA2 and WPA2/WPA3
  transition networks work.
* **The LED is assumed on GPIO38**, as on the ESP32-S3-DevKitC-1 v1.1.
  Version 1.0 of that board has it on GPIO48; other boards need a different
  pin, or none. Change it in `main.rs`.
* **The second core is not started.** `esp-storage` refuses flash writes
  while the second core runs (its default `MultiCoreStrategy::Error`), so a
  firmware that starts it needs `FlashStorage::multicore_auto_park`.
* **Tested on one board.** On 2026-11-08 this firmware ran on an ESP32-S3
  (revision v0.2, 16 MB flash, CH343 USB-to-UART bridge, RGB LED on GPIO38
  taking red first) with the `impromptu-wifi` CLI on Linux (BlueZ):
  provisioning over serial and over BLE with a button press, a failed join,
  rejoining the saved network after a restart, the factory reset, the
  LED patterns, both transports agreeing on
  `Provisioned`, losing the network (a phone hotspot switched off: back to
  `Authorized` and advertising after three failed rejoins) and rejoining
  when it returned, identify (white blink), and the short-press flash and
  BLE authorization. Not tested: ESP Web Tools and Home Assistant.
