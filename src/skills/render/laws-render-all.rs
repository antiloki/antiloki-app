//! render/laws-render-all.rs — twin of skills/render/laws-render-all.mjs (db lane). NAMES ARE
//! CONTRACT: `lawsRenderAll` (default).
//!
//! BESPOKE-NATIVE (no conform-db fixture/frontier entry for this file, per the crossing
//! playbook): the JS writes AGENTS.md + CLAUDE.md to a hardcoded real repo path
//! (`PROJECT_ROOT = resolve(HERE, '..', '..', '..')`, `HERE` = this JS module's own directory —
//! `antiloki-core/skills/render`, 3 levels up = the repo root) with NO env-var override anywhere
//! in the JS source. A hardcoded-constant fs root, same category as god-driver-halt.rs's
//! HALT_FILE / migrate-list.rs's MIGRATIONS_DIR. Real file writes, not simulated; proven below
//! via native `#[cfg(test)]` tests — NEVER against the actual repo root (this fn writes
//! AGENTS.md/CLAUDE.md for real; a native test run must never mutate this repo's own committed
//! files).
//!
//! `project_root()`: JS's self-derived PROJECT_ROOT resolves, on every real deployment of this
//! substrate, to the identical directory as roots.mjs's `SUBSTRATE_ROOT` (both self-derive to
//! "parent of antiloki-core" — `resolve(HERE, '..','..','..')` from `antiloki-core/skills/render`
//! vs `resolve(ENGINE_ROOT, '..')` from `antiloki-core/skills/engine`), even though
//! laws-render-all.mjs computes it independently rather than importing roots.mjs. So this twin
//! reuses the SAME already-established Rust convention every sibling twin inlines for
//! SUBSTRATE_ROOT (workspace-root.rs / sandbox-resolve-path.rs / source-list.rs's own private
//! `substrate_root()`): `ANTILOKI_SUBSTRATE_ROOT` env override, else `CARGO_MANIFEST_DIR`
//! (antiloki-core-rust) parent — same real directory as the JS AND, unlike the JS, a
//! test-injection seam (per the playbook's bespoke-native rule: "temp dir + env-var injection
//! matching the god-driver-halt.rs / stack-facts.rs precedent").
//!
//! Writing logic is parameterized on the output root (`render_outputs_at`) so most tests inject
//! an explicit temp dir directly; one end-to-end test exercises the real `lawsRenderAll` export
//! itself, safely, via the `ANTILOKI_SUBSTRATE_ROOT` env-var seam pointed at a temp dir.
//!
//! JS `agentsMd({}, {db})` -> cross-module reuse of the already-converted
//! `render_agents_md::agentsMd` twin (per the playbook's cross-module rule).
//!
//! `o.content.length` (JS string length = UTF-16 code units, NOT byte length) ->
//! `.encode_utf16().count()`. `o.path.replace(PROJECT_ROOT, '').replace(/\\/g, '/')` (success
//! branch) -> strip the root prefix then normalize `\` to `/`; the catch branch keeps the
//! untouched absolute path (`results.push({ path: o.path, written: false, error: e.message,
//! role: o.role })`) — note the DIFFERENT key order vs the success branch
//! (path,bytes,role,written), transcribed exactly (gotcha #1).
//!
//! `context.db` missing -> JS throws `err.contextMissing('laws-render-all', 'db')`. Not modeled:
//! Rust's `db: &Connection` is a required, non-nullable reference — same "guard-throws are not
//! exercised" convention used by every sibling twin.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

const CLAUDE_MD_SHIM: &str = "# CLAUDE.md\n\n@AGENTS.md\n\n## Claude Code — extra discipline\n\nThe substrate laws above (rendered from `.antiloki/db/laws.sqlite` into\nAGENTS.md) apply to every AI agent that opens this repo. Two extra rules\napply specifically to Claude Code sessions:\n\n1. **Never edit AGENTS.md or this file by hand.** Both are auto-generated.\n   Source of truth is `.antiloki/db/laws.sqlite`. To change a rule:\n   `system.law.add` or `system.law.supersede`. To re-render:\n   `node bin/antiloki.mjs system.laws.render.all '{}'`.\n\n2. **Pre-commit hook runs consistency-scan.** If you bypass with\n   `--no-verify` you will leave the substrate in a drifted state and the\n   next session will surface a violation. Don't bypass without the human\n   asking you to.\n";

