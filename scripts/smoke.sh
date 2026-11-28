#!/usr/bin/env bash
# IS ANTILOKI STILL ALIVE? (PLAN-THE-LEAN-YEARS I.2)
#
#   scripts/smoke.sh            build, start, check, stop — under two minutes, no AI call, no token spent
#   SMOKE_PORT=41999 …          a different port (default 41999, never the operator's 41931)
#
# 6,027 Rust tests and 2,585 FE tests take minutes and answer a different question. This answers the only one
# that matters after an agent merges into this repo: does the product still start and serve. It is meant to run
# as a gate after every episode merge, and to be the first thing tried when something feels wrong.
#
# Measured 2026-09-25 on this box: 87s warm (only the changed crate relinks — the case after a merge, which is
# the one that matters), 252s cold. Most of both is the debug link and the FE typecheck; the seven checks
# themselves take about six seconds.
#
# It never touches the operator's data: a throwaway ANTILOKI_DATA_DIR, and a port nothing else uses.
set -uo pipefail
cd "$(dirname "$0")/.."
PORT="${SMOKE_PORT:-41999}"
# THE TOOLCHAIN MIGHT BE BEHIND A SHIM. On this box `cargo` is a mise shim that refuses without a pinned
# version; on the Windows box and on a CI image it is a real binary. Ask, rather than assuming either.
if cargo --version >/dev/null 2>&1 && { ! grep -q mold .cargo/config.toml 2>/dev/null || ld.mold --version >/dev/null 2>&1; }; then
  CARGO=(cargo)
elif command -v mise >/dev/null 2>&1; then
  # …and the LINKER may be behind a shim too. `.cargo/config.toml` here pins mold, which is untracked and
  # machine-specific, so the version is read from what mise has rather than written into this script — another
  # box may have a different one, or none, and then only rust is needed.
  TOOLS=(rust@stable)
  if grep -q mold .cargo/config.toml 2>/dev/null && ! ld.mold --version >/dev/null 2>&1; then
    m=$(mise ls --installed 2>/dev/null | awk '$1=="mold"{print $2; exit}')
    [ -n "$m" ] && TOOLS+=("mold@$m")
  fi
  CARGO=(mise x "${TOOLS[@]}" -- cargo)
  "${CARGO[@]}" --version >/dev/null 2>&1 || { echo "· mise has no usable rust toolchain" >&2; exit 2; }
else
  echo "· no usable cargo — install rustup, or 'mise use -g rust@stable'" >&2; exit 2
fi
DATA="$(mktemp -d "${TMPDIR:-/tmp}/antiloki-smoke.XXXX")"
PID=""
fail=0
step() { printf "  %-46s" "$1"; }
ok()   { echo "ok${1:+ · $1}"; }
bad()  { echo "FAILED${1:+ · $1}"; fail=1; }
# KILL BY LITERAL PID, NEVER BY PATTERN: `pkill -f antiloki` matches the shell running this script, and has
# killed this harness twice before.
cleanup() { [ -n "$PID" ] && kill "$PID" 2>/dev/null; rm -rf "$DATA"; }
trap cleanup EXIT

t0=$(date +%s)
echo "· smoke · port $PORT · data $DATA"

step "the serve builds"
if out=$("${CARGO[@]}" build -q -p antiloki-serve --bin antiloki-serve 2>&1); then ok; else bad "see below"; echo "$out" | tail -15; fi
[ "$fail" = 1 ] && { echo "· a build failure makes the rest meaningless — stopping"; exit 1; }

step "it starts and binds its port"
ANTILOKI_SERVE_PORT="$PORT" ANTILOKI_DATA_DIR="$DATA" ./target/debug/antiloki-serve > "$DATA/serve.log" 2>&1 &
PID=$!
up=0
for _ in $(seq 1 60); do curl -fsS -m 2 "http://127.0.0.1:$PORT/healthz" >/dev/null 2>&1 && { up=1; break; }; sleep 1; done
[ "$up" = 1 ] && ok "pid $PID" || { bad "never answered /healthz"; tail -20 "$DATA/serve.log"; exit 1; }

step "/healthz/engine - the engine is wired"
curl -fsS -m 10 "http://127.0.0.1:$PORT/healthz/engine" >/dev/null 2>&1 && ok || bad

step "the machine probe answers"
n=$(curl -fsS -m 20 "http://127.0.0.1:$PORT/api/v1/engines" 2>/dev/null | python3 -c 'import sys,json;print(len(json.load(sys.stdin).get("engines",[])))' 2>/dev/null)
[ "${n:-0}" = "3" ] && ok "$n engines" || bad "expected 3 engines, got ${n:-none}"

step "the folder picker answers honestly"
r=$(curl -fsS -m 15 -X POST "http://127.0.0.1:$PORT/api/v1/workspaces/pick-folder" 2>/dev/null)
echo "$r" | grep -q '"supported"' && ok || bad "no `supported` field — the Linux silence is back"

step "the route table is whole"
routes=$(grep -oE "[0-9]+ routes" "$DATA/serve.log" | tail -1 | grep -oE "[0-9]+")
[ "${routes:-0}" -ge 300 ] && ok "$routes routes" || bad "only ${routes:-0} routes registered"

step "the FE typechecks"
if out=$(cd ../antiloki-ui && npx tsc --noEmit -p tsconfig.json 2>&1); then ok; else bad; echo "$out" | head -10; fi

kill "$PID" 2>/dev/null; PID=""
echo "· $(( $(date +%s) - t0 ))s"
[ "$fail" = 0 ] && echo "· ALIVE" || echo "· BROKEN — do not merge"
exit "$fail"
