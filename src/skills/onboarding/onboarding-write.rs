//! onboarding-write.rs — twin of skills/onboarding/onboarding-write.mjs (db WRITE lane, attached
//! `memory` + `knowledge` dbs). NAMES ARE CONTRACT: `onboardingWrite` (default). Persists onboarding
//! scan + inference into memory.memories (preference/correction/reference rows) and knowledge.documents
//! (architecture + stack markdown snapshots). Each write is best-effort — a failed knowledge/memory
//! INSERT (constraint violation, wrong table shape, …) is swallowed by the JS try/catch and simply
//! doesn't increment the counter; it never aborts the whole call. Guard-throws (missing scan/inference)
//! are transcribed but — per playbook rule 9 — unfixtured; only the success path is conformed.
// QUALITY: n/a — straight transcription: three memory-write loops + two best-effort knowledge writes
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v:&Value)->String{ match v{Value::Null=>"null".into(),Value::String(s)=>s.clone(),Value::Number(n)=>n.to_string(),Value::Bool(b)=>b.to_string(),Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","),Value::Object(_)=>"[object Object]".into()} } // = JS String(v)
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` left-operand filter

// Array.prototype.join semantics: null/undefined elements render as '', everything else as String(x).
fn arr_join(arr: &[Value], sep: &str) -> String {
    arr.iter().map(|v| if v.is_null() { String::new() } else { js_string(v) }).collect::<Vec<_>>().join(sep)
}

fn last_sep(p: &str) -> Option<usize> { p.rfind(|c| c == '/' || c == '\\') }
// node:path basename(p) (no ext arg) — win32 impl treats BOTH separators as splits.
fn basename(p: &str) -> String {
    let trimmed = p.trim_end_matches(|c| c == '/' || c == '\\');
    match last_sep(trimmed) { Some(i) => trimmed[i + 1..].to_string(), None => trimmed.to_string() }
}

fn safe_insert_memory(
    db: &Connection, kind: &str, title: &str, situation: &str, guidance: &str,
    naive_response: Option<&str>, tags: &Value, source_card: &str, created_by: &str, confidence: f64,
) -> bool {
    let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".into());
    crate::db_json::run_write(
        db,
        "INSERT INTO memory.memories
      (type, title, situation, guidance, naive_response, tags_json, source_card, created_by, validated, confidence)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0, ?)",
        &[
            json!(kind), json!(title), json!(situation), json!(guidance),
            naive_response.map(|s| json!(s)).unwrap_or(Value::Null),
            json!(tags_json), json!(source_card), json!(created_by), json!(confidence),
        ],
    ).is_ok()
}

fn safe_insert_knowledge(db: &Connection, title: &str, content_md: &str) -> bool {
    crate::db_json::run_write(
        db,
        "INSERT INTO knowledge.documents (title, content_md) VALUES (?, ?)",
        &[json!(title), json!(content_md)],
    ).is_ok()
}

pub fn onboardingWrite(input: &Value, db: &Connection) -> Result<Value, String> {
    let scan = nz(input.get("scan")).ok_or_else(|| "onboarding-write: scan is required".to_string())?;
    let inference = nz(input.get("inference")).ok_or_else(|| "onboarding-write: inference is required".to_string())?;

    // `basename(scan.repo_path ?? 'unknown')` drives `source`. The two knowledge-doc titles call
    // `basename(scan.repo_path)` WITHOUT the `?? 'unknown'` fallback (a real JS edge case: an
    // absent repo_path would throw there) — unreachable on the success path this twin fixtures
    // (repo_path is always a supplied string), so one `base` value covers all three call sites.
    let repo_path_val = nz(scan.get("repo_path")).cloned().unwrap_or(json!("unknown"));
    let repo_path = repo_path_val.as_str().unwrap_or("unknown").to_string();
    let base = basename(&repo_path);
    let source = format!("onboarding:{base}");
    let repo_path_tmpl = tmpl(scan.get("repo_path")); // `${scan.repo_path}` (no ?? fallback) for situation text

    let mut memories_written: i64 = 0;
    let mut knowledge_written: i64 = 0;

    // Patterns → preference memories
    let patterns = inference.get("patterns").and_then(|p| p.get("patterns")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    for p in &patterns {
        let kind = tmpl(p.get("kind"));
        let value = tmpl(p.get("value"));
        let confidence = nz(p.get("confidence")).and_then(|v| v.as_f64()).unwrap_or(0.7);
        let tags = json!(["onboarding", "convention", p.get("kind").cloned().unwrap_or(Value::Null)]);
        let ok = safe_insert_memory(
            db, "preference",
            &format!("Convention: {kind} = {value}"),
            &format!("Detected during onboarding scan of {repo_path_tmpl}"),
            &format!("Match the {kind} convention ({value}) when adding new files"),
            None, &tags, &source, "onboarding:auto", confidence,
        );
        if ok { memories_written += 1; }
    }

    // Anti-patterns → correction memories
    let anti_patterns = inference.get("patterns").and_then(|p| p.get("anti_patterns")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    for a in &anti_patterns {
        let kind = tmpl(a.get("kind"));
        let tags = json!(["onboarding", "anti-pattern", a.get("kind").cloned().unwrap_or(Value::Null)]);
        let ok = safe_insert_memory(
            db, "correction",
            &format!("Anti-pattern: {kind}"),
            &format!("Detected during onboarding scan of {repo_path_tmpl}"),
            &format!("Investigate and decide whether {kind} is intentional or worth filing a card to address"),
            Some(&format!("Assumed {kind} was intentional and proceeded without addressing it")),
            &tags, &source, "onboarding:auto", 0.6,
        );
        if ok { memories_written += 1; }
    }

    // Domain entities → reference memories (top 5)
    let entities = inference.get("domain").and_then(|d| d.get("entities")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    for e in entities.iter().take(5) {
        let term = tmpl(e.get("term"));
        let tags = json!(["onboarding", "domain"]);
        let ok = safe_insert_memory(
            db, "reference",
            &format!("Domain term: {term}"),
            "Discovered in commit subjects + doc titles during onboarding",
            &format!("'{term}' is a recurring concept in this codebase — preserve when refactoring"),
            None, &tags, &source, "onboarding:auto", 0.5,
        );
        if ok { memories_written += 1; }
    }

    // Architecture snapshot → knowledge doc (best-effort)
    let architecture = inference.get("architecture");
    let layers: Vec<Value> = architecture.and_then(|a| a.get("layers")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if !layers.is_empty() {
        let monorepo = if truthy(architecture.and_then(|a| a.get("is_monorepo"))) { "yes" } else { "no" };
        let layer_count = tmpl(architecture.and_then(|a| a.get("layer_count")));
        let mut lines = vec![
            format!("# Architecture snapshot — {base}"),
            "Generated by onboarding scan.".to_string(),
            String::new(),
            format!("**Monorepo:** {monorepo}"),
            format!("**Layers detected:** {layer_count}"),
            String::new(),
        ];
        for l in &layers {
            lines.push(format!("- `{}` → {}", tmpl(l.get("name")), tmpl(l.get("kind"))));
        }
        let md = lines.join("\n");
        if safe_insert_knowledge(db, &format!("Architecture: {base}"), &md) { knowledge_written += 1; }
    }

    // Stack snapshot → knowledge doc
    let stack = scan.get("stack");
    let languages: Vec<Value> = stack.and_then(|s| s.get("languages")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if !languages.is_empty() {
        let ecosystems: Vec<Value> = stack.and_then(|s| s.get("ecosystems")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let markers: Vec<Value> = stack.and_then(|s| s.get("markers")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let mut lines = vec![
            format!("# Tech stack — {base}"),
            "Generated by onboarding scan.".to_string(),
            String::new(),
            format!("**Languages:** {}", arr_join(&languages, ", ")),
            format!("**Ecosystems:** {}", arr_join(&ecosystems, ", ")),
            String::new(),
            "## Markers detected".to_string(),
        ];
        for m in &markers {
            let lang = nz(m.get("lang")).cloned().unwrap_or(json!("?"));
            let eco_suffix = if truthy(m.get("ecosystem")) { format!(" ({})", tmpl(m.get("ecosystem"))) } else { String::new() };
            lines.push(format!("- `{}` → {}{}", tmpl(m.get("path")), tmpl(Some(&lang)), eco_suffix));
        }
        let md = lines.join("\n");
        if safe_insert_knowledge(db, &format!("Stack: {base}"), &md) { knowledge_written += 1; }
    }

    let mut out = Map::new();
    out.insert("source".into(), json!(source));
    out.insert("memories_written".into(), json!(memories_written));
    out.insert("knowledge_written".into(), json!(knowledge_written));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory; ATTACH ':memory:' AS knowledge;
            CREATE TABLE memory.memories (id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT NOT NULL, title TEXT NOT NULL, situation TEXT NOT NULL, guidance TEXT NOT NULL, naive_response TEXT, tags_json TEXT NOT NULL DEFAULT '[]', source_card TEXT, created_by TEXT NOT NULL, validated INTEGER NOT NULL DEFAULT 0, confidence REAL NOT NULL DEFAULT 0.5);
            CREATE TABLE knowledge.documents (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL CHECK (length(title) <= 40), content_md TEXT NOT NULL);",
        ).unwrap();
        c
    }

    #[test]
    fn writes_memories_and_knowledge_docs() {
        let c = db();
        let input = json!({
            "scan": { "repo_path": "C:/dev/shops/checkout-service", "stack": { "languages": ["JavaScript"], "ecosystems": ["npm"], "markers": [{ "path": "package.json", "lang": "javascript", "ecosystem": "npm" }] } },
            "inference": {
                "patterns": { "patterns": [{ "kind": "naming", "value": "kebab-case", "confidence": 0.8 }], "anti_patterns": [{ "kind": "no_tests_found" }] },
                "domain": { "entities": [{ "term": "payment" }, { "term": "refund" }, { "term": "invoice" }, { "term": "ledger" }, { "term": "account" }, { "term": "wallet" }] },
                "architecture": { "is_monorepo": true, "layer_count": 1, "layers": [{ "name": "backend", "kind": "backend" }] },
            },
        });
        let r = onboardingWrite(&input, &c).unwrap();
        assert_eq!(r["source"], json!("onboarding:checkout-service"));
        // 1 pattern + 1 anti-pattern + 5 (of 6) domain entities (top-5 slice)
        assert_eq!(r["memories_written"], json!(7));
        assert_eq!(r["knowledge_written"], json!(2));
        let mem_count = crate::db_json::query_rows(&c, "SELECT COUNT(*) AS n FROM memory.memories", &[]).unwrap()[0]["n"].clone();
        assert_eq!(mem_count, json!(7));
        let doc_titles = crate::db_json::query_rows(&c, "SELECT title FROM knowledge.documents ORDER BY id", &[]).unwrap();
        assert_eq!(doc_titles[0]["title"], json!("Architecture: checkout-service"));
        assert_eq!(doc_titles[1]["title"], json!("Stack: checkout-service"));
    }

    #[test]
    fn empty_inference_writes_nothing_and_falls_back_to_unknown_repo() {
        let c = db();
        let r = onboardingWrite(&json!({ "scan": {}, "inference": {} }), &c).unwrap();
        assert_eq!(r, json!({ "source": "onboarding:unknown", "memories_written": 0, "knowledge_written": 0 }));
    }

    #[test]
    fn knowledge_insert_failure_is_swallowed_and_does_not_increment() {
        let c = db();
        // basenames long enough to violate the title CHECK (<=40 chars) on BOTH docs.
        let long = "this-is-a-very-long-repository-directory-name-for-onboarding-write-testing";
        let input = json!({
            "scan": { "repo_path": long, "stack": { "languages": ["Go"], "ecosystems": [], "markers": [] } },
            "inference": {
                "patterns": { "patterns": [], "anti_patterns": [] },
                "domain": { "entities": [] },
                "architecture": { "is_monorepo": false, "layer_count": 1, "layers": [{ "name": "src", "kind": "shared" }] },
            },
        });
        let r = onboardingWrite(&input, &c).unwrap();
        assert_eq!(r["knowledge_written"], json!(0));
        assert_eq!(r["memories_written"], json!(0));
        let n = crate::db_json::query_rows(&c, "SELECT COUNT(*) AS n FROM knowledge.documents", &[]).unwrap()[0]["n"].clone();
        assert_eq!(n, json!(0));
    }

    #[test]
    fn missing_scan_or_inference_errs() {
        let c = db();
        assert!(onboardingWrite(&json!({ "inference": {} }), &c).is_err());
        assert!(onboardingWrite(&json!({ "scan": {} }), &c).is_err());
    }
}
