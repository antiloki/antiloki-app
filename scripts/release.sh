#!/usr/bin/env bash
# THE RELEASE (operator 2026-09-21, continuous delivery): ONE version everywhere, one tag, CI does the rest.
#
#   scripts/release.sh 0.2.0            bump + changelog stamp + commit + tag v0.2.0 (nothing pushed)
#   scripts/release.sh 0.2.0 --push     … and push the branch + the tag → .github/workflows/desktop.yml
#                                       builds mac/linux/windows into a DRAFT release with latest.json;
#                                       publishing the draft is the moment installed apps start updating.
#
# The version lives in tauri/tauri.conf.json (what the updater compares), tauri/Cargo.toml, serve/Cargo.toml
# (the upgrade guard's stamp), agent/Cargo.toml and the workspace Cargo.toml — CI refuses a tag whose version
# is not the conf's. CHANGELOG.md must already have a `## v<version>` heading (write the notes first).
set -euo pipefail
cd "$(dirname "$0")/.."
ver="${1:-}"; push="${2:-}"
[[ "$ver" =~ ^[0-9]+\.[0-9]+\.[0-9]+(\.[0-9]+)?$ ]] || { echo "usage: scripts/release.sh X.Y.Z[.W] [--push]" >&2; exit 2; }
# THE VERSION SCHEME (operator 2026-10-11): the public number may have a FOURTH part - 0.5.4.1, 0.5.4.2, ... - counted up build by build; 0.5.5 only when the operator says; 1.0 when the alpha ends.
# Tauri's updater, Cargo and the Windows installer need a three-part semver, so a four-part public version a.b.c.d is built as a.b.(c+1)-c.d (0.5.4.1 -> 0.5.5-4.1): that sorts AFTER 0.5.4 and
# BEFORE a real 0.5.5, so the updater offers it in the right order. The tag, the changelog heading, the release title and everything a person reads keep the public number.
sem=$(python3 - "$ver" <<'PY'
import sys
p = sys.argv[1].split(".")
print(".".join(p) if len(p) == 3 else f"{p[0]}.{p[1]}.{int(p[2]) + 1}-{p[2]}.{p[3]}")
PY
)

[[ -z "$(git status --porcelain)" ]] || { echo "the tree is not clean — commit or stash first" >&2; exit 2; }
grep -q "^## v$ver" CHANGELOG.md || { echo "CHANGELOG.md has no '## v$ver' heading — write the notes first" >&2; exit 2; }
git rev-parse -q --verify "refs/tags/v$ver" >/dev/null && { echo "tag v$ver already exists" >&2; exit 2; }
# every version field, in place
python3 - "$sem" <<'PY'
import json, re, sys
ver = sys.argv[1]
p = "tauri/tauri.conf.json"; c = json.load(open(p)); c["version"] = ver; open(p, "w").write(json.dumps(c, indent=2, ensure_ascii=False) + "\n")
for p in ["tauri/Cargo.toml", "serve/Cargo.toml", "agent/Cargo.toml", "Cargo.toml"]:
    s = open(p).read(); s2 = re.sub(r'^version = "[^"]+"', f'version = "{ver}"', s, count=1, flags=re.M)
    if s2 == s: print(f"no version line in {p}", file=sys.stderr); sys.exit(2)
    open(p, "w").write(s2)
PY
# the date on the changelog heading, if it has none yet
today="$(date -u +%Y-%m-%d)"
sed -i -E "s/^## v$ver( — [0-9]{4}-[0-9]{2}-[0-9]{2})?/## v$ver — $today/" CHANGELOG.md
cargo update -w -q 2>/dev/null || true   # Cargo.lock follows the crate versions
git add tauri/tauri.conf.json tauri/Cargo.toml serve/Cargo.toml agent/Cargo.toml Cargo.toml Cargo.lock CHANGELOG.md
git commit -q -m "release v$ver (version $sem)"
git tag -a "v$ver" -m "antiloki v$ver"
echo "v$ver (version $sem): committed and tagged (build date = the tag's push time, baked by CI)"
if [[ "$push" == "--push" ]]; then git push -q origin HEAD && git push -q origin "v$ver"; echo "pushed — watch: gh run list -w desktop; then scripts/release-move.sh $ver (no RELEASES_TOKEN: the draft lands here, this moves it to antiloki-releases and publishes)"; else echo "not pushed: git push origin HEAD && git push origin v$ver"; fi
