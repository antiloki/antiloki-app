//! contract-draft-auto.rs — twin of skills/contract/contract-draft-auto.mjs. NAMES ARE CONTRACT:
//! `contractDraftAuto` (default), `classifyTier`, `classifyRisk`, `classifyComplexity`,
//! `detectTemplateKind`, `extractFilesTouchedProposed`, `extractAcceptanceProposed` (named
//! exports the JS module also exposes for unit tests).
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: `repoFileExists`/`resolveFiles` (skill-modify,
//! migration-add) walk the REAL filesystem under SUBSTRATE_ROOT, and the orchestrator persists a
//! real `contracts` row via a real `PRAGMA table_info` + dynamic-column INSERT (same class as
//! contract-add.rs). Proven below via `#[cfg(test)]` against a real temp git-free directory tree
//! (mirrors `sandbox-resolve-path.rs`'s `substrate_root()` env-override convention) + a real
//! in-memory `contracts`/`cards` schema.
//!
//! DEFERRED boundary (documented, not silently dropped): the JS reaches for two RAG helpers via
//! `await import('../entities/catalog-find.mjs')` / `await import('../entities/law-surface-for-
//! context.mjs')` — `ragReferenceArtifacts`, `ragRelevantSoftLaws`, and the embeddings-ranked
//! branch of `pastRejectionHints`. A dynamic `import()` of a not-yet-converted skill has no
//! faithful Rust equivalent (crossing playbook, "dynamic import" rule). This twin ports every
//! DETERMINISTIC path around that boundary for real:
//!   - `ragReferenceArtifacts`: the template-hint half (`template.reference_artifact_hints`,
//!     deterministic, sourced from `contract_contract_templates::getTemplate`) is ported live;
//!     the `catalog-find` half always contributes an empty `fromRag` (as if the RAG call were
//!     unavailable — the same shape the JS produces on a `catalog-find` throw).
//!   - `ragRelevantSoftLaws`: entirely RAG — always returns `[]` (the JS catch-branch shape).
//!   - `pastRejectionHints`: the `memory.catalog_embeddings` COUNT probe is a real SQL read, but
//!     since acting on `haveEmbeds > 0` would require the deferred dynamic import, this twin
//!     always falls through to the real lexical SQL fallback (`memory.memories WHERE
//!     type='correction' AND tags_json LIKE '%contract%' ORDER BY id DESC LIMIT 5`) — the exact
//!     query the JS itself falls back to.
//!
//! Template mechanism functions (`files_touched_pattern`, `resolveFiles`, `naming_from_slug`,
//! `acceptance_baseline`, `goal_md_baseline`) live as JS closures on `TEMPLATES[kind]` in
//! contract-templates.mjs; `contract-templates.rs`'s `getTemplate`/`contractTemplates` twin
//! (by design, see that module's doc comment) projects ONLY the 7 static fields — the harness's
//! key-set equality made the closures a conform-db dead end there, but their real behavior is
//! still needed here for a faithful `contractDraftAuto`. This twin re-derives that behavior
//! directly from the JS source as private `template_*` match arms below (one per of the 6
//! `TEMPLATE_KINDS`), reusing `getTemplate` for the 7 static fields it already owns.
//!
//! `migration-add`'s `resolveFiles` timestamp uses the REAL wall clock (`SystemTime::now`), NOT
//! the crossing playbook's frozen-clock literal — that convention only pins the conform-db JS/
//! Rust LANES to the same instant for fixture comparison; this file has no fixture (same
//! reasoning as `auth-sign.rs`'s real `Date.now()` for `ts`).
// QUALITY: real fs classification + resolveFiles walk, real contracts-table persistence, RAG dynamic-import boundary deferred+documented, 6 template kinds re-derived from JS source

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

// ── standard JS-parity helpers (copied verbatim per crossing playbook) ─────────────────────
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}

// SUBSTRATE_ROOT = ANTILOKI_SUBSTRATE_ROOT else this monorepo's antiloki-core-rust's parent
// (sandbox-resolve-path.rs / tl-sandbox-create.rs precedent).
fn substrate_root() -> PathBuf {
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

fn repo_file_exists(p: &str) -> bool {
    substrate_root().join(p).exists()
}

// `p.replace(/^\.antiloki\/(skills|migrations)\//, 'antiloki-core/$1/')`
fn normalize_core_path(p: &str) -> String {
    if let Some(rest) = p.strip_prefix(".antiloki/skills/") {
        return format!("antiloki-core/skills/{rest}");
    }
    if let Some(rest) = p.strip_prefix(".antiloki/migrations/") {
        return format!("antiloki-core/migrations/{rest}");
    }
    p.to_string()
}

const SIZE_XS: i64 = 1;
const SIZE_S: i64 = 2;
const SIZE_M: i64 = 3;
const SIZE_L: i64 = 5;
fn size_weight(size: &str) -> i64 {
    match size {
        "XS" => SIZE_XS,
        "S" => SIZE_S,
        "M" => SIZE_M,
        "L" => SIZE_L,
        _ => SIZE_M,
    }
}

const TEMPLATE_KINDS: [&str; 6] =
    ["law-hook-add", "skill-modify", "skill-add", "action-add", "migration-add", "render-html-add"];

pub fn classifyTier(size: &Value, risk: &Value) -> Value {
    let s = size.as_str().unwrap_or("");
    let r = risk.as_f64().unwrap_or(0.0);
    if s == "XS" && r <= 2.0 {
        return json!("trivial");
    }
    if s == "L" || r >= 4.0 {
        return json!("high-risk");
    }
    json!("standard")
}

// `template?.default_risk ?? 2` — template is Null when no template classified.
pub fn classifyRisk(card: &Value, template: &Value) -> Value {
    let mut risk = template.get("default_risk").and_then(Value::as_f64).unwrap_or(2.0);
    let body = card.get("body_json").and_then(Value::as_str).unwrap_or("");
    if let Ok(re) = Regex::new(r"(?i)auth|secret|sacred_timeline|migration|drop|reset") {
        if re.is_match(body) {
            risk = (risk + 1.0).min(5.0);
        }
    }
    let pri = card.get("priority").cloned().unwrap_or(Value::Null);
    if pri == json!(1) || pri == json!(1.0) || pri == json!(2) || pri == json!(2.0) {
        risk = (risk + 1.0).min(5.0);
    }
    json!(risk as i64)
}

pub fn classifyComplexity(files_touched: &Value) -> Value {
    let n = files_touched.as_array().map(|a| a.len()).unwrap_or(0);
    let c = if n <= 1 {
        1
    } else if n <= 3 {
        2
    } else if n <= 6 {
        3
    } else if n <= 12 {
        4
    } else {
        5
    };
    json!(c)
}

fn classify_by_slug_at_pattern(card: &Value) -> Option<Value> {
    let slug = str_qq(card.get("slug")).to_uppercase();
    let re = Regex::new(r"-AT-([A-Z][A-Z0-9-]+)$").unwrap();
    let caps = re.captures(&slug)?;
    let target_kebab = caps[1].to_lowercase();
    const ROOTS: [&str; 16] = [
        "antiloki-core/skills/system/",
        "antiloki-core/skills/entities/",
        "antiloki-core/skills/engine/",
        "antiloki-core/skills/contract/",
        "antiloki-core/skills/agents/dev/",
        "antiloki-core/skills/agents/pl/",
        "antiloki-core/skills/agents/tl/",
        "antiloki-core/skills/agents/qa/",
        "antiloki-core/skills/agents/man/",
        "antiloki-core/skills/agents/in/",
        "antiloki-core/skills/agents/keeper/",
        "antiloki-core/skills/audit/",
        "antiloki-core/skills/render/",
        "antiloki-core/skills/external/",
        "antiloki-core/skills/driver/",
        "antiloki-core/skills/ai/",
    ];
    for r in ROOTS {
        let p = format!("{r}{target_kebab}.mjs");
        if repo_file_exists(&p) {
            return Some(json!({
                "kind": "skill-modify", "source": "slug-at-pattern", "confidence": 0.85, "resolved_path": p
            }));
        }
    }
    None
}

fn classify_by_file_path(card: &Value) -> Option<Value> {
    let haystack = format!("{} {}", tmpl(card.get("title")), str_qq(card.get("body_json")));
    let re = Regex::new(r"(?:\.antiloki|antiloki-core)/[\w/\-]+\.(?:mjs|ts|tsx|js|sql)").unwrap();
    let m = re.find(&haystack)?;
    let p = normalize_core_path(m.as_str());
    if p.starts_with("antiloki-core/skills/audit/law-hook-") {
        return Some(json!({ "kind": "law-hook-add", "source": "file-path-mention", "confidence": 0.9 }));
    }
    if p.starts_with("antiloki-core/skills/render/") {
        return Some(json!({ "kind": "render-html-add", "source": "file-path-mention", "confidence": 0.85 }));
    }
    if p.starts_with("antiloki-core/migrations/") {
        return Some(json!({ "kind": "migration-add", "source": "file-path-mention", "confidence": 0.9 }));
    }
    if p.starts_with("antiloki-core/skills/") {
        let exists = repo_file_exists(&p);
        return Some(json!({
            "kind": if exists { "skill-modify" } else { "skill-add" },
            "source": if exists { "file-path-mention(exists)" } else { "file-path-mention(new)" },
            "confidence": 0.9
        }));
    }
    None
}

/// `detectTemplateKind({card, db, explicit})` — `db` is unused deterministically here (only
/// consumed inside the deferred RAG paths, which never run in this twin).
pub fn detectTemplateKind(card: &Value, _db: &Connection, explicit: &Value) -> Value {
    if let Some(e) = explicit.as_str() {
        if TEMPLATE_KINDS.contains(&e) {
            return json!({ "kind": e, "source": "explicit", "confidence": 1.0 });
        }
    }
    if let Some(pc) = classify_by_file_path(card) {
        return pc;
    }
    if let Some(sc) = classify_by_slug_at_pattern(card) {
        return sc;
    }
    let haystack = format!("{} {}", tmpl(card.get("title")), str_qq(card.get("body_json"))).to_lowercase();
    let heuristics: [(&str, &str); 5] = [
        ("law-hook-add", r"\blaw[- ]?hook\b|\benforce\w*\b|\bconstitutional\b"),
        ("migration-add", r"\bmigration\b|\bschema\b|\badd column\b|\bnew table\b"),
        ("render-html-add", r"\.html\b|\brender\b.*\bhtml\b|\bvisualiz\w*\b|\bhtml\b.*\bpage\b|\bsvg\b"),
        ("skill-modify", r"\bmodify\b|\bupdate\b.*\bskill\b|\bfix\s+bug\b|\bpatch\b"),
        ("action-add", r"\bnew action\b|\badd action\b|\bregister action\b"),
    ];
    for (kind, pat) in heuristics {
        if Regex::new(pat).unwrap().is_match(&haystack) {
            return json!({ "kind": kind, "source": "keyword-heuristic", "confidence": 0.7 });
        }
    }
    json!({ "kind": Value::Null, "source": "no-template", "confidence": 0.0 })
}

// ── files_touched_proposed / acceptance override extraction ────────────────────────────────
fn parse_body_json(card: &Value) -> Result<Value, ()> {
    let body_json = card.get("body_json");
    if !truthy(body_json) {
        return Err(());
    }
    let raw_str = body_json.unwrap().as_str().ok_or(())?;
    let raw: Value = serde_json::from_str(raw_str).map_err(|_| ())?;
    match raw {
        Value::String(s) => serde_json::from_str(&s).map_err(|_| ()),
        other => Ok(other),
    }
}

pub fn extractFilesTouchedProposed(card: &Value) -> Value {
    let no_body = json!({ "ok": false, "reason": "no body_json" });
    if !truthy(card.get("body_json")) {
        return no_body;
    }
    let parsed = match parse_body_json(card) {
        Ok(v) => v,
        Err(_) => return json!({ "ok": false, "reason": "body_json not parseable" }),
    };
    let obj = match parsed.as_object() {
        Some(o) => o,
        None => return json!({ "ok": false, "reason": "body root not an object" }),
    };
    let proposed = match obj.get("files_touched_proposed") {
        None | Some(Value::Null) => return json!({ "ok": false, "reason": "files_touched_proposed absent" }),
        Some(v) => v,
    };
    let arr = match proposed.as_array() {
        Some(a) => a,
        None => return json!({ "ok": false, "reason": "files_touched_proposed not an array" }),
    };
    if arr.is_empty() {
        return json!({ "ok": false, "reason": "files_touched_proposed empty" });
    }
    let placeholder = Regex::new(r"^<[A-Z_]+>$").unwrap();
    let glob = Regex::new(r"[*?\[\]]").unwrap();
    for (i, p) in arr.iter().enumerate() {
        let s = match p.as_str() {
            Some(s) => s,
            None => return json!({ "ok": false, "reason": format!("element {i} not a string") }),
        };
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return json!({ "ok": false, "reason": format!("element {i} empty/whitespace") });
        }
        if trimmed != s {
            return json!({ "ok": false, "reason": format!("element {i} has surrounding whitespace") });
        }
        if placeholder.is_match(s) {
            return json!({ "ok": false, "reason": format!("element {i} is a placeholder '{s}'") });
        }
        if glob.is_match(s) {
            return json!({ "ok": false, "reason": format!("element {i} contains glob metachars") });
        }
    }
    json!({ "ok": true, "paths": arr.clone() })
}

