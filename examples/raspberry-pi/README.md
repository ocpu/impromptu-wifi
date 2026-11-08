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
| Switches NetworkManager's Wi-Fi radio on, if it is off | Fresh Raspberry Pi OS 13 images start with it off |
| Unblocks Bluetooth in `/sys/class/rfkill` (with `--ble`) | Fresh Raspberry Pi OS 13 images can start with Bluetooth soft-blocked; BlueZ then can't power the adapter. systemd-rfkill keeps the change |
| Starts `bluetoothd` with `--noplugin=midi` (a systemd drop-in) | BlueZ's MIDI plugin reads a connecting client's BLE-MIDI service, which needs encryption. Linux desktops with PipeWire offer one; when they refuse to pair, BlueZ drops the link a few seconds into the session |
| Turns off WirePlumber's BLE-MIDI, on images with PipeWire (`monitor.bluez-midi = disabled` for WirePlumber 0.5, `bluez_midi_monitor.enabled = false` for 0.4) and restarts it | The same in the other direction: WirePlumber offers a BLE-MIDI service and reads a client's. On a Pi 3B with the desktop image, each session from a Linux laptop turned into a pairing attempt, and the laptop's BlueZ 5.87 crashed on the refusal. The 0.4 setting is untested |
| Sets the default advertising interval in `/etc/bluetooth/main.conf` to 100-150 ms (`LEMinAdvertisementInterval=160` and `LEMaxAdvertisementInterval=240` in BlueZ 5.55; `MinAdvertisementInterval` and `MaxAdvertisementInterval` in the `[LE]` section in 5.66 and later; units of 0.625 ms) | BlueZ 5.55 and 5.66 ignore the interval the daemon asks for and advertise every 1.28 s. A 5-second scan then missed a Pi Zero W about one time in three, and a laptop missed a Pi 4 entirely. BlueZ 5.82 (Raspberry Pi OS 13) uses the daemon's interval |
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

On 2026-11-08, on a Raspberry Pi 5 Model B Rev 1.0 with Raspberry Pi OS 13
(trixie, 2025-10-01 image, desktop), BlueZ 5.82, NetworkManager 1.52, reached
over Ethernet:

* `setup.sh --country SE --ble --ignore-wired`: NetworkManager was already in
  use, so the network setup was left as it was.
* With Wi-Fi disconnected (`nmcli device disconnect wlan0`), the daemon
  advertised at the 100-150 ms it asks for.
* Home Assistant (Improv via BLE, through an ESPHome Bluetooth proxy)
  provisioned it: joined in 7 s, Bluetooth stopped 5 s later.

On 2026-11-08, on a Raspberry Pi 4 Model B Rev 1.1 with Raspberry Pi OS 12
(bookworm, 2024-07-04 image, desktop), BlueZ 5.66, NetworkManager 1.42,
reached over Ethernet:

* `setup.sh --country SE --ble --ignore-wired`, with Wi-Fi not connected.
* Before the `[LE]` interval was set, a laptop's scans never found it; after,
  5 of 5 did.
* Home Assistant (Improv via BLE, through an ESPHome Bluetooth proxy)
  provisioned it: joined in 7 s, Bluetooth stopped 5 s later.

On 2026-11-08, on a Raspberry Pi 3 Model B Rev 1.2 with a fresh Raspberry Pi
OS 13 (trixie, 2026-10-06 image, desktop), BlueZ 5.82, NetworkManager 1.52,
reached over Ethernet, on a power supply too weak for it (under-voltage
reported throughout):

* `setup.sh --country SE --ble --ignore-wired` had to switch the Wi-Fi radio
  on, unblock Bluetooth, and turn off WirePlumber's BLE-MIDI.
* Provisioning over BLE with the `impromptu-wifi` CLI from a Linux laptop:
  joined in 4 s, Bluetooth stopped 5 s later.

Not tested: serial, provisioning from a phone, and whether turning off
WirePlumber's BLE-MIDI also helps with the known issue below.

On Raspberry Pi OS 12, NetworkManager leaves a USB gadget's `usb0`
unmanaged, so a profile for it is not applied without
`nmcli device set usb0 managed yes`.

### Known issue: a laptop's MediaTek Bluetooth and the Pi 4 and 5

From one laptop (MediaTek Bluetooth `0e8d:0717`, BlueZ 5.87) every
connection to the Pi 5 dropped within 1-2 seconds, and every connection to
the Pi 4 timed out; both have a Cypress CYW43455. On the Pi 5 the Pi
reported Connection Timeout (0x08): the laptop stopped answering shortly
after the two had switched to longer packets (LE Data Length Extension). The `impromptu-wifi`
CLI then fails with "timed out connecting" or ATT error 0x0e. Both Pis ran
the desktop image, with WirePlumber's BLE-MIDI on; `setup.sh` did not turn
it off yet at the time. It is not the
Pi 5 alone: from the Pi Zero W (BlueZ 5.55) the link held, and Home
Assistant's ESP32 proxy provisioned it. It is not power or USB interference
either: the drops stayed the same on the Pi's own power supply, and with its
Wi-Fi off. If a client shows this, provision from another one.

### A pitfall found on the way

The systemd unit used to have `Wants=NetworkManager.service`. On a dhcpcd
system, starting the daemon then started NetworkManager, whose conflict with
dhcpcd stopped dhcpcd and the wpa_supplicant it runs. The unit now only
orders itself after NetworkManager.
