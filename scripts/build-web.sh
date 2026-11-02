#!/usr/bin/env bash
# Builds crates/wongtty-web (the Rust front end) and generates its wasm-bindgen glue into the npm package as
# dist/xterm_web.{js,d.ts} + dist/xterm_web_bg.wasm, next to the engine (build-wasm.sh) and the wongtty/xterm entry.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
dist="$root/packages/wongtty/dist"
# The glue must come from the same wasm-bindgen as the crate.
want="$(sed -n 's/^wasm-bindgen = "=\(.*\)"$/\1/p' "$root/crates/wongtty-web/Cargo.toml")"
have="$(wasm-bindgen --version 2>/dev/null | awk '{print $2}')" || true
if [ "$have" != "$want" ]; then
  echo "wasm-bindgen CLI $want is required (found ${have:-none}): cargo install wasm-bindgen-cli --version '=$want' --locked" >&2
  exit 1
fi
rustup target add wasm32-unknown-unknown >/dev/null 2>&1 || true
cargo build --manifest-path "$root/Cargo.toml" -p wongtty-web --target wasm32-unknown-unknown --profile wasm
mkdir -p "$dist"
wasm-bindgen --target web --out-dir "$dist" --out-name xterm_web "$root/target/wasm32-unknown-unknown/wasm/wongtty_web.wasm"
cp "$root/packages/wongtty/src/xterm.js" "$root/packages/wongtty/src/xterm.d.ts" "$dist/"
size="$(wc -c < "$dist/xterm_web_bg.wasm")"
echo "packages/wongtty/dist/xterm_web_bg.wasm: $size bytes"
# Size budget of the front end (the engine's is in build-wasm.sh's caller, PRD §7).
budget=$((800 * 1024))
if [ "$size" -gt "$budget" ]; then
  echo "front-end wasm is over its $budget-byte budget" >&2
  exit 1
fi