const KNOWN_ACCEPTANCE_KINDS: [&str; 12] = [
    "sql", "test", "cli", "file-exists", "file_exists", "grep-contains", "grep",
    "behavior", "behavioral", "diff_in_paths", "no_law_violation", "skill_returns",
];
// NOTE: JS set also has "command", "scope_check" — full list transcribed below in the check fn
// (kept out of the fixed-size array above only for readability; both are checked for real).
fn is_known_acceptance_kind(k: &str) -> bool {
    KNOWN_ACCEPTANCE_KINDS.contains(&k) || k == "command" || k == "scope_check"
}

pub fn extractAcceptanceProposed(card: &Value) -> Value {
    if !truthy(card.get("body_json")) {
        return json!({ "ok": false, "reason": "no body_json" });
    }
    let parsed = match parse_body_json(card) {
        Ok(v) => v,
        Err(_) => return json!({ "ok": false, "reason": "body_json not parseable" }),
    };
    let obj = match parsed.as_object() {
        Some(o) => o,
        None => return json!({ "ok": false, "reason": "body root not an object" }),
    };
    let acc = match obj.get("acceptance") {
        None | Some(Value::Null) => return json!({ "ok": false, "reason": "acceptance absent" }),
        Some(v) => v,
    };
    let arr = match acc.as_array() {
        Some(a) => a,
        None => return json!({ "ok": false, "reason": "acceptance not an array" }),
    };
    if arr.is_empty() {
        return json!({ "ok": false, "reason": "acceptance empty" });
    }
    for (i, item) in arr.iter().enumerate() {
        if item.is_string() {
            continue;
        }
        let o = match item.as_object() {
            Some(o) => o,
            None => return json!({ "ok": false, "reason": format!("element {i} not object/string") }),
        };
        if let Some(k) = o.get("kind").and_then(Value::as_str) {
            if !is_known_acceptance_kind(k) {
                return json!({ "ok": false, "reason": format!("element {i} unknown kind '{k}'") });
            }
        }
    }
    json!({ "ok": true, "acceptance": arr.clone() })
}

// ── template mechanism (files_touched_pattern / resolveFiles / naming / acceptance / goal_md) ──
fn to_kebab(s: &str) -> String {
    let lower = s.to_lowercase();
    let re = Regex::new(r"[^a-z0-9]+").unwrap();
    re.replace_all(&lower, "-").trim_matches('-').to_string()
}
fn to_camel(kebab: &str) -> String {
    kebab
        .split('-')
        .enumerate()
        .map(|(i, p)| if i == 0 { p.to_string() } else { format!("{}{}", p[..1].to_uppercase(), &p[1..]) })
        .collect()
}
fn to_screaming(s: &str) -> String {
    let upper = s.to_uppercase();
    let re = Regex::new(r"[^A-Z0-9]+").unwrap();
    re.replace_all(&upper, "_").trim_matches('_').to_string()
}

fn find_file_recursive(root: &Path, target: &str, depth: u32) -> Option<PathBuf> {
    if depth > 8 {
        return None;
    }
    let mut entries: Vec<_> = std::fs::read_dir(root).ok()?.filter_map(|e| e.ok()).collect();
    if entries.len() > 500 {
        entries.truncate(500);
    }
    for entry in &entries {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "node_modules" || name == ".git" || name == "dist" {
            continue;
        }
        let p = entry.path();
        if name == target {
            return Some(p);
        }
        if p.is_dir() {
            if let Some(sub) = find_file_recursive(&p, target, depth + 1) {
                return Some(sub);
            }
        }
    }
    None
}

