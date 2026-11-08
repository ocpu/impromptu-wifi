# Security policy

## Reporting a vulnerability

Report vulnerabilities privately through GitHub:
<https://github.com/ocpu/impromptu-wifi/security/advisories/new>. Do not open a
public issue for a vulnerability.

Include:

- the affected crate, version or commit, and enabled features;
- the role and transport (device or client; serial or Bluetooth LE);
- steps to reproduce, and what an attacker gains.

## Supported versions

Fixes go to the `main` branch. No released versions exist yet.

## Scope

The Improv Wi-Fi protocol has no encryption, and on Bluetooth LE its only
authorization is an optional physical interaction. The following are
properties of the protocol and are not vulnerabilities in this
implementation:

- a passphrase readable by anyone who captures the serial or BLE traffic
  during provisioning;
- a device without a button that accepts settings from anyone in radio range
  while it offers BLE.

The [security model](docs/explanation/security-model.md) describes what this
implementation protects and how. In scope, for example:

- a write accepted while authorization is required and absent;
- the Linux daemon offering BLE while the machine is online, other than with
  `--ignore-wired`;
- a passphrase in a log, an error message, or any output other than the
  backend's own credential storage;
- memory-safety or denial-of-service bugs reachable from a frame received
  over serial or BLE.
