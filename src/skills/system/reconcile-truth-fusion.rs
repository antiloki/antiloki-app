//! reconcile-truth-fusion.rs — twin of skills/system/reconcile-truth-fusion.mjs (db lane,
//! cross-db `memory`). NAMES ARE CONTRACT: 5 db-first named helpers —
//! `detectCardDoneWithoutGitCommit`, `detectContractWithoutAnyArtifacts`,
//! `detectAuditDisagreesWithCurrentCard`, `detectAuditDisagreesWithCurrentContract`,
//! `detectCardCorrectionWithoutFollowup` — plus `reconcileTruthFusion` (default, standard lane),
//! the aggregator that unions all 5 detectors' findings and tallies `summary.by_severity`. Card
//! #383, companion to reconcile.mjs (a separate, unconverted module — its own 6 detectors are
//! untouched by this file).
//!
//! GIT SUBPROCESS — DISCLOSED, NOT CONFORM-DB-EXERCISED (constitution-estimate.rs's
//! `git_diff_paths` precedent, playbook lesson 5's flag-skippable pattern): `safeGitLogGrep`
//! (`execSync('git log -F -i --all --grep="<pattern>" --oneline -n <maxLines>', {cwd: REPO_ROOT})`)
//! is ported here as a REAL `git` subprocess call for production parity — but it is called
//! unconditionally, once per row, from inside `detectCardDoneWithoutGitCommit`'s `dones` loop, and
//! that loop's row source (`WHERE status = 'done'`) IS data-gated. Both this unit's own
//! conform-db fixture and the `reconcileTruthFusion` aggregator's fixture (which shares the same
//! `cards` table) deliberately seed ZERO `status='done'` rows, so the loop body — and therefore
//! the subprocess — never executes: a zero-iteration loop, not a stubbed branch. This avoids
//! pinning the fixture to this repo's actual git/non-git state at `REPO_ROOT`. The git-invoking
//! path itself IS proven correct, but via a `#[cfg(test)]` NATIVE test against a real temp-dir git
//! repo (uncommitted-dev-output.rs / reconcile-external-code-drift.rs precedent), not conform-db.
//!
//! `REPO_ROOT` (JS: `resolve(HERE, '..', '..', '..')` from a file under
//! `antiloki-core/skills/system`) resolves to the antiloki workspace root — the SAME real
//! directory this crate's `CARGO_MANIFEST_DIR`'s parent resolves to (both files sit 2 levels
//! under their respective repo roots' `skills/`-equivalent trees). `ANTILOKI_SUBSTRATE_ROOT` is a
//! TEST-ONLY injection seam (no JS equivalent, never fires in production) — same lever
//! `reconcile-external-code-drift.rs` / `card-commits.rs` / `uncommitted-dev-output.rs` already
//! use for this identical real directory.
//!
//! `detectContractWithoutAnyArtifacts`'s `resolve(REPO_ROOT, f)` / `resolve(REPO_ROOT,
//! sandbox_path, f)` + `existsSync` calls ARE conform-db-fixture-tested (constitution-estimate.rs's
//! committed-testdata-dir precedent, applied to already-real repo files instead of a
//! purpose-built testdata dir): the fixture references real, stable, already-committed files
//! (`CLAUDE.md` at the workspace root; `antiloki-core-rust/Cargo.toml` reached via a
//! `sandbox_path` of `antiloki-core-rust`) — both lanes read the identical real bytes.
//!
//! `context.db` missing → JS throws `err.contextMissing('reconcile-truth-fusion', 'db')`. Not
//! modeled: `db: &Connection` is a required, non-nullable reference — same "guard-throws are not
//! exercised" convention every sibling twin uses. `input` is unused by the JS aggregator body
//! (only `context.db` is read) — `_input` here for the same reason `reconcileExternalCodeDrift`
//! uses an ignored parameter.
// QUALITY: db-first x5 + 1 standard aggregator; git subprocess real+correct, proven by native test
// only (disclosed above); fs-exists checks in detectContractWithoutAnyArtifacts ARE fixture-tested
// against real repo files.

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const FROZEN_ISO: &str = "2026-01-01T00:00:00.000Z";

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`

// `function safeJsonParse(s, fallback = null) { try { return JSON.parse(s ?? ''); } catch {
// return fallback; } }`
fn safe_json_parse(s: Option<&Value>, fallback: Value) -> Value {
    match s.and_then(Value::as_str) {
        Some(text) => serde_json::from_str(text).unwrap_or(fallback),
        None => fallback,
    }
}

fn card_slug_prefix_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^(EPIC|FEAT|FIX|TRIVIAL|PROOF)-").unwrap())
}

// See module doc: TEST-ONLY injection seam, no JS equivalent. Defaults to the real antiloki
// workspace root (this crate's parent dir), matching JS's REPO_ROOT computation exactly.
fn repo_root() -> PathBuf {
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

/// `function safeGitLogGrep(pattern, maxLines = 20)` — see module doc: real subprocess, disclosed
/// zero-iteration-untested-by-conform-db (proven instead by a native temp-repo test below). The
/// JS builds one shell-interpreted command string with the pattern double-quoted inline; passing
/// `--grep=<pattern>` as a single `Command` arg (no shell) achieves the same effective grep term
/// without a shell-quoting dependency.
fn safe_git_log_grep(root: &Path, pattern: &str, max_lines: i64) -> Vec<String> {
    let output = crate::proc::cmd("git")
        .args(["log", "-F", "-i", "--all", &format!("--grep={pattern}"), "--oneline", "-n", &max_lines.to_string()])
        .current_dir(root)
        .output();
    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .split('\n')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect(),
        _ => Vec::new(), // spawn failure, missing git, non-zero exit (not a repo, no match with --grep and nonzero? no: git log exits 0 on no matches) — any failure collapses to []
    }
}

/// `node:path` win32 `isAbsolute` — same disclosed win32-only scope as constitution-estimate.rs's
/// private copy (this file carries its own, per that file's documented convention).
fn win_is_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    if b.is_empty() { return false; }
    if b[0] == b'/' || b[0] == b'\\' { return true; }
    if b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\') { return true; }
    false
}

