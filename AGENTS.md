# AGENTS.md

Guidance for coding agents working in this repository.

## What this is

impromptu-wifi: a Rust implementation of the Improv Wi-Fi provisioning protocol
(BLE spec up to 2.4, serial v1), for both roles: the *device* being
provisioned (firmware or a Linux daemon) and the *client* that provisions it
(CLI, firmware). Cargo workspace in `crates/*`; examples in `examples/` are
**not** workspace members.

## Commands

CI (`.github/workflows/ci.yml`) is the source of truth; it pins Rust 1.99.1.
Most crates have optional features, so plain `cargo test` misses a lot. Use
the CI feature list:

```sh
HF=$(sed -n '/HOST_FEATURES: >-/{n;p}' .github/workflows/ci.yml | tr -d ' ')

cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --features "$HF" -- -D warnings
cargo clippy --locked --workspace --all-targets -- -D warnings   # default features too
cargo test --locked --workspace --features "$HF"
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps --features "$HF"
cargo deny --locked check
python3 -I scripts/check-dep-age.py --min-days 14 Cargo.lock examples/esp32s3/Cargo.lock
```

Single tests:

```sh
cargo test -p impromptu-wifi-core machine::tests::unprovisioned_leaves_other_states_alone
cargo test -p impromptu-wifi-linux --features daemon --test dbus going_online
cargo test -p impromptu-wifi-embassy --features flash --test lifecycle
```

Other checks CI runs:

- `no_std`: build the `NO_STD_CRATES` (see ci.yml) and `impromptu-wifi-embassy`
  for `thumbv6m-none-eabi`, `thumbv7em-none-eabihf`, `riscv32imac-unknown-none-elf`.
  `trouble`/`trouble-central` don't build on thumbv6m (no atomic CAS).
- MSRV: 1.87 for the `no_std` crates, 1.91 for embassy, host, cli and linux
  (`cargo +1.87 check ...`, args in the `msrv` job).
- Raspberry Pi daemon builds (static musl, linked by `rust-lld` via
  `.cargo/config.toml`, no cross toolchain needed):
  `cargo build --release -p impromptu-wifi-linux --features daemon --bin impromptu-wifi-device --target aarch64-unknown-linux-musl`
  (also `arm-unknown-linux-musleabihf`, `armv7-unknown-linux-musleabihf`).
- ESP32-S3 example (`examples/esp32s3`, own `Cargo.lock`, own
  `rust-toolchain.toml` selecting the Espressif `esp` toolchain with
  `build-std`): `cd examples/esp32s3 && . ~/export-esp.sh && cargo clippy --locked --release -- -D warnings && cargo build --locked --release`.

## Dependency rules

- `scripts/check-dep-age.py` fails if any locked crates.io version is younger
  than 14 days. When adding or updating a dependency, pin older versions with
  `cargo update -p NAME --precise VERSION`, in the root lock and in
  `examples/esp32s3/Cargo.lock`.
- `cargo deny` uses `deny.toml` (license allow-list, ignored advisories).
- Nothing may need `libdbus` or `libudev`: BlueZ, NetworkManager and hostnamed
  are reached through `zbus`; serial through `serialport` without `libudev`
  (ADR-0001). Don't add crates that link C libraries.
- `default-features = false` is ignored on workspace-inherited dependencies;
  the facade lists `impromptu-wifi-host` and `-linux` as direct path deps for
  this reason.

## Architecture

Layered; lower layers know nothing of higher ones, and the `no_std` layers
allocate nothing (fixed buffers, `heapless`).

- `impromptu-wifi-proto`: wire format only (frames, states, errors, RPC
  encoding, BLE advertising/scan-response data). States/errors a transport
  doesn't define are never sent on it (`valid_for`).
- `impromptu-wifi-traits`: what a device provides. Optional capabilities are
  traits with a `const SUPPORTED`; the driver answers commands and sets BLE
  capability bits from these constants, so support is decided by types. Opt
  out with the `unsupported!` macro. Also `AuthSource`, `EventSource`
  (`DeviceEvent::{Available, Provisioned, Unprovisioned}`), `CredentialStore`.
