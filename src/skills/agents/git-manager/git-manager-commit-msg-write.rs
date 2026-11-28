//! git-manager-commit-msg-write.rs — twin of
//! skills/agents/git-manager/git-manager-commit-msg-write.mjs (db READ + real `git` subprocess +
//! LLM-PARTIAL). NAMES ARE CONTRACT: `gitManagerCommitMsgWrite` (default).
//!
//! DETERMINISTIC surface, fully ported: the `card_slug` guard, the card lookup (NOT_FOUND on
//! miss), the real `git diff --cached --stat` subprocess (same `std::process::Command` shape as
//! git-manager-stage-validate.rs's `git_diff_cached_names`, disclosed UNTESTED by conform-db for
//! the same git-state-not-fixturable reason — stdout is captured either way, `''` folds to the JS
//! `'(no staged changes)'` fallback), the slug-prefix `inferredKind` derivation, `ensurePrompt`'s
//! idempotent registration via the already-converted `ai_prompt_find::promptFind` /
//! `ai_prompt_add::promptAdd` twins, and the response-shaping (`shape_commit_msg`, ported as its
//! own pure fn per the playbook so it can be fed a canned `aiResult` value directly in tests).
//!
//! PARITY-DEFERRED: the `aiCall` leg calls `crate::ai_ai_call::aiCall`, itself an always-`Err`
//! deferred stub (see its own module doc — a real network round-trip is not fixturable). This
//! twin's `Err(_)` arm therefore always fires in practice, which is exactly the JS best-effort
//! shape: `shape_commit_msg` is written to accept EITHER a real `{ output_kv, ai_call_id }`
//! response OR an empty `{}` fallback object and produce the identical subject/body/full — so the
//! deferred boundary changes nothing about the returned shape, only `ai_call_id` (`null` when
//! deferred vs. the real row id on a live call).
//!
//! `context.db` missing → JS throws `err.contextMissing('git-manager-commit-msg-write', 'db')`.
//! Unreachable here: `db: &Connection` is a required, non-nullable reference (same convention as
//! every sibling db-lane twin).
//!
//! DISCLOSED narrowing: JS's `kind ?? (...)` only skips inference when `input.kind` is nullish —
//! an explicit non-string, non-null `kind` (a number, object, etc.) would be forwarded verbatim by
//! JS and only stringified at template-interpolation time. Real callers only ever pass a string or
//! omit the field (same "callers pass real column-shaped values" convention as the sibling
//! git-manager twins), so this twin treats "present, non-null, and a JSON string" as the only
//! override case; anything else (absent, null, or a non-string) falls through to inference.
// QUALITY: mixed unit — guard/lookup/subprocess/prompt-registration/response-shaping fully ported
// and native-tested; the AI dispatch itself is the same class of deferred boundary as ai-call.rs's
// own `aiCall`.

use rusqlite::Connection;
use serde_json::{json, Value};

const PROMPT_NAME: &str = "git-manager.commit.msg.v1";
const PROMPT_TEMPLATE_MD: &str = "You are writing a conventional-commit message for a single card-scoped commit.\n\nCARD slug: {{card_slug}}\nCARD title: {{card_title}}\nCARD kind: {{kind}}\n\nSTAGED DIFF STATS (file paths + lines added/removed):\n{{diff_stat}}\n\nRules:\n  - First line: <kind>(<scope>): <subject>  — under 72 chars, imperative mood.\n    kind ∈ {feat, fix, refactor, chore, docs, test}. scope = lowercase card slug.\n  - Blank line.\n  - Body: 2-4 short lines explaining WHY (the motivation), not WHAT (the diff already shows that).\n  - No trailing fluff. No \"this commit...\" preamble. Sharp + factual.\n\nOutput JSON of this exact shape:\n{ \"subject\": \"<kind>(<scope>): <subject line>\", \"body\": \"<2-4 line WHY>\" }";

fn prompt_output_fields() -> Value {
    json!([
        { "field_name": "subject", "field_type": "string", "required": true },
        { "field_name": "body",    "field_type": "string", "required": true },
    ])
}

// = JS `.length` (UTF-16 code units) / `.slice(0,n)` — chars-based approximation, exact for the
// ASCII content this unit ever renders (same convention as driver-inspect.rs's js_len/js_slice).
fn js_slice(s: &str, n: usize) -> String { s.chars().take(n).collect() }

/// `function ensurePrompt(db)` — JS `try { return promptFind(...) } catch { promptAdd(...); return
/// promptFind(...) }`. The return value is discarded by the caller in both JS and here; only the
/// registration side effect matters.
fn ensure_prompt(db: &Connection) {
    if crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db).is_ok() { return; }
    let _ = crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME, "template_md": PROMPT_TEMPLATE_MD,
            "default_tier": "low", "default_temperature": "cold",
            "max_output_tokens": 400, "output_fields": prompt_output_fields(),
        }),
        db,
    );
}

