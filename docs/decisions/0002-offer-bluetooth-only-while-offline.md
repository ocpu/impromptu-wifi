---
status: accepted
date: 2026-11-08
decision-makers: project maintainer
---

# Offer Bluetooth LE provisioning on Linux only while offline

## Context and Problem Statement

The Improv Wi-Fi BLE specification lets a device without a physical
authorization start in the Authorized state. A Linux machine has no button,
so while `impromptu-wifi-device` advertises, anyone in radio range can make it
join a network of their choosing. When should it advertise?

## Decision Drivers

* A working machine must not be re-pointable by anyone in range.
* A new or disconnected machine must be provisionable without a keyboard.
* No extra hardware such as a button.

## Considered Options

* Advertise always.
* Advertise only while the machine is offline, and stop as soon as it is
  online, however it got online.
* Require a physical authorization (a GPIO button) on Linux.

## Decision Outcome

Chosen option: "Advertise only while offline", because it keeps a working
machine closed without needing hardware, and matches the specification's
"the Improv service is stopped" after provisioning.

### Consequences

* Good, because a machine that rejoins its saved network at boot stops
  advertising within seconds.
* Bad, because a wired machine never advertises by default. `--ignore-wired`
  counts Wi-Fi only, and is opt-in because it opens BLE provisioning on a
  networked machine.
* Bad, because a machine is open to anyone in range while it is offline.

### Confirmation

On a Raspberry Pi Zero W, the daemon stopped advertising within seconds of
NetworkManager rejoining a saved network at boot, and after
`nmcli networking on`; on a Pi 4 and Pi 5, after their Wi-Fi was reconnected
with `nmcli device connect wlan0`.

## More Information

The first version checked "online" only before advertising, so a machine
that came online later kept advertising; fixed by following the backend's
online state (`NmOnline`, `WpaOnline`).
