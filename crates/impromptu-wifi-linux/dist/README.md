# Running impromptu-wifi-device as a service

`impromptu-wifi-device` serves Improv on a serial port, over Bluetooth LE, or
both, with NetworkManager for Wi-Fi and systemd-hostnamed for the hostname
and device name. Building needs `libdbus-1-dev`, `libudev-dev` and
`pkg-config`.

```sh
cargo build --release -p impromptu-wifi-linux --features daemon
sudo install target/release/impromptu-wifi-device /usr/local/bin/

# A dedicated user that may use serial ports and Bluetooth, and nothing else.
sudo useradd --system --no-create-home --groups dialout,bluetooth impromptu-wifi

# Allow it to change Wi-Fi and the hostname (polkit 0.106+).
sudo install -m 644 crates/impromptu-wifi-linux/dist/50-impromptu-wifi-device.rules /etc/polkit-1/rules.d/

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

Bluetooth is offered only while the machine is offline and stops once it's
provisioned, as the Improv spec describes: there's no button to authorize
with, so this keeps anyone in radio range from re-pointing a working
machine. If your distribution's BlueZ policy doesn't grant the `bluetooth`
group access, the service needs to run as root for `--ble`.

What the service does to NetworkManager: a successful join creates a saved
profile named after the SSID and removes older profiles for the same SSID;
a failed join removes the profile it created.
