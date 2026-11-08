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
2. Select **Connect over USB** or **Connect over Bluetooth**.
3. Choose the device in the browser's list.

   Over USB the page opens the port at 115200 baud, leaves the DTR and RTS
   lines as the browser sets them (as the command-line tool does), and
   discards output that the device sent before the port was opened. Over
   Bluetooth the list shows only devices that advertise the Improv service.

The page then reads the device's state and information, and over USB its
network state, and asks the device to scan for networks. If you close the
list without choosing, the page stays at step 1. One command runs at a time;
the controls are disabled until it finishes.

## Provision

1. In **Networks the device can see**, select the network. For a hidden
   network, or one that is not listed, select **Other network** and type its
   name.
2. Enter the passphrase. For an open network no passphrase field is shown;
   under **Other network**, select **Open network**.
3. Select **Connect device**.

   If the device requires authorization, which is a Bluetooth LE option of
   the Improv specification, the page asks you to press the button on the
   device and waits up to 60 seconds. Joining can take up to 60 seconds more.

When the device reports an address, the page shows it with **Open the
device**. If the device cannot join, the page scans and explains the likely
cause, in the same way as the command-line tool, and offers the next step:

| Cause | Offered action |
|---|---|
| The device cannot see the network | Use a network with a similar name, if the device sees one; choose another network |
| The network is open, but a passphrase was sent | Connect without a passphrase |
| A passphrase is needed, or the network did not accept the device, or it is WPA3-only | Re-enter the passphrase |
| The button was not pressed within 60 s | Send again |

## Device details

| Control | Does | Transport |
|---|---|---|
| State badge | The device state: ready, waiting for authorization, connecting, provisioned | Both |
| **Device** panel | Firmware name and version, hardware, operating system; over USB also connectivity and addresses | Both |
| **Identify** | Asks the device to show itself, for example by blinking | Bluetooth LE, if the device supports it |
| **Names** | Reads and sets the device name and hostname | Both, if the device supports it |
| **Scan again** | Lists the networks again | Both |

Each command waits up to 10 seconds for an answer.

## Troubleshooting

| Message | Cause and action |
|---|---|
| "This browser can't talk to devices" | The browser provides neither API. Use a browser from the table above. |
| "Bluetooth is off or unavailable" | The browser found no Bluetooth adapter it can use. Turn Bluetooth on, or allow the browser to use it. |
| "Don't see your device in the list?" | The device is not advertising: it is provisioned, or (Linux daemon) online, or out of range. See [Troubleshoot Bluetooth LE provisioning](troubleshoot-bluetooth.md). |
| "The port is already open" | Another tab or program uses the serial port. Close it there. |
| "… disconnected" | The cable was unplugged, the device restarted or went out of range. Connect again. |
| "No answer from the device within 10 s" | The serial port belongs to another device, or the firmware does not run Improv on that port. |

Bluetooth LE from desktop Linux browsers has not been tested.