/// `resolve(REPO_ROOT, f)` (2-arg form).
fn abs_path(root: &Path, p: &str) -> PathBuf {
    if win_is_absolute(p) { PathBuf::from(p) } else { root.join(p) }
}

/// `resolve(REPO_ROOT, sandboxPath, f)` (3-arg form) — Node's `path.resolve` scans right-to-left
/// for the first absolute segment. `f`/`sandboxPath` are always relative in practice (declared
/// touched files + contract sandbox paths), so this covers the realistic input space while
/// staying correct for the absolute-segment edge.
fn resolve3(root: &Path, sandbox: &str, f: &str) -> PathBuf {
    if win_is_absolute(f) { return PathBuf::from(f); }
    if win_is_absolute(sandbox) { return Path::new(sandbox).join(f); }
    root.join(sandbox).join(f)
}

/// `export function detectCardDoneWithoutGitCommit(db)`
pub fn detectCardDoneWithoutGitCommit(db: &Connection) -> Value {
    let root = repo_root();
    let dones = crate::db_json::query_rows(
        db,
        "SELECT slug, updated_at FROM cards WHERE status = 'done' ORDER BY updated_at DESC LIMIT 50",
        &[],
    )
    .unwrap_or(json!([]));
    let mut findings: Vec<Value> = Vec::new();
    for c in dones.as_array().cloned().unwrap_or_default() {
        let slug = c.get("slug").cloned().unwrap_or(Value::Null);
        let updated_at = c.get("updated_at").cloned().unwrap_or(Value::Null);
        let slug_s = tmpl(Some(&slug)); // JS: safeGitLogGrep(c.slug, 3) → pattern embedded via `${pattern}`
        let commits = safe_git_log_grep(&root, &slug_s, 3);
        if commits.is_empty() {
            let mut detail = Map::new();
            detail.insert("card_slug".into(), slug.clone());
            detail.insert("updated_at".into(), updated_at);
            detail.insert("searched_commits_for".into(), slug.clone());
            findings.push(json!({
                "kind": "card_done_no_git_commit",
                "severity": "warn",
                "message": format!(
                    "Card '{slug_s}' is status=done but no git commit references the slug. Verify the work was actually committed (or the commit message was missing the slug reference)."
                ),
                "card_slug": slug,
                "detail": Value::Object(detail),
                "suggested_action": "Inspect the work: was a commit actually made? If yes, the commit message just missed the slug — informational. If no, the card was prematurely marked done; investigate before further work.",
            }));
        }
    }
    json!(findings)
}

