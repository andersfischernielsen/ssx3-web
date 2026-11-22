#!/bin/sh
# Build the browser version into www/pkg and www/pkg-vu (serve www/:
# serve.py). Needs the game set up in the submodule (cd ssx3 && python3
# tools/setup.py) and wasm-bindgen-cli matching the wasm-bindgen in
# Cargo.lock.
set -e
cd "$(dirname "$0")"
# Deep guest call chains: a 32 MiB stack. The stack pointer is exported:
# crates/web/js/jspi.js swaps it at every guest thread switch.
CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS="-C link-arg=-zstack-size=33554432 -C link-arg=--export=__stack_pointer -C target-feature=+simd128" \
  cargo build --profile web --target wasm32-unknown-unknown -p ssx3-web -p ssx3-vuworker -j "${JOBS:-4}"
wasm-bindgen --target web --no-typescript --out-dir www/pkg \
  target/wasm32-unknown-unknown/web/ssx3_web.wasm
wasm-bindgen --target web --no-typescript --out-dir www/pkg-vu \
  target/wasm32-unknown-unknown/web/ssx3_vuworker.wasm
ls -la www/pkg www/pkg-vu
