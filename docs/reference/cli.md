# `impromptu-wifi` command-line tool

`impromptu-wifi` provisions and queries Improv Wi-Fi devices over a serial port
or Bluetooth LE. It is the `impromptu-wifi-cli` crate.

```sh
cargo build --release -p impromptu-wifi-cli
./target/release/impromptu-wifi --help
```

On Linux, Bluetooth LE goes through BlueZ over D-Bus (the `bluetooth` service
must run); on macOS and Windows through btleplug. No C library is needed to
build.

## Synopsis

```text
impromptu-wifi [OPTIONS] <COMMAND>
```

Choose the device with `--port` (serial) or `--ble` (Bluetooth LE). The two
are mutually exclusive. `ports` and `discover` need neither.

## Global options

| Option | Default | Meaning |
|---|---|---|
| `-p`, `--port <PORT>` | - | Serial port of the device, for example `/dev/ttyACM0`. See `ports`. |
| `-b`, `--ble <BLE>` | - | Bluetooth device: its address, or part of its name. The tool scans for up to 10 s and takes the strongest match. See `discover`. |
| `--baud <BAUD>` | `115200` | Serial baud rate. |
| `--reset` | off | Serial only: reset the device first, for ESP32 boards with the usual DTR/RTS auto-reset wiring, then wait 1.5 s for it to boot. |
| `--timeout <SECONDS>` | `10` | How long to wait for an answer. Joining a network, and waiting for a button press, always allow at least 60 s. |
| `-h`, `--help` | | Print help. |
| `-V`, `--version` | | Print the version. |

Opening a serial port leaves DTR and RTS as the operating system sets them,
so boards with auto-reset wiring are not reset by connecting. Output that the
device sent before the port was opened is read and dropped first (until the
port is silent for 100 ms, at most 1 s), so the first answer is current.

## Commands

### `ports`

List serial ports, with USB product, manufacturer and vendor/product ID where
known.

### `discover [--seconds <SECONDS>]`

Scan for Improv Wi-Fi devices over Bluetooth LE for `SECONDS` (default 5) and
list those seen during the scan, strongest first: address, signal strength,
the advertised state, and the name.

```text
$ impromptu-wifi discover
Scanning for 5s...
D4:C1:9F:5C:52:8C   -46 dBm  needs authorization        Impromptu
```

A device that is provisioned stops advertising, so it is not listed.

### `state`

Print the device's state, and on BLE its capabilities.

```text
$ impromptu-wifi --ble Impromptu state
Looking for Impromptu...
Waiting for authorization: press the device's button
Supports: identify, device info, scan
```

### `info`

Print the firmware name and version, hardware, device name, and, where the
device reports them, the OS name and version.

### `scan`

Ask the device which Wi-Fi networks it sees, and print signal strength,
security and name. Over serial the security is only "secured" or "open";
over BLE it is the authentication type the device reports.

### `provision --ssid <SSID> [--password <PASSWORD> | --open]`

Send Wi-Fi settings and wait for the result. On success, print the URL the
device reports, if any.

```text
$ impromptu-wifi --port /dev/ttyACM0 --timeout 70 provision --ssid home
Passphrase for home:
Sending credentials for home...
Connected. The device is at http://192.168.0.234
```

The passphrase comes from, in order:

1. `--open`: none, for an open network.
2. `--password`: the value given. This puts it in the shell history and the
   process list.
3. A prompt, when standard input is a terminal.
4. Otherwise the first line of standard input, for scripts.

If the device requires authorization (BLE), the tool asks for the button to
be pressed and waits for the device to become authorized.

If the join fails, the tool asks the device to scan and explains the failure
from what it sees: a network it can't see (name, range, 2.4 GHz), an open
network sent a passphrase, a secured network sent none, a WPA3-only network,
or otherwise the passphrase or security mode.

### `identify`

Ask the device to identify itself, for example by blinking a light. Bluetooth
LE only.

### `hostname [NAME]`

Print the hostname, or set it to `NAME`. A hostname may contain letters,
digits and hyphens, up to 255 characters, and may not start or end with a
hyphen.

### `name [NAME]`

Print the device name, or set it to `NAME`.

### `network`

Print whether the device is online, which network interfaces it has, and the
URLs it can be reached at. Serial only.

```text
$ impromptu-wifi --port /dev/ttyACM0 network
Online:   yes
Supports: Wi-Fi
URL:      http://192.168.0.234
```

## Exit status

| Status | Meaning |
|---|---|
| `0` | The command succeeded. |
| `1` | It failed. A message starting with `error:` on standard error says why. |

## Errors

| Message | Meaning |
|---|---|
| `no Improv device matching "<query>" is advertising` | No device matching the address or name was seen within 10 s. A provisioned device stops advertising. |
| `the device could not join "<ssid>" ...` | The join failed; the rest of the message is the explanation described under `provision`. |
| `the device needs authorization; press its button and try again` | A write was refused because the device was not authorized. |
| `the device doesn't support this command` | The device does not implement the command (for example `hostname` on the ESP32-S3 example). |
| `no answer from the device within <n>s` | Raise `--timeout`, or check the connection. |
| `starting Bluetooth; check that the bluetooth service (BlueZ) is running and an adapter is present: ...` | Linux: BlueZ is not reachable or has no adapter. On macOS and Windows the message asks to check that Bluetooth is on and the program may use it. |

For Bluetooth problems such as dropped connections, see
[Troubleshoot Bluetooth LE provisioning](../how-to/troubleshoot-bluetooth.md).
