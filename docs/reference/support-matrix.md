# Support matrix

What impromptu-wifi runs on, and how far each combination has been
verified. Statuses:

| Status | Meaning |
|---|---|
| **Tested** | Run on the hardware named, with the result in the row. Date and setup in the linked README. |
| **Built** | Compiled (and, for the host crates, linted and unit-tested) by the CI definition in `.github/workflows/ci.yml`; not run on hardware. |
| **Untested** | Neither. Expected to work only where noted. |

All tests below were made on 2026-11-08. The CI workflow has been run
locally; it has not run on a hosted CI service yet.

## Devices

Devices that are provisioned: they run the Improv Wi-Fi device side.

### Microcontrollers

| Device | Firmware | Serial | Bluetooth LE | Status |
|---|---|---|---|---|
| ESP32-S3 (rev v0.2, 16 MB flash, DevKitC-1-style board) | [`examples/esp32s3`](../../examples/esp32s3) | USB-Serial-JTAG: Tested | Tested | **Tested**: provisioning over serial and BLE, saved network and rejoin, factory reset, status LED, identify, web page |
| Other ESP32 chips supported by esp-radio 1.0.0-beta.1 | `impromptu-wifi-embassy` feature `esp` in your firmware | - | - | **Untested**. The `esp` backend is only built for the ESP32-S3, through the example |
| Other `no_std` targets with embassy (nRF52, STM32, RP2040, ESP32-C3/C6) | `impromptu-wifi-embassy` with your Wi-Fi backend | - | - | **Built** for `thumbv6m-none-eabi`, `thumbv7em-none-eabihf` and `riscv32imac-unknown-none-elf`; no firmware example |