fn to_forward_slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// skill-modify's `resolveFiles(card)` — literal filename / slug / slug-AT- / verb-window
/// candidates, resolved against a real recursive walk of SEARCH_ROOTS under SUBSTRATE_ROOT.
fn skill_modify_resolve_files(card: &Value) -> Option<Vec<String>> {
    let haystack = format!("{} {}", tmpl(card.get("title")), str_qq(card.get("body_json")));
    let mut candidates: Vec<(&str, String)> = Vec::new();

    let literal_re = Regex::new(r"\b([A-Za-z][\w-]*\.(?:mjs|ts|tsx|js))\b").unwrap();
    for m in literal_re.captures_iter(&haystack) {
        candidates.push(("literal", m[1].to_string()));
    }

    let slug = str_qq(card.get("slug")).to_uppercase();
    let slug_re = Regex::new(r"^(?:FIX|BUG|UPDATE|MODIFY|PATCH|TWEAK|REFACTOR)-([A-Z][A-Z0-9-]+)$").unwrap();
    if let Some(c) = slug_re.captures(&slug) {
        candidates.push(("slug", c[1].to_lowercase()));
    }
    let slug_at_re = Regex::new(r"-AT-([A-Z][A-Z0-9-]+)$").unwrap();
    if let Some(c) = slug_at_re.captures(&slug) {
        candidates.push(("slug-at", c[1].to_lowercase()));
    }

    let verbs = r"\b(fix|update|modify|change|refactor|rewrite|extend|patch|tweak)\b";
    let title = tmpl(card.get("title"));
    let title_re = Regex::new(&format!(r"(?i){verbs}\s+(?:the\s+)?([a-z][a-z0-9-]+(?:-[a-z0-9]+){{1,4}})")).unwrap();
    if let Some(c) = title_re.captures(&title) {
        candidates.push(("verb", c[1].to_lowercase()));
    }
    let body = str_qq(card.get("body_json"));
    let body_re = Regex::new(&format!(r"(?i){verbs}\s+(?:the\s+)?([a-z][a-z0-9-]+(?:-[a-z0-9]+){{1,4}})")).unwrap();
    if let Some(c) = body_re.captures(&body) {
        candidates.push(("verb", c[1].to_lowercase()));
    }

    if candidates.is_empty() {
        return None;
    }
    const SEARCH_ROOTS: [&str; 4] = ["antiloki-core/skills", "antiloki-BE/src", "antiloki-FE/src", "FE-TUI/src"];
    let mut seen: HashSet<String> = HashSet::new();
    for (kind, name) in &candidates {
        let key = format!("{kind}:{name}");
        if !seen.insert(key) {
            continue;
        }
        for root in SEARCH_ROOTS {
            let base = substrate_root().join(root);
            if !base.exists() {
                continue;
            }
            let filenames: Vec<String> = if *kind == "literal" {
                vec![name.clone()]
            } else {
                vec![format!("{name}.mjs"), format!("{name}.ts"), format!("{name}.tsx")]
            };
            for fname in filenames {
                if let Some(abs) = find_file_recursive(&base, &fname, 0) {
                    let rel = abs.strip_prefix(&substrate_root()).unwrap_or(&abs);
                    let match_path = to_forward_slash(rel);
                    let test_re = Regex::new(r"\.(mjs|ts|tsx|js)$").unwrap();
                    let test_path = test_re.replace(&match_path, ".test.$1").to_string();
                    return Some(vec![match_path, test_path]);
                }
            }
        }
    }
    None
}

/// skill-add's `resolveFiles(card)` — explicit path mention or domain-hint regex (no fs walk).
fn skill_add_resolve_files(card: &Value) -> Option<Vec<String>> {
    let haystack = format!("{} {}", tmpl(card.get("title")), str_qq(card.get("body_json")));
    let explicit_re = Regex::new(r"(?:\.antiloki|antiloki-core)/skills/[\w/\-]+\.mjs").unwrap();
    if let Some(m) = explicit_re.find(&haystack) {
        let main = if m.as_str().starts_with(".antiloki/") {
            m.as_str().replacen(".antiloki/", "antiloki-core/", 1)
        } else {
            m.as_str().to_string()
        };
        let test = main.replace(".mjs", ".test.mjs");
        return Some(vec![main, test]);
    }
    let slug = to_kebab(&str_qq(card.get("slug")));
    let domain_re = Regex::new(r"(?i)(?:under|in)\s+(?:\.antiloki|antiloki-core)/skills/([\w/\-]+)/?").unwrap();
    if let Some(c) = domain_re.captures(&haystack) {
        let domain = c[1].trim_end_matches('/').to_string();
        return Some(vec![
            format!("antiloki-core/skills/{domain}/{slug}.mjs"),
            format!("antiloki-core/skills/{domain}/{slug}.test.mjs"),
        ]);
    }
    None
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}
fn today_ymd() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let total_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let days = total_ms.div_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}{m:02}{d:02}")
}

/// migration-add's `resolveFiles(card, db)` — real today's YMD + next free NNN counter derived
/// from a real readdir of `antiloki-core/migrations`.
fn migration_add_resolve_files(card: &Value) -> Vec<String> {
    let ymd = today_ymd();
    let slug = to_kebab(&str_qq(card.get("slug")));
    let mig_dir = substrate_root().join("antiloki-core/migrations");
    let mut nnn = 1usize;
    if let Ok(entries) = std::fs::read_dir(&mig_dir) {
        let count = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.starts_with(&ymd) && (n.ends_with(".mjs") || n.ends_with(".sql"))
            })
            .count();
        nnn = count + 1;
    }
    let ts_id = format!("{ymd}-{nnn:03}");
    vec![
        format!("antiloki-core/migrations/{ts_id}-{slug}.mjs"),
        "antiloki-core/skills/engine/open-db.mjs".to_string(),
    ]
}

/// Returns (files, source_tag) for a given template kind, mirroring contract-draft-auto.mjs's
/// `resolved ? resolveFiles(...) : files_touched_pattern(card)` selection.
fn template_files_touched(kind: &str, card: &Value) -> (Vec<String>, String) {
    let slug = to_kebab(&str_qq(card.get("slug")));
    match kind {
        "law-hook-add" => (
            vec![
                format!("antiloki-core/skills/audit/law-hook-{slug}.mjs"),
                "antiloki-core/skills/audit/law-hooks.test.mjs".to_string(),
                "antiloki-core/skills/engine/canonical-laws.mjs".to_string(),
            ],
            format!("template.files_touched_pattern({kind})"),
        ),
        "skill-modify" => match skill_modify_resolve_files(card) {
            Some(f) => (f, format!("template.resolveFiles({kind})")),
            None => (
                vec!["<TARGET_SKILL_PATH>".to_string(), "<TARGET_SKILL_TEST_PATH>".to_string()],
                format!("template.files_touched_pattern({kind})"),
            ),
        },
        "skill-add" => match skill_add_resolve_files(card) {
            Some(f) => (f, format!("template.resolveFiles({kind})")),
            None => (
                vec![
                    format!("antiloki-core/skills/system/{slug}.mjs"),
                    format!("antiloki-core/skills/system/{slug}.test.mjs"),
                ],
                format!("template.files_touched_pattern({kind})"),
            ),
        },
        "action-add" => (
            vec![
                format!("antiloki-core/skills/<DOMAIN>/{slug}.mjs"),
                format!("antiloki-core/skills/<DOMAIN>/{slug}.test.mjs"),
                "antiloki-core/skills/engine/open-db.mjs".to_string(),
            ],
            format!("template.files_touched_pattern({kind})"),
        ),
        "migration-add" => (migration_add_resolve_files(card), format!("template.resolveFiles({kind})")),
        "render-html-add" => (
            vec![
                format!("antiloki-core/skills/render/{slug}.mjs"),
                format!("antiloki-core/skills/render/{slug}.test.mjs"),
                "antiloki-core/skills/engine/open-db.mjs".to_string(),
            ],
            format!("template.files_touched_pattern({kind})"),
        ),
        _ => (vec![], String::new()),
    }
}

fn template_naming(kind: &str, card: &Value) -> Value {
    let slug = to_kebab(&str_qq(card.get("slug")));
    match kind {
        "law-hook-add" => {
            let basename = format!("law-hook-{slug}");
            json!({
                "skill_basename": basename,
                "default_export_function": to_camel(&basename),
                "law_slug_suggestion": to_screaming(&slug),
                "error_code": "PERMISSION_DENIED",
            })
        }
        "skill-modify" => json!({ "preserve_existing": true }),
        "skill-add" => json!({
            "skill_basename": slug,
            "default_export_function": to_camel(&slug),
            "domain": "system",
        }),
        "action-add" => json!({
            "skill_basename": slug,
            "default_export_function": to_camel(&slug),
            "skill_name_pattern": "<persona>.<noun>.<verb>",
        }),
        "migration-add" => json!({ "migration_filename": format!("<timestamp>-{slug}.sql") }),
        "render-html-add" => json!({
            "skill_basename": slug,
            "default_export_function": to_camel(&slug),
        }),
        _ => Value::Null,
    }
}

