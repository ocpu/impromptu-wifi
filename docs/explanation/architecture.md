# Architecture

## Purpose and scope

This document describes how the impromptu-wifi crates divide the Improv Wi-Fi
protocol between them, and how a command travels from a transport to a Wi-Fi
backend. It covers both roles: the *device* (the gadget being provisioned)
and the *client* (the tool or app that provisions it). Per-crate API details
are in each crate's rustdoc.

Terms: a *transport* is serial or Bluetooth LE (BLE). A *backend* is the code
that joins Wi-Fi networks on a platform, such as NetworkManager on Linux or
esp-radio on an ESP32. *Provisioned* means the device joined the network the
client sent.

## Context

```text
 Client (provisioning tool)                     Device (being provisioned)
 ---------------------------                    ---------------------------------
 impromptu-wifi CLI, Home Assistant,   serial   firmware or Linux daemon
 ESP Web Tools, firmware as client    <------>  using impromptu-wifi
                                         BLE    -> Wi-Fi backend -> access point
```

The protocol is defined by the Improv Wi-Fi specification, in separate
documents for BLE (versions up to 2.4) and serial (version 1); see
[Protocol conformance](protocol-conformance.md).

## Components

The crates form layers. Lower layers have no knowledge of higher ones; the
`no_std` layers allocate no memory.

```text
                     impromptu-wifi (facade: features select layers)
 ------------------------------------------------------------------------------
 Platform   impromptu-wifi-embassy     impromptu-wifi-linux    impromptu-wifi-host
            (firmware, no_std)         (device, tokio)         (client, tokio)
                                                               impromptu-wifi-cli
 ------------------------------------------------------------------------------
 Transport  impromptu-wifi-serial      impromptu-wifi-ble      (both roles, no_std)
 ------------------------------------------------------------------------------
 Role       impromptu-wifi-core (device)          impromptu-wifi-client (client)
 ------------------------------------------------------------------------------
 Contracts  impromptu-wifi-traits (backend traits)
 Wire       impromptu-wifi-proto (frames, states, errors, RPC encoding)
```

### impromptu-wifi-proto

- **Responsibility:** the wire format for both transports: serial packets
  with header, version, type and checksum; BLE characteristic values; RPC
  commands and results; states, error codes and capabilities; BLE
  advertisement and scan-response data.
- **Interfaces:** parsing and encoding functions over byte slices.
- **State:** none.
- **Constraints:** `no_std`, allocation-free. A state or error code that a
  transport does not define (for example `Stopped` on BLE) is not reported on
  that transport.

### impromptu-wifi-traits

- **Responsibility:** what a device must provide. Required: `WifiConnect`
  (join a network) and `DeviceInfoSource` (firmware and device names).
  Optional, each with a `SUPPORTED` constant: `WifiScanner`, `NetworkStatus`,
  `Identify`, `HostnameProvider`. Also `AuthSource` (physical authorization),
  `EventSource` (events from outside the driver) and `CredentialStore`.
- **Constraints:** the `SUPPORTED` constants decide which commands the device
  answers and which capability bits it advertises, so support can't drift
  from the implementation.

### impromptu-wifi-core (device role)

- **Responsibility:** the protocol rules and the device loop.
- **Interfaces:**
  - `Machine`: a synchronous state machine without I/O. It decides what each
    command means in the current state, enforces authorization, and tracks
    which state and error updates the client has not seen.
  - `Driver`: an async loop that connects a `Link` (a transport), a Wi-Fi
    object and a device object to a `Machine`.
  - `Link`: what a transport provides: receive RPC frames; send state, error
    and result.
  - `NotifyOnJoin` and `PersistOnJoin`: wrappers around a Wi-Fi object that
    report and save successful joins.
- **State:** the current state, error, authorization and the redirect URL.

### impromptu-wifi-client (client role)

- **Responsibility:** the client side: a session state machine and an async
  `Client` over a `ClientLink`.

### impromptu-wifi-serial and impromptu-wifi-ble (transports)

- **Responsibility:** framing for each transport, in both roles.
- **Interfaces:** `SerialLink` and `SerialClientLink` over any
  `embedded-io-async` stream; `BleLink` and `BleClientLink` over the
  `GattServer` and `GattClient` traits, so any BLE stack can be used.

### Platform crates

