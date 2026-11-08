---
status: accepted
date: 2026-11-08
decision-makers: project maintainer
---

# Talk to BlueZ over D-Bus with zbus

## Context and Problem Statement

On Linux, the device side needs a BLE GATT peripheral and the client side a
GATT central. Both go through BlueZ's D-Bus API. The first versions used
`bluer` (peripheral) and `btleplug` (central), which link `libdbus` through
the `dbus` crate. That needs `libdbus-1-dev` to build, and a matching C
library to cross-compile for a Raspberry Pi.

## Decision Drivers

* Build and cross-compile without C libraries.
* One D-Bus stack for NetworkManager, systemd-hostnamed and BlueZ.
* Testability against stand-in services on a private bus.

## Considered Options

* Keep `bluer` and `btleplug`.
* Implement the BlueZ peripheral and central directly on `zbus`, keeping
  `btleplug` for macOS and Windows.

## Decision Outcome

Chosen option: "Implement on zbus", because it removes the only C library in
the Linux build and uses the D-Bus stack the NetworkManager and hostnamed
backends already use.

### Consequences

* Good, because `cargo build` needs no system packages for BLE, and static
  musl builds for the Raspberry Pi need no target C library (ADR-0006).
* Good, because the peripheral and central are tested against a stand-in
  `bluetoothd` on a private `dbus-daemon`.
* Bad, because this code follows the BlueZ D-Bus API documents itself,
  instead of a maintained crate; BlueZ behavior differences between versions
  (for example, BlueZ 5.55 and 5.66 ignore an advertisement's `MinInterval`, while 5.82 uses it) are found on hardware.

### Confirmation

`tests/bluez.rs` in `impromptu-wifi-linux` and `impromptu-wifi-host`; provisioning
on a Raspberry Pi Zero W and Pi 3B from the CLI on Linux.

## More Information

The BlueZ D-Bus API is documented in the `doc/` directory of the BlueZ
source tree (the GATT manager, LE advertising manager, adapter and device
interfaces).