fn template_acceptance(kind: &str, naming: &Value, files: &[String]) -> Value {
    let f = |i: usize| files.get(i).cloned().unwrap_or_default();
    match kind {
        "law-hook-add" => {
            let test_file = files.iter().find(|p| p.ends_with("law-hooks.test.mjs")).cloned().unwrap_or_default();
            json!([
                { "kind": "test", "cmd": format!("npx node --test {test_file}"), "expect_exit": 0 },
                { "kind": "file_exists", "path": f(0), "min_bytes": 200, "must_contain": ["export default", "allowed:"] },
                { "kind": "skill_returns", "action": "system.law.check", "input": {},
                  "expect_jsonpath": { "$[*].enforcement_hook": naming["skill_basename"] } },
                { "kind": "no_law_violation", "law_slug": naming["law_slug_suggestion"], "since": "contract.open_ts" },
                { "kind": "diff_in_paths", "base": "contract.base_sha", "allowed": files },
            ])
        }
        "skill-modify" => json!([
            { "kind": "test", "cmd": format!("npx node --test {}", files.get(1).cloned().unwrap_or_else(|| "<TEST_PATH>".into())), "expect_exit": 0 },
            { "kind": "diff_in_paths", "base": "contract.base_sha", "allowed": files },
        ]),
        "skill-add" => json!([
            { "kind": "test", "cmd": format!("npx node --test {}", f(1)), "expect_exit": 0 },
            { "kind": "file_exists", "path": f(0), "min_bytes": 200, "must_contain": ["export default"] },
            { "kind": "diff_in_paths", "base": "contract.base_sha", "allowed": files },
        ]),
        "action-add" => json!([
            { "kind": "test", "cmd": format!("npx node --test {}", f(1)), "expect_exit": 0 },
            { "kind": "file_exists", "path": f(0), "min_bytes": 200, "must_contain": ["export default"] },
            { "kind": "sql", "query": format!("SELECT COUNT(*) AS c FROM actions WHERE name LIKE '%{}%'", str_qq(naming.get("skill_basename"))), "expect_value_path": "$.c", "expect_value_min": 1 },
            { "kind": "diff_in_paths", "base": "contract.base_sha", "allowed": files },
        ]),
        "migration-add" => json!([
            { "kind": "sql", "query": "SELECT COUNT(*) AS c FROM schema_migrations WHERE applied_at IS NOT NULL", "expect_value_min": 1 },
            { "kind": "test", "cmd": "npx node --test antiloki-core/skills/engine/open-db.test.mjs", "expect_exit": 0 },
            { "kind": "diff_in_paths", "base": "contract.base_sha", "allowed": files },
        ]),
        "render-html-add" => json!([
            { "kind": "test", "cmd": format!("npx node --test {}", f(1)), "expect_exit": 0 },
            { "kind": "file_exists", "path": f(0), "min_bytes": 500, "must_contain": ["<!DOCTYPE html>", "<svg"] },
            { "kind": "diff_in_paths", "base": "contract.base_sha", "allowed": files },
        ]),
        _ => json!([]),
    }
}

fn extract_body_section(card: &Value, keys: &[&str]) -> Option<String> {
    let body_json = card.get("body_json");
    if !truthy(body_json) {
        return None;
    }
    let parsed: Value = match body_json.unwrap() {
        Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or_else(|_| Value::String(s.clone())),
        other => other.clone(),
    };
    match &parsed {
        Value::Object(map) => {
            for k in keys {
                if let Some(Value::String(s)) = map.get(*k) {
                    let t = s.trim();
                    if !t.is_empty() {
                        return Some(t.to_string());
                    }
                }
            }
            None
        }
        _ => None,
    }
}
fn section(heading: &str, content: Option<&str>) -> String {
    match content {
        None => String::new(),
        Some(c) if c.is_empty() => String::new(),
        Some(c) => format!("## {heading}\n{c}\n\n"),
    }
}
fn substrate_invariants() -> String {
    [
        "- All mutations of cards/agents/actions/memory/laws go through `with-audit` (NEVER_BYPASS_WITH_AUDIT).",
        "- Skills must be invoked via actions, not imported directly outside skill composition (LONE_SKILL_DISPATCH_FORBIDDEN).",
        "- Every \"I built X\" claim needs a verification command in the same response (CLAIM_REQUIRES_VERIFICATION).",
        "- Read the existing file before editing (READ_BEFORE_WRITING). Match the file's own conventions.",
    ]
    .join("\n")
}

