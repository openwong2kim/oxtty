#!/usr/bin/env bash
# Builds crates/wongtty-wasm for the browser and copies it into the npm package.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
rustup target add wasm32-unknown-unknown >/dev/null 2>&1 || true
# SIMD128 (every browser xterm.js supports, Node 16.4+): the parser checks UTF-8 and controls 16 bytes at a time.
RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+simd128" \
  cargo build --manifest-path "$root/Cargo.toml" -p wongtty-wasm --target wasm32-unknown-unknown --profile wasm
mkdir -p "$root/packages/wongtty/dist"
cp "$root/target/wasm32-unknown-unknown/wasm/wongtty_wasm.wasm" "$root/packages/wongtty/dist/wongtty.wasm"
cp "$root/packages/wongtty/src/index.js" "$root/packages/wongtty/src/index.d.ts" "$root/packages/wongtty/dist/"
echo "packages/wongtty/dist/wongtty.wasm: $(wc -c < "$root/packages/wongtty/dist/wongtty.wasm") bytes"
