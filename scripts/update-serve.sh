#!/usr/bin/env bash
# THE QUICK UPDATE (operator 2026-11-18: "how can we update so I see the most updated code in a faster way"):
# build the serve and the agent from this worktree, put them in a versioned folder, switch `main` to it, restart the serve, check it, and roll back by itself when it is unhealthy.
#   scripts/update-serve.sh          fast: a debug build in target-fast, incremental (the first one ~9 min, the next ones a few minutes); a bit slower to run than the release
#   scripts/update-serve.sh release  the optimized build (thin LTO, ~20 min): for the night
# The web UI needs none of this: the vite on :5199 reloads on every save. Terminals live in a separate process and survive the restart.
# Never pushes, never touches the person's own files. Status: ~/.local/share/antiloki/update-status.txt. Run it in the background: `setsid nohup scripts/update-serve.sh &`.
set -u
MODE=${1:-fast}; B=${ANTILOKI_BIN_DIR:-$HOME/.local/share/antiloki-bin}; ST=$HOME/.local/share/antiloki/update-status.txt; L=$HOME/.local/share/antiloki/update-build.log; PORT=${ANTILOKI_SERVE_PORT:-41999}
say() { echo "$(date '+%F %T') $*" >> "$ST"; }
: > "$ST"; cd "$(dirname "$0")/.." || exit 1; ROOT=$(pwd); say "update ($MODE) from $(git rev-parse --short HEAD) in $ROOT"
if [ "$MODE" = release ]; then PROFILE=release; FLAG=--release; TD=$ROOT/../target-release; MEM=5500M; else PROFILE=debug; FLAG=; TD=$ROOT/target-fast; MEM=4500M; fi
systemd-run --user --scope -q -p MemoryMax=$MEM nice -n 10 mise x rust@stable mold@2.42.0 -- env CARGO_TARGET_DIR="$TD" CARGO_INCREMENTAL=1 cargo build $FLAG -j 2 -p antiloki-serve -p antiloki-agent > "$L" 2>&1
rc=$?; say "cargo exit $rc"
[ $rc -ne 0 ] && { say "FAILED: the build did not finish; the running serve is untouched. $(tail -3 "$L" | tr '\n' ' ' | cut -c1-300)"; exit 1; }
# THE WEB APP THE PHONE IS HANDED (2026-11-18: the QR showed a very old antiloki): the serve hands out the `dist` of the folder it runs from, and this script used to link the OLD one forward; build the current one every time
FE=${ANTILOKI_FE_SRC:-$ROOT/../wt-fe-cowork}; FEOK=0
if [ -d "$FE" ]; then say "building the web app from $FE"; ( cd "$FE" && systemd-run --user --scope -q -p MemoryMax=3500M nice -n 10 mise x node@24.19.0 -- npm run build >> "$L" 2>&1 ) && FEOK=1 || say "WARNING: the web app did not build; the previous one is kept"; fi
N=$B/main-$(date +%Y%m%d-%H%M%S)-$MODE; old=$(readlink "$B/main")
mkdir -p "$N" && cp "$TD/$PROFILE/antiloki-serve" "$N/" && cp "$TD/$PROFILE/antiloki-agent" "$N/" && { if [ $FEOK -eq 1 ]; then cp -r "$FE/dist" "$N/dist"; else ln -sfn "$old/dist" "$N/dist"; fi; } || { say "FAILED: could not stage $N"; exit 1; }
say "staged $N; previous main was $old"
start() { cd "$HOME" && setsid nohup env ANTILOKI_SERVE_PORT=$PORT ANTILOKI_BACKGROUND=1 ANTILOKI_PTY_HOST=sidecar ANTILOKI_PTY_SOCK=$HOME/.local/share/antiloki/pty.sock ANTILOKI_AGENT_BIN=$B/main/antiloki-agent ANTILOKI_FE_DIR=$B/main/dist "$B/main/antiloki-serve" >> "$HOME/.local/share/antiloki/serve-fast.log" 2>&1 & }
stop() { p=$(ss -ltnp 2>/dev/null | grep ":$PORT " | grep -o 'pid=[0-9]*' | head -1 | cut -d= -f2); [ -n "$p" ] && kill "$p"; for i in 1 2 3 4 5 6 7 8 9 10; do ss -ltn 2>/dev/null | grep -q ":$PORT " || break; sleep 1; done; }
stop; ln -sfn "$N" "$B/main"; start
# a busy machine starts it slowly: wait up to 120 s for it to answer, then check
for i in $(seq 1 24); do sleep 5; curl -s -m 5 -o /dev/null -w "%{http_code}" "localhost:$PORT/api/v1/engines" | grep -q 200 && break; done
ok=1; for p in /api/v1/engines /api/v1/sagas; do curl -s -m 90 -o /dev/null -w "%{http_code}" "localhost:$PORT$p" | grep -q 200 || ok=0; done   # the first sagas list can take 15 s on a real workspace (the serve warms it itself, but it may still be doing it)
curl -s -m 60 -X POST "localhost:$PORT/api/v1/cockpit/cowork/threads" -H 'content-type: application/json' -d '{"op":"list"}' | grep -q '"ok":true' || ok=0
# KEEP THE LAST THREE VERSIONS (2026-11-20: 53 saved versions of 1.1 GB filled the disk and crashed the browser and the linker): the live one and the two before it are the rollback; older ones are rebuilt, never needed
prune() { [ -n "$B" ] && [ -d "$B" ] && ( cd "$B" && ls -dt main-*-fast main-*-release 2>/dev/null | tail -n +4 | xargs -r rm -rf ) ; }
if [ $ok -eq 1 ]; then prune; say "INSTALLED and healthy: $N"; else say "UNHEALTHY: rolling back to $old"; stop; ln -sfn "$old" "$B/main"; start; sleep 10; say "rolled back to $old"; fi