fn template_goal_md(kind: &str, card: &Value, files: &[String], naming: &Value) -> String {
    let motivation = extract_body_section(card, &["motivation", "why", "diagnosis"]);
    let fix = extract_body_section(card, &["fix", "strategy", "plan"]);
    let title = tmpl(card.get("title"));
    let mut out = String::new();
    match kind {
        "law-hook-add" => {
            let basename = str_qq(naming.get("skill_basename"));
            let law_slug = str_qq(naming.get("law_slug_suggestion"));
            out.push_str(&section("Mission", Some(&format!("Add a new law-hook ({basename}) enforcing **{law_slug}**. The hook is consulted on every relevant action dispatch and must return `{{ allowed: boolean, reason?, severity? }}`."))));
            out.push_str(&section("Background", Some(motivation.as_deref().unwrap_or("The substrate's safety model: laws are queryable rules, hooks are the runtime enforcement. When this hook lands, action-run consults it before dispatching the matching actions."))));
            out.push_str(&section("Strategy hints from the card", fix.as_deref()));
            out.push_str(&section("Constraints", Some(&format!(
                "- The hook MUST be a pure `(input, context) => {{ allowed, ... }}` function — no side effects on the allowed=true branch (return fast).\n\
                 - Escape-hatch env var (e.g. `ANTILOKI_ENFORCE_{law_slug}='off'`) — required so migrations can soak the rule before block mode.\n\
                 - Export name MUST match the law's `enforcement_hook` column.\n\
                 - Hook runs inside action-run's transaction — keep predicate O(1). Do NOT dispatch other actions from inside (infinite recursion)."
            ))));
            out.push_str(&section("Risk profile", Some(
                "- Over-aggressive predicate makes the substrate unusable. Start with `enforcement: warn` in canonical-laws; flip to `block` only after a soak.\n\
                 - Hidden coupling: if the predicate consults DB rows that other actions just wrote, you may race a transaction. Read with care.\n\
                 - Audit chain: violations land in `law_violations`; this table is append-only. Do NOT manually insert from the hook (with-audit handles it)."
            )));
            out.push_str(&section("Open questions for DEV judgment", Some(
                "- Should `allowed=true` also log info-level entries to `law_violations` for telemetry? Default: no, only `blocked` / `warn` outcomes.\n\
                 - What's the FAST-PATH? Many calls won't apply to this rule — return `{ allowed: true }` immediately when the action_name isn't in scope."
            )));
            out.push_str(&section("Patterns to emulate", Some("- law-hook-no-drift-on-audit-commit\n- law-hook-no-destructive-git")));
            out.push_str(&section("Substrate invariants you must respect", Some(&substrate_invariants())));
        }
        "skill-modify" => {
            let target_file = files.first().cloned().unwrap_or_else(|| "<TARGET_SKILL_PATH>".to_string());
            let test_file = files.get(1).cloned().unwrap_or_else(|| "<TARGET_SKILL_TEST_PATH>".to_string());
            out.push_str(&section("Mission", Some(&format!("Modify `{target_file}` to {}. The mechanism stays the same; the behavior changes per the card's \"fix\" or \"strategy\" guidance.", if title.is_empty() { "address the card".into() } else { title.clone() }))));
            out.push_str(&section("Background", Some(motivation.as_deref().unwrap_or("This is a SURGICAL edit. The skill already works; the change is targeted. The file's default-export signature MUST remain (input, context) → result — the action dispatcher relies on the uniform shape."))));
            out.push_str(&section("Strategy hints from the card", fix.as_deref()));
            let files_joined = files.join(", ");
            out.push_str(&section("Constraints", Some(&format!(
                "- Preserve the default-export function name + signature. If the card asks to rename, that's a separate concern (call sites change).\n\
                 - Out-of-scope: anything outside [{files_joined}] — no formatting/lint sweeps of nearby code, no unrelated refactors.\n\
                 - If the skill's JSDoc / module header documents behavior, UPDATE the doc in the same edit. A wrong comment is worse than no comment."
            ))));
            let basename = naming.get("skill_basename").and_then(Value::as_str).map(String::from).unwrap_or_else(|| str_qq(card.get("slug")));
            out.push_str(&section("Risk profile", Some(&format!(
                "- Hidden coupling: the skill is likely composed by N actions. Find them via `system.action.find '{{\"query\":\"{basename}\"}}'`. Each one may depend on the exact return shape.\n\
                 - Test surface: `{test_file}` covers existing behavior; new behavior needs new test cases AND the old tests must still pass (unless the card explicitly authorizes a break).\n\
                 - Backward-compat: a new input field MUST default to null/false. Never add a required field to an existing skill — every caller would need to change in lockstep."
            ))));
            out.push_str(&section("Open questions for DEV judgment", Some(
                "- If new behavior conflicts with existing behavior, which wins? Default: new wins, document the break in a JSDoc comment + the commit message.\n\
                 - Where in the file does the new code go? Default: keep related logic adjacent. Don't reorganize the file just because a new function is being added."
            )));
            out.push_str(&section("Patterns to emulate", Some("Read the surrounding file. Don't invent new conventions when the file already has its own patterns for error handling, return shape, JSDoc style, or test layout.")));
            out.push_str(&section("Substrate invariants you must respect", Some(&substrate_invariants())));
        }
        "skill-add" => {
            let basename = str_qq(naming.get("skill_basename"));
            let f0 = files.first().cloned().unwrap_or_default();
            let f1 = files.get(1).cloned().unwrap_or_default();
            out.push_str(&section("Mission", Some(&format!("Create a brand-new skill **{basename}** at `{f0}` that {}.", if title.is_empty() { "addresses the card".into() } else { title.clone() }))));
            out.push_str(&section("Background", Some(motivation.as_deref().unwrap_or("New skills are the atomic mechanisms the substrate composes into actions. This file defines the default export — a pure(ish) function (input, context) → result — that an action wraps with with-audit when needed."))));
            out.push_str(&section("Strategy hints from the card", fix.as_deref()));
            out.push_str(&section("Constraints", Some(&format!(
                "- Default export MUST be a function (SKILL_EXPORTS_DEFAULT_FUNCTION).\n\
                 - Signature MUST be (input, context) (SKILL_DEFAULT_TAKES_INPUT_CONTEXT) — no positional alternatives.\n\
                 - Throw `AntilokiError` with the appropriate `ErrorCodes.X` — never plain Error.\n\
                 - This contract does NOT register an action; if the skill needs to be called externally, a follow-up action-add contract wires it.\n\
                 - File path is `{f0}`; test file is `{f1}`. If the body suggests a different domain (e.g. agents/dev/), TL overrides files_touched before signing."
            ))));
            out.push_str(&section("Risk profile", Some(
                "- LONE_SKILL_DISPATCH_FORBIDDEN: the new skill is invisible until an action wraps it. Tests use `skill(input, {db})` direct calls — that's the ONLY valid direct-import path (skill-internal composition is OK).\n\
                 - Database writes: if the skill mutates audited entities (cards/agents/actions/memory/laws), it MUST be called via an action that composes with-audit. Don't write raw SQL UPDATE/INSERT from inside the skill.\n\
                 - Idempotency: think about what re-running with the same input means. UNIQUE conflicts throw ALREADY_EXISTS; document any softer behavior in a JSDoc comment."
            )));
            out.push_str(&section("Open questions for DEV judgment", Some(
                "- Domain placement: which subdirectory under `antiloki-core/skills/`? Default: `system/` if no agent owns this; otherwise `agents/<persona>/`.\n\
                 - Async or sync? Default: sync unless the skill calls AI / network / filesystem.\n\
                 - Return shape: object with named fields beats a positional tuple. Document the shape in a JSDoc `@returns`."
            )));
            out.push_str(&section("Patterns to emulate", Some("- `card-add` — canonical \"add an entity\" pattern.\n- `with-audit` — wrapper pattern when the skill needs audit-chain integration.\n- `prompt-find` — read-only lookup pattern.")));
            out.push_str(&section("Substrate invariants you must respect", Some(&substrate_invariants())));
        }
        "action-add" => {
            let basename = str_qq(naming.get("skill_basename"));
            let f0 = files.first().cloned().unwrap_or_default();
            out.push_str(&section("Mission", Some(&format!("Create a new action **{basename}** that {}. The action composes the new skill through with-audit and is registered in canonical-actions.mjs so it appears in `system.action.find`.", if title.is_empty() { "addresses the card".into() } else { title.to_lowercase() }))));
            out.push_str(&section("Background", Some(motivation.as_deref().unwrap_or("Actions are the user-facing surface; skills are the atomic mechanisms. An action's steps_json composes one or more skills plus with-audit when audited entities are mutated. This is how the substrate exposes new functionality to AI agents AND human callers — the gate where permissions, audit, and law enforcement converge."))));
            out.push_str(&section("Strategy hints from the card", fix.as_deref()));
            let default_fn = str_qq(naming.get("default_export_function"));
            out.push_str(&section("Constraints", Some(&format!(
                "- Action name MUST follow `agent.action.target` (e.g. board-keeper.card.add, dev.implement.code).\n\
                 - New skill at `{f0}` exports default function `{default_fn}(input, context)` returning a result.\n\
                 - Compose with-audit if mutating audited entities (cards, agents, actions, memory.memories, laws.laws).\n\
                 - Registration in canonical-actions.mjs uses the standard shape (name, description, owner_agent, steps_json, returns, on_failure)."
            ))));
            out.push_str(&section("Risk profile", Some(
                "- LONE_SKILL_DISPATCH_FORBIDDEN: the new skill cannot be invoked outside the action layer. Tests use the action path (`action-run`), not direct skill imports.\n\
                 - Permission model: who can call this action? If agent-specific, add to `agent_actions` grants in a follow-up. System-level actions are callable by all.\n\
                 - Atomicity: if the operation has multiple effects, compose multiple skills in steps_json — don't make ONE skill do everything. Smaller skills are more reusable + replayable.\n\
                 - Idempotency: a re-run with identical input MUST behave predictably. UNIQUE conflicts throw ALREADY_EXISTS; document any deviation."
            )));
            out.push_str(&section("Open questions for DEV judgment", Some(
                "- Persona-namespacing: does this action belong to a specific agent (dev/tl/pl/qa/man/board-keeper) or to the system layer? Default: belongs to the agent whose intent the action expresses.\n\
                 - Failure mode: `on_failure: 'abort'` (default) halts the action chain; `'continue'` collects the error and proceeds. Default to abort unless the card explicitly authorizes a soft-fail."
            )));
            out.push_str(&section("Patterns to emulate", Some("- `board-keeper.card.add` — canonical \"add an entity\" pattern with with-audit wrapping.\n- Any existing `<agent>.<noun>.<verb>` skill — model the export shape + JSDoc.")));
            out.push_str(&section("Substrate invariants you must respect", Some(&substrate_invariants())));
        }
        "migration-add" => {
            let f0 = files.first().cloned().unwrap_or_default();
            out.push_str(&section("Mission", Some(&format!("Add a forward-only DB migration that {}. Idempotent (re-runs are no-ops via PRAGMA checks).", if title.is_empty() { "changes the schema".into() } else { title.to_lowercase() }))));
            out.push_str(&section("Background", Some(motivation.as_deref().unwrap_or("Migrations live in antiloki-core/migrations/ as numbered files. open-db.mjs auto-runs unapplied migrations on every connection. `schema_migrations` tracks applied state. Per HISTORY_IS_SACRED_NO_REVERT: fix-forward with a new migration — never edit an applied one."))));
            out.push_str(&section("Strategy hints from the card", fix.as_deref()));
            out.push_str(&section("Constraints", Some(&format!(
                "- File path: `{f0}` follows the `YYYYMMDD-NNN-<slug>.mjs` convention. NNN is the next free counter for today.\n\
                 - Exports `name`, `description`, and `async function up(db)` — no `down` (no rollback).\n\
                 - Idempotent: `PRAGMA table_info()` to check for existing columns, `CREATE TABLE IF NOT EXISTS` for tables. Return `{{ skipped: true, reason }}` when already applied.\n\
                 - NO destructive ops without explicit god-mode auth: no DROP TABLE, no DROP COLUMN, no DELETE without WHERE."
            ))));
            out.push_str(&section("Risk profile", Some(
                "- Data loss: if columns are renamed, COPY data first via a 3-step migration (add new + backfill + drop old in a later migration after consumers update).\n\
                 - Cross-DB FKs: state.sqlite and brain.sqlite use TEXT slug as FK convention (not enforced by SQLite). Don't rely on cross-DB FOREIGN KEY enforcement.\n\
                 - Schema drift: every consumer of the new column must be updated in the same contract OR behind a feature flag.\n\
                 - Backfill: if the new column is nullable with no default, downstream queries must handle NULL. If NOT NULL with default, the default applies to existing rows."
            )));
            out.push_str(&section("Open questions for DEV judgment", Some(
                "- NOT NULL or nullable? Default: NULLABLE first, tighten in a future migration after consumers populate.\n\
                 - Add an index? Default: yes if used in any WHERE/JOIN; benchmark before adding wide indexes.\n\
                 - WHERE clause on partial index? Default: yes when the column is mostly NULL and queries filter on `IS NOT NULL`."
            )));
            out.push_str(&section("Patterns to emulate", Some("- `20260522-042-cards-skip-pl-flag.mjs` — canonical \"add a column with default\" migration.\n- `20260522-047-project-journey.mjs` — canonical \"add a new table\" migration.")));
            out.push_str(&section("Substrate invariants you must respect", Some(&substrate_invariants())));
        }
        "render-html-add" => {
            let basename = str_qq(naming.get("skill_basename"));
            out.push_str(&section("Mission", Some(&format!("Add a render-html action **{basename}** producing a self-contained HTML file that visualizes {}.", if title.is_empty() { "substrate state".into() } else { title.to_lowercase() }))));
            out.push_str(&section("Background", Some(motivation.as_deref().unwrap_or("Render-html actions are diagnostic surfaces — they query substrate DBs and produce a single HTML file the user opens in a browser. Examples: anatomy-html, memory-graph-html. They make substrate state legible without requiring a running frontend."))));
            out.push_str(&section("Strategy hints from the card", fix.as_deref()));
            out.push_str(&section("Constraints", Some(
                "- ONE HTML file with inline CSS/SVG/JS. No CDN URLs, no asset folders, no `<script src=\"...\">` to external hosts.\n\
                 - Valid HTML: `<!DOCTYPE html>` at top; SVG or table elements present per the acceptance check.\n\
                 - Runs server-side (Node); reads from the 4 SQLite DBs; writes to the file path the action's caller specifies.\n\
                 - Handle empty source: render an empty-state placeholder, never crash on `SELECT ... LIMIT 0`."
            )));
            out.push_str(&section("Risk profile", Some(
                "- Large data: queries pulling thousands of rows produce 10-50MB HTML — slow to render in the browser. Cap rows, paginate, or aggregate.\n\
                 - Cross-DB queries: state/brain/memory/laws are separate DBs. Use ATTACH or explicit per-DB queries via openDb.\n\
                 - Color encoding: when using semantic colors (red=blocked, green=done), make the legend visible — colorblind-friendly palette preferred.\n\
                 - Streaming: the user may regenerate the HTML often. Render in <2s for the common case; otherwise add a \"(slow query — partial)\" banner."
            )));
            out.push_str(&section("Open questions for DEV judgment", Some(
                "- Single file or set (index.html + assets/)? Default: single file, always.\n\
                 - Snapshot of \"now\" or filterable date range? Default: snapshot; add a `since` input param only if the card explicitly mentions it.\n\
                 - Interactive vs static? Default: static SVG/HTML. Inline JS for filtering only if essential (no React, no frameworks)."
            )));
            out.push_str(&section("Patterns to emulate", Some("- `anatomy-html` — substrate inventory render.\n- `memory-graph-html` — typed-edge visualization with semantic colors.\n- `substrate-defense-html` — law-coverage overlay.")));
            out.push_str(&section("Substrate invariants you must respect", Some(&substrate_invariants())));
        }
        _ => {}
    }
    out.trim().to_string()
}

