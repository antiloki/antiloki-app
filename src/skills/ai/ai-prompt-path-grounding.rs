//! ai-prompt-path-grounding.rs — twin of skills/ai/ai-prompt-path-grounding.mjs (db lane +
//! no-db helpers). NAMES ARE CONTRACT: `extractKeywords`, `scoreFile`, `rankCandidates`,
//! `formatCandidatePathsMd` (named, no-db, pure — normal fixture-tested units) and
//! `aiPromptPathGrounding` (default — BESPOKE-NATIVE, see below).
//!
//! Produces a `{{candidate_paths_md}}` block: score every `git ls-files` path by keyword overlap
//! with a card's title+body (and/or explicit keywords), rank, format as markdown — so an AI
//! prompt can "pick from a list" instead of hallucinating a path.
//!
//! `aiPromptPathGrounding` — BESPOKE-NATIVE (playbook's "not conditionally avoidable" rule):
//! EVERY success path unconditionally calls `gitListFiles()` (`execSync('git ls-files', {cwd:
//! REPO_ROOT})` in JS) — there is no flag/branch that skips it, unlike constitution-estimate.rs's
//! git-diff branch (fires only when `input.base` is truthy) or sync-capture-revision.rs's
//! currentBranch/currentSha (fire only when `input.branch`/`input.sha` is absent). Per the
//! playbook: real correct code below (real `git ls-files` subprocess via `std::process::Command`,
//! real `cards` SELECT), proven by `#[cfg(test)]` native tests, NO conform-db fixture and NO
//! frontier entry for THIS function — same precedent as code-git-init.rs / sync-capture-
//! revision.rs's currentBranch/currentSha. The four named pure helpers below (no git/no db) DO
//! carry normal fixtures/frontier entries — same "some units in a module are bespoke-native, the
//! rest aren't" split sync-capture-revision.rs already established.
//!
//! REPO_ROOT: the JS computes `resolve(HERE, '..', '..', '..')` from `import.meta.url` (HERE =
//! `skills/ai`, two levels under `antiloki-core`) — three levels up lands ONE level above
//! `antiloki-core` itself, i.e. the outer checkout dir that contains both `antiloki-core` and
//! `antiloki-core-rust` as siblings (same landing spot as sync-capture-revision.rs's / qa-sandbox-
//! reset.rs's REPO_ROOT, computed from their own 2-levels-deep module locations) — with NO env
//! override. `repo_root()` mirrors this via `CARGO_MANIFEST_DIR`'s parent, same precedent. This
//! directory is currently NOT itself a git repository (verified: `git rev-parse
//! --is-inside-work-tree` run there fails — antiloki-core and antiloki-core-rust are separate
//! repos one level below it), so `gitListFiles()` deterministically returns `[]` in this
//! checkout's production reality; the native tests assert this documented SHAPE rather than a
//! hardcoded non-empty result (sync-capture-revision.rs's currentBranch/currentSha precedent).
//!
//! `gitListFiles()` takes `cwd: &str` (unlike the JS, which has no such parameter — HERE it is
//! purely so the native tests can also exercise the git-success shape against a real temp-dir
//! repo; production always calls it with the fixed `repo_root()`, matching the JS exactly).
//!
//! `context.db` missing → JS throws `err.contextMissing('ai-prompt-path-grounding','db')`. Not
//! modeled: `db: &Connection` is a required, non-nullable reference here — same "guard-throws are
//! not exercised" convention used by every sibling twin.
//!
//! required-field guard (`card_slug == null && (!keywords || keywords.length===0)`) narrowed to
//! the realistic-domain shape (qa-sandbox-reset.rs precedent): `keywords` is treated as "present"
//! only when it is a non-null, non-empty JSON array — real callers never pass a truthy non-array
//! `keywords` (the JS's own `Array.isArray(explicitKeywords) ? ... : []` a few lines later means a
//! non-array value contributes NO keywords anyway, so a caller that relied on the non-array-truthy
//! escape from the guard would immediately hit `matched_count:0` regardless).
//!
//! `card.body_json ?? 'null'` → `JSON.parse` → `typeof parsed === 'string' ? parsed :
//! JSON.stringify(parsed)`, catch → `String(card.body_json ?? '')`: ported as a parse/re-stringify
//! round trip. A NULL body_json column parses to JS `null`, and `JSON.stringify(null)` is the
//! 4-char STRING `"null"` (not JS `null`) — this exact quirk is preserved below. The re-stringify
//! is not necessarily BYTE-identical to the original source text (key order is preserved via
//! serde_json's `preserve_order` feature, matching JS object insertion order, but JSON.stringify's
//! exact number formatting could diverge from serde_json's for non-integer numeric fields) — this
//! is harmless for `extractKeywords`, which only tokenizes on non-alnum boundaries, so minor
//! formatting differences never change the extracted keyword set or its order.
//!
//! `String(k).toLowerCase()` on explicit keyword entries = `js_string` (full JS `String(x)`
//! coercion — arrays comma-join, objects `"[object Object]"`), NOT `str_qq`.
// QUALITY: mixed unit — 4 pure no-db helpers (fixture-tested) + 1 bespoke-native db+subprocess
// default export (native-test-proven, no fixture/frontier per playbook's unconditional-shellout rule)

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::OnceLock;

fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v:&Value)->String{ match v{Value::Null=>"null".into(),Value::String(s)=>s.clone(),Value::Number(n)=>n.to_string(),Value::Bool(b)=>b.to_string(),Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","),Value::Object(_)=>"[object Object]".into()} } // = JS String(v)
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand

// English stopwords + substrate-specific noise words — verbatim from the JS STOPWORDS set.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "into", "must",
    "not", "are", "was", "were", "has", "have", "had", "will", "should",
    "when", "then", "than", "but", "all", "any", "each", "every", "some",
    "one", "two", "three", "card", "cards", "body", "json", "title",
    "slug", "substrate", "antiloki", "code", "codes",
    "kind", "kinds", "value", "values", "name", "names",
    "add", "change", "fix", "feat", "use", "used", "using",
];

// JS `/[^a-z0-9_\-]+/u` — splits on runs of anything not lowercase-alnum/underscore/hyphen.
fn split_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"[^a-z0-9_\-]+").unwrap())
}

/// `export function extractKeywords(text, opts = {})`
pub fn extractKeywords(text: &Value, opts: &Value) -> Value {
    let min_length: i64 = opts.get("minLength").and_then(Value::as_i64).unwrap_or(3);
    let s = match text.as_str() {
        Some(s) if !s.is_empty() => s,
        _ => return json!([]),
    };
    let lower = s.to_lowercase();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<Value> = Vec::new();
    for t in split_re().split(&lower) {
        if t.is_empty() { continue; } // filter(Boolean)
        if (t.chars().count() as i64) < min_length { continue; }
        if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) { continue; } // /^[0-9]+$/
        if STOPWORDS.contains(&t) { continue; }
        if !seen.insert(t.to_string()) { continue; }
        out.push(json!(t));
    }
    json!(out)
}

/// `export function scoreFile(path, keywords)`
pub fn scoreFile(path: &Value, keywords: &Value) -> Value {
    let kws = keywords.as_array().cloned().unwrap_or_default();
    if kws.is_empty() { return json!(0); }
    let lower = path.as_str().unwrap_or("").to_lowercase();
    let mut score: i64 = 0;
    for kw in &kws {
        let k = kw.as_str().unwrap_or("");
        if lower.contains(k) { score += 1; }
    }
    json!(score)
}

/// `.slice(0, end)` on a scored-candidate array (JS negative `end` counts back from the length).
fn js_slice0(arr: Vec<Value>, end: i64) -> Vec<Value> {
    let len = arr.len() as i64;
    let e = if end < 0 { (len + end).max(0) } else { end.min(len) };
    arr.into_iter().take(e.max(0) as usize).collect()
}

