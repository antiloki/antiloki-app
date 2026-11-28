#!/usr/bin/env bash
# Build the `antiloki-agent` sidecar (release) and stage it as tauri/binaries/antiloki-agent-<host triple>,
# the name Tauri's `externalBin` looks for. Used by scripts/desktop-build.sh and the desktop workflow.
#   TARGET=aarch64-apple-darwin scripts/stage-agent-sidecar.sh   (a cross target: `cargo build --target`)
set -euo pipefail
cd "$(dirname "$0")/.."
TARGET="${TARGET:-}"
HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
TRIPLE="${TARGET:-$HOST_TRIPLE}"
if [ -n "$TARGET" ]; then
  cargo build --release -p antiloki-agent --target "$TARGET"
  OUT="target/$TARGET/release/antiloki-agent"
else
  cargo build --release -p antiloki-agent
  OUT="target/release/antiloki-agent"
fi
EXT=""; case "$TRIPLE" in *windows*) EXT=".exe";; esac
mkdir -p tauri/binaries
cp "$OUT$EXT" "tauri/binaries/antiloki-agent-$TRIPLE$EXT"
echo "· sidecar staged: tauri/binaries/antiloki-agent-$TRIPLE$EXT"
