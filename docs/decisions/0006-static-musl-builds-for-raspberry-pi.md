---
status: accepted
date: 2026-11-08
decision-makers: ocpu (maintainer)
---

# Ship static musl builds for the Raspberry Pi

## Context and Problem Statement

`impromptu-wifi-device` runs on Raspberry Pi models from the Zero W (ARMv6) to
the Pi 5 (AArch64), on Raspberry Pi OS 11 to 13. How should it be built?

## Considered Options

* Build on each Pi.
* Cross-compile against each OS release's glibc.
* Cross-compile static musl binaries, linked with the `rust-lld` that rustup
  ships.

## Decision Outcome

Chosen option: "Static musl binaries", because one binary per architecture
runs on every OS release, and the build needs no cross toolchain or target
libraries (the daemon links no C library; ADR-0001).

### Consequences

* Good, because three targets (`arm-unknown-linux-musleabihf`,
  `armv7-unknown-linux-musleabihf`, `aarch64-unknown-linux-musl`) cover all
  models; `.cargo/config.toml` sets the linker.
* Neutral, because the binaries are 7 to 8 MB, unstripped.

### Confirmation

The ARMv6 build ran on a Pi Zero W (Raspbian 11), the AArch64 build on a Pi 3B,
Pi 4 and Pi 5 (Raspberry Pi OS 12 and 13). CI builds all three.
