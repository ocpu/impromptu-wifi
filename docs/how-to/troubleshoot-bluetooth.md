# Troubleshoot Bluetooth LE provisioning

Find the cause of a Bluetooth LE (BLE) provisioning problem from its symptom,
and fix it. Each section comes from a problem seen on real hardware; the
[support matrix](../../SUPPORT.md) lists the hardware.

## Before you begin

- On a Linux device: shell access with `sudo`, and the daemon's log,
  `journalctl -u impromptu-wifi-device`.
- On a Linux client: `bluetoothctl`, and the client's BlueZ log,
  `journalctl -u bluetooth`.
- For packet-level checks: `btmon` (root) on the Linux device.

## The device is not found

### What it means

`impromptu-wifi discover` lists nothing, or `--ble <name>` reports `no Improv
device matching "<name>" is advertising`.

### Check

1. Is the device provisioned? A provisioned device stops advertising. Over
   serial, `impromptu-wifi --port <port> state` reports `Provisioned`; on a
   Linux device the log ends with `stopped offering Improv over Bluetooth`.
2. On a Linux device, is it offline? The daemon advertises only while
   offline, and by default Ethernet counts as online. The log shows `offline:
   offering Improv over Bluetooth on hci0` when it advertises.
3. On a Linux device, is the adapter powered? `bluetoothctl show` should say
   `Powered: yes`. `PowerState: off-blocked` means rfkill blocks it.
4. Is the device seen only sometimes? Run `discover` several times. On BlueZ
   5.55 and 5.66 the advertising interval was 1.28 s, and a 5-second scan
   missed a Pi Zero W about one time in three and a Pi 4 every time.

### Resolve

1. To provision a provisioned device again: use serial, or reset it (the
   ESP32-S3 example: hold BOOT for 5 s).
2. For a wired Linux device that should also get Wi-Fi: start the daemon with
   `--ignore-wired`.
3. For an rfkill block: `rfkill unblock bluetooth`, or write `0` to
   `/sys/class/rfkill/<n>/soft` for the `bluetooth` entry. `setup.sh` does
   this.
4. For the 1.28 s interval: set `LEMinAdvertisementInterval=160` and
   `LEMaxAdvertisementInterval=240` (BlueZ 5.55), or `MinAdvertisementInterval`
   and `MaxAdvertisementInterval` in the `[LE]` section (5.66), in
   `/etc/bluetooth/main.conf`. Units are 0.625 ms, so this is 100 to 150 ms.
   Then restart `bluetooth`; this disconnects every Bluetooth device on the
   machine. `setup.sh` does this.

### Verify

`impromptu-wifi discover` lists the device in each of several runs.

## The connection drops after a few seconds, or a pairing prompt appears

### What it means

The tool fails with ATT error `0x0e`, `Not connected` or `timed out
connecting`, or the client shows a pairing request, during a session with a
Linux device.

### Check

1. On the device, watch `sudo btmon` during a connection attempt. Look for
   `Insufficient Encryption` followed by `SMP: Security Request` and
   `Pairing Failed`.
2. Note which characteristic was read. UUID `7772e5db-3868-4112-a1a9-f2669d106bf3`
   (shown byte-reversed as `f36b109d-...` in some traces) is BLE-MIDI. Its
   reads need encryption, so a client and device that both offer BLE-MIDI
   start pairing, and BlueZ drops the link when the pairing is refused.

### Resolve

On the device:

1. Start `bluetoothd` without its MIDI plugin. Find its path with
   `systemctl show -p ExecStart bluetooth` (for example
   `/usr/libexec/bluetooth/bluetoothd`), and create
   `/etc/systemd/system/bluetooth.service.d/no-midi.conf`:

   ```ini
   [Service]
   ExecStart=
   ExecStart=/usr/libexec/bluetooth/bluetoothd --noplugin=midi
   ```

   The empty `ExecStart=` clears the original command. Run
   `systemctl daemon-reload` and `systemctl restart bluetooth`; the restart
   disconnects every Bluetooth device on the machine.
2. On images with PipeWire, turn off WirePlumber's BLE-MIDI:
   - WirePlumber 0.5: a file in `/etc/wireplumber/wireplumber.conf.d/` with
     `wireplumber.profiles = { main = { monitor.bluez-midi = disabled } }`.
   - WirePlumber 0.4 releases that have a BLE-MIDI monitor (a
     `/usr/share/wireplumber/bluetooth.lua.d/*midi*` file): a file in
     `/etc/wireplumber/bluetooth.lua.d/` with
     `if bluez_midi_monitor then bluez_midi_monitor.enabled = false end`.
     WirePlumber 0.4.13 has no such monitor and needs nothing.

   Then restart WirePlumber for each logged-in user; this interrupts their
   audio briefly.

`setup.sh` does both.

### Verify

`impromptu-wifi --ble <name> info` succeeds several times in a row, and
`btmon` shows no `SMP` packets.

### If the problem continues

If the trace shows `Disconnect Complete` with reason `Connection Timeout
(0x08)` and no pairing, see the next section.

## Connections time out between some clients and devices

### What it means

Every connection drops within 1-2 seconds with `Connection Timeout (0x08)` on
the device, or never completes (`timed out connecting`), although other
clients work. Seen between one laptop (MediaTek Bluetooth `0e8d:0717`, BlueZ
5.87) and a Pi 4 and Pi 5 (CYW43455); the same Pis held connections from a Pi
Zero W and from an ESPHome Bluetooth proxy.

### Check

1. Try another client, such as Home Assistant or another Linux machine. If it
   works, the problem is the combination.
2. Rule out power and radio sharing on the device: the drops stayed the same
   on a 5 A supply and with Wi-Fi switched off.

### Resolve

Provision from another client. No fix on the device side is known.

## The client finds the device but not its Improv service

### What it means

The tool reports `device has no Improv Wi-Fi service characteristic`, or a
client fails to subscribe with `Write not permitted`, after the device's GATT
layout changed (a firmware update, or a service such as BLE-MIDI added or
removed).

### Check

Compare the client's view with the device: `bluetoothctl info <address>`
lists the service UUIDs the client has cached.

### Resolve

1. A Linux client: `bluetoothctl remove <address>`, then connect again. This
   also deletes any pairing with the device.
2. Home Assistant: restart Home Assistant. Its `bleak_esphome` library keeps
   each device's layout in memory and does not check the GATT Database Hash.
3. For firmware you build: expose a Database Hash (`add_gap_and_gatt` in
   `impromptu-wifi-embassy`), so clients that check it rediscover by
   themselves, and avoid reusing an address that other firmware used
   (ADR-0004).

### Verify

The tool's `info` succeeds, or Home Assistant completes the setup.

## The client's `bluetoothd` crashes

### What it means

The client's BlueZ restarts during a session; `journalctl -u bluetooth` on
the client shows `code=dumped, status=11/SEGV`. Seen once with BlueZ 5.87
after a refused pairing caused by BLE-MIDI.

### Resolve

Remove the cause of the pairing on the device (BLE-MIDI, above), and report
the crash to the client's BlueZ packager with the stack trace from
`coredumpctl info`.
