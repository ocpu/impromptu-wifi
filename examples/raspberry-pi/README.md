# Raspberry Pi example

Makes a Raspberry Pi provisionable over Improv Wi-Fi with the
`impromptu-wifi-device` daemon from `crates/impromptu-wifi-linux`: over
Bluetooth LE, a serial port, or both. NetworkManager or wpa_supplicant joins
the network, and systemd-hostnamed provides the names.

| Backend | For | `setup.sh` |
|---|---|---|
| NetworkManager (default) | Raspberry Pi OS 12 (bookworm) and later; older versions after switching | `--backend networkmanager`: switches Raspberry Pi OS 11 and older from dhcpcd to NetworkManager |
| wpa_supplicant | Raspberry Pi OS 11 (bullseye) and older as installed: dhcpcd starts wpa_supplicant for `wlan0` | `--backend wpa-supplicant`: changes nothing in the network setup |

| Model | Rust target | Raspberry Pi OS |
|---|---|---|
| Pi Zero W, Zero WH, Pi 1 | `arm-unknown-linux-musleabihf` (ARMv6) | 32-bit |
| Pi 2, Pi 3, Zero 2 W with a 32-bit OS | `armv7-unknown-linux-musleabihf` | 32-bit |
| Pi 3, Pi 4, Pi 5, Zero 2 W with a 64-bit OS | `aarch64-unknown-linux-musl` | 64-bit |

The binaries are static (musl), so they run on any of these OS versions
without other libraries. `uname -m` on the Pi tells the architecture:
`armv6l`, `armv7l` or `aarch64`.

## Build

On a development machine with rustup; no cross toolchain is needed, since
`.cargo/config.toml` links with the `rust-lld` that rustup ships:

```sh
rustup target add arm-unknown-linux-musleabihf   # or one of the others
cargo build --release -p impromptu-wifi-linux --features daemon \
    --bin impromptu-wifi-device --target arm-unknown-linux-musleabihf
```

The binary is in `target/<target>/release/impromptu-wifi-device`.

## Install

Copy the binary, `setup.sh` and the `dist` directory to the Pi, then run
`setup.sh` as root. From the repository root, with `TARGET` and `PI` set:

```sh
mkdir -p /tmp/impromptu && cp target/$TARGET/release/impromptu-wifi-device \
    examples/raspberry-pi/setup.sh /tmp/impromptu/
cp -r crates/impromptu-wifi-linux/dist /tmp/impromptu/
scp -r /tmp/impromptu $PI:/tmp/
ssh $PI 'cd /tmp/impromptu && sudo ./setup.sh --country SE --ble'
```

