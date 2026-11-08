# `impromptu-wifi-device` daemon

`impromptu-wifi-device` makes a Linux machine provisionable over Improv Wi-Fi,
over a serial port, Bluetooth LE, or both. It is the binary of the
`impromptu-wifi-linux` crate, built with the `daemon` feature:

```sh
cargo build --release -p impromptu-wifi-linux --features daemon --bin impromptu-wifi-device
```

For installation as a systemd service see
[`crates/impromptu-wifi-linux/dist/README.md`](../../crates/impromptu-wifi-linux/dist/README.md);
for a Raspberry Pi see [`examples/raspberry-pi`](../../examples/raspberry-pi).

## Requirements

| Needed | For |
|---|---|
| systemd-hostnamed (D-Bus `org.freedesktop.hostname1`) | Device info, hostname and device name. Always. |
| NetworkManager, or wpa_supplicant with a control socket | Joining networks. See `--backend`. |
| BlueZ (D-Bus `org.bluez`) with an LE adapter | `--ble`. |
| A serial device | `--serial`. |

## Options

At least one of `--serial` and `--ble` is required; both may be given.

| Option | Default | Meaning |
|---|---|---|
| `--serial <PATH>` | - | Serve Improv on this serial port, for example `/dev/ttyGS0` (USB gadget mode) or `/dev/serial0` (UART). If it can't be opened, retry every 2 s; a gadget port appears only once a USB host enumerates it. |
| `--ble` | off | Offer Improv over Bluetooth LE while the machine is offline; see [When Bluetooth is offered](#when-bluetooth-is-offered). |
| `--baud <BAUD>` | `115200` | Serial baud rate. |
| `--interface <NAME>` | first one | Wi-Fi interface. With wpa_supplicant, the first control socket in `--wpa-ctrl-dir` that is not a P2P device. |
| `--backend <BACKEND>` | `auto` | `networkmanager`, `wpa-supplicant`, or `auto`: NetworkManager if it holds its D-Bus name, otherwise wpa_supplicant. |
| `--wpa-ctrl-dir <DIR>` | `/var/run/wpa_supplicant` | wpa_supplicant's control socket directory (its `ctrl_interface`). |
| `--ignore-wired` | off | Count the machine as offline while Wi-Fi is, even when it is online through Ethernet or another interface. |
| `--firmware-name <NAME>` | `impromptu-wifi-linux` | Firmware name reported in device info. |
| `--firmware-version <VERSION>` | the crate version | Firmware version reported in device info. |

## Backends

### NetworkManager

- Scans through the Wi-Fi device; joins by creating and activating a
  connection profile named after the SSID.
- On success, removes older profiles for the same SSID; on failure, removes
  the profile it created.
- Needs a polkit grant for NetworkManager actions (see [Files](#files)).

### wpa_supplicant

- Uses the control socket that `wpa_cli` uses, `<--wpa-ctrl-dir>/<interface>`.
  The service user must be in the socket's group (`GROUP=` in
  `ctrl_interface=`, `netdev` on Debian).
- Joins with `ADD_NETWORK`, `SET_NETWORK` and `SELECT_NETWORK`. A join
  succeeds on `CTRL-EVENT-CONNECTED` followed by an IPv4 address (from
  whatever runs DHCP, such as dhcpcd); fails on a wrong key, or after three
  scans that don't find the network; times out after 30 s to associate and
  20 s for an address.
- On success, removes older networks with the same SSID and saves with
  `SAVE_CONFIG`, which needs `update_config=1`. On failure, removes the network
  and re-enables the others.
- Requests WPA3 (SAE) as well as WPA2 where wpa_supplicant accepts it.

## When Bluetooth is offered

With `--ble`, the daemon advertises only while the machine is offline, and
stops as soon as it is online, however that happened.

| Backend | Online means | With `--ignore-wired` |
|---|---|---|
| NetworkManager | Its global state is `CONNECTED_LOCAL` or better, through any interface it manages | The Wi-Fi device is activated |
| wpa_supplicant | wpa_supplicant is associated and the interface has an address, or the machine has an IPv4 default route | wpa_supplicant is associated with an address |

After a successful session the daemon stops advertising at once, and keeps
the GATT service registered 5 s longer so the client can read the result.
The machine starts authorized, since it has no button; see
[Security model](../explanation/security-model.md).

## Serial transport

The serial driver runs continuously. It reports:

- `Stopped` while NetworkManager's Wi-Fi radio is switched off (NetworkManager
  backend only);
- `Provisioned` when the machine comes online by any means, and the idle state
  again when it goes offline;
- joins made over Bluetooth LE.

## Files

From `crates/impromptu-wifi-linux/dist/`:

| File | Install to | Purpose |
|---|---|---|
| `impromptu-wifi-device.service` | `/etc/systemd/system/` | The systemd unit. Edit `ExecStart` or add a drop-in for the options. |
| `50-impromptu-wifi-device.rules` | `/etc/polkit-1/rules.d/` | polkit 0.106 and later: lets the user `impromptu-wifi` run NetworkManager actions and set the static hostname and machine info. |
| `impromptu-wifi-device.pkla` | `/etc/polkit-1/localauthority/50-local.d/` | The same grant for polkit 0.105. |

The unit runs the daemon as the user `impromptu-wifi` (group `dialout`,
supplementary group `bluetooth`), with `NoNewPrivileges=yes`,
`ProtectSystem=strict`, `ProtectHome=yes`, `PrivateTmp=yes` and
`RestrictAddressFamilies=AF_UNIX`. It orders itself after NetworkManager,
dhcpcd, wpa_supplicant, systemd-hostnamed and bluetooth, without starting
any of them.

## Logs

One line per event on standard error, which the journal keeps:
`journalctl -u impromptu-wifi-device`.

| Message | Meaning |
|---|---|
| `info: Wi-Fi through NetworkManager` / `wpa_supplicant on <interface>` | The backend in use. |
| `info: offline: offering Improv over Bluetooth on <adapter>` | Advertising started. |
| `info: joining "<ssid>"` / `joined "<ssid>", reachable at <url>` | A join and its result. |
| `warn: joining "<ssid>" failed: <reason>` | The join failed; the client sees `Unable to connect`. |
| `info: online; reachable at <url>` | The machine is online; Bluetooth stops. |
| `info: stopped offering Improv over Bluetooth` | Advertising and the GATT service are gone. |
| `info: serving Improv on <path>` / `warn: serial link ended: ...; reopening` | Serial transport. |
| `error: registering the Improv service: the Bluetooth adapter is blocked by rfkill; ...` | Unblock Bluetooth, for example `rfkill unblock bluetooth`. |
| `error: no wpa_supplicant control socket in <dir>` | wpa_supplicant isn't running there, or the user can't read the directory. |

On an `error:` line the daemon exits with status 1, and the unit restarts it
after 5 s.
