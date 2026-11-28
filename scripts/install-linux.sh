#!/usr/bin/env bash
# Install antiloki on this Linux machine from this checkout (operator 2026-10-26: "an install command so I can open it with a single word",
# "antiloki is not installed in the programs list"):
#
#   scripts/install-linux.sh                 build for release, install `antiloki` (the terminal agent) + the window + the programs-list entry
#   scripts/install-linux.sh --debug         the same from a debug build: much faster to build, slower to run (good for trying a change)
#   scripts/install-linux.sh --no-window     only the terminal agent and the serve (no Tauri build, no desktop entry)
#   scripts/install-linux.sh --uninstall     remove everything this installed
#   PREFIX=/usr/local scripts/install-linux.sh        install somewhere else (default ~/.local)
#
# After it: `antiloki` in any git repository opens the agent there; `antiloki app` (or the programs list) opens the window.
#
# It follows OPERATING-RULES.md: every cargo build runs in a memory-capped systemd scope with -j 2 (set INSTALL_MEM, default 3500M, to change the cap;
# INSTALL_NO_SCOPE=1 turns the cap off) - the link step of this workspace once took a whole terminal down on a 15 GiB machine.
# The window build needs the frontend checkout beside this one (../antiloki-ui, on the same branch; FRONTEND=<dir> to override), node, and the
# system's WebKitGTK/GTK development packages (webkit2gtk-4.1, gtk3).
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"

PREFIX="${PREFIX:-$HOME/.local}"
PROFILE=release; WINDOW=1; UNINSTALL=0
for a in "$@"; do
  case "$a" in
    --debug) PROFILE=debug ;;
    --release) PROFILE=release ;;
    --no-window) WINDOW=0 ;;
    --uninstall) UNINSTALL=1 ;;
    --prefix=*) PREFIX="${a#--prefix=}" ;;
    -h|--help) sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option $a (try --help)" >&2; exit 2 ;;
  esac
done

BIN="$PREFIX/bin"
APPS="$PREFIX/share/applications"
ICONS="$PREFIX/share/icons/hicolor/128x128/apps"
INSTALLED=(antiloki antiloki-serve antiloki-agent antiloki-desktop)

if [ "$UNINSTALL" = 1 ]; then
  [ -x "$BIN/antiloki" ] && "$BIN/antiloki" stop 2>/dev/null || true
  for f in "${INSTALLED[@]}"; do rm -f "$BIN/$f"; done
  rm -f "$APPS/antiloki.desktop" "$ICONS/antiloki.png"
  command -v update-desktop-database >/dev/null && update-desktop-database "$APPS" 2>/dev/null || true
  echo "· removed antiloki from $PREFIX (its data in ~/.local/share/antiloki is kept)"
  exit 0
fi

# --- a toolchain: cargo on PATH, or through mise like the rest of this repo ---
if command -v cargo >/dev/null 2>&1; then CARGO=(cargo)
elif command -v mise >/dev/null 2>&1; then CARGO=(mise x rust@stable -- cargo)
else echo "cargo was not found (install Rust, or mise with rust@stable)" >&2; exit 1; fi

capped() {   # run a command inside a memory-capped scope when systemd can give us one
  if [ "${INSTALL_NO_SCOPE:-0}" != 1 ] && command -v systemd-run >/dev/null 2>&1 && systemd-run --user --scope -q -p MemoryMax=1G true 2>/dev/null; then
    systemd-run --user --scope -q -p "MemoryMax=${INSTALL_MEM:-3500M}" -- "$@"
  else "$@"; fi
}
FLAGS=(-j 2); [ "$PROFILE" = release ] && FLAGS+=(--release)
# a lighter link for the debug build (debug info is what made the link heavy)
[ "$PROFILE" = debug ] && export CARGO_PROFILE_DEV_DEBUG=0
OUT="$ROOT/target/$PROFILE"

echo "· building the terminal agent and the serve ($PROFILE)"
capped "${CARGO[@]}" build "${FLAGS[@]}" -p antiloki-serve -p antiloki-agent

if [ "$WINDOW" = 1 ]; then
  FRONTEND="${FRONTEND:-$ROOT/../antiloki-ui}"
  [ -d "$FRONTEND" ] || { echo "the frontend checkout was not found at $FRONTEND (set FRONTEND=, or use --no-window)" >&2; exit 1; }
  echo "· building the window ($PROFILE): the frontend, then the Tauri shell"
  [ -d "$FRONTEND/node_modules" ] || ( cd "$FRONTEND" && npm ci --no-audit --no-fund )
  scripts/stage-agent-sidecar.sh
  command -v cargo-tauri >/dev/null 2>&1 || "${CARGO[@]}" tauri --version >/dev/null 2>&1 || capped "${CARGO[@]}" install tauri-cli --version '^2' --locked
  TAURI_FLAGS=(--no-bundle); [ "$PROFILE" = debug ] && TAURI_FLAGS+=(--debug)
  ( cd tauri && capped "${CARGO[@]}" tauri build "${TAURI_FLAGS[@]}" -- -j 2 )
fi

echo "· installing into $PREFIX"
install -d "$BIN"
install -m755 "$OUT/antiloki-chat" "$BIN/antiloki"          # the cargo bin is antiloki-chat; the one word is antiloki
install -m755 "$OUT/antiloki-serve" "$BIN/antiloki-serve"
install -m755 "$OUT/antiloki-agent" "$BIN/antiloki-agent"
if [ "$WINDOW" = 1 ]; then
  install -m755 "$OUT/antiloki-desktop" "$BIN/antiloki-desktop"
  install -d "$APPS" "$ICONS"
  install -m644 tauri/icons/128x128.png "$ICONS/antiloki.png"
  cat > "$APPS/antiloki.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=antiloki
Comment=The coding agent with a router: Heimdall picks the model, the role and the checks
Exec=env WEBKIT_DISABLE_DMABUF_RENDERER=1 $BIN/antiloki app
Icon=antiloki
Terminal=false
Categories=Development;
EOF
  command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APPS" 2>/dev/null || true
  command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t "$PREFIX/share/icons/hicolor" 2>/dev/null || true
fi

echo
echo "installed:"; for f in "${INSTALLED[@]}"; do [ -x "$BIN/$f" ] && echo "  $BIN/$f"; done
[ "$WINDOW" = 1 ] && echo "  $APPS/antiloki.desktop   (the programs list)"
case ":$PATH:" in *":$BIN:"*) ;; *) echo "note: $BIN is not on your PATH — add it to use the one word: export PATH=\"$BIN:\$PATH\"" ;; esac
echo
echo "try:  cd <a git repository> && antiloki"
echo "      antiloki \"explain what this project does\""
echo "      antiloki app        (the window)        antiloki stop        (end the serve the terminal started)"
