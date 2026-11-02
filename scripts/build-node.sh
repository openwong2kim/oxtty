#!/usr/bin/env bash
# Builds the N-API binding for this machine and places it at crates/wongtty-node/wongtty.node.
# (Publishing per-platform prebuilds via @napi-rs/cli is a later step.)
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cargo build --manifest-path "$root/Cargo.toml" -p wongtty-node --release
case "$(uname -s)" in
  Darwin) lib="libwongtty_node.dylib" ;;
  MINGW*|MSYS*|CYGWIN*) lib="wongtty_node.dll" ;;
  *) lib="libwongtty_node.so" ;;
esac
cp "$root/target/release/$lib" "$root/crates/wongtty-node/wongtty.node"
echo "crates/wongtty-node/wongtty.node: $(wc -c < "$root/crates/wongtty-node/wongtty.node") bytes"
