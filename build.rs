// THE FIXTURES ROOT (2026-09-21): the tests read committed testdata from the sibling repo
// `antiloki-crossing/fixtures/testdata` (the conformance crossing's fixtures). The crate was authored
// on Windows and every test hardcoded `C:/dev/antiloki/antiloki-crossing/fixtures/testdata`; the
// root is now a compile-time constant — `ANTILOKI_FIXTURES` in the environment wins (CI checks the
// crossing out wherever it likes), else the sibling of this crate's directory.
fn main() {
    let root = std::env::var("ANTILOKI_FIXTURES").ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| {
        let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        manifest.parent().map(|p| p.join("antiloki-crossing").join("fixtures").join("testdata")).unwrap_or_default().to_string_lossy().replace('\\', "/")
    });
    println!("cargo:rustc-env=ANTILOKI_FIXTURES={root}");
    println!("cargo:rerun-if-env-changed=ANTILOKI_FIXTURES");
}