/// `export function detectContractWithoutAnyArtifacts(db)`
pub fn detectContractWithoutAnyArtifacts(db: &Connection) -> Value {
    let root = repo_root();
    let rows = crate::db_json::query_rows(
        db,
        "SELECT slug, status, files_touched_json, sandbox_path FROM contracts \
         WHERE status IN ('tl_approved', 'impl', 'qa') AND superseded_by IS NULL",
        &[],
    )
    .unwrap_or(json!([]));
    let mut findings: Vec<Value> = Vec::new();
    for c in rows.as_array().cloned().unwrap_or_default() {
        let files = safe_json_parse(c.get("files_touched_json"), json!([]));
        let Some(files_arr) = files.as_array() else { continue };
        if files_arr.is_empty() { continue; } // covered by other detectors

        let sandbox_v = c.get("sandbox_path").cloned().unwrap_or(Value::Null);
        let sandbox_str = if truthy(Some(&sandbox_v)) { sandbox_v.as_str() } else { None };

        let mut any_exists = false;
        for f in files_arr {
            let Some(fs) = f.as_str() else { continue };
            if abs_path(&root, fs).exists() { any_exists = true; break; }
            if let Some(sb) = sandbox_str {
                if resolve3(&root, sb, fs).exists() { any_exists = true; break; }
            }
        }

        if !any_exists {
            let slug = c.get("slug").cloned().unwrap_or(Value::Null);
            let status = c.get("status").cloned().unwrap_or(Value::Null);
            let mut detail = Map::new();
            detail.insert("contract_slug".into(), slug.clone());
            detail.insert("status".into(), status.clone());
            detail.insert("files_touched".into(), files.clone());
            detail.insert("sandbox_path".into(), sandbox_v);
            findings.push(json!({
                "kind": "contract_active_no_artifacts",
                "severity": "error",
                "message": format!(
                    "Contract '{}' is status={} but NONE of its {} declared files_touched exist in working tree OR sandbox. DEV produced no output; likely interrupted mid-implementation.",
                    tmpl(Some(&slug)), tmpl(Some(&status)), files_arr.len()
                ),
                "contract_slug": slug,
                "detail": Value::Object(detail),
                "suggested_action": "Inspect via Pipeline mode. Either: rerun dev.implement.code (if AI was the failure), or contract.admin.reject to trigger auto-redraft.",
            }));
        }
    }
    json!(findings)
}

/// `export function detectAuditDisagreesWithCurrentCard(db)`
pub fn detectAuditDisagreesWithCurrentCard(db: &Connection) -> Value {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT c.slug, c.status AS current_status, \
         (SELECT after_json FROM sacred_timeline \
          WHERE entity_type='card' AND entity_id = c.slug \
          ORDER BY id DESC LIMIT 1) AS last_audit_after_json \
         FROM cards c WHERE c.status NOT IN ('archived')",
        &[],
    )
    .unwrap_or(json!([]));
    let mut findings: Vec<Value> = Vec::new();
    for r in rows.as_array().cloned().unwrap_or_default() {
        let raw = r.get("last_audit_after_json");
        if !truthy(raw) { continue; } // never audited
        let after = safe_json_parse(raw, Value::Null);
        let Some(after_obj) = after.as_object() else { continue };
        let Some(after_status) = after_obj.get("status").and_then(Value::as_str) else { continue };
        let current_status = r.get("current_status").cloned().unwrap_or(Value::Null);
        let current_status_s = current_status.as_str().unwrap_or("");
        if after_status != current_status_s {
            let slug = r.get("slug").cloned().unwrap_or(Value::Null);
            findings.push(json!({
                "kind": "audit_disagrees_with_current_card",
                "severity": "error",
                "message": format!(
                    "Card '{}': sacred_timeline says status='{after_status}' but cards.status='{current_status_s}'. Direct UPDATE bypassed the audit chain.",
                    tmpl(Some(&slug))
                ),
                "card_slug": slug.clone(),
                "detail": { "card_slug": slug, "current_status": current_status, "audit_status": after_status },
                "suggested_action": "This is a NEVER_BYPASS_WITH_AUDIT violation. Investigate which caller mutated cards.status without with-audit composition. If the current state is correct, re-audit via a no-op status change to restore chain continuity.",
            }));
        }
    }
    json!(findings)
}

