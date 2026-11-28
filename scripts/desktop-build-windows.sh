#!/usr/bin/env bash
# THE WINDOWS INSTALLER FROM LINUX (2026-09-21): Tauri's experimental cross-build — clang + lld-link against
# Microsoft's SDK (cargo-xwin downloads it once to ~/.cache/cargo-xwin), NSIS for the .exe installer.
#   scripts/desktop-build-windows.sh        → target/x86_64-pc-windows-msvc/release/bundle/nsis/antiloki_<ver>_x64-setup.exe
# Needs: `rustup target add x86_64-pc-windows-msvc`, `cargo install cargo-xwin`, lld + nsis (pacman / AUR).
# Like every local build: no TAURI_SIGNING_PRIVATE_KEY → no .sig / latest.json (the app installs, never self-updates).
set -euo pipefail
cd "$(dirname "$0")/.."
T=x86_64-pc-windows-msvc
command -v cargo-xwin >/dev/null || { echo "cargo-xwin missing: cargo install cargo-xwin" >&2; exit 2; }
command -v lld-link >/dev/null || { echo "lld-link missing: sudo pacman -S lld" >&2; exit 2; }
# the sidecar for the Windows triple, staged where externalBin looks (binaries/antiloki-agent-<triple>.exe)
cargo xwin build --release -p antiloki-agent --target "$T"
mkdir -p tauri/binaries && cp "target/$T/release/antiloki-agent.exe" "tauri/binaries/antiloki-agent-$T.exe"
echo "· sidecar staged: tauri/binaries/antiloki-agent-$T.exe"
command -v makensis >/dev/null || echo "· makensis missing (yay -S nsis): the .exe is built, the installer is not — rerun with nsis to package"
( cd tauri && cargo tauri build --runner cargo-xwin --target "$T" --bundles nsis "$@" )
echo "· bundles:"; find "target/$T/release/bundle" -maxdepth 2 -type f -name '*-setup.exe' 2>/dev/null