/// `function git(['diff','--cached','--stat'], cwd)` — real subprocess, disclosed UNTESTED by the
/// conform-db fixture (git-state dependent; see module doc). Mirrors JS `spawnSync`: only stdout
/// is consulted, exit status/stderr are never checked.
fn git_diff_cached_stat(cwd: &str) -> String {
    match crate::proc::cmd("git").arg("diff").arg("--cached").arg("--stat").current_dir(cwd).output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(_) => String::new(),
    }
}

/// `const inferredKind = kind ?? (card_slug.startsWith('BUG-') ? 'fix' : ...)` — see module doc
/// for the disclosed `kind` narrowing.
fn infer_kind(card_slug: &str) -> String {
    if card_slug.starts_with("BUG-") { "fix" }
    else if card_slug.starts_with("REFACTOR-") { "refactor" }
    else if card_slug.starts_with("DOC-") { "docs" }
    else if card_slug.starts_with("TEST-") { "test" }
    else if card_slug.starts_with("CHORE-") { "chore" }
    else { "feat" }
    .to_string()
}

/// The response-shaping tail of the JS function, ported as its own pure fn: `for (const row of
/// aiResult.output_kv ?? []) if (row.value_text != null) out[row.field_name] = row.value_text;`
/// then the `subject`/`body`/`full` derivation. Accepts either a real aiCall response or `{}` (the
/// deferred-boundary fallback) — both produce the identical default-subject shape when no
/// `output_kv` rows are present.
fn shape_commit_msg(ai_result: &Value, inferred_kind: &str, card_slug: &str, card_title: &str) -> (String, String, String) {
    let mut subject_out: Option<String> = None;
    let mut body_out: Option<String> = None;
    if let Some(rows) = ai_result.get("output_kv").and_then(Value::as_array) {
        for row in rows {
            let vt = row.get("value_text");
            if vt.map_or(true, Value::is_null) { continue; }
            let text = vt.and_then(Value::as_str).unwrap_or_default().to_string();
            match row.get("field_name").and_then(Value::as_str) {
                Some("subject") => subject_out = Some(text),
                Some("body") => body_out = Some(text),
                _ => {}
            }
        }
    }
    let default_subject = format!("{inferred_kind}({}): work on {card_title}", card_slug.to_lowercase());
    let subject = js_slice(&subject_out.unwrap_or(default_subject), 72);
    let body = body_out.unwrap_or_default().trim().to_string();
    let full = if body.is_empty() { subject.clone() } else { format!("{subject}\n\n{body}") };
    (subject, body, full)
}