/// `export function detectAuditDisagreesWithCurrentContract(db)` — same pattern as detector 3,
/// scoped to contracts.status with no status filter (JS has no `WHERE ... NOT IN (...)` here).
pub fn detectAuditDisagreesWithCurrentContract(db: &Connection) -> Value {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT c.slug, c.status AS current_status, \
         (SELECT after_json FROM sacred_timeline \
          WHERE entity_type='contract' AND entity_id = c.slug \
          ORDER BY id DESC LIMIT 1) AS last_audit_after_json \
         FROM contracts c",
        &[],
    )
    .unwrap_or(json!([]));
    let mut findings: Vec<Value> = Vec::new();
    for r in rows.as_array().cloned().unwrap_or_default() {
        let raw = r.get("last_audit_after_json");
        if !truthy(raw) { continue; }
        let after = safe_json_parse(raw, Value::Null);
        let Some(after_obj) = after.as_object() else { continue };
        let Some(after_status) = after_obj.get("status").and_then(Value::as_str) else { continue };
        let current_status = r.get("current_status").cloned().unwrap_or(Value::Null);
        let current_status_s = current_status.as_str().unwrap_or("");
        if after_status != current_status_s {
            let slug = r.get("slug").cloned().unwrap_or(Value::Null);
            findings.push(json!({
                "kind": "audit_disagrees_with_current_contract",
                "severity": "error",
                "message": format!(
                    "Contract '{}': sacred_timeline says status='{after_status}' but contracts.status='{current_status_s}'. Direct UPDATE bypassed the audit chain.",
                    tmpl(Some(&slug))
                ),
                "contract_slug": slug.clone(),
                "detail": { "contract_slug": slug, "current_status": current_status, "audit_status": after_status },
                "suggested_action": "NEVER_BYPASS_WITH_AUDIT violation. Locate the call site that mutated contracts.status without with-audit composition.",
            }));
        }
    }
    json!(findings)
}

/// `export function detectCardCorrectionWithoutFollowup(db)` — JS wraps the WHOLE body (query +
/// loop) in one try/catch that swallows any failure (e.g. `memory.memories` missing on a legacy
/// substrate) and returns whatever `findings` had accumulated. Only the outer query's failure
/// mode is modeled here (the realistic one — a missing/unattached cross-db table); the per-row
/// `cards` lookup below is a plain `SELECT ... WHERE slug = ?` that cannot realistically throw for
/// a string parameter, so a query error there is treated as "no such card" (skip) rather than
/// aborting the whole loop — an unreachable-in-practice divergence from JS's single try/catch.
pub fn detectCardCorrectionWithoutFollowup(db: &Connection) -> Value {
    let rows = match crate::db_json::query_rows(
        db,
        "SELECT m.id, m.created_at, m.tags_json FROM memory.memories m \
         WHERE m.type = 'correction' ORDER BY m.id DESC LIMIT 100",
        &[],
    ) {
        Ok(v) => v,
        Err(_) => return json!([]), // memory DB may be missing on legacy substrates
    };
    let mut findings: Vec<Value> = Vec::new();
    for m in rows.as_array().cloned().unwrap_or_default() {
        let tags = safe_json_parse(m.get("tags_json"), json!([]));
        let Some(tags_arr) = tags.as_array() else { continue };
        let Some(card_slug_v) = tags_arr
            .iter()
            .find(|t| t.as_str().map_or(false, |s| card_slug_prefix_re().is_match(s)))
            .cloned()
        else {
            continue;
        };
        let card_slug_s = card_slug_v.as_str().unwrap_or("");

        let card = crate::db_json::query_rows(db, "SELECT slug, status, updated_at FROM cards WHERE slug = ?", &[json!(card_slug_s)])
            .ok()
            .and_then(|rows| rows.as_array().and_then(|a| a.first()).cloned());
        let Some(card) = card else { continue };

        let status = card.get("status").and_then(Value::as_str).unwrap_or("");
        let updated_at = card.get("updated_at").and_then(Value::as_str).unwrap_or("");
        let created_at = m.get("created_at").and_then(Value::as_str).unwrap_or("");
        if status == "done" && updated_at < created_at {
            let m_id = m.get("id").cloned().unwrap_or(Value::Null);
            findings.push(json!({
                "kind": "card_correction_without_followup",
                "severity": "info",
                "message": format!(
                    "Card '{card_slug_s}' is done but a correction memory (#{}) was filed AFTER the card closed. The correction may suggest follow-up work.",
                    tmpl(Some(&m_id))
                ),
                "card_slug": card_slug_v.clone(),
                "detail": { "card_slug": card_slug_v, "card_status": status, "memory_id": m_id, "memory_created_at": created_at },
                "suggested_action": "Read the correction memory; if it indicates remaining work, file a follow-up card.",
            }));
        }
    }
    json!(findings)
}

