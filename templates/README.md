# Substrate scaffold (the "template DBs")

The empty substrate a **brand-new project / workspace** starts from: all 9 substrate DBs with the
full live schema + the canonical vocabulary (agents / actions / laws) seeded, but **zero operational
data** — "empty tables, ready to be filled".

- `db/*.sqlite` — the generated scaffold (9 files: `state memory brain laws knowledge timeline
  judgments code notebooks`). **Gitignored** — all DB binaries are local-only (see `.gitignore`;
  `brain.sqlite` in a real workspace is ~1.4 GB). Regenerable, so never committed.
- `make-scaffold.mjs` — generates **and verifies** the scaffold from the *live* schema, so it can
  never drift from `open-db.mjs`.

## Regenerate / re-verify
```
node --experimental-sqlite --disable-warning=ExperimentalWarning antiloki-core/templates/make-scaffold.mjs
```
It wipes `db/`, runs a fresh `openDb` (schema DDL + `seedCanonical`), switches the files to `DELETE`
journal mode (clean single-file copy-sources, no `-wal`/`-shm`), and **asserts**: 9 DBs present with
schema · vocabulary seeded · operational tables (cards / contracts / runtime_events / memories /
timeline / ai_calls …) empty · no WAL sidecars. Exits non-zero if anything is off.

## How a new workspace uses it
A new project's substrate is created by `system.project.registry { op:"register", provision:true }`
(or any `openDb` to a fresh `<root>/.antiloki/db`) — which produces this exact scaffold from the live
schema. This `db/` is the verified reference + a fast copy-source.

## Is it safe?
Yes. No operational data, and **no secrets** — BYOK keys + tokens live only in `~/.antiloki/auth`,
never in any DB. The vocabulary (agents/actions/laws) is the system's "constitution" — part of every
substrate, not user data.
