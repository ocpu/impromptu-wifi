---
status: accepted
date: 2026-11-08
decision-makers: project maintainer
---

# Expose a GATT Database Hash from trouble-host firmware

## Context and Problem Statement

trouble-host's `GapConfig` adds an empty GATT service, so a client that caches
a device's GATT layout cannot tell that a firmware update changed it, and
keeps using old handles.

## Considered Options

* Leave the GATT service empty.
* Add Service Changed, Client Supported Features and Database Hash (Bluetooth
  Core Specification, Vol 3, Part G, 7), computing the hash with the `aes`
  and `cmac` crates over trouble-host's public table API.
* Enable trouble-host's `security` feature, which has a hash function, but
  also pairing support.

## Decision Outcome

Chosen option: "Add the characteristics with `aes` and `cmac`", because it
adds the hash without pairing support.

### Consequences

* Good, because clients that check the hash can rediscover the services after
  a layout change. After a change on an ESP32-S3 and a Home Assistant
  restart, the ESPHome proxy returned the new layout; whether the proxy
  checked the hash or had not cached the old layout was not determined.
* Bad, because two crypto dependencies are added to the `trouble` feature.
* Bad, because Home Assistant's `bleak_esphome` (4.1.0) keeps layouts in
  memory without checking the hash; it needs a restart after a change.

### Confirmation

Unit tests against trouble-host's openssl-derived reference values; on an
ESP32-S3, the value read over raw ATT equalled an `openssl mac` CMAC over the
layout the device reported.