// ── RAG helpers — see module doc for the deferred dynamic-import boundary ──────────────────
fn rag_reference_artifacts(template: &Value, limit: usize) -> Value {
    let template_hints: Vec<Value> = template
        .get("reference_artifact_hints")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|name| json!({ "name": name, "score": 1.0, "source": "template" }))
        .collect();
    // fromRag: [] — catalog-find is reached via a deferred dynamic import (see module doc).
    let mut suggestions = template_hints;
    suggestions.truncate(limit);
    json!({ "suggestions": suggestions })
}

fn rag_relevant_soft_laws() -> Value {
    // law-surface-for-context is reached via a deferred dynamic import (see module doc).
    json!([])
}

fn past_rejection_hints(db: &Connection) -> Value {
    // The RAG-ranked branch (memory.catalog_embeddings + catalog-find) requires the deferred
    // dynamic import; this twin always takes the real lexical SQL fallback (see module doc).
    let rows = match crate::db_json::query_rows(
        db,
        "SELECT id, situation, guidance, naive_response, tags_json FROM memory.memories \
         WHERE type = 'correction' AND tags_json LIKE '%contract%' ORDER BY id DESC LIMIT 5",
        &[],
    ) {
        Ok(v) => v,
        Err(_) => return json!([]),
    };
    let arr = rows.as_array().cloned().unwrap_or_default();
    let out: Vec<Value> = arr
        .into_iter()
        .map(|r| {
            json!({
                "memory_id": r.get("id").cloned().unwrap_or(Value::Null),
                "guidance": r.get("guidance").cloned().unwrap_or(Value::Null),
                "situation": r.get("situation").cloned().unwrap_or(Value::Null),
                "source": "lexical-fallback",
            })
        })
        .collect();
    json!(out)
}