- `impromptu-wifi-core`: `Machine` is the sans-IO protocol state machine (all
  rules, authorization via one `may_write` gate, which updates are unsent);
  `Driver` is the async loop racing `Link::recv`, `AuthSource::next` and
  `EventSource::next` with `select3`. `Link` impls must be cancel-safe.
  `NotifyOnJoin`/`PersistOnJoin` wrap a Wi-Fi object.
- `impromptu-wifi-client`: the client role (session machine + `Client`).
- `impromptu-wifi-serial` / `-ble`: transport framing for both roles; BLE is
  stack-agnostic via the `GattServer`/`GattClient` traits.
- Platform: `impromptu-wifi-embassy` (firmware: `ButtonAuth`, `Button`,
  `WifiShare` sharing one backend between drivers, `keep_online`, `FlashStore`,
  trouble-host bindings with a GATT Database Hash, ESP32 backend over
  esp-radio), `impromptu-wifi-linux` (device: NetworkManager `nm`,
  wpa_supplicant control socket `wpa`, hostnamed, BlueZ peripheral, and the
  `impromptu-wifi-device` daemon), `impromptu-wifi-host` (client: serial, BLE via
  BlueZ/zbus on Linux, btleplug elsewhere), `impromptu-wifi-cli`.
- `impromptu-wifi`: facade crate; features select layers (no defaults).

Cross-cutting behavior that spans several files:

- A device serving serial and BLE runs one `Driver` per transport; joins on
  one are announced to the other as `DeviceEvent::Provisioned` (`WifiShare`
  in firmware, `join_channel` in the Linux daemon), so both report the same
  state.
- `Unprovisioned` (leaving Provisioned after a lost network) is an extension
  beyond the Improv spec (ADR-0003).
- The Linux daemon offers BLE **only while the machine is offline** and stops
  as soon as it is online by any means (`NmOnline`/`WpaOnline`); it starts
  authorized because it has no button. This is a security invariant
  (ADR-0002); `--ignore-wired` is the only opt-in widening.
- The systemd unit must not `Wants=NetworkManager.service` (on dhcpcd systems
  it stops dhcpcd via Debian's `Conflicts=`); ordering only.
- The wpa_supplicant backend redacts every `SET_NETWORK` value except the
  SSID; keep all command formatting going through `redact`.

## Tests

- Linux D-Bus tests (`crates/impromptu-wifi-linux/tests/dbus.rs`,
  `bluez.rs`, and the host `bluez.rs`) start a private `dbus-daemon` with
  stand-in NetworkManager/hostnamed/BlueZ services; they need the
  `dbus-daemon` binary but no real services.
- `tests/wpa.rs` runs a stand-in wpa_supplicant on a UNIX datagram socket.
- Embassy timing tests use embassy-time's std driver (real clock).
- Hardware behavior (BlueZ versions, Raspberry Pi OS quirks, client
  incompatibilities) is not covered by tests; it is recorded in
  `docs/reference/support-matrix.md`
  and the example READMEs. Update those when testing on hardware.

## Documentation

- `docs/` follows Diátaxis (`how-to/`, `reference/`, `explanation/`; add
  `tutorials/` with the first tutorial) with MADR 4.0.0 records in `docs/decisions/NNNN-*.md`;
  index in `docs/README.md`. Accepted ADRs are immutable; supersede instead.
- `docs/reference/support-matrix.md` distinguishes Tested (on named hardware, with date) / Built /
  Untested; don't upgrade a status without a hardware test.
- `examples/raspberry-pi/setup.sh` encodes the per-OS setup (Raspberry Pi OS
  11/12/13: dhcpcd vs NetworkManager, BlueZ advertising-interval key names,
  BLE-MIDI, rfkill, polkit `.pkla` vs `.rules`) and must stay idempotent.
