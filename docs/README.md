# impromptu-wifi documentation

Documentation for impromptu-wifi, an implementation of the Improv Wi-Fi
provisioning protocol in Rust. The [repository README](../README.md) is the
entry point; this page lists the rest by what the reader needs.

## Learn and set up

| Document | Covers |
|---|---|
| [ESP32-S3 example](../examples/esp32s3/README.md) | Build and flash firmware that is provisioned over USB serial and Bluetooth LE; its lifecycle, status LED and Bluetooth address |
| [Raspberry Pi example](../examples/raspberry-pi/README.md) | Build the Linux daemon for a Raspberry Pi, install it with `setup.sh`, and provision the Pi |
| [Running the daemon as a service](../crates/impromptu-wifi-linux/dist/README.md) | Install `impromptu-wifi-device` on any Linux machine with systemd |

## How-to guides

| Document | Covers |
|---|---|
| [Troubleshoot Bluetooth LE provisioning](how-to/troubleshoot-bluetooth.md) | Devices not found, dropped connections, pairing prompts, stale caches |

## Reference

| Document | Covers |
|---|---|
| [`impromptu-wifi` command-line tool](reference/cli.md) | Commands, options, exit status |
| [`impromptu-wifi-device` daemon](reference/impromptu-wifi-device.md) | Options, backends, files, systemd unit, logs |
| [Support matrix](../SUPPORT.md) | Devices, operating systems, clients and build targets, and how far each is verified |
| API documentation | `cargo doc --open --features <features>` in the repository; the crate-level documentation of each crate describes its public API |

## Explanation

| Document | Covers |
|---|---|
| [Architecture](explanation/architecture.md) | Crates, layers, and how a command flows from a transport to a Wi-Fi backend |
| [Security model](explanation/security-model.md) | Authorization, when Bluetooth LE is offered, handling of credentials |
| [Protocol conformance](explanation/protocol-conformance.md) | How the implementation maps to the Improv Wi-Fi specification, and where it goes beyond it |

## Decisions

Architecture decision records, in [MADR 4.0.0](https://adr.github.io/madr/)
format, in [`decisions/`](decisions/):

| ADR | Decision |
|---|---|
| [0001](decisions/0001-use-zbus-for-bluez.md) | Talk to BlueZ over D-Bus with zbus instead of libdbus-based crates |
| [0002](decisions/0002-offer-bluetooth-only-while-offline.md) | Offer Bluetooth LE provisioning on Linux only while the machine is offline |
| [0003](decisions/0003-report-unprovisioned-after-a-lost-network.md) | Report a lost network as a return from `Provisioned` |
| [0004](decisions/0004-random-static-ble-address-on-esp32.md) | Advertise from a random static address on the ESP32 |
| [0005](decisions/0005-gatt-database-hash.md) | Expose a GATT Database Hash from trouble-host firmware |
| [0006](decisions/0006-static-musl-builds-for-raspberry-pi.md) | Ship static musl builds for the Raspberry Pi |
| [0007](decisions/0007-wpa-supplicant-control-socket-backend.md) | Support systems without NetworkManager through wpa_supplicant's control socket |