#[allow(non_snake_case)]
pub fn gitManagerCommitMsgWrite(input: &Value, db: &Connection) -> Result<Value, String> {
    let card_slug = match input.get("card_slug").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => return Err("git-manager-commit-msg-write: card_slug is required".into()),
    };
    // See module doc: only a present, non-null, string `kind` overrides inference.
    let kind_override = input.get("kind").filter(|v| !v.is_null()).and_then(Value::as_str).map(|s| s.to_string());

    let card_rows = crate::db_json::query_rows(db, "SELECT slug, title FROM cards WHERE slug = ?", &[json!(card_slug.clone())])?;
    let card = match card_rows.as_array().and_then(|a| a.first()) {
        Some(c) => c.clone(),
        None => return Err(format!("git-manager-commit-msg-write: card '{card_slug}' not found")),
    };
    let card_title = card.get("title").and_then(Value::as_str).unwrap_or_default().to_string();

    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root = root_v.as_str().unwrap_or_default();
    let stat_stdout = git_diff_cached_stat(root);
    let diff_stat = if stat_stdout.is_empty() { "(no staged changes)".to_string() } else { stat_stdout };

    let inferred_kind = kind_override.unwrap_or_else(|| infer_kind(&card_slug));

    ensure_prompt(db);

    let call_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [
            { "key": "card_slug",  "value": card_slug },
            { "key": "card_title", "value": card_title },
            { "key": "kind",       "value": inferred_kind },
            { "key": "diff_stat",  "value": js_slice(&diff_stat, 2000) },
        ],
        "tier": "low", "temperature": "cold",
    });

    let (subject, body, full, ai_call_id) = match crate::ai_ai_call::aiCall(&call_input, db) {
        Ok(res) => {
            let (s, b, f) = shape_commit_msg(&res, &inferred_kind, &card_slug, &card_title);
            (s, b, f, res.get("ai_call_id").cloned().unwrap_or(Value::Null))
        }
        Err(_) => {
            let (s, b, f) = shape_commit_msg(&json!({}), &inferred_kind, &card_slug, &card_title);
            (s, b, f, Value::Null)
        }
    };

    Ok(json!({
        "card_slug": card_slug, "kind": inferred_kind,
        "subject": subject, "body": body, "full": full,
        "ai_call_id": ai_call_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');",
        ).unwrap();
        c
    }

    #[test]
    fn missing_card_slug_errors() {
        let c = db();
        let e = gitManagerCommitMsgWrite(&json!({}), &c).unwrap_err();
        assert_eq!(e, "git-manager-commit-msg-write: card_slug is required");
    }

    #[test]
    fn unknown_card_slug_errors() {
        let c = db();
        let e = gitManagerCommitMsgWrite(&json!({ "card_slug": "GHOST" }), &c).unwrap_err();
        assert_eq!(e, "git-manager-commit-msg-write: card 'GHOST' not found");
    }

    #[test]
    fn deferred_ai_call_still_returns_a_default_subject_and_registers_the_prompt() {
        let c = db();
        c.execute("INSERT INTO cards (slug, title) VALUES ('BUG-FOO', 'Fix the foo bug')", []).unwrap();
        let r = gitManagerCommitMsgWrite(&json!({ "card_slug": "BUG-FOO" }), &c).unwrap();
        assert_eq!(r["kind"], json!("fix")); // BUG- prefix inference
        assert_eq!(r["subject"], json!("fix(bug-foo): work on Fix the foo bug"));
        assert_eq!(r["body"], json!(""));
        assert_eq!(r["full"], r["subject"]);
        assert_eq!(r["ai_call_id"], Value::Null);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = ?1", [PROMPT_NAME], |row| row.get(0)).unwrap();
        assert_eq!(n, 1, "ensurePrompt must register the prompt even though aiCall is deferred");
    }

    #[test]
    fn explicit_kind_overrides_slug_prefix_inference() {
        let c = db();
        c.execute("INSERT INTO cards (slug, title) VALUES ('BUG-FOO', 'Fix the foo bug')", []).unwrap();
        let r = gitManagerCommitMsgWrite(&json!({ "card_slug": "BUG-FOO", "kind": "chore" }), &c).unwrap();
        assert_eq!(r["kind"], json!("chore"));
        assert_eq!(r["subject"], json!("chore(bug-foo): work on Fix the foo bug"));
    }

    #[test]
    fn infer_kind_covers_every_prefix() {
        assert_eq!(infer_kind("BUG-1"), "fix");
        assert_eq!(infer_kind("REFACTOR-1"), "refactor");
        assert_eq!(infer_kind("DOC-1"), "docs");
        assert_eq!(infer_kind("TEST-1"), "test");
        assert_eq!(infer_kind("CHORE-1"), "chore");
        assert_eq!(infer_kind("FEAT-1"), "feat");
    }

    #[test]
    fn shape_commit_msg_prefers_a_real_ai_response_and_trims_body() {
        let res = json!({
            "output_kv": [
                { "field_name": "subject", "value_text": "feat(x): add thing" },
                { "field_name": "body", "value_text": "  Because reasons.\nMore reasons.  " },
                { "field_name": "ignored", "value_text": "n/a" },
            ],
            "ai_call_id": 42,
        });
        let (subject, body, full) = shape_commit_msg(&res, "feat", "x", "X title");
        assert_eq!(subject, "feat(x): add thing");
        assert_eq!(body, "Because reasons.\nMore reasons.");
        assert_eq!(full, "feat(x): add thing\n\nBecause reasons.\nMore reasons.");
    }

    #[test]
    fn shape_commit_msg_ignores_null_value_text_and_caps_subject_at_72() {
        let long_subject = "x".repeat(100);
        let res = json!({ "output_kv": [
            { "field_name": "subject", "value_text": long_subject.clone() },
            { "field_name": "body", "value_text": Value::Null },
        ]});
        let (subject, body, full) = shape_commit_msg(&res, "feat", "x", "X title");
        assert_eq!(subject.len(), 72);
        assert_eq!(subject, js_slice(&long_subject, 72));
        assert_eq!(body, "");
        assert_eq!(full, subject);
    }

    #[test]
    fn shape_commit_msg_empty_object_falls_back_to_the_default_subject() {
        let (subject, body, full) = shape_commit_msg(&json!({}), "feat", "card-x", "Some Title");
        assert_eq!(subject, "feat(card-x): work on Some Title");
        assert_eq!(body, "");
        assert_eq!(full, subject);
    }
}
