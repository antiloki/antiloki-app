# fx-flow

A cut-down shape of the React monorepo, for the Flow pre-pass (`flow-strip.rs`) and the workspace-alias
resolver (`dep-resolve.rs`): `shared/…` and `react-reconciler/…` are bare specifiers that only a
`workspaces` map can resolve. Three real files from facebook/react (MIT, headers kept):
`packages/shared/getComponentNameFromType.js`, `packages/react-reconciler/src/ReactChildFiber.js` and `getComponentNameFromFiber.js`.
Exercised by the Flow tests in `ast-parse.rs`, `atlas-ingest.rs` and `dep-resolve.rs` — not by `run.sh` (nothing to build).