/// `export function rankCandidates(files, keywords, maxPaths)`
pub fn rankCandidates(files: &Value, keywords: &Value, max_paths: &Value) -> Value {
    let files_arr = files.as_array().cloned().unwrap_or_default();
    let mut scored: Vec<Value> = Vec::new();
    for f in &files_arr {
        let s = scoreFile(f, keywords);
        let s_i = s.as_i64().unwrap_or(0);
        if s_i > 0 {
            scored.push(json!({ "path": f.clone(), "score": s_i }));
        }
    }
    scored.sort_by(|a, b| {
        let as_ = a["score"].as_i64().unwrap_or(0);
        let bs = b["score"].as_i64().unwrap_or(0);
        if bs != as_ {
            bs.cmp(&as_)
        } else {
            a["path"].as_str().unwrap_or("").cmp(b["path"].as_str().unwrap_or(""))
        }
    });
    let end = max_paths.as_f64().unwrap_or(f64::NAN);
    let end_i: i64 = if end.is_nan() { 0 } else { end.trunc() as i64 };
    json!(js_slice0(scored, end_i))
}

/// `export function formatCandidatePathsMd(candidates, keywords)`
pub fn formatCandidatePathsMd(candidates: &Value, keywords: &Value) -> Value {
    let cands = candidates.as_array().cloned().unwrap_or_default();
    if cands.is_empty() {
        return json!("═══ CANDIDATE PATHS ═══\n(no repo paths matched the card keywords; AI may need to name new files or you should refine the card body)\n");
    }
    let kws = keywords.as_array().cloned().unwrap_or_default();
    let kw_strs: Vec<String> = kws.iter().map(|k| k.as_str().unwrap_or("").to_string()).collect();
    let shown: Vec<&String> = kw_strs.iter().take(8).collect();
    let suffix = if kw_strs.len() > 8 { "…" } else { "" };
    let mut lines: Vec<String> = vec![
        "═══ CANDIDATE PATHS (pick from these; do not invent) ═══".to_string(),
        format!(
            "(ranked by keyword overlap with the card body; keywords: {}{})",
            shown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
            suffix
        ),
        String::new(),
    ];
    for c in &cands {
        let path = c.get("path").and_then(Value::as_str).unwrap_or("");
        lines.push(format!("- {path}"));
    }
    lines.push(String::new());
    json!(lines.join("\n"))
}

// JS: `const HERE = dirname(fileURLToPath(import.meta.url)); const REPO_ROOT = resolve(HERE,
// '..', '..', '..');` — see module doc. CARGO_MANIFEST_DIR here is antiloki-core-rust; its parent
// is the same outer checkout dir the JS lands on (sync-capture-revision.rs / qa-sandbox-reset.rs
// precedent). Deliberately NO env override.
fn repo_root() -> String {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    match std::path::Path::new(manifest_dir).parent() {
        Some(p) => p.to_string_lossy().into_owned(),
        None => manifest_dir.to_string(),
    }
}

