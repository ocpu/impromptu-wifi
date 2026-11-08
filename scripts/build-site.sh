#!/usr/bin/env bash
# Build the GitHub Pages site into OUT (default: target/site).
#
# Needs: the wasm32-unknown-unknown target, wasm-bindgen-cli at the version
# of the wasm-bindgen crate in Cargo.lock.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-"$root/target/site"}
case $out in /*) ;; *) out="$PWD/$out" ;; esac

cd "$root"
rm -rf "$out"
mkdir -p "$out/pkg"

# web-sys exposes Web Serial and Web Bluetooth only with this cfg.
RUSTFLAGS="--cfg=web_sys_unstable_apis" \
  cargo build --locked --release -p impromptu-wifi-web --target wasm32-unknown-unknown
wasm-bindgen --target web --no-typescript --out-dir "$out/pkg" \
  target/wasm32-unknown-unknown/release/impromptu_wifi_web.wasm

cp site/index.html site/style.css site/app.js "$out/"