// See module doc: JS PROJECT_ROOT == roots.mjs SUBSTRATE_ROOT on every real deployment; same
// inline-per-twin convention as workspace-root.rs / sandbox-resolve-path.rs.
fn project_root() -> PathBuf {
    if let Ok(v) = std::env::var("ANTILOKI_SUBSTRATE_ROOT") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .to_path_buf()
}

struct RenderOutput<'a> {
    path: PathBuf,
    content: &'a str,
    role: &'static str,
}

// JS: `mkdirSync(dirname(o.path), { recursive: true }); writeFileSync(o.path, o.content, 'utf8');`
// wrapped in a per-output try/catch.
fn write_output(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

// JS `outputs` loop body. Parameterized on `root` so tests inject a temp dir instead of the real
// fixed PROJECT_ROOT.
fn render_outputs_at(root: &Path, agents_md_text: &str) -> Value {
    let outputs = [
        RenderOutput {
            path: root.join("AGENTS.md"),
            content: agents_md_text,
            role: "Cross-tool AI agent instructions (full law rendering, single source of truth)",
        },
        RenderOutput {
            path: root.join("CLAUDE.md"),
            content: CLAUDE_MD_SHIM,
            role: "Claude Code shim — imports @AGENTS.md + Claude-specific discipline",
        },
    ];

    let root_str = root.to_string_lossy().into_owned();
    let mut results = Vec::new();
    for o in outputs {
        let mut m = Map::new();
        match write_output(&o.path, o.content) {
            Ok(()) => {
                let path_str = o.path.to_string_lossy().into_owned();
                let rel = path_str
                    .strip_prefix(root_str.as_str())
                    .unwrap_or(path_str.as_str())
                    .replace('\\', "/");
                m.insert("path".into(), json!(rel));
                m.insert("bytes".into(), json!(o.content.encode_utf16().count() as i64));
                m.insert("role".into(), json!(o.role));
                m.insert("written".into(), json!(true));
            }
            Err(e) => {
                m.insert("path".into(), json!(o.path.to_string_lossy().into_owned()));
                m.insert("written".into(), json!(false));
                m.insert("error".into(), json!(e.to_string()));
                m.insert("role".into(), json!(o.role));
            }
        }
        results.push(Value::Object(m));
    }

    json!({ "results": results })
}

pub fn lawsRenderAll(_input: &Value, db: &Connection) -> Result<Value, String> {
    let agents_md_text = crate::render_agents_md::agentsMd(&json!({}), db)?;
    let agents_md_str = agents_md_text.as_str().unwrap_or_default();
    Ok(render_outputs_at(&project_root(), agents_md_str))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS laws; \
             CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT, kind TEXT, domain TEXT, title TEXT, statement TEXT, rationale TEXT, enforcement TEXT, enforcement_hook TEXT, trigger_sql TEXT, established_by TEXT, source_card TEXT, superseded_by INTEGER); \
             CREATE TABLE laws.law_clauses (id INTEGER PRIMARY KEY, law_id INTEGER, clause_type TEXT, text TEXT, order_index INTEGER);",
        )
        .unwrap();
        c
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-laws-render-all-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn render_outputs_at_writes_both_files_with_expected_shape_and_key_order() {
        let dir = temp_dir("basic");
        let r = render_outputs_at(&dir, "AGENTS BODY");

        let results = r["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);

        let agents = &results[0];
        assert_eq!(agents["path"], json!("/AGENTS.md"));
        assert_eq!(agents["bytes"], json!("AGENTS BODY".len() as i64));
        assert_eq!(
            agents["role"],
            json!("Cross-tool AI agent instructions (full law rendering, single source of truth)")
        );
        assert_eq!(agents["written"], json!(true));
        let agents_keys: Vec<&String> = agents.as_object().unwrap().keys().collect();
        assert_eq!(agents_keys, vec!["path", "bytes", "role", "written"]);

        let claude = &results[1];
        assert_eq!(claude["path"], json!("/CLAUDE.md"));
        assert_eq!(
            claude["role"],
            json!("Claude Code shim — imports @AGENTS.md + Claude-specific discipline")
        );
        assert_eq!(claude["written"], json!(true));
        assert_eq!(claude["bytes"], json!(CLAUDE_MD_SHIM.encode_utf16().count() as i64));

        assert_eq!(fs::read_to_string(dir.join("AGENTS.md")).unwrap(), "AGENTS BODY");
        assert_eq!(fs::read_to_string(dir.join("CLAUDE.md")).unwrap(), CLAUDE_MD_SHIM);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_outputs_at_creates_missing_parent_dir() {
        let base = temp_dir("nested-parent");
        let nested_root = base.join("does").join("not").join("exist");
        assert!(!nested_root.exists());
        let r = render_outputs_at(&nested_root, "X");
        assert_eq!(r["results"][0]["written"], json!(true));
        assert!(nested_root.join("AGENTS.md").exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn render_outputs_at_bytes_counts_utf16_code_units_not_utf8_byte_length() {
        let dir = temp_dir("utf16");
        // "—" (em dash, U+2014) is 3 UTF-8 bytes but 1 UTF-16 code unit — JS `.length` counts 1.
        let content = "a—b";
        let r = render_outputs_at(&dir, content);
        assert_eq!(content.len(), 5); // UTF-8 byte length (would be the wrong answer here)
        assert_eq!(r["results"][0]["bytes"], json!(3i64)); // UTF-16 code units: a, —, b
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_outputs_at_write_failure_is_captured_not_panicking_with_error_key_order() {
        let dir = temp_dir("write-fails");
        // Make the AGENTS.md target path itself a directory so std::fs::write fails there.
        fs::create_dir_all(dir.join("AGENTS.md")).unwrap();
        let r = render_outputs_at(&dir, "X");

        let agents = &r["results"][0];
        assert_eq!(agents["written"], json!(false));
        assert_eq!(agents["path"], json!(dir.join("AGENTS.md").to_string_lossy().into_owned()));
        assert!(!agents["error"].as_str().unwrap().is_empty());
        let agents_keys: Vec<&String> = agents.as_object().unwrap().keys().collect();
        assert_eq!(agents_keys, vec!["path", "written", "error", "role"]);

        // CLAUDE.md target is untouched (not a directory) so it still succeeds independently.
        assert_eq!(r["results"][1]["written"], json!(true));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_root_defaults_to_cargo_manifest_dir_parent() {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT");
        }
        let expected = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
        assert_eq!(project_root(), expected);
    }

    #[test]
    fn project_root_honors_antiloki_substrate_root_override() {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", "C:/somewhere/else");
        }
        let r = project_root();
        unsafe {
            std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT");
        }
        assert_eq!(r, PathBuf::from("C:/somewhere/else"));
    }

    #[test]
    fn laws_render_all_end_to_end_via_env_override_writes_real_files_into_a_temp_root() {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let dir = temp_dir("end-to-end");
        unsafe {
            std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", dir.to_str().unwrap());
        }
        let c = db();
        c.execute(
            "INSERT INTO laws.laws (id,slug,kind,domain,title,statement,rationale,enforcement,established_by) \
             VALUES (1,'X_LAW','constitutional','audit','T','S','R','hard','human:dani')",
            [],
        )
        .unwrap();
        let result = lawsRenderAll(&json!({}), &c);
        unsafe {
            std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT");
        }
        let r = result.unwrap();

        assert_eq!(r["results"][0]["written"], json!(true));
        assert_eq!(r["results"][1]["written"], json!(true));
        let agents_on_disk = fs::read_to_string(dir.join("AGENTS.md")).unwrap();
        assert!(agents_on_disk.contains("X_LAW"));
        let claude_on_disk = fs::read_to_string(dir.join("CLAUDE.md")).unwrap();
        assert_eq!(claude_on_disk, CLAUDE_MD_SHIM);

        let _ = fs::remove_dir_all(&dir);
    }
}
