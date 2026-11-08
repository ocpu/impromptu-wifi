---
status: accepted
date: 2026-11-08
decision-makers: project maintainer
---

# Advertise from a random static address on the ESP32

## Context and Problem Statement

Home Assistant, through an ESPHome Bluetooth proxy, failed to subscribe to
the ESP32-S3 example with "Write not permitted". The proxy's GATT map for the
chip's public address placed the Improv service at handle 2; the firmware has
it at handle 7. Proxies cache GATT layouts by address, and a layout cached
from earlier firmware on the same chip stayed in use.

## Considered Options

* Keep the public address; ask users to clear client caches.
* Advertise from a random static address derived from the factory MAC.

## Decision Outcome

Chosen option: "Random static address", because it gives this firmware an
address no earlier firmware on the chip used, without user action.

### Consequences

* Good, because Home Assistant provisioned the device after the change.
* Neutral, because the address is stable across boots (derived from the
  factory MAC, with the two most significant bits set as a static random
  address requires; Bluetooth Core Specification, Vol 6, Part B, 1.3.2.1).
* Bad, because a later firmware with a different GATT layout can hit the same
  problem under this address; ADR-0005 addresses that.
