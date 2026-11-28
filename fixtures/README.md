# Fixtures — the rig, not the benchmark

Six small projects, one per technology, so that antiloki's discovery, gates and technology axis can be tested
against **real directories** instead of hand-built structs. PLAN-THE-LEAN-YEARS I.4.

Until now everything antiloki claimed about "your codebase" was measured against `ministore`: 19 files and a
`package.json`. One React app, standing in for every project anyone would ever open.

Each fixture is deliberately tiny and, where the language allows, **needs nothing installed**: `fx-node-api`
uses node's built-in test runner, `fx-rust` needs only cargo. Where a toolchain genuinely is required the gate
is expected to say so — "the box's dependencies are not installed" is a correct answer and one worth testing.

| fixture | stack | exercises |
|---|---|---|
| `fx-react` | `web` | a UI framework in the manifest → `web`, and the npm-script branch of `gate_cmd` |
| `fx-node-api` | `node` | a package.json with no UI framework is a service, not a frontend |
| `fx-rust` | `rust` | Cargo discovery, and `[lints]` as the project declaring it lints |
| `fx-python` | `python` | pyproject, pytest and ruff declared as tools |
| `fx-go` | `go` | `go test ./...`, and vet as a linter that always exists |
| `fx-mixed` | `mixed` | a repo that is two stacks says so rather than picking a winner |

## Running them

```bash
fixtures/run.sh
```

Reports what this machine could run and what it skipped. **A skipped fixture is never counted as a pass** —
the same rule the gates follow, for the same reason: "no Go on this box" and "the Go tests passed" are
different facts, and only one of them is evidence.

On this Linux box today: 3 pass (`fx-rust`, `fx-mixed`, `fx-node-api`), 3 skip (no Go toolchain, no pytest, and
`fx-react` has no `node_modules` **on purpose** — "the box's dependencies are not installed" is an answer the
gates must give, and this is where it is exercised).

Two traps the runner handles rather than assumes, both of which cost an afternoon when they were discovered:

- **`command -v cargo` is not enough.** On a machine using a tool manager, `cargo` is a shim that exists on the
  PATH and then refuses to run without a pinned version. Only asking it for its version tells a usable
  toolchain from a stub.
- **`RUSTFLAGS` is cleared for the Rust fixtures.** Cargo merges config from every ancestor directory and
  *joins* `rustflags` arrays rather than overriding them, so this repo's untracked, machine-local
  `.cargo/config.toml` — which points at the mold linker through a shim — otherwise reaches inside a fixture
  that is supposed to build anywhere. A child `.cargo/config.toml` cannot undo it; the environment can.

Both Rust fixtures carry an empty `[workspace]` table so the parent workspace does not absorb them: without it
cargo refuses to build them standalone, and the main build carries a test rig it has no business compiling.

They are **not** benchmark tasks. No model is asked to do anything here. They exist so that
`gates::probe`, `gates::gate_cmd` and `gates::stack_of` are tested against directories that behave like real
ones, on both operating systems.
