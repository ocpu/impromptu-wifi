# Running impromptu-wifi-device as a service

`impromptu-wifi-device` serves Improv on a serial port, over Bluetooth LE, or
both, with NetworkManager or wpa_supplicant (`--backend`) for Wi-Fi and
systemd-hostnamed for the hostname and device name. It needs no C library: serial ports and BlueZ's D-Bus API
are used from Rust. For a Raspberry Pi, `examples/raspberry-pi` has static
builds and a `setup.sh` that does the steps below.

```sh
cargo build --release -p impromptu-wifi-linux --features daemon
sudo install target/release/impromptu-wifi-device /usr/local/bin/

# A dedicated user that may use serial ports and Bluetooth, and nothing else.
sudo useradd --system --no-create-home --groups dialout,bluetooth impromptu-wifi

# Allow it to change Wi-Fi and the hostname. polkit 0.106 and later:
sudo install -m 644 crates/impromptu-wifi-linux/dist/50-impromptu-wifi-device.rules /etc/polkit-1/rules.d/
# polkit 0.105 (Debian 11 and older; `pkaction --version` tells) instead:
# sudo install -m 644 crates/impromptu-wifi-linux/dist/impromptu-wifi-device.pkla /etc/polkit-1/localauthority/50-local.d/

sudo install -m 644 crates/impromptu-wifi-linux/dist/impromptu-wifi-device.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now impromptu-wifi-device
journalctl -u impromptu-wifi-device -f
```

Edit `ExecStart` for your port. On a Raspberry Pi Zero in USB gadget mode
(the `g_serial` module, or a ConfigFS ACM function) it's `/dev/ttyGS0`; for
the UART it's usually `/dev/serial0`, with the serial console disabled.

Then, from another machine: `impromptu-wifi --port /dev/ttyACM0 provision --ssid …`,
or over Bluetooth: `impromptu-wifi discover`, then `impromptu-wifi --ble <name> provision --ssid …`.

Bluetooth is offered only while the machine is offline, and stops as soon as
it is online, whether through Improv Wi-Fi or any other way (a saved network
rejoined at boot, a cable): there's no button to authorize with, so this
keeps anyone in radio range from re-pointing a working machine. With
NetworkManager, offline is its global state; interfaces it doesn't manage
don't count. With wpa_supplicant, online is Wi-Fi associated with an
address, or an IPv4 default route through any interface. `--ignore-wired`
counts Wi-Fi only.

Without NetworkManager (`--backend wpa-supplicant`, or `auto` when
NetworkManager isn't running), the daemon uses wpa_supplicant's control
socket in `/var/run/wpa_supplicant` (`--wpa-ctrl-dir`). Add the service user
to the socket's group, `netdev` on Debian
(`sudo usermod -aG netdev impromptu-wifi`), and set `update_config=1` in
`wpa_supplicant.conf` so joined networks are saved. The address comes from
whatever runs DHCP, such as dhcpcd. No polkit rule is needed for Wi-Fi then. If your distribution's BlueZ policy doesn't grant the `bluetooth`
group access, the service needs to run as root for `--ble`.

What the service does to NetworkManager: a successful join creates a saved
profile named after the SSID and removes older profiles for the same SSID;
a failed join removes the profile it created.
