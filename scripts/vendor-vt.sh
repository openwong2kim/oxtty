#!/usr/bin/env bash
# Moves a patched alacritty_terminal source tree into the `vt` module of the wongtty crate.
# To upgrade Alacritty, follow "Upgrading" in crates/wongtty/src/vt/PATCHES.md.
#
#   scripts/vendor-vt.sh <alacritty_terminal directory> [output directory]
#
# If an output directory is given, builds there instead of crates/wongtty (for extracting our patches as a diff — see PATCHES.md).
#
# What it does
# - Does not move the PTY and event loop modules (tty, event_loop, sync, thread) — the engine only receives bytes.
# - lib.rs → mod.rs, dropping the Alacritty crate-wide lint policy (#![...]) and the module declarations above.
# - Internal paths `crate::` → `crate::vt::`, `alacritty_terminal::` in doc examples → `wongtty::vt::`
# - DA2 reply version: pinned to the Alacritty version so `CARGO_PKG_VERSION` does not become the wongtty version
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
src="${1:?specify the alacritty_terminal directory}"
out="${2:-$root/crates/wongtty}"
dst="$out/src/vt"
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$src/Cargo.toml" | head -1)"

if [ -e "$dst" ]; then
  echo "$dst already exists. Remove it and run again." >&2
  exit 1
fi

mkdir -p "$dst"
rsync -a \
  --exclude tty --exclude event_loop.rs --exclude sync.rs --exclude thread.rs \
  "$src/src/" "$dst/"
mv "$dst/lib.rs" "$dst/mod.rs"
cp "$src/README.md" "$dst/UPSTREAM-README.md"
# The two below exist only in the patched tree (the old vendor/). The original Alacritty lacks them, so move them only if present.
if [ -f "$src/WMUX-PATCH.md" ]; then cp "$src/WMUX-PATCH.md" "$dst/PATCHES.md"; fi
if [ -f "$src/LICENSE-XTERM" ]; then cp "$src/LICENSE-XTERM" "$out/LICENSE-XTERM"; fi

find "$dst" -name '*.rs' -exec perl -pi -e 's/\bcrate::/crate::vt::/g; s/\balacritty_terminal::/wongtty::vt::/g; s/env!\("CARGO_PKG_VERSION"\)/"'"$version"'"/g' {} +

perl -0pi -e '
  s{\A//! Alacritty - The GPU Enhanced Terminal\.\n}{//! Fork of Alacritty `alacritty_terminal` 0.26 (Apache-2.0). Changes are listed in `PATCHES.md`.\n//!\n//! A low-level API outside the wongtty version stability promise. Renderers use [`crate::Engine`].\n};
  s{^#!\[.*\]\n}{}mg;
  s{^#\[cfg\(not\(target_arch = "wasm32"\)\)\]\n(?=pub mod (event_loop|tty);)}{}mg;
  s{^pub mod (event_loop|sync|thread|tty);\n}{}mg;
  s{\n{3,}}{\n\n}g;
' "$dst/mod.rs"

echo "vt module: $(find "$dst" -name '*.rs' | wc -l | tr -d ' ') files → $dst"
