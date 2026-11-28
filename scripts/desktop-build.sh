#!/usr/bin/env bash
# Local standalone build (operator 2026-09-17) — the same thing CI does, on this box, for this OS.
#   scripts/desktop-build.sh            → target/release/bundle/{deb,appimage,dmg,nsis,msi}/…
# Updater artifacts (.sig + latest.json) need TAURI_SIGNING_PRIVATE_KEY — see DESKTOP.md.
set -euo pipefail
cd "$(dirname "$0")/.."
command -v cargo-tauri >/dev/null || cargo install tauri-cli --version '^2' --locked
if [ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]; then
  echo "· no TAURI_SIGNING_PRIVATE_KEY — the installer builds, the updater artifacts (.sig, latest.json) do not"
fi
# THE SIDECAR (2026-09-18): `antiloki-agent` (hook · mcp · statusline) ships next to the app as a Tauri
# externalBin — built here for the host triple and staged where tauri.conf.json's `binaries/antiloki-agent`
# expects it (`binaries/antiloki-agent-<triple>`). The bake resolves it next to the executable at runtime.
scripts/stage-agent-sidecar.sh
( cd tauri && cargo tauri build "$@" )
echo "· bundles:"; find target/release/bundle -maxdepth 2 -type f \( -name '*.dmg' -o -name '*.AppImage' -o -name '*.deb' -o -name '*.msi' -o -name '*-setup.exe' -o -name 'latest.json' \) 2>/dev/null