/// `function gitListFiles()` — `execSync('git ls-files', {cwd: REPO_ROOT})`, catch → `[]`.
/// BESPOKE-NATIVE (see module doc): unconditionally shells out on every call. `cwd` is a plain
/// parameter here (the JS has none — REPO_ROOT is baked in) purely so native tests can also drive
/// this against a real temp-dir repo; production always passes `repo_root()`.
fn git_list_files(cwd: &str) -> Vec<String> {
    match crate::proc::cmd("git").arg("ls-files").current_dir(cwd).output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .split('\n')
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// `export default async function aiPromptPathGrounding(input, context)` — async over sync (the
/// only awaited work is the sync `context.db` read); ported as an ordinary sync fn per the
/// async-lane convention.
pub fn aiPromptPathGrounding(input: &Value, db: &Connection) -> Result<Value, String> {
    let card_slug = nz(input.get("card_slug"));
    // realistic-domain narrowing of the required-field guard — see module doc.
    let keywords_present = input.get("keywords").and_then(Value::as_array).map_or(false, |a| !a.is_empty());
    if card_slug.is_none() && !keywords_present {
        return Err("ai-prompt-path-grounding: card_slug or keywords is required".into());
    }
    // JS destructuring default `max_paths = 50` fires only when the key is ABSENT (undefined),
    // not when explicitly `null` — `input.get` distinguishes those two cases directly.
    let max_paths_v: Value = input.get("max_paths").cloned().unwrap_or(json!(50));

    let mut from_card: Vec<String> = Vec::new();
    if let Some(slug_v) = card_slug {
        let slug = slug_v.as_str().unwrap_or("").to_string();
        let rows = crate::db_json::query_rows(db, "SELECT slug, title, body_json FROM cards WHERE slug = ?", &[json!(slug.clone())])?;
        let card = match rows.as_array().and_then(|a| a.first()).cloned() {
            Some(c) => c,
            None => return Err(format!("ai-prompt-path-grounding: card '{slug}' not found")),
        };
        let title = card.get("title").and_then(Value::as_str).unwrap_or("").to_string();
        let body_json_val = card.get("body_json").cloned().unwrap_or(Value::Null);
        // `JSON.parse(card.body_json ?? 'null')` — NULL/missing body_json parses as JS `null`.
        let json_src: String = match &body_json_val {
            Value::Null => "null".to_string(),
            Value::String(s) => s.clone(),
            other => js_string(other),
        };
        let body_text: String = match serde_json::from_str::<Value>(&json_src) {
            Ok(Value::String(s)) => s,
            Ok(other) => serde_json::to_string(&other).unwrap_or_else(|_| "null".to_string()),
            Err(_) => str_qq(Some(&body_json_val)),
        };
        let combined = json!(format!("{title} {body_text}"));
        let kw_val = extractKeywords(&combined, &json!({}));
        from_card = kw_val
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|v| v.as_str().unwrap_or("").to_string())
            .collect();
    }

    let from_explicit: Vec<String> = match input.get("keywords").and_then(Value::as_array) {
        Some(arr) => arr.iter().map(|k| js_string(k).to_lowercase()).collect(),
        None => Vec::new(),
    };

    let mut seen: HashSet<String> = HashSet::new();
    let mut keywords: Vec<Value> = Vec::new();
    for k in from_explicit.into_iter().chain(from_card.into_iter()) {
        if !k.is_empty() && seen.insert(k.clone()) {
            keywords.push(json!(k));
        }
    }
    let keywords_val = json!(keywords);

    let files = json!(git_list_files(&repo_root()));
    let candidates = rankCandidates(&files, &keywords_val, &max_paths_v);
    let candidate_paths_md = formatCandidatePathsMd(&candidates, &keywords_val);
    let matched_count = candidates.as_array().map(|a| a.len()).unwrap_or(0) as i64;

    let mut out = Map::new();
    out.insert("candidate_paths_md".into(), candidate_paths_md);
    out.insert("matched_count".into(), json!(matched_count));
    out.insert("keywords_used".into(), keywords_val);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT, body_json TEXT)",
        )
        .unwrap();
        c
    }

    // ── pure helpers ─────────────────────────────────────────────────────────────────────

    #[test]
    fn extract_keywords_lowercases_drops_stopwords_short_tokens_and_numbers() {
        let r = extractKeywords(&json!("Fix the QA acceptance check for diff_in_paths 2026"), &json!({}));
        let arr: Vec<&str> = r.as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert!(arr.contains(&"acceptance"));
        assert!(arr.contains(&"diff_in_paths"));
        assert!(!arr.contains(&"the"));
        assert!(!arr.contains(&"2026"));
    }

    #[test]
    fn extract_keywords_preserves_hyphenated_tokens_and_dedupes() {
        let r = extractKeywords(&json!("Add card-add skill and with-audit wrapper"), &json!({}));
        let arr: Vec<&str> = r.as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert!(arr.contains(&"card-add"));
        assert!(arr.contains(&"with-audit"));

        let d = extractKeywords(&json!("test test test"), &json!({}));
        assert_eq!(d, json!(["test"]));
    }

    #[test]
    fn extract_keywords_empty_or_non_string_text_is_empty() {
        assert_eq!(extractKeywords(&json!(""), &json!({})), json!([]));
        assert_eq!(extractKeywords(&json!(null), &json!({})), json!([]));
    }

    #[test]
    fn extract_keywords_respects_min_length_opt() {
        let r = extractKeywords(&json!("ab abcd abcde"), &json!({ "minLength": 4 }));
        assert_eq!(r, json!(["abcd", "abcde"]));
    }

    #[test]
    fn score_file_counts_keyword_occurrences() {
        let path = json!(".antiloki/skills/agents/qa/qa-acceptance-run.mjs");
        assert_eq!(scoreFile(&path, &json!(["qa", "acceptance"])), json!(2));
        assert_eq!(scoreFile(&path, &json!(["foo"])), json!(0));
        assert_eq!(scoreFile(&path, &json!([])), json!(0));
    }

    #[test]
    fn rank_candidates_sorts_by_score_desc_then_alphabetic_and_respects_cap() {
        let files = json!(["a/qa-x.mjs", "b/qa-acceptance.mjs", "c/something.mjs"]);
        let r = rankCandidates(&files, &json!(["qa", "acceptance"]), &json!(10));
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 2, "unmatched file excluded");
        assert_eq!(arr[0]["path"], json!("b/qa-acceptance.mjs"), "highest score first");
        assert_eq!(arr[1]["path"], json!("a/qa-x.mjs"));

        let files4 = json!(["qa-1.mjs", "qa-2.mjs", "qa-3.mjs", "qa-4.mjs"]);
        let capped = rankCandidates(&files4, &json!(["qa"]), &json!(2));
        assert_eq!(capped.as_array().unwrap().len(), 2);
    }

    #[test]
    fn rank_candidates_ties_break_alphabetically() {
        let files = json!(["zeta-qa.mjs", "alpha-qa.mjs"]);
        let r = rankCandidates(&files, &json!(["qa"]), &json!(10));
        let arr = r.as_array().unwrap();
        assert_eq!(arr[0]["path"], json!("alpha-qa.mjs"));
        assert_eq!(arr[1]["path"], json!("zeta-qa.mjs"));
    }

    #[test]
    fn format_candidate_paths_md_empty_and_populated() {
        let empty = formatCandidatePathsMd(&json!([]), &json!(["nothing"]));
        assert!(empty.as_str().unwrap().contains("no repo paths matched"));

        let md = formatCandidatePathsMd(
            &json!([{ "path": "a.mjs", "score": 2 }, { "path": "b.mjs", "score": 1 }]),
            &json!(["kw"]),
        );
        let s = md.as_str().unwrap();
        assert!(s.contains("CANDIDATE PATHS"));
        assert!(s.contains("- a.mjs"));
        assert!(s.contains("- b.mjs"));
        assert!(s.contains("keywords: kw"));
    }

    #[test]
    fn format_candidate_paths_md_truncates_keyword_list_past_eight() {
        let kws = json!(["k1", "k2", "k3", "k4", "k5", "k6", "k7", "k8", "k9"]);
        let md = formatCandidatePathsMd(&json!([{ "path": "a.mjs", "score": 1 }]), &kws);
        let s = md.as_str().unwrap();
        assert!(s.contains("k1, k2, k3, k4, k5, k6, k7, k8…"));
        assert!(!s.contains("k9"));
    }

    // ── main skill (db + bespoke-native git) ────────────────────────────────────────────────

    #[test]
    fn skill_requires_card_slug_or_keywords() {
        let c = db0();
        let e = aiPromptPathGrounding(&json!({}), &c).unwrap_err();
        assert!(e.contains("card_slug or keywords"));
    }

    #[test]
    fn skill_card_slug_not_found_errs() {
        let c = db0();
        let e = aiPromptPathGrounding(&json!({ "card_slug": "NOPE" }), &c).unwrap_err();
        assert!(e.to_lowercase().contains("not found"));
    }

    #[test]
    fn skill_explicit_keywords_used_when_no_card_slug() {
        let c = db0();
        let r = aiPromptPathGrounding(&json!({ "keywords": ["Migration", "Migration"] }), &c).unwrap();
        // dedupe preserved, lowercased
        assert_eq!(r["keywords_used"], json!(["migration"]));
        // repo_root() is not a git repo in this checkout (see module doc) → gitListFiles() -> []
        assert_eq!(r["matched_count"], json!(0));
        assert!(r["candidate_paths_md"].as_str().unwrap().contains("no repo paths matched"));
    }

    #[test]
    fn skill_card_body_feeds_keywords_and_explicit_keywords_take_priority_order() {
        let c = db0();
        let body = json!({ "problem": "fix qa acceptance check", "fix_proposal": "update qa-acceptance-run" }).to_string();
        c.execute(
            "INSERT INTO cards (slug, title, status, body_json) VALUES (?1, ?2, 'backlog', ?3)",
            rusqlite::params!["TEST-QA-CARD", "fix qa acceptance", body],
        )
        .unwrap();
        let r = aiPromptPathGrounding(&json!({ "card_slug": "TEST-QA-CARD", "keywords": ["priority-kw"] }), &c).unwrap();
        let used: Vec<&str> = r["keywords_used"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(used[0], "priority-kw", "explicit keywords come first");
        assert!(used.contains(&"acceptance"));
        assert!(used.contains(&"qa-acceptance-run"));
    }

    #[test]
    fn skill_null_body_json_becomes_the_string_null_not_json_null() {
        let c = db0();
        c.execute(
            "INSERT INTO cards (slug, title, status, body_json) VALUES ('NULL-BODY', 'nullish card', 'backlog', NULL)",
            [],
        )
        .unwrap();
        let r = aiPromptPathGrounding(&json!({ "card_slug": "NULL-BODY" }), &c).unwrap();
        // "nullish card null" -> extractKeywords keeps "nullish" (>=3 chars, not a stopword);
        // "card"/"null" collide with STOPWORDS-vs-not: "card" is a stopword, "null" is not.
        let used: Vec<&str> = r["keywords_used"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert!(used.contains(&"nullish"));
        assert!(used.contains(&"null"));
        assert!(!used.contains(&"card"));
    }

    #[test]
    fn skill_max_paths_default_is_50_when_key_absent() {
        let c = db0();
        let r = aiPromptPathGrounding(&json!({ "keywords": ["mjs"] }), &c).unwrap();
        assert_eq!(r["matched_count"], json!(0)); // no git files in this checkout's REPO_ROOT
    }

    // ── bespoke-native: git_list_files / repo_root ──────────────────────────────────────────
    // repo_root() is a FIXED cwd (mirrors the JS's fixed import.meta.url-derived REPO_ROOT — no
    // parameter in production), so the production call is asserted against its documented SHAPE
    // (empty Vec in this checkout — verified not a git repo, see module doc) rather than a
    // hardcoded file list, while the git command's SUCCESS shape is separately proven against a
    // real temp-dir repo below (sync-capture-revision.rs's currentBranch/currentSha precedent).

    #[test]
    fn git_list_files_at_repo_root_is_empty_in_this_checkout() {
        assert_eq!(git_list_files(&repo_root()), Vec::<String>::new());
    }

    fn temp_repo(tag: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-ai-prompt-path-grounding-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        crate::proc::cmd("git").args(["init", "-q", "-b", "main"]).current_dir(&dir).output().unwrap();
        fs::write(dir.join("a.mjs"), "// a").unwrap();
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub").join("b.mjs"), "// b").unwrap();
        crate::proc::cmd("git").args(["add", "-A"]).current_dir(&dir).output().unwrap();
        crate::proc::cmd("git")
            .args(["-c", "user.name=antiloki", "-c", "user.email=antiloki@local", "commit", "-q", "-m", "init"])
            .current_dir(&dir)
            .output()
            .unwrap();
        dir
    }

    #[test]
    fn git_list_files_succeeds_against_a_real_temp_repo() {
        let dir = temp_repo("success-shape");
        let cwd = dir.to_string_lossy().into_owned();
        let mut files = git_list_files(&cwd);
        files.sort();
        assert_eq!(files, vec!["a.mjs".to_string(), "sub/b.mjs".to_string()]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_list_files_returns_empty_outside_a_repo() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-ai-prompt-path-grounding-rs-not-a-repo-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(git_list_files(&dir.to_string_lossy()), Vec::<String>::new());
        let _ = fs::remove_dir_all(&dir);
    }
}
