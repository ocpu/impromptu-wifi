# Protocol conformance

How impromptu-wifi maps to the Improv Wi-Fi specification, and where it
interprets or goes beyond it. The specification has two documents:

- BLE: <https://www.improv-wifi.com/ble/>, versions up to 2.4 (2.4 added the
  Device Name command).
- Serial: <https://www.improv-wifi.com/serial/>, protocol version 1.

Both were read on 2026-11-08. The wire format is implemented in
`impromptu-wifi-proto`; the rules in `impromptu-wifi-core` (`Machine`).

## Implemented

### States

| Value | BLE | Serial | Reported when |
|---|---|---|---|
| `0x00` | - | Stopped | Serial only: provisioning is unavailable, for example the Wi-Fi radio is switched off (`DeviceEvent::Available(false)`) |
| `0x01` | Authorization Required | - | BLE with authorization, before a button press or after the timeout |
| `0x02` | Authorized | Ready (Authorized) | Ready to accept settings |
| `0x03` | Provisioning | Provisioning | A join is in progress |
| `0x04` | Provisioned | Provisioned | The join succeeded |

A state that a transport does not define is never sent on it.

### Commands

| ID | Command | BLE | Serial |
|---|---|---|---|
| `0x01` | Send Wi-Fi settings | Yes | Yes |
| `0x02` | Identify (BLE) / Request current state (serial) | Yes | Yes |
| `0x03` | Device info, including the optional OS name and version | Yes | Yes |
| `0x04` | Scan Wi-Fi networks | Yes | Yes |
| `0x05` | Get or set hostname | Yes | Yes |
| `0x06` | Get or set device name | Yes | Yes |
| `0x07` | Get network state | - | Yes |

A device answers only the optional commands its backends support (the
`SUPPORTED` constants in `impromptu-wifi-traits`), and on BLE advertises the
matching capability bits.

### Other rules followed

- Every RPC first resets the error to "no error", as the serial specification
  asks.
- "Request current state" on a provisioned serial device also re-sends the
  result of the successful provisioning, as the serial specification asks.
- Hostnames are checked against the specification's rule (letters, digits and
  hyphens, at most 255 characters) and refused with error `0x05`.
- The BLE authorization timeout restarts after a failed join and after a
  hostname or device-name write.
- The BLE advertisement carries the service UUID and the service data
  together, not in the scan response, as BLE version 2.1 requires. The device
  name goes in the scan response, since the advertisement is full.

## Interpretations

### Writes in the Provisioned state

The specification says setting the hostname or device name "requires the
device to be in an 'Authorized' state". `Machine` treats authorization as a
property separate from the state: a provisioned device accepts new Wi-Fi
settings, a hostname or a device name while it is authorized (on BLE, after a
button press within the timeout; on serial and on a device without a button,
always). This allows a device to be re-provisioned without a reset.

### Identify and device info in every state

The BLE specification describes Identify and Device Info as available in the
Authorization Required and Authorized states. `Machine` answers them in every
state, including Provisioned. A client that follows the specification sends
them only in the states it lists, so this changes nothing for it.

## Extensions

### Leaving Provisioned (`Unprovisioned`)

The specification defines no transition out of Provisioned. impromptu-wifi
returns a provisioned device to Authorized (or Authorization Required, if
authorization lapsed) when the device has lost its network:

- in firmware, when `keep_online` gives up rejoining after repeated failures;
- on Linux, when the backend reports the machine offline.

A client can then provision the device again without a restart. A client
that stopped polling after Provisioned sees the change when it next reads the
state. Rationale: [ADR-0003](../decisions/0003-report-unprovisioned-after-a-lost-network.md).

### Provisioned without a join through this transport

A device that serves serial and BLE at once reports Provisioned on both
transports when either one joins a network, or when the platform joins one by
itself (for example a Linux machine rejoining a saved network at boot). The
specification describes one service; it does not cover several transports on
one device.

### GATT Database Hash

Firmware built with `add_gap_and_gatt` (`impromptu-wifi-embassy`) adds the
Service Changed, Client Supported Features and Database Hash characteristics
of the Bluetooth Core Specification (Vol 3, Part G, 7) to its GATT service.
This is outside the Improv Wi-Fi specification. Rationale:
[ADR-0005](../decisions/0005-gatt-database-hash.md).

## Limits

| Limit | Value | Source |
|---|---|---|
| SSID | 32 bytes | 802.11 |
| Passphrase | 64 bytes (8-63 characters, or 64 hexadecimal digits) | IEEE 802.11-2020, Annex J.4 |
| Redirect URL | 128 bytes | `MAX_URL_LEN` in `impromptu-wifi-traits` |
| Networks per scan answer | 16, strongest first, one per SSID | `SCAN_CAPACITY` in `impromptu-wifi-core` |
