# Provision a device from a browser

The project site, <https://ocpu.github.io/impromptu-wifi/>, runs the
impromptu-wifi client in the browser. It is the `impromptu-wifi-web` crate
compiled to WebAssembly, and offers the commands of the
[`impromptu-wifi` command-line tool](../reference/cli.md): state, device
information, Wi-Fi scan, provisioning, identify, hostname, device name and
network state.

The network name and passphrase go only to the chosen device. The page
loads no third-party scripts and stores nothing; the passphrase field is
cleared after each attempt.

## Prerequisites

- A browser with the Web API for the transport. The page shows a button only
  for an API the browser provides.

  | Transport | Browser API | Status |
  |---|---|---|
  | USB serial | [Web Serial](https://developer.mozilla.org/en-US/docs/Web/API/Web_Serial_API) | See the [support matrix](../reference/support-matrix.md) |
  | Bluetooth LE | [Web Bluetooth](https://developer.mozilla.org/en-US/docs/Web/API/Web_Bluetooth_API) | See the [support matrix](../reference/support-matrix.md) |

  Both APIs work only on pages served over HTTPS or from `localhost`. The
  MDN pages linked above list which browsers provide each API.
- A device that offers Improv Wi-Fi: over Bluetooth LE it advertises only
  while it is not provisioned; a Linux machine running
  `impromptu-wifi-device` advertises only while it is offline (see the
  [security model](../explanation/security-model.md)).

## Connect

1. Open <https://ocpu.github.io/impromptu-wifi/#try>.
2. Select **Connect over USB serial** or **Connect over Bluetooth**.
3. Choose the device in the browser's list.

   Over serial the page opens the port at 115200 baud, leaves the DTR and
   RTS lines as the browser sets them (as the command-line tool does), and
   discards output that the device sent before the port was opened. Over
   Bluetooth the list shows only devices that advertise the Improv service.

If you close the list without choosing, the page stays at the connect
buttons. One command runs at a time; the buttons are disabled until it
finishes.

## Provision

1. Optional: select **Scan**, then select a network to fill in its name.
2. Enter the network name and the passphrase, or select **Open network**.
3. Select **Connect device to Wi-Fi**.

   If the device requires authorization, which is a Bluetooth LE option of
   the Improv specification, the page asks you to press the button on the
   device and waits up to 60 seconds.

The page shows the address the device reports, if any. If the device cannot
join, the page scans and explains the likely cause, in the same way as the
command-line tool: a network that the device cannot see, a missing or
unexpected passphrase, or a WPA3-only network.

## Other commands

| Button | Does | Transport |
|---|---|---|
| **State** | Shows the device state; over Bluetooth also its optional commands | Both |
| **Info** | Firmware name and version, hardware, operating system | Both |
| **Identify** | Asks the device to show itself, for example by blinking | Bluetooth LE |
| **Network** | Connectivity, interfaces and addresses | Serial |
| **Hostname**, **Device name**: **Read** / **Set** | Reads or sets the name | Both, if the device supports it |

Each command waits up to 10 seconds for an answer.

## Troubleshooting

| Symptom | Cause and action |
|---|---|
| No connect button | The browser does not provide the API; the page says which one. Use a browser that provides it. |
| The Bluetooth list is empty | The device is not advertising: it is provisioned, or (Linux daemon) online, or out of range. See [Troubleshoot Bluetooth LE provisioning](troubleshoot-bluetooth.md). |
| "the device disconnected" | The cable was unplugged, the device restarted or went out of range. Connect again. |
| "No answer from the device within 10 s" | The serial port belongs to another device, or the firmware does not run Improv on that port. |

Bluetooth LE from desktop Linux browsers has not been tested.
