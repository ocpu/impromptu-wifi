---
status: accepted
date: 2026-11-08
decision-makers: project maintainer
---

# Report a lost network as a return from Provisioned

## Context and Problem Statement

The Improv Wi-Fi specification has no transition out of Provisioned. A
device that loses its network for good (the access point is gone, the
passphrase changed) then stays Provisioned and can't be provisioned again
without a restart or reset.

## Considered Options

* Stay Provisioned; require a reset.
* Restart the device when the network is lost.
* Add a `DeviceEvent::Unprovisioned` that returns a provisioned driver to its
  idle state (Authorized or Authorization Required).

## Decision Outcome

Chosen option: "`DeviceEvent::Unprovisioned`", because it lets a client
provision the device again over the transport it uses, without losing the
saved network, which a background rejoin can still use.

### Consequences

* Good, because firmware with `keep_online` reopens provisioning after three
  failed rejoins and keeps retrying; the Linux daemon does the same when its
  backend reports the machine offline.
* Bad, because it is not in the specification. A client that stops reading
  the state after Provisioned sees the change only when it reads again.

### Confirmation

On an ESP32-S3: with its access point switched off, the device reported
Authorized over serial and advertised over BLE again; when the access point
returned, it rejoined and reported Provisioned.

## More Information

[Protocol conformance](../explanation/protocol-conformance.md#leaving-provisioned-unprovisioned).