`--country` is the Wi-Fi regulatory country (ISO 3166-1 alpha-2); Raspberry
Pi OS keeps Wi-Fi blocked until one is set. `--serial PORT` adds a serial
transport; see [Serial](#serial). `--backend` chooses the backend above, and
`--ignore-wired` lets a Pi on Ethernet offer Bluetooth while its Wi-Fi is
down; see [When Bluetooth is offered](#when-bluetooth-is-offered).

`setup.sh` is safe to run again; each step checks first. What it changes:

| What | Why |
|---|---|
| Sets the Wi-Fi country, if none is set | Raspberry Pi OS blocks Wi-Fi without one |
| NetworkManager backend: installs `network-manager` and switches the Pi to it with `raspi-config nonint do_netconf 2` (Raspberry Pi OS 11 and older); disables `dhcpcd` | The daemon joins networks through NetworkManager. Debian's `NetworkManager.service` declares `Conflicts=dhcpcd.service`, so the two can't share the interfaces across a reboot |
| wpa_supplicant backend: adds the service user to `netdev`; checks that `wpa_supplicant.conf` has `ctrl_interface=... GROUP=netdev` and `update_config=1` | The daemon uses wpa_supplicant's control socket, the one `wpa_cli` uses, and saves joined networks with `SAVE_CONFIG` |
| Starts `bluetoothd` with `--noplugin=midi` (a systemd drop-in) | BlueZ's MIDI plugin reads a connecting client's BLE-MIDI service, which needs encryption. Linux desktops with PipeWire offer one; when they refuse to pair, BlueZ drops the link a few seconds into the session |
| Sets `LEMinAdvertisementInterval=160` and `LEMaxAdvertisementInterval=240` in `/etc/bluetooth/main.conf` | 100 to 150 ms instead of 1.28 s. BlueZ before 5.56 ignores the interval the daemon asks for. With the default, a 5-second scan missed a Pi Zero W about one time in three |
| Installs the daemon, a system user `impromptu-wifi`, a polkit grant, and the systemd unit with a drop-in for the transports | See `crates/impromptu-wifi-linux/dist/README.md`. polkit 0.105 (Raspberry Pi OS 11) reads `.pkla` files, later versions JavaScript `.rules` |

Switching to NetworkManager keeps existing Wi-Fi settings only if
NetworkManager has a profile for them. On a Pi reached over Wi-Fi, run
`setup.sh` from a local console, or over Ethernet or USB networking.

## Provision it

```sh
impromptu-wifi discover
impromptu-wifi --ble <hostname> provision --ssid home
```

Home Assistant's Improv via BLE integration finds it too. No button press is
needed: the Pi has no button, so the daemon starts authorized.

## When Bluetooth is offered

Only while the Pi is offline, and only until it is online again, through
Improv Wi-Fi, a saved network rejoined at boot, or anything else. Without an
authorization step, this keeps anyone in radio range from re-pointing a
working machine.

Online means NetworkManager's global state is connected, or, with the
wpa_supplicant backend, that Wi-Fi is associated with an address or the Pi
has an IPv4 default route. So Ethernet counts: a Pi connected by cable
doesn't offer Bluetooth, even without Wi-Fi. `--ignore-wired` counts Wi-Fi
only, so a wired Pi offers Bluetooth until its Wi-Fi is up; anyone in radio
range can then set its Wi-Fi network. At boot the daemon usually starts before
NetworkManager has rejoined a saved network; it advertises for those seconds
and stops when the Pi comes online.

## Serial

* **USB gadget mode** (Pi Zero, Zero W, Zero 2 W, Pi 4 on its USB-C port):
  the Pi appears to the computer as a serial port, `/dev/ttyGS0` on the Pi.
  Enable the `dwc2` overlay and the `g_serial` module, then pass
  `--serial /dev/ttyGS0`. The port is only usable while the OTG port is in
  device mode, not while a hub or adapter is plugged into it.
* **GPIO UART**: `/dev/serial0`, after disabling the serial console with
  `raspi-config` (Interface Options, Serial Port: login shell no, hardware
  yes).

Neither serial setup has been tested on hardware with this example.

## Tested

On 2026-11-08, on a Raspberry Pi Zero W Rev 1.1 with Raspbian GNU/Linux 11
(bullseye), BlueZ 5.55, NetworkManager 1.30.6, reached over a USB Ethernet
adapter:

* `setup.sh --country SE --ble`, and again on the configured Pi.
* Provisioning over BLE with the `impromptu-wifi` CLI from a Linux laptop
  (BlueZ 5.87, PipeWire): joined in 8 s; Bluetooth stopped.
* Reboot: Ethernet and Wi-Fi up through NetworkManager; the daemon advertised
  for 11 s at boot, until NetworkManager rejoined the saved network.
* `nmcli networking off` for 40 s: Bluetooth offered; when networking
  returned, advertising stopped at once.
* Switched back to dhcpcd and wpa_supplicant (`raspi-config nonint
  do_netconf 1`), `setup.sh --backend wpa-supplicant --ignore-wired`:
  a failed join (a missing network) was reported after 12 s and left nothing
  behind; a join over BLE took 8 s and was written to
  `wpa_supplicant.conf`, replacing the older entry for the SSID; after a
  reboot, dhcpcd's wpa_supplicant rejoined it and NetworkManager stayed off.

Not tested: the Pi 3B, 4 and 5, Raspberry Pi OS 12 (bookworm) and later,
serial, and provisioning from Home Assistant or a phone.

### A pitfall found on the way

The systemd unit used to have `Wants=NetworkManager.service`. On a dhcpcd
system, starting the daemon then started NetworkManager, whose conflict with
dhcpcd stopped dhcpcd and the wpa_supplicant it runs. The unit now only
orders itself after NetworkManager.
