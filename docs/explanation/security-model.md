# Security model

## What is protected

A device being provisioned accepts a Wi-Fi network name and passphrase and
joins that network. The two risks are:

- **Re-pointing:** someone other than the owner makes the device join a
  network of their choosing, so its traffic goes through their access point.
- **Disclosure:** the passphrase of the owner's network is read in transit,
  from storage, or from logs.

The Improv Wi-Fi specification addresses the first risk with optional
physical authorization on BLE. It defines no encryption for either transport.

## Authorization by transport

| Transport | Specification | This implementation |
|---|---|---|
| Serial | No authorization; the serial specification has no Authorization Required state. Physical access to the port is assumed. | Same. |
| BLE, device with a button | The device may require a physical interaction. The authorization is revoked after a timeout chosen by the device; the specification suggests 1 minute. A failed join, and a hostname or device-name write, restart the timeout. | `ButtonAuth` in `impromptu-wifi-embassy`: a press authorizes for the configured time (60 s in the ESP32-S3 example), and the restarts happen as specified. |
| BLE, device without a button | A device that does not require authorization starts in the Authorized state. | The Linux daemon starts authorized, so it limits *when* BLE is offered instead; see below. |

Every write command (Wi-Fi settings, hostname, device name) passes through
one check in the `Machine`. A write is refused with `Not Authorized` while
authorization is required and absent, in every state, including
`Provisioned`.

Source: Improv Wi-Fi BLE specification, <https://www.improv-wifi.com/ble/>;
serial specification, <https://www.improv-wifi.com/serial/>.

## When BLE is offered

### Firmware

The ESP32-S3 example advertises while it is not provisioned and stops when it
is, as the BLE specification describes for a provisioned device. Writes need
a press of the BOOT button.

### Linux daemon

A Linux machine has no button, so anyone in radio range could provision it
while it advertises. To limit this, `impromptu-wifi-device`:

- offers BLE only while the machine is offline;
- stops advertising as soon as the machine is online, through Improv Wi-Fi or
  any other way, such as a saved network rejoined at boot;
- counts any interface as online by default, Ethernet included.

`--ignore-wired` counts Wi-Fi only. A wired machine then offers BLE until its
Wi-Fi is up, to anyone in radio range. Use it only where that is acceptable.

Rationale: [ADR-0002](../decisions/0002-offer-bluetooth-only-while-offline.md).

## Credentials

| Where | Handling |
|---|---|
| In transit, serial | Plain bytes on the serial link. |
| In transit, BLE | The specification defines no encryption, and this implementation does not require an encrypted link. The passphrase is readable by anyone who captures the BLE traffic during provisioning. |
| On the command line | `impromptu-wifi provision` prompts for the passphrase when it is not given. `--password` puts it in the shell history and the process list. |
| In logs | The Linux daemon logs network names, not passphrases. The wpa_supplicant backend redacts every `SET_NETWORK` value except the SSID from messages and logs. |
| Stored, ESP32 firmware | `FlashStore` writes the passphrase in plain text to one flash sector, as ESP-IDF's NVS does by default. Flash encryption protects it where the chip has it enabled. |
| Stored, Linux | By NetworkManager (a keyfile under `/etc/NetworkManager/system-connections/`) or by wpa_supplicant (`wpa_supplicant.conf`), with the permissions those programs set. |

## Linux service privileges

The systemd unit in `crates/impromptu-wifi-linux/dist/` runs the daemon as
the unprivileged user `impromptu-wifi`, with `NoNewPrivileges=yes`,
`ProtectSystem=strict`, `ProtectHome=yes`, `PrivateTmp=yes` and
`RestrictAddressFamilies=AF_UNIX`. It reaches the network services over
D-Bus or a UNIX socket only:

- A polkit grant lets that user run NetworkManager actions and set the static
  hostname and machine information, and nothing else
  (`50-impromptu-wifi-device.rules`; `impromptu-wifi-device.pkla` for polkit
  0.105).
- With the wpa_supplicant backend, membership of the control socket's group
  (`netdev` on Debian) gives access to wpa_supplicant.
- Membership of `bluetooth` gives access to BlueZ, and `dialout` to serial
  ports.

## Bluetooth identity

- The ESP32-S3 firmware advertises from a random static address derived from
  the chip, not the chip's public address. Clients that cached another
  firmware's GATT layout for the public address then don't use it.
  [ADR-0004](../decisions/0004-random-static-ble-address-on-esp32.md).
- Firmware built with `add_gap_and_gatt` exposes a GATT Database Hash, so
  clients that check it notice a changed layout.
  [ADR-0005](../decisions/0005-gatt-database-hash.md).
- The Raspberry Pi setup turns off BLE-MIDI in BlueZ and WirePlumber. Their
  services need encryption, and reading them made sessions with Linux clients
  start pairing; see [Troubleshoot Bluetooth LE
  provisioning](../how-to/troubleshoot-bluetooth.md).

## Not covered

- Encryption or authentication of the Improv Wi-Fi exchange itself; the
  specification has none.
- Enterprise (802.1X) networks; the protocol sends an SSID and a passphrase
  only.