ESP32-S3 limits: WPA3-only networks can't be joined (esp-radio 1.0.0-beta.1
has no WPA3); no hostname; the LED pin and colour order are board-specific.
See [its README](../../examples/esp32s3/README.md#limitations).

### Linux machines

With the `impromptu-wifi-device` daemon; setup in
[`examples/raspberry-pi`](../../examples/raspberry-pi).

| Device | OS | Bluetooth chip | Wi-Fi backend | Provisioned from | Status |
|---|---|---|---|---|---|
| Raspberry Pi Zero W Rev 1.1 | Raspbian 11 (bullseye) | BCM43438 | NetworkManager, and wpa_supplicant | `impromptu-wifi` CLI over BLE | **Tested** with both backends |
| Raspberry Pi 3 Model B Rev 1.2 | Raspberry Pi OS 13 (trixie) | BCM43438 | NetworkManager | `impromptu-wifi` CLI over BLE | **Tested** |
| Raspberry Pi 4 Model B Rev 1.1 | Raspberry Pi OS 12 (bookworm) | CYW43455 | NetworkManager | Home Assistant over BLE | **Tested**; see [known issues](#known-issues) for one client |
| Raspberry Pi 5 Model B Rev 1.0 | Raspberry Pi OS 13 (trixie) | CYW43455 | NetworkManager | Home Assistant over BLE | **Tested**; see [known issues](#known-issues) for one client |
| Raspberry Pi Zero 2 W, Pi 2, Pi 400, Compute Modules | Raspberry Pi OS | - | - | - | **Untested**; the binaries for their architectures are built |
| Other Linux machines with systemd and BlueZ | any | any | NetworkManager or wpa_supplicant | - | **Untested** on hardware; the daemon is tested against stand-in NetworkManager, hostnamed, BlueZ and wpa_supplicant services |

Serial transports on Linux (USB gadget `/dev/ttyGS0`, GPIO UART) are
**Untested** on hardware.

## Operating systems for the Linux daemon

What each Raspberry Pi OS release needs, all handled by
[`setup.sh`](../../examples/raspberry-pi/setup.sh).

| | Raspbian / Raspberry Pi OS 11 (bullseye) | Raspberry Pi OS 12 (bookworm) | Raspberry Pi OS 13 (trixie) |
|---|---|---|---|
| Network by default | dhcpcd with wpa_supplicant | NetworkManager | NetworkManager |
| NetworkManager backend | **Tested** (Zero W), after switching with `raspi-config nonint do_netconf 2` | **Tested** (Pi 4) | **Tested** (Pi 3B, Pi 5) |
| wpa_supplicant backend | **Tested** (Zero W), as installed | **Untested** | **Untested** |
| BlueZ | 5.55 | 5.66 | 5.82 |
| Advertising interval | Set in `main.conf` (`LEMinAdvertisementInterval`) | Set in `main.conf` (`[LE] MinAdvertisementInterval`) | Taken from the daemon |
| BlueZ MIDI plugin | Disabled (`--noplugin=midi`) | Disabled | Disabled |
| WirePlumber BLE-MIDI | Not checked | WirePlumber 0.4.13: none | WirePlumber 0.5: disabled |
| polkit | 0.105: `.pkla` | 122: `.rules` | 126: `.rules` |
| Fresh-image quirks | Wi-Fi blocked until a country is set | NetworkManager leaves a USB gadget's `usb0` unmanaged | Wi-Fi radio off; Bluetooth can be rfkill-blocked |

Architectures: static musl binaries for `arm-unknown-linux-musleabihf`
(ARMv6), `armv7-unknown-linux-musleabihf` and `aarch64-unknown-linux-musl`,
all **Built**; ARMv6 and AArch64 are **Tested** (Zero W; Pi 3B, 4, 5).

## Clients

Tools and apps that provision a device.

| Client | Platform | Serial | Bluetooth LE | Status |
|---|---|---|---|---|
| `impromptu-wifi` CLI | Linux (Fedora, BlueZ 5.87, MediaTek Bluetooth) | **Tested** (ESP32-S3) | **Tested** (ESP32-S3, Pi Zero W, Pi 3B); fails to Pi 4 and Pi 5 | **Tested** |
| `impromptu-wifi` CLI | macOS, Windows (BLE through btleplug) | Built | Built | **Built** (lint only) |
| Home Assistant, Improv via BLE | Through an ESPHome Bluetooth proxy | - | **Tested** (ESP32-S3, Pi 4, Pi 5) | **Tested**; restart Home Assistant after a device's GATT layout changes |
| ESP Web Tools (improv-wifi.com) | Firefox with Web Serial | **Tested** (ESP32-S3) | - | **Tested** |
| Phones (Android, iOS) | - | - | - | **Untested** |
| Firmware as a client (`impromptu-wifi-client`, `trouble-central`) | `no_std` targets | Built | Built | **Built** |

## Build targets

| Target | What | Status |
|---|---|---|
| `x86_64-unknown-linux-gnu` | All host crates: build, clippy, tests, rustdoc | **Built** and tested (unit and stand-in service tests) |
| macOS, Windows | `impromptu-wifi-host`, `impromptu-wifi-cli`: clippy | **Built** |
| `thumbv6m-none-eabi` | `no_std` crates; embassy without `trouble` (no atomic compare-and-swap) | **Built** |
| `thumbv7em-none-eabihf`, `riscv32imac-unknown-none-elf` | `no_std` crates, embassy with all features | **Built** |
| `xtensa-esp32s3-none-elf` | `examples/esp32s3` | **Built** and **Tested** |
| `arm-unknown-linux-musleabihf`, `armv7-unknown-linux-musleabihf`, `aarch64-unknown-linux-musl` | `impromptu-wifi-device` | **Built**; ARMv6 and AArch64 **Tested** |
| Minimum Rust | 1.87 for the `no_std` crates; 1.91 for embassy, host, CLI and Linux | **Built** |

## Known issues

| Issue | Affects | Workaround |
|---|---|---|
| Connections from one laptop (MediaTek Bluetooth `0e8d:0717`, BlueZ 5.87) to the Pi 4 and Pi 5 drop or time out; a raw ATT client on the same laptop works | Pi 4, Pi 5 (CYW43455) with that client | Provision from another client, such as Home Assistant ([details](../../examples/raspberry-pi/README.md#known-issue-a-laptops-mediatek-bluetooth-and-the-pi-4-and-5)) |
| Home Assistant keeps a device's GATT layout in memory and doesn't check the Database Hash | Any device whose layout changes, such as after a firmware update | Restart Home Assistant ([details](../../examples/esp32s3/README.md#bluetooth-address)) |
| A Linux client's BlueZ can keep a stale GATT cache after a device's layout changes | Linux clients | `bluetoothctl remove <address>` on the client |
| The same laptop's BlueZ 5.87 crashed when a pairing it refused coincided with a device-list request | That BlueZ build | `setup.sh` turns off what triggered the pairing (BLE-MIDI) |
| WPA3-only networks | ESP32-S3 (esp-radio 1.0.0-beta.1) | Use WPA2 or WPA2/WPA3 transition mode |
| Bluetooth is not offered while a machine is online through Ethernet | Linux daemon, by design | `--ignore-wired` |
