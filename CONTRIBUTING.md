# Contributing

Bug reports, hardware test results and pull requests are welcome. For a
vulnerability, follow [SECURITY.md](SECURITY.md) instead of opening an issue.

## Before a pull request

CI (`.github/workflows/ci.yml`) defines the checks. Most crates have optional
features, so run them with the feature list CI uses:

```sh
HF=$(sed -n '/HOST_FEATURES: >-/{n;p}' .github/workflows/ci.yml | tr -d ' ')

cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --features "$HF" -- -D warnings
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --features "$HF"
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps --features "$HF"
cargo deny --locked check
python3 -I scripts/check-dep-age.py --min-days 14 Cargo.lock examples/esp32s3/Cargo.lock
```

The Linux D-Bus tests need the `dbus-daemon` binary. CI also builds the
`no_std` crates for embedded targets, checks the MSRVs, and builds the
ESP32-S3 example and the Raspberry Pi binaries; [AGENTS.md](AGENTS.md) lists
those commands.

## Rules

- **Commits:** [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/),
  for example `fix(linux): ...` or `docs: ...`. History is linear; pull
  requests are squash-merged or rebased.
- **Dependencies:** every locked crates.io version must be at least 14 days
  old. Pin an older version with `cargo update -p NAME --precise VERSION`, in
  `Cargo.lock` and in `examples/esp32s3/Cargo.lock`. Licenses must be in the
  allow-list in `deny.toml`.
- **No C libraries:** nothing may link `libdbus`, `libudev` or another C
  library; see [ADR-0001](docs/decisions/0001-use-zbus-for-bluez.md).
- **`no_std` crates** allocate nothing; they use fixed buffers and `heapless`.
- **Security invariants:** keep the rules in the
  [security model](docs/explanation/security-model.md), such as offering BLE
  on Linux only while offline, and never logging a passphrase.
- **Documentation:** `docs/` follows [Diátaxis](https://diataxis.fr/).
  Record a significant decision as an ADR in `docs/decisions/`. Accepted ADRs
  are not edited; a new ADR supersedes them.

## Hardware test results

Behavior on real hardware is recorded in the
[support matrix](docs/reference/support-matrix.md). A status changes to
**Tested** only with a test on named hardware: include the board, OS and
version, BlueZ version where relevant, the client, and the date.

## License

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this work by you, as defined in the Apache-2.0
license, shall be dual licensed as MIT OR Apache-2.0, without any additional
terms or conditions.