| Crate | Role | Provides |
|---|---|---|
| `impromptu-wifi-embassy` | Device and client, `no_std` firmware | `ButtonAuth`, `Button`, `WifiShare` (one backend for several drivers), `keep_online`, `FlashStore`, trouble-host GATT bindings with a Database Hash, USB CDC-ACM serial, embassy-net helpers, an ESP32 backend over esp-radio |
| `impromptu-wifi-linux` | Device, Linux | `NmWifi` (NetworkManager), `WpaWifi` (wpa_supplicant), `LinuxDevice` (systemd-hostnamed), a BlueZ GATT peripheral, serial ports, and the `impromptu-wifi-device` daemon |
| `impromptu-wifi-host` | Client, Linux, macOS, Windows | Serial ports, and BLE through BlueZ over D-Bus on Linux and btleplug elsewhere |
| `impromptu-wifi-cli` | Client | The `impromptu-wifi` command-line tool |

## Data and control flow

One command on the device side, for example "send Wi-Fi settings":

1. The transport receives a frame. `Link::recv` returns it to the `Driver`.
2. The `Driver` parses it with `impromptu-wifi-proto` and passes the command
   to `Machine::on_command`.
3. The `Machine` checks the state and authorization and returns a
   `Decision`, such as `Connect { ssid, password }` or `Reject(error)`. It
   updates its own state first (here to `Provisioning`).
4. The `Driver` performs the decision through the backend traits, here
   `WifiConnect::connect`, and reports the outcome back to the `Machine`.
5. The `Driver` publishes the changed state, error and RPC result through the
   `Link`.

Between commands the `Driver` waits on three sources at once: the `Link`, the
`AuthSource` (button presses and their timeout) and the `EventSource`. Events
change what the device reports without a command:

| Event | Effect |
|---|---|
| `Available(false)` / `Available(true)` | Serial reports `Stopped`, then the idle state again (for example the Wi-Fi radio switched off and on) |
| `Provisioned(url)` | An idle driver reports `Provisioned` (another transport, or the platform, joined a network) |
| `Unprovisioned` | A provisioned driver reports its idle state again (the network was lost); see [ADR-0003](../decisions/0003-report-unprovisioned-after-a-lost-network.md) |

A device that offers serial and BLE at once runs one `Driver` per transport.
They share one Wi-Fi object (`WifiShare` in firmware, clones of the backend
on Linux), and joins made through one are announced to the other as
`Provisioned`, so both report the same state.

## Reliability and failure behavior

- A transport error ends the `Driver` loop; the application decides whether
  to reopen the transport. The examples reopen serial, and restart
  advertising after a BLE disconnect.
- A failed join leaves the device in its idle state with the error
  `Unable to connect`. The Linux backends remove the profile or network entry
  they created for the failed attempt.
- Firmware with `keep_online` rejoins a saved network at boot and after a
  lost connection, with increasing delays, and reports `Unprovisioned` after
  repeated failures.
- On Linux the daemon follows the backend's notion of "online" and stops
  offering BLE as soon as the machine is online, however that happened.

## Security boundaries

Covered in [Security model](security-model.md): authorization per transport,
when BLE is offered, and how credentials are handled and stored.

## Deployment and runtime topology

- **Firmware:** one embassy executor; one task per transport, plus tasks for
  the network stack, the button, the status LED and `keep_online`. See
  [`examples/esp32s3`](../../examples/esp32s3/README.md).
- **Linux:** one `impromptu-wifi-device` process, a systemd service running as
  an unprivileged user, talking to NetworkManager or wpa_supplicant, to
  systemd-hostnamed and to BlueZ. See the
  [daemon reference](../reference/impromptu-wifi-device.md).
- **Client:** the `impromptu-wifi` tool is a single process; it uses BlueZ over
  D-Bus on Linux.

## Key decisions and tradeoffs

- The protocol rules live in a state machine without I/O, so they are tested
  with byte buffers and no hardware.
- Optional capabilities are compile-time constants on the backend types,
  not runtime flags.
- The `no_std` crates allocate nothing, so they fit small microcontrollers;
  the cost is fixed-size buffers (for example at most 16 networks per scan).

## Related decisions

- [ADR-0001](../decisions/0001-use-zbus-for-bluez.md): zbus for BlueZ.
- [ADR-0002](../decisions/0002-offer-bluetooth-only-while-offline.md): BLE only
  while offline on Linux.
- [ADR-0003](../decisions/0003-report-unprovisioned-after-a-lost-network.md):
  `Unprovisioned`.
- [ADR-0007](../decisions/0007-wpa-supplicant-control-socket-backend.md):
  wpa_supplicant backend.
