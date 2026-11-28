# Contributing to antiloki

Thanks for helping. antiloki is Apache-2.0; by contributing you agree your work is licensed the same way.

## Before you start

- To add a seat or a node type, start from [EXTENDING.md](EXTENDING.md).
- Open an issue first for anything bigger than a small fix, so we agree on the direction. [ARCHITECTURE.md](ARCHITECTURE.md) says how the parts fit.
- Read the docs: <https://antiloki.com/docs.html>.
- The front end lives in [`antiloki/antiloki-ui`](https://github.com/antiloki/antiloki-ui). The two repos sit side by side (see the README).

## Working without running out of memory

A full Rust build is heavy. On a 16 GB machine:

- run **one** cargo job at a time, with a memory cap if you can, for example on Linux:
  `systemd-run --user --scope -p MemoryMax=4500M cargo test -p antiloki-serve --lib -j 2`
- do not run cargo next to browser tests;
- prefer `cargo check` while you iterate, and the full tests before you open a pull request.

## Rules for a change

1. **A test before the claim.** Fix a bug with a test that fails without the fix.
2. **Deterministic first.** Let plain rules decide what rules can decide; use a model only for questions about meaning.
3. **A failure must say why.** A silent path is a bug.
4. **Windows must work.** In `serve`, do not call `Command::new`, `sh -c` or `which` directly: use `crate::proc`. Do not assume POSIX paths. If you touch process or path code, run `cargo xwin check -p antiloki-serve`.
5. **Do not publish unmeasured claims** in docs, release notes or the site.
6. **Never commit a secret.** Signing keys and API keys are read from the environment or a file, never baked into a binary.
7. Keep a change small and one-purpose. Update `CHANGELOG.md` for anything a user would notice.

## Pull requests

- Branch from `main`, keep the history readable, describe what changed and why.
- CI runs the `serve` and `agent` tests and the front end's typecheck and tests. They must pass.
- The core library's conformance suite needs fixtures from a private repository; CI does not run it.

## Security issues

Do not open a public issue. See [SECURITY.md](SECURITY.md).
