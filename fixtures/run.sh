#!/usr/bin/env bash
# RUN WHAT THIS MACHINE CAN (PLAN-THE-LEAN-YEARS I.4, I.5).
#
#   fixtures/run.sh          run every fixture whose toolchain is here; say plainly which were skipped
#
# This is the rig's entry point and what the two-OS matrix calls. A skipped fixture is reported, never counted
# as a pass — the same rule the gates follow, for the same reason: "no Go on this box" and "the Go tests
# passed" are different facts.
#
# RUSTFLAGS is cleared for the Rust fixtures. Cargo merges config from every ancestor directory and JOINS
# rustflags arrays rather than overriding them, so this repo's untracked, machine-local `.cargo/config.toml` —
# which points at the mold linker through a tool-manager shim — otherwise applies inside a fixture that is
# supposed to build anywhere. A child config cannot undo it; the environment can.
set -uo pipefail
cd "$(dirname "$0")"
pass=0; fail=0; skip=0
ok()   { echo "  ok       $1"; pass=$((pass+1)); }
no()   { echo "  FAILED   $1"; fail=$((fail+1)); }
sk()   { echo "  skipped  $1 — $2"; skip=$((skip+1)); }
have() { command -v "$1" >/dev/null 2>&1; }

# `command -v cargo` is not enough: on this box cargo is a tool-manager shim that exists on the PATH and then
# refuses to run without a pinned version. Ask it for its version, which is the only answer that distinguishes
# a usable toolchain from a stub — the same trap scripts/smoke.sh discovers.
CARGO=""
if cargo --version >/dev/null 2>&1; then CARGO="cargo"
elif have mise && mise x rust@stable -- cargo --version >/dev/null 2>&1; then CARGO="mise x rust@stable -- cargo"; fi

if [ -n "$CARGO" ]; then
  for f in fx-rust fx-mixed; do
    ( cd "$f" && RUSTFLAGS="" $CARGO test --quiet >/dev/null 2>&1 ) && ok "$f (cargo)" || no "$f (cargo)"
  done
else sk "fx-rust, fx-mixed" "no usable cargo"; fi

if have node; then ( cd fx-node-api && node --test >/dev/null 2>&1 ) && ok "fx-node-api (node --test)" || no "fx-node-api (node --test)"
else sk "fx-node-api" "no node"; fi

if have go; then ( cd fx-go && go test ./... >/dev/null 2>&1 ) && ok "fx-go (go test)" || no "fx-go (go test)"
else sk "fx-go" "no go toolchain"; fi

if python3 -c "import pytest" >/dev/null 2>&1; then ( cd fx-python && python3 -m pytest -q >/dev/null 2>&1 ) && ok "fx-python (pytest)" || no "fx-python (pytest)"
else sk "fx-python" "pytest not installed"; fi

# fx-react needs an npm install to run anything; NOT installing it is deliberate — "the box's dependencies are
# not installed" is a real answer the gates must give, and this fixture is where that answer is exercised.
[ -d fx-react/node_modules ] && { ( cd fx-react && npx vitest run >/dev/null 2>&1 ) && ok "fx-react (vitest)" || no "fx-react (vitest)"; } \
  || sk "fx-react" "no node_modules, on purpose — the gate must say so rather than pass"

echo "· $pass passed · $fail failed · $skip skipped"
exit $(( fail > 0 ? 1 : 0 ))