// ── default export: contractDraftAuto(input, context) ──────────────────────────────────────
pub fn contractDraftAuto(input: &Value, db: &Connection) -> Result<Value, String> {
    let empty = json!({});
    let input = if input.is_object() { input } else { &empty };
    let card_slug = input.get("card_slug").and_then(Value::as_str).unwrap_or("");
    if card_slug.is_empty() {
        return Err("contract-draft-auto: card_slug is required".to_string());
    }
    let explicit = input.get("template_kind").cloned().unwrap_or(Value::Null);
    let rag_limit = input.get("rag_limit").and_then(Value::as_u64).unwrap_or(3) as usize;

    // ── 1. Card ──
    let rows = crate::db_json::query_rows(db, "SELECT * FROM cards WHERE slug = ?", &[json!(card_slug)])?;
    let card = rows
        .as_array()
        .and_then(|a| a.first())
        .cloned()
        .ok_or_else(|| format!("contract-draft-auto: card '{card_slug}' not found"))?;

    // ── 2. Template detection ──
    let tpl_detect = detectTemplateKind(&card, db, &explicit);
    let kind_opt = tpl_detect.get("kind").and_then(Value::as_str).map(String::from);
    let template = match &kind_opt {
        Some(k) => crate::contract_contract_templates::getTemplate(&json!(k)),
        None => Value::Null,
    };

    // ── 3-4. RAG (deferred boundary; template-hint half is real) ──
    let refs = rag_reference_artifacts(&template, rag_limit);
    let soft_laws = rag_relevant_soft_laws();

    // ── 5. Template-derived fields, honoring card overrides ──
    // files_source mirrors the JS `filesSource` local (tracked for parity; not surfaced in the
    // returned contract shape, same as upstream — the JS variable is likewise write-only).
    let files_touched: Vec<String>;
    let from_body = extractFilesTouchedProposed(&card);
    if from_body.get("ok") == Some(&Value::Bool(true)) {
        files_touched = from_body["paths"].as_array().unwrap().iter().map(|v| str_qq(Some(v))).collect();
    } else if let Some(k) = &kind_opt {
        let (f, _src) = template_files_touched(k, &card);
        files_touched = f;
    } else {
        files_touched = vec!["<UNKNOWN_PATHS_TL_REVIEW>".to_string()];
    }

    let naming = match &kind_opt {
        Some(k) => template_naming(k, &card),
        None => json!({ "note": "no template — TL must provide naming explicitly" }),
    };
    let edge_cases = template.get("edge_cases_baseline").cloned().unwrap_or(json!([]));

    let acc_from_body = extractAcceptanceProposed(&card);
    let acceptance = if acc_from_body.get("ok") == Some(&Value::Bool(true)) {
        acc_from_body["acceptance"].clone()
    } else if let Some(k) = &kind_opt {
        template_acceptance(k, &naming, &files_touched)
    } else {
        json!([])
    };

    // ── 6. Past TL rejection hints ──
    let hints_from_past = past_rejection_hints(db);

    // ── 7. Classification ──
    let risk = classifyRisk(&card, &template);
    let files_val = json!(files_touched);
    let complexity = classifyComplexity(&files_val);
    let size = template.get("default_size").and_then(Value::as_str).unwrap_or("M").to_string();
    let tier = classifyTier(&json!(size), &risk);

    // ── 8. Must-obey ──
    const REQUIRED_CORE_HARD_LAWS: [&str; 2] = ["NEVER_BYPASS_WITH_AUDIT", "HISTORY_IS_SACRED_NO_REVERT"];
    let mut seen_law: HashSet<String> = HashSet::new();
    let mut must_obey: Vec<Value> = Vec::new();
    for law in REQUIRED_CORE_HARD_LAWS.iter().map(|s| s.to_string()) {
        if seen_law.insert(law.clone()) {
            must_obey.push(json!(law));
        }
    }
    if let Some(extra) = template.get("must_obey_extra").and_then(Value::as_array) {
        for law in extra {
            if let Some(s) = law.as_str() {
                if seen_law.insert(s.to_string()) {
                    must_obey.push(json!(s));
                }
            }
        }
    }
    for l in soft_laws.as_array().cloned().unwrap_or_default() {
        if let Some(s) = l.get("slug").and_then(Value::as_str) {
            if seen_law.insert(s.to_string()) {
                must_obey.push(json!(s));
            }
        }
    }

    // ── 9. Out-of-scope ──
    let mut out_of_scope: Vec<Value> =
        crate::contract_contract_templates::universalOutOfScopeList().as_array().cloned().unwrap_or_default();
    if let Some(extra) = template.get("out_of_scope_extra").and_then(Value::as_array) {
        out_of_scope.extend(extra.iter().cloned());
    }

    // Unresolved <TARGET_X> placeholders → refuse the draft.
    let placeholder_re = Regex::new(r"^<TARGET_[A-Z_]+>$").unwrap();
    let unresolved: Vec<&String> = files_touched.iter().filter(|p| placeholder_re.is_match(p)).collect();
    if !unresolved.is_empty() {
        let kind_str = kind_opt.clone().unwrap_or_default();
        let unresolved_joined = unresolved.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
        return Err(format!(
            "contract-draft-auto: template '{kind_str}' returned unresolved placeholders in files_touched ({unresolved_joined}). \
             The template's AST refinement step is not implemented yet. EPIC cards rarely fit a single template — split into children via pl.session.start."
        ));
    }

    // ── goal_md ──
    let goal_md = if let Some(k) = &kind_opt {
        template_goal_md(k, &card, &files_touched, &naming)
    } else {
        crate::contract_contract_templates::noTemplateGoalMd(&card).as_str().unwrap_or("").to_string()
    };

    // ── Assemble contract ──
    let card_title = card.get("title").cloned().unwrap_or(Value::Null);
    let contract_slug = format!("{card_slug}-CONTRACT-1");
    let body_json = card.get("body_json").and_then(Value::as_str).map(String::from);
    let briefing_md = body_json.as_ref().map(|b| {
        let snippet: String = b.chars().take(500).collect();
        let refs_lines = refs["suggestions"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|s| format!("- {} (score {:.2})", str_qq(s.get("name")), s.get("score").and_then(Value::as_f64).unwrap_or(0.0)))
            .collect::<Vec<_>>()
            .join("\n");
        format!("## Card\n{snippet}\n\n## Reference shape\n{refs_lines}")
    });
    let ref_names: Vec<Value> = refs["suggestions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|s| s.get("name").cloned().unwrap_or(Value::Null))
        .collect();

    let mut contract = Map::new();
    contract.insert("slug".into(), json!(contract_slug));
    contract.insert("title".into(), card_title);
    contract.insert("card_slug".into(), json!(card_slug));
    contract.insert("template_kind".into(), kind_opt.clone().map(Value::String).unwrap_or(Value::Null));
    contract.insert("size".into(), json!(size));
    contract.insert("risk".into(), risk.clone());
    contract.insert("complexity".into(), complexity.clone());
    contract.insert("size_weight".into(), json!(size_weight(&size)));
    contract.insert("tier".into(), tier);
    contract.insert("goal_md".into(), json!(goal_md));
    contract.insert("base_sha".into(), Value::Null);
    contract.insert("files_touched".into(), json!(files_touched));
    contract.insert("naming".into(), naming);
    contract.insert("acceptance_json".into(), acceptance.clone());
    contract.insert("out_of_scope".into(), json!(out_of_scope));
    contract.insert("must_obey".into(), json!(must_obey));
    contract.insert("edge_cases".into(), edge_cases);
    contract.insert("reference_artifacts".into(), json!(ref_names));
    contract.insert("briefing_md".into(), briefing_md.map(Value::String).unwrap_or(Value::Null));
    contract.insert("current_attempt".into(), json!(1));
    let templ_conf = if !template.is_null() { 0.9 } else { 0.2 };
    let naming_conf = if !template.is_null() { 0.9 } else { 0.1 };
    let edge_conf = if !template.is_null() { 0.7 } else { 0.0 };
    let acc_conf = if !template.is_null() { 0.8 } else { 0.0 };
    let ref_conf = if !ref_names.is_empty() { 0.85 } else { 0.3 };
    contract.insert(
        "confidence".into(),
        json!({
            "template_kind": tpl_detect.get("confidence").cloned().unwrap_or(json!(0.0)),
            "files_touched": templ_conf,
            "naming": naming_conf,
            "edge_cases": edge_conf,
            "acceptance": acc_conf,
            "reference_artifacts": ref_conf,
            "must_obey": 0.9,
        }),
    );
    let mut contract = Value::Object(contract);

    // ── Persist (opt-out via dry_run) ──
    let dry_run = input.get("dry_run").map(truthy_val).unwrap_or(false);
    let mut persisted_id: Value = Value::Null;
    if !dry_run {
        let existing = crate::db_json::query_rows(
            db,
            "SELECT id, slug FROM contracts WHERE card_slug = ? AND COALESCE(superseded_by, '') = '' ORDER BY id DESC LIMIT 1",
            &[json!(card_slug)],
        )?;
        if let Some(row) = existing.as_array().and_then(|a| a.first()) {
            persisted_id = row.get("id").cloned().unwrap_or(Value::Null);
            if let Some(obj) = contract.as_object_mut() {
                obj.insert("slug".into(), row.get("slug").cloned().unwrap_or(Value::Null));
            }
        } else {
            let pragma = crate::db_json::query_rows(db, "PRAGMA table_info(contracts)", &[])?;
            let col_set: HashSet<String> = pragma
                .as_array()
                .map(|a| a.iter().filter_map(|c| c.get("name").and_then(Value::as_str).map(String::from)).collect())
                .unwrap_or_default();
            let has_goal_md = col_set.contains("goal_md");

            let mut cols: Vec<&str> = vec![
                "slug", "title", "status", "template_kind", "size", "complexity", "risk", "tier",
                "card_slug", "files_touched_json", "acceptance_json", "naming_json",
                "edge_cases_json", "must_obey_json", "out_of_scope_json",
                "reference_artifacts_json", "briefing_md", "current_attempt",
            ];
            let mut args: Vec<Value> = vec![
                contract["slug"].clone(),
                contract["title"].clone(),
                json!("proposed"),
                contract["template_kind"].clone(),
                contract["size"].clone(),
                contract["complexity"].clone(),
                contract["risk"].clone(),
                contract["tier"].clone(),
                contract["card_slug"].clone(),
                Value::String(serde_json::to_string(&contract["files_touched"]).unwrap_or_default()),
                Value::String(serde_json::to_string(&contract["acceptance_json"]).unwrap_or_default()),
                Value::String(serde_json::to_string(&contract["naming"]).unwrap_or_default()),
                Value::String(serde_json::to_string(&contract["edge_cases"]).unwrap_or_default()),
                Value::String(serde_json::to_string(&contract["must_obey"]).unwrap_or_default()),
                Value::String(serde_json::to_string(&contract["out_of_scope"]).unwrap_or_default()),
                Value::String(serde_json::to_string(&contract["reference_artifacts"]).unwrap_or_default()),
                contract["briefing_md"].clone(),
                contract["current_attempt"].clone(),
            ];
            if has_goal_md {
                cols.push("goal_md");
                args.push(contract["goal_md"].clone());
            }
            let placeholders = vec!["?"; cols.len()].join(", ");
            let sql = format!("INSERT INTO contracts ({}) VALUES ({})", cols.join(", "), placeholders);
            let (_, rowid) = crate::db_json::run_write(db, &sql, &args)?;
            persisted_id = json!(rowid);
        }
    }

    Ok(json!({
        "contract": contract,
        "persisted_id": persisted_id,
        "persisted": !persisted_id.is_null(),
        "sources": {
            "template_detected": tpl_detect,
            "rag_references": refs,
            "rag_soft_laws": soft_laws,
            "past_rejection_hints": hints_from_past,
        },
    }))
}

fn truthy_val(v: &Value) -> bool {
    truthy(Some(v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn with_substrate_root<T>(root: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", root);
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-contract-draft-auto-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, body_json TEXT, priority INTEGER, status TEXT); \
             CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL UNIQUE, title TEXT, \
               status TEXT NOT NULL DEFAULT 'proposed', template_kind TEXT, size TEXT, complexity INTEGER, risk INTEGER, \
               tier TEXT, card_slug TEXT, files_touched_json TEXT, acceptance_json TEXT, naming_json TEXT, \
               edge_cases_json TEXT, must_obey_json TEXT, out_of_scope_json TEXT, reference_artifacts_json TEXT, \
               briefing_md TEXT, current_attempt INTEGER, goal_md TEXT, superseded_by TEXT); \
             ATTACH DATABASE ':memory:' AS memory; \
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, type TEXT, situation TEXT, guidance TEXT, tags_json TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn classify_tier_boundaries() {
        assert_eq!(classifyTier(&json!("XS"), &json!(1)), json!("trivial"));
        assert_eq!(classifyTier(&json!("XS"), &json!(3)), json!("standard"));
        assert_eq!(classifyTier(&json!("L"), &json!(1)), json!("high-risk"));
        assert_eq!(classifyTier(&json!("M"), &json!(4)), json!("high-risk"));
        assert_eq!(classifyTier(&json!("M"), &json!(2)), json!("standard"));
    }

    #[test]
    fn classify_risk_bumps_on_sensitive_body_and_priority() {
        let card = json!({ "body_json": "touches auth flow", "priority": 1 });
        let r = classifyRisk(&card, &Value::Null); // default_risk 2
        assert_eq!(r, json!(4)); // +1 sensitive, +1 priority, capped at 5 but 2+1+1=4
        let card2 = json!({ "body_json": "plain change" });
        assert_eq!(classifyRisk(&card2, &json!({ "default_risk": 5 })), json!(5)); // capped
    }

    #[test]
    fn classify_complexity_buckets() {
        assert_eq!(classifyComplexity(&json!([])), json!(1));
        assert_eq!(classifyComplexity(&json!(["a", "b", "c"])), json!(2));
        assert_eq!(classifyComplexity(&json!(["a", "b", "c", "d", "e", "f", "g"])), json!(4));
    }

    #[test]
    fn extract_files_touched_proposed_rejects_placeholders_and_globs() {
        let card = json!({ "body_json": "{\"files_touched_proposed\":[\"<FOO>\"]}" });
        assert_eq!(extractFilesTouchedProposed(&card)["ok"], json!(false));
        let card2 = json!({ "body_json": "{\"files_touched_proposed\":[\"a/*.js\"]}" });
        assert_eq!(extractFilesTouchedProposed(&card2)["ok"], json!(false));
        let card3 = json!({ "body_json": "{\"files_touched_proposed\":[\"a/b.mjs\"]}" });
        let r3 = extractFilesTouchedProposed(&card3);
        assert_eq!(r3["ok"], json!(true));
        assert_eq!(r3["paths"], json!(["a/b.mjs"]));
    }

    #[test]
    fn extract_acceptance_proposed_rejects_unknown_kind() {
        let card = json!({ "body_json": "{\"acceptance\":[{\"kind\":\"bogus\"}]}" });
        assert_eq!(extractAcceptanceProposed(&card)["ok"], json!(false));
        let card2 = json!({ "body_json": "{\"acceptance\":[{\"kind\":\"sql\"}, \"legacy string\"]}" });
        let r2 = extractAcceptanceProposed(&card2);
        assert_eq!(r2["ok"], json!(true));
        assert_eq!(r2["acceptance"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn detect_template_kind_prefers_explicit_then_file_path_then_keyword() {
        let c = Connection::open_in_memory().unwrap();
        let explicit = detectTemplateKind(&json!({}), &c, &json!("migration-add"));
        assert_eq!(explicit["source"], json!("explicit"));

        let by_path = detectTemplateKind(
            &json!({ "title": "add hook", "body_json": "edit antiloki-core/skills/audit/law-hook-foo.mjs" }),
            &c,
            &Value::Null,
        );
        assert_eq!(by_path["kind"], json!("law-hook-add"));
        assert_eq!(by_path["source"], json!("file-path-mention"));

        let by_kw = detectTemplateKind(&json!({ "title": "new migration for schema", "body_json": "" }), &c, &Value::Null);
        assert_eq!(by_kw["kind"], json!("migration-add"));

        let none = detectTemplateKind(&json!({ "title": "just a random thing", "body_json": "" }), &c, &Value::Null);
        assert_eq!(none["kind"], Value::Null);
        assert_eq!(none["source"], json!("no-template"));
    }

    #[test]
    fn law_hook_files_touched_pattern_is_deterministic() {
        let card = json!({ "slug": "ADD-FOO-HOOK" });
        let (files, src) = template_files_touched("law-hook-add", &card);
        assert_eq!(files[0], "antiloki-core/skills/audit/law-hook-add-foo-hook.mjs");
        assert!(src.contains("files_touched_pattern"));
    }

    #[test]
    fn skill_modify_resolve_files_finds_a_real_file_under_substrate_root() {
        let root = temp_dir("skill-modify");
        let skills_dir = root.join("antiloki-core/skills/system");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::write(skills_dir.join("card-add.mjs"), "export default function cardAdd() {}").unwrap();

        let card = json!({ "slug": "FIX-CARD-ADD", "title": "Fix card-add validation", "body_json": "" });
        let (files, src) = with_substrate_root(root.to_str().unwrap(), || template_files_touched("skill-modify", &card));
        assert_eq!(files, vec!["antiloki-core/skills/system/card-add.mjs", "antiloki-core/skills/system/card-add.test.mjs"]);
        assert!(src.contains("resolveFiles"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn skill_modify_falls_back_to_placeholder_when_unresolvable() {
        let root = temp_dir("skill-modify-miss");
        fs::create_dir_all(root.join("antiloki-core/skills")).unwrap();
        let card = json!({ "slug": "SOME-CARD", "title": "totally unrelated", "body_json": "" });
        let (files, _src) = with_substrate_root(root.to_str().unwrap(), || template_files_touched("skill-modify", &card));
        assert_eq!(files, vec!["<TARGET_SKILL_PATH>", "<TARGET_SKILL_TEST_PATH>"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn contract_draft_auto_missing_card_errs() {
        let c = db();
        let e = contractDraftAuto(&json!({ "card_slug": "NOPE" }), &c).unwrap_err();
        assert_eq!(e, "contract-draft-auto: card 'NOPE' not found");
    }

    #[test]
    fn contract_draft_auto_no_template_produces_stub_and_persists() {
        let c = db();
        c.execute(
            "INSERT INTO cards (slug, title, body_json) VALUES ('CARD-1', 'Do a novel thing', NULL)",
            [],
        )
        .unwrap();
        let r = contractDraftAuto(&json!({ "card_slug": "CARD-1" }), &c).unwrap();
        assert_eq!(r["contract"]["template_kind"], Value::Null);
        assert_eq!(r["contract"]["files_touched"], json!(["<UNKNOWN_PATHS_TL_REVIEW>"]));
        assert_eq!(r["contract"]["size"], json!("M"));
        assert_eq!(r["persisted"], json!(true));
        assert!(r["contract"]["goal_md"].as_str().unwrap().contains("## Mission"));

        let count: i64 = c.query_row("SELECT COUNT(*) FROM contracts WHERE card_slug='CARD-1'", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn contract_draft_auto_is_idempotent_on_rerun() {
        let c = db();
        c.execute("INSERT INTO cards (slug, title) VALUES ('CARD-2', 'Add migration for foo')", []).unwrap();
        let r1 = contractDraftAuto(&json!({ "card_slug": "CARD-2", "template_kind": "migration-add" }), &c).unwrap();
        let r2 = contractDraftAuto(&json!({ "card_slug": "CARD-2", "template_kind": "migration-add" }), &c).unwrap();
        assert_eq!(r1["contract"]["slug"], r2["contract"]["slug"]);
        assert_eq!(r1["persisted_id"], r2["persisted_id"]);
        let count: i64 = c.query_row("SELECT COUNT(*) FROM contracts WHERE card_slug='CARD-2'", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn contract_draft_auto_dry_run_does_not_persist() {
        let c = db();
        c.execute("INSERT INTO cards (slug, title) VALUES ('CARD-3', 'x')", []).unwrap();
        let r = contractDraftAuto(&json!({ "card_slug": "CARD-3", "dry_run": true }), &c).unwrap();
        assert_eq!(r["persisted"], json!(false));
        assert!(r["persisted_id"].is_null());
        let count: i64 = c.query_row("SELECT COUNT(*) FROM contracts", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn contract_draft_auto_honors_files_touched_proposed_override() {
        let c = db();
        c.execute(
            "INSERT INTO cards (slug, title, body_json) VALUES ('CARD-4', 'x', '{\"files_touched_proposed\":[\"a/b.mjs\"]}')",
            [],
        )
        .unwrap();
        let r = contractDraftAuto(&json!({ "card_slug": "CARD-4" }), &c).unwrap();
        assert_eq!(r["contract"]["files_touched"], json!(["a/b.mjs"]));
    }

    #[test]
    fn contract_draft_auto_law_hook_template_full_pipeline() {
        let c = db();
        c.execute("INSERT INTO cards (slug, title, priority) VALUES ('ADD-FOO-HOOK', 'Add a law hook for foo', 2)", []).unwrap();
        let r = contractDraftAuto(&json!({ "card_slug": "ADD-FOO-HOOK", "template_kind": "law-hook-add" }), &c).unwrap();
        assert_eq!(r["contract"]["template_kind"], json!("law-hook-add"));
        assert_eq!(r["contract"]["size"], json!("S"));
        assert_eq!(r["contract"]["risk"], json!(4)); // default_risk 3 + priority bump
        assert_eq!(r["contract"]["files_touched"][0], json!("antiloki-core/skills/audit/law-hook-add-foo-hook.mjs"));
        let must_obey = r["contract"]["must_obey"].as_array().unwrap();
        assert!(must_obey.iter().any(|v| v == "NEVER_BYPASS_WITH_AUDIT"));
        assert!(must_obey.iter().any(|v| v == "CLAIM_REQUIRES_VERIFICATION"));
        assert!(r["contract"]["goal_md"].as_str().unwrap().contains("Add a new law-hook"));
    }
}