/// `export default function reconcileTruthFusion(input, context)`
pub fn reconcileTruthFusion(_input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `if (!db) throw err.contextMissing(...)` — unreachable in the db lane (db always present).
    let mut findings: Vec<Value> = Vec::new();
    findings.extend(detectCardDoneWithoutGitCommit(db).as_array().cloned().unwrap_or_default());
    findings.extend(detectContractWithoutAnyArtifacts(db).as_array().cloned().unwrap_or_default());
    findings.extend(detectAuditDisagreesWithCurrentCard(db).as_array().cloned().unwrap_or_default());
    findings.extend(detectAuditDisagreesWithCurrentContract(db).as_array().cloned().unwrap_or_default());
    findings.extend(detectCardCorrectionWithoutFollowup(db).as_array().cloned().unwrap_or_default());

    let mut by_sev: Map<String, Value> = Map::new();
    for f in &findings {
        let sev = f.get("severity").and_then(Value::as_str).unwrap_or("").to_string();
        let n = by_sev.get(&sev).and_then(Value::as_i64).unwrap_or(0) + 1;
        by_sev.insert(sev, json!(n));
    }

    Ok(json!({
        "ran_at": FROZEN_ISO,
        "findings": findings,
        "summary": { "total": findings.len() as i64, "by_severity": Value::Object(by_sev) },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const CARDS_SCHEMA: &str = "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT, updated_at TEXT)";
    const CONTRACTS_SCHEMA: &str = "CREATE TABLE contracts (slug TEXT PRIMARY KEY, status TEXT, files_touched_json TEXT, sandbox_path TEXT, superseded_by TEXT)";
    const TIMELINE_SCHEMA: &str = "CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY AUTOINCREMENT, entity_type TEXT, entity_id TEXT, after_json TEXT)";
    const MEMORIES_SCHEMA: &str = "CREATE TABLE memory.memories (id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT, tags_json TEXT, created_at TEXT)";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory").unwrap();
        c.execute_batch(&format!("{CARDS_SCHEMA}; {CONTRACTS_SCHEMA}; {TIMELINE_SCHEMA}; {MEMORIES_SCHEMA};")).unwrap();
        c
    }

    // ── detectCardDoneWithoutGitCommit ──────────────────────────────────

    #[test]
    fn card_done_zero_rows_never_invokes_git() {
        let c = db();
        c.execute("INSERT INTO cards (slug, status, updated_at) VALUES ('BACKLOG-1', 'backlog', '2026-01-01 00:00:00')", []).unwrap();
        assert_eq!(detectCardDoneWithoutGitCommit(&c), json!([]));
    }

    // SAFETY: ANTILOKI_SUBSTRATE_ROOT is a process-global env var — shared crate-wide lock, same
    // reasoning as reconcile-external-code-drift.rs's with_substrate_root helper.
    fn with_substrate_root<T>(root: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", root); }
        let result = f();
        unsafe { std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT"); }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-reconcile-truth-fusion-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn git(args: &[&str], cwd: &Path) {
        let status = crate::proc::cmd("git").args(args).current_dir(cwd).status().unwrap();
        assert!(status.success(), "git {args:?} failed in {cwd:?}");
    }

    // NATIVE test proving the real git subprocess path (see module doc: not conform-db-exercised).
    #[test]
    fn card_done_git_commit_referencing_slug_suppresses_finding() {
        let dir = temp_dir("has-commit");
        git(&["init", "-q"], &dir);
        git(&["config", "user.email", "test@example.com"], &dir);
        git(&["config", "user.name", "Test"], &dir);
        fs::write(dir.join("f.txt"), "x").unwrap();
        git(&["add", "."], &dir);
        git(&["commit", "-q", "-m", "ship CARD-HASCOMMIT: done"], &dir);

        let c = db();
        c.execute("INSERT INTO cards (slug, status, updated_at) VALUES ('CARD-HASCOMMIT', 'done', '2026-01-01 00:00:00')", []).unwrap();
        let r = with_substrate_root(dir.to_str().unwrap(), || detectCardDoneWithoutGitCommit(&c));
        assert_eq!(r, json!([]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn card_done_no_matching_commit_produces_warn_finding() {
        let dir = temp_dir("no-commit");
        git(&["init", "-q"], &dir);
        git(&["config", "user.email", "test@example.com"], &dir);
        git(&["config", "user.name", "Test"], &dir);
        fs::write(dir.join("f.txt"), "x").unwrap();
        git(&["add", "."], &dir);
        git(&["commit", "-q", "-m", "unrelated change"], &dir);

        let c = db();
        c.execute("INSERT INTO cards (slug, status, updated_at) VALUES ('CARD-NOCOMMIT', 'done', '2026-01-01 00:00:00')", []).unwrap();
        let r = with_substrate_root(dir.to_str().unwrap(), || detectCardDoneWithoutGitCommit(&c));
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["kind"], json!("card_done_no_git_commit"));
        assert_eq!(arr[0]["severity"], json!("warn"));
        assert_eq!(arr[0]["card_slug"], json!("CARD-NOCOMMIT"));
        assert_eq!(arr[0]["detail"]["searched_commits_for"], json!("CARD-NOCOMMIT"));
        let _ = fs::remove_dir_all(&dir);
    }

    // ── detectContractWithoutAnyArtifacts ────────────────────────────────

    #[test]
    fn contract_no_artifacts_anywhere_is_error_finding() {
        let c = db();
        c.execute(
            "INSERT INTO contracts (slug, status, files_touched_json, sandbox_path, superseded_by) VALUES ('C-1', 'impl', '[\"nope/nope.md\"]', NULL, NULL)",
            [],
        ).unwrap();
        let r = detectContractWithoutAnyArtifacts(&c);
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["kind"], json!("contract_active_no_artifacts"));
        assert_eq!(arr[0]["severity"], json!("error"));
        assert_eq!(arr[0]["contract_slug"], json!("C-1"));
        assert!(arr[0]["message"].as_str().unwrap().contains("NONE of its 1 declared files_touched"));
    }

    #[test]
    fn contract_file_exists_in_working_tree_no_finding() {
        // Hermetic: the declared file lives in a temp substrate root (the repo root is the parent of the
        // crate, whose contents differ per checkout layout — CI has no CLAUDE.md there).
        let dir = temp_dir("contract-working-tree");
        fs::write(dir.join("CLAUDE.md"), "x").unwrap();
        let c = db();
        c.execute(
            "INSERT INTO contracts (slug, status, files_touched_json, sandbox_path, superseded_by) VALUES ('C-2', 'impl', '[\"CLAUDE.md\"]', NULL, NULL)",
            [],
        ).unwrap();
        let r = with_substrate_root(&dir.to_string_lossy(), || detectContractWithoutAnyArtifacts(&c));
        assert_eq!(r, json!([]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn contract_file_exists_only_in_sandbox_no_finding() {
        // Same hermetic shape: the file exists only under <root>/<sandbox_path>/, not at <root>/.
        let dir = temp_dir("contract-sandbox");
        fs::create_dir_all(dir.join("antiloki-core-rust")).unwrap();
        fs::write(dir.join("antiloki-core-rust").join("Cargo.toml"), "x").unwrap();
        let c = db();
        c.execute(
            "INSERT INTO contracts (slug, status, files_touched_json, sandbox_path, superseded_by) VALUES ('C-3', 'tl_approved', '[\"Cargo.toml\"]', 'antiloki-core-rust', NULL)",
            [],
        ).unwrap();
        let r = with_substrate_root(&dir.to_string_lossy(), || detectContractWithoutAnyArtifacts(&c));
        assert_eq!(r, json!([]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn contract_terminal_status_and_superseded_are_excluded() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, status, files_touched_json, sandbox_path, superseded_by) VALUES ('C-4', 'done', '[\"nope.md\"]', NULL, NULL)", []).unwrap();
        c.execute("INSERT INTO contracts (slug, status, files_touched_json, sandbox_path, superseded_by) VALUES ('C-5', 'impl', '[\"nope.md\"]', NULL, 'C-99')", []).unwrap();
        assert_eq!(detectContractWithoutAnyArtifacts(&c), json!([]));
    }

    #[test]
    fn contract_empty_files_touched_is_skipped() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, status, files_touched_json, sandbox_path, superseded_by) VALUES ('C-6', 'qa', '[]', NULL, NULL)", []).unwrap();
        assert_eq!(detectContractWithoutAnyArtifacts(&c), json!([]));
    }

    // ── detectAuditDisagreesWithCurrentCard ──────────────────────────────

    #[test]
    fn card_audit_drift_produces_finding() {
        let c = db();
        c.execute("INSERT INTO cards (slug, status, updated_at) VALUES ('CARD-D', 'doing', '2026-01-01 00:00:00')", []).unwrap();
        c.execute("INSERT INTO sacred_timeline (entity_type, entity_id, after_json) VALUES ('card', 'CARD-D', '{\"status\":\"backlog\"}')", []).unwrap();
        let r = detectAuditDisagreesWithCurrentCard(&c);
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["kind"], json!("audit_disagrees_with_current_card"));
        assert_eq!(arr[0]["detail"], json!({ "card_slug": "CARD-D", "current_status": "doing", "audit_status": "backlog" }));
    }

    #[test]
    fn card_audit_matches_latest_row_no_finding() {
        let c = db();
        c.execute("INSERT INTO cards (slug, status, updated_at) VALUES ('CARD-E', 'doing', '2026-01-01 00:00:00')", []).unwrap();
        c.execute("INSERT INTO sacred_timeline (entity_type, entity_id, after_json) VALUES ('card', 'CARD-E', '{\"status\":\"backlog\"}')", []).unwrap();
        c.execute("INSERT INTO sacred_timeline (entity_type, entity_id, after_json) VALUES ('card', 'CARD-E', '{\"status\":\"doing\"}')", []).unwrap();
        assert_eq!(detectAuditDisagreesWithCurrentCard(&c), json!([]));
    }

    #[test]
    fn card_archived_excluded_even_with_drift() {
        let c = db();
        c.execute("INSERT INTO cards (slug, status, updated_at) VALUES ('CARD-F', 'archived', '2026-01-01 00:00:00')", []).unwrap();
        c.execute("INSERT INTO sacred_timeline (entity_type, entity_id, after_json) VALUES ('card', 'CARD-F', '{\"status\":\"backlog\"}')", []).unwrap();
        assert_eq!(detectAuditDisagreesWithCurrentCard(&c), json!([]));
    }

    #[test]
    fn card_no_audit_row_no_finding() {
        let c = db();
        c.execute("INSERT INTO cards (slug, status, updated_at) VALUES ('CARD-G', 'backlog', '2026-01-01 00:00:00')", []).unwrap();
        assert_eq!(detectAuditDisagreesWithCurrentCard(&c), json!([]));
    }

    #[test]
    fn card_malformed_after_json_skipped() {
        let c = db();
        c.execute("INSERT INTO cards (slug, status, updated_at) VALUES ('CARD-H', 'doing', '2026-01-01 00:00:00')", []).unwrap();
        c.execute("INSERT INTO sacred_timeline (entity_type, entity_id, after_json) VALUES ('card', 'CARD-H', 'not json')", []).unwrap();
        assert_eq!(detectAuditDisagreesWithCurrentCard(&c), json!([]));
    }

    #[test]
    fn card_after_status_non_string_skipped() {
        let c = db();
        c.execute("INSERT INTO cards (slug, status, updated_at) VALUES ('CARD-I', 'doing', '2026-01-01 00:00:00')", []).unwrap();
        c.execute("INSERT INTO sacred_timeline (entity_type, entity_id, after_json) VALUES ('card', 'CARD-I', '{\"status\":5}')", []).unwrap();
        assert_eq!(detectAuditDisagreesWithCurrentCard(&c), json!([]));
    }

    // ── detectAuditDisagreesWithCurrentContract ──────────────────────────

    #[test]
    fn contract_audit_drift_produces_finding() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, status, files_touched_json, sandbox_path, superseded_by) VALUES ('CT-D', 'done', NULL, NULL, NULL)", []).unwrap();
        c.execute("INSERT INTO sacred_timeline (entity_type, entity_id, after_json) VALUES ('contract', 'CT-D', '{\"status\":\"proposed\"}')", []).unwrap();
        let r = detectAuditDisagreesWithCurrentContract(&c);
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["kind"], json!("audit_disagrees_with_current_contract"));
        assert_eq!(arr[0]["severity"], json!("error"));
        assert_eq!(arr[0]["contract_slug"], json!("CT-D"));
    }

    #[test]
    fn contract_audit_matches_no_finding() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, status, files_touched_json, sandbox_path, superseded_by) VALUES ('CT-E', 'proposed', NULL, NULL, NULL)", []).unwrap();
        c.execute("INSERT INTO sacred_timeline (entity_type, entity_id, after_json) VALUES ('contract', 'CT-E', '{\"status\":\"proposed\"}')", []).unwrap();
        assert_eq!(detectAuditDisagreesWithCurrentContract(&c), json!([]));
    }

    // ── detectCardCorrectionWithoutFollowup ─────────────────────────────

    #[test]
    fn correction_after_card_done_is_info_finding() {
        let c = db();
        c.execute("INSERT INTO cards (slug, status, updated_at) VALUES ('FIX-1', 'done', '2025-12-01 00:00:00')", []).unwrap();
        c.execute("INSERT INTO memory.memories (type, tags_json, created_at) VALUES ('correction', '[\"FIX-1\",\"other\"]', '2026-01-15 00:00:00')", []).unwrap();
        let r = detectCardCorrectionWithoutFollowup(&c);
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["kind"], json!("card_correction_without_followup"));
        assert_eq!(arr[0]["severity"], json!("info"));
        assert_eq!(arr[0]["card_slug"], json!("FIX-1"));
        assert_eq!(arr[0]["detail"]["memory_id"].as_i64().unwrap(), 1);
    }

    #[test]
    fn correction_card_not_done_no_finding() {
        let c = db();
        c.execute("INSERT INTO cards (slug, status, updated_at) VALUES ('FIX-2', 'doing', '2025-12-01 00:00:00')", []).unwrap();
        c.execute("INSERT INTO memory.memories (type, tags_json, created_at) VALUES ('correction', '[\"FIX-2\"]', '2026-01-15 00:00:00')", []).unwrap();
        assert_eq!(detectCardCorrectionWithoutFollowup(&c), json!([]));
    }

    #[test]
    fn correction_before_card_closed_no_finding() {
        let c = db();
        c.execute("INSERT INTO cards (slug, status, updated_at) VALUES ('FIX-3', 'done', '2026-01-20 00:00:00')", []).unwrap();
        c.execute("INSERT INTO memory.memories (type, tags_json, created_at) VALUES ('correction', '[\"FIX-3\"]', '2026-01-15 00:00:00')", []).unwrap();
        assert_eq!(detectCardCorrectionWithoutFollowup(&c), json!([]));
    }

    #[test]
    fn correction_tags_without_card_prefix_no_finding() {
        let c = db();
        c.execute("INSERT INTO memory.memories (type, tags_json, created_at) VALUES ('correction', '[\"random-tag\"]', '2026-01-15 00:00:00')", []).unwrap();
        assert_eq!(detectCardCorrectionWithoutFollowup(&c), json!([]));
    }

    #[test]
    fn correction_unknown_card_slug_no_finding() {
        let c = db();
        c.execute("INSERT INTO memory.memories (type, tags_json, created_at) VALUES ('correction', '[\"FIX-GHOST\"]', '2026-01-15 00:00:00')", []).unwrap();
        assert_eq!(detectCardCorrectionWithoutFollowup(&c), json!([]));
    }

    // ── reconcileTruthFusion aggregator ──────────────────────────────────

    #[test]
    fn aggregator_empty_substrate_zero_findings() {
        let c = db();
        let r = reconcileTruthFusion(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "ran_at": FROZEN_ISO, "findings": [], "summary": { "total": 0, "by_severity": {} } }));
    }

    #[test]
    fn aggregator_unions_detectors_and_tallies_by_severity() {
        let c = db();
        // detector 2: no artifacts
        c.execute("INSERT INTO contracts (slug, status, files_touched_json, sandbox_path, superseded_by) VALUES ('C-AGG', 'impl', '[\"nope.md\"]', NULL, NULL)", []).unwrap();
        // detector 3: card drift (status='doing' — NOT 'done', so detector 1 never invokes git)
        c.execute("INSERT INTO cards (slug, status, updated_at) VALUES ('CARD-AGG', 'doing', '2026-01-01 00:00:00')", []).unwrap();
        c.execute("INSERT INTO sacred_timeline (entity_type, entity_id, after_json) VALUES ('card', 'CARD-AGG', '{\"status\":\"backlog\"}')", []).unwrap();
        // detector 4: contract drift, distinct slug, status not in detector 2's ACTIVE set
        c.execute("INSERT INTO contracts (slug, status, files_touched_json, sandbox_path, superseded_by) VALUES ('CT-AGG', 'proposed', NULL, NULL, NULL)", []).unwrap();
        c.execute("INSERT INTO sacred_timeline (entity_type, entity_id, after_json) VALUES ('contract', 'CT-AGG', '{\"status\":\"backlog\"}')", []).unwrap();
        // detector 5: correction present but card not done -> contributes no finding (proves wiring)
        c.execute("INSERT INTO cards (slug, status, updated_at) VALUES ('EPIC-NOFOLLOWUP', 'backlog', '2026-01-01 00:00:00')", []).unwrap();
        c.execute("INSERT INTO memory.memories (type, tags_json, created_at) VALUES ('correction', '[\"EPIC-NOFOLLOWUP\"]', '2026-01-15 00:00:00')", []).unwrap();

        let r = reconcileTruthFusion(&json!({}), &c).unwrap();
        assert_eq!(r["ran_at"], json!(FROZEN_ISO));
        let findings = r["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 3);
        assert_eq!(findings[0]["kind"], json!("contract_active_no_artifacts"));
        assert_eq!(findings[1]["kind"], json!("audit_disagrees_with_current_card"));
        assert_eq!(findings[2]["kind"], json!("audit_disagrees_with_current_contract"));
        assert_eq!(r["summary"], json!({ "total": 3, "by_severity": { "error": 3 } }));
    }
}
