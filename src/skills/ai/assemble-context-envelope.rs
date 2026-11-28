//! assemble-context-envelope.rs — twin of skills/ai/assemble-context-envelope.mjs (db READ lane,
//! cross-db ATTACH). NAMES ARE CONTRACT: `assembleContextEnvelope` (default). Builds the canonical
//! "interaction envelope" for an AI call: user/system inputs PLUS operator-supplied references
//! (cards, knowledge docs, allowlisted db rows, workspace code files, live substrate sources) and
//! attachments (knowledge.documents), all resolved to grounding markdown blocks.
//!
//! Read-only: every resolver is a SELECT (code_file: a bounded workspace file read via the
//! already-converted `codebase_codebase_file_read` twin; source: the same live gather the
//! cockpit/studio source chips preview, via the already-converted `cockpit__helpers_context_brief`
//! twin). No mutation, so no with-audit.
// QUALITY: 80/100 — five reference resolvers dispatched by ref_type, an allowlist-gated db_row
// lookup, cross-module reuse of two prior twins (code_file + source), success-path guard-throws
// left as Err (not fixture-exercised per playbook gotcha 9).

use rusqlite::Connection;
use serde_json::{json, Map, Value};

const ENVELOPE_VERSION: i64 = 1;
const MAX_RENDER_CHARS: usize = 4000;

// mirrors JS SOURCE_KINDS (skills/cockpit/cockpit-context-preview.mjs) — assemble-context-envelope
// shares the same allowlist so a chat turn can ground on the same live sources the cockpit/studio
// chips offer.
const SOURCE_KINDS: [&str; 21] = [
    "ast", "descriptions", "digest", "entrypoints", "data_stories", "design", "tests", "git", "docs",
    "cards", "contracts", "failures", "court", "decisions", "timeline", "runtime",
    "memory", "blueprint", "user_stories",
    "faculty_arch", "faculty_product",
];

// ── standard JS-coercion helpers (playbook) ──
fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn js_string(v:&Value)->String{ match v{Value::Null=>"null".into(),Value::String(s)=>s.clone(),Value::Number(n)=>n.to_string(),Value::Bool(b)=>b.to_string(),Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","),Value::Object(_)=>"[object Object]".into()} }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

fn clamp(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() > MAX_RENDER_CHARS {
        let head: String = chars.into_iter().take(MAX_RENDER_CHARS).collect();
        format!("{head}\n\u{2026}(truncated)")
    } else {
        s.to_string()
    }
}

fn render_body_json(body_json: Option<&Value>) -> String {
    let bj = match nz(body_json) { None => return String::new(), Some(v) => v };
    if !truthy(Some(bj)) { return String::new(); }
    let body: Value = match bj {
        Value::String(s) => match serde_json::from_str::<Value>(s) { Ok(v) => v, Err(_) => return str_qq(Some(bj)) },
        other => other.clone(),
    };
    serde_json::to_string_pretty(&body).unwrap_or_else(|_| str_qq(Some(&body)))
}

fn q(db: &Connection, sql: &str, params: &[Value]) -> Vec<Value> {
    crate::db_json::query_rows(db, sql, params).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default()
}

fn resolve_card(db: &Connection, ref_: &Value) -> Value {
    let slug = ref_.get("id").cloned().unwrap_or(Value::Null);
    let rows = q(db, "SELECT slug, title, status, body_json FROM cards WHERE slug = ?", &[slug.clone()]);
    match rows.first() {
        None => json!({
            "ref_type": "card", "id": slug, "title": Value::Null, "source": "state.cards",
            "rendered_md": format!("### [card] {}\n(not found)", tmpl(Some(&slug))),
        }),
        Some(row) => {
            let rendered_md = clamp(&format!(
                "### [card] {} \u{2014} {} ({})\n```json\n{}\n```",
                tmpl(row.get("slug")), tmpl(row.get("title")), tmpl(row.get("status")), render_body_json(row.get("body_json")),
            ));
            json!({
                "ref_type": "card", "id": row.get("slug").cloned().unwrap_or(Value::Null),
                "title": row.get("title").cloned().unwrap_or(Value::Null),
                "status": row.get("status").cloned().unwrap_or(Value::Null),
                "source": "state.cards", "rendered_md": rendered_md,
            })
        }
    }
}

fn resolve_knowledge(db: &Connection, ref_: &Value) -> Value {
    let id = ref_.get("id").cloned().unwrap_or(Value::Null);
    let docs = q(db, "SELECT id, vault_path, doc_type, summary FROM knowledge.documents WHERE id = ?", &[id.clone()]);
    let doc = match docs.first() {
        Some(d) => d,
        None => return json!({
            "ref_type": "knowledge", "id": id, "title": Value::Null, "source": "knowledge.documents",
            "rendered_md": format!("### [knowledge] doc {}\n(not found)", tmpl(Some(&id))),
        }),
    };
    let extracts = q(db, "SELECT extract_type, text FROM knowledge.extracts WHERE document_id = ? ORDER BY id ASC LIMIT 5", &[id.clone()]);
    let extracts_md = extracts.iter()
        .map(|e| format!("- ({}) {}", tmpl(e.get("extract_type")), js_string(e.get("text").unwrap_or(&Value::Null)).chars().take(600).collect::<String>()))
        .collect::<Vec<_>>().join("\n");
    let title = doc.get("vault_path").cloned().unwrap_or(Value::Null);
    let summary_part = match nz(doc.get("summary")) { Some(s) => tmpl(Some(s)), None => "(no summary)".to_string() };
    let mut body = format!("### [knowledge] {} ({})\n{}", tmpl(Some(&title)), tmpl(doc.get("doc_type")), summary_part);
    if !extracts_md.is_empty() { body.push_str(&format!("\n\nExtracts:\n{extracts_md}")); }
    json!({
        "ref_type": "knowledge", "id": doc.get("id").cloned().unwrap_or(Value::Null), "title": title,
        "doc_type": doc.get("doc_type").cloned().unwrap_or(Value::Null),
        "source": "knowledge.documents", "rendered_md": clamp(&body),
    })
}

struct RowSpec { db: &'static str, table: &'static str, pk: &'static str, cols: &'static [&'static str] }

fn db_row_allowlist(table: &str) -> Option<RowSpec> {
    match table {
        "cards" => Some(RowSpec { db: "main", table: "cards", pk: "slug", cols: &["slug", "title", "status", "body_json"] }),
        "contracts" => Some(RowSpec { db: "main", table: "contracts", pk: "slug", cols: &["slug", "title", "status", "briefing_md"] }),
        "memories" => Some(RowSpec { db: "memory", table: "memories", pk: "id", cols: &["id", "type", "title", "situation", "guidance"] }),
        "ai_calls" => Some(RowSpec { db: "brain", table: "ai_calls", pk: "id", cols: &["id", "prompt_template_id", "outcome", "model_id", "cost_usd_actual"] }),
        "failures" => Some(RowSpec { db: "memory", table: "error_events", pk: "id", cols: &["id", "ts", "error_code", "error_message", "action_name", "skill_name", "pattern_key", "recovery_outcome"] }),
        _ => None,
    }
}

fn resolve_db_row(db: &Connection, ref_: &Value) -> Result<Value, String> {
    let table = str_qq(ref_.get("table"));
    let spec = match db_row_allowlist(&table) {
        Some(s) => s,
        None => return Err(format!(
            "assemble-context-envelope: references[].table must be one of [cards, contracts, memories, ai_calls, failures], got '{table}'"
        )),
    };
    let qualified = if spec.db == "main" { spec.table.to_string() } else { format!("{}.{}", spec.db, spec.table) };
    let col_list = spec.cols.join(", ");
    let sql = format!("SELECT {col_list} FROM {qualified} WHERE {} = ?", spec.pk);
    let id = ref_.get("id").cloned().unwrap_or(Value::Null);
    let rows = q(db, &sql, &[id.clone()]);
    let source = format!("{}.{}", spec.db, spec.table);
    match rows.first() {
        None => Ok(json!({
            "ref_type": "db_row", "table": table, "id": id, "source": source,
            "rendered_md": format!("### [{table}] {}\n(not found)", tmpl(Some(&id))),
        })),
        Some(row) => {
            let fields_md = spec.cols.iter()
                .map(|c| format!("- **{c}**: {}", str_qq(row.get(*c)).chars().take(600).collect::<String>()))
                .collect::<Vec<_>>().join("\n");
            Ok(json!({
                "ref_type": "db_row", "table": table, "id": id, "source": source,
                "rendered_md": clamp(&format!("### [{table}] {}\n{fields_md}", tmpl(Some(&id)))),
            }))
        }
    }
}

fn resolve_attachment(db: &Connection, att: &Value) -> Value {
    let id = nz(att.get("document_id")).or_else(|| nz(att.get("id"))).cloned().unwrap_or(Value::Null);
    let docs = q(db, "SELECT id, vault_path, doc_type, summary, bytes FROM knowledge.documents WHERE id = ?", &[id.clone()]);
    let doc = match docs.first() {
        Some(d) => d,
        None => return json!({
            "ref_type": "attachment", "document_id": id, "title": Value::Null, "source": "knowledge.documents",
            "rendered_md": format!("### [attachment] doc {}\n(not found)", tmpl(Some(&id))),
        }),
    };
    let summary_part = match nz(doc.get("summary")) { Some(s) => tmpl(Some(s)), None => "(no summary)".to_string() };
    let rendered_md = clamp(&format!(
        "### [attachment] {} ({}, {} bytes)\n{}",
        tmpl(doc.get("vault_path")), tmpl(doc.get("doc_type")), tmpl(doc.get("bytes")), summary_part,
    ));
    json!({
        "ref_type": "attachment", "document_id": doc.get("id").cloned().unwrap_or(Value::Null),
        "doc_type": doc.get("doc_type").cloned().unwrap_or(Value::Null),
        "title": doc.get("vault_path").cloned().unwrap_or(Value::Null),
        "vault_path": doc.get("vault_path").cloned().unwrap_or(Value::Null),
        "source": "knowledge.documents", "rendered_md": rendered_md,
    })
}

// code_file: the chat context wizard's "ground this turn in a source file" pick, delegated
// verbatim to the already-converted `codebase-file-read` twin (path safety lives there).
fn resolve_code_file(db: &Connection, ref_: &Value) -> Value {
    let path: String = str_qq(ref_.get("id")).trim().to_string();
    let content = match crate::codebase_codebase_file_read::codebaseFileRead(&json!({ "path": path, "max_bytes": 12000 }), db) {
        Ok(v) => str_qq(v.get("content")),
        Err(_) => String::new(),
    };
    if content.trim().is_empty() {
        return json!({
            "ref_type": "code_file", "id": path, "source": "workspace-file",
            "rendered_md": format!("### [code] {path}\n(not readable \u{2014} is the Atlas built and the workspace active?)"),
        });
    }
    json!({
        "ref_type": "code_file", "id": path, "source": "workspace-file",
        "rendered_md": clamp(&format!("### [code] {path}\n```\n{content}\n```")),
    })
}

// source: a LIVE substrate source, resolved through the SAME already-converted gather-wrapper
// (`cockpit__helpers_context_brief::resolveSourceText`) the cockpit source preview uses.
fn resolve_source(db: &Connection, ref_: &Value) -> Result<Value, String> {
    let kind: String = str_qq(ref_.get("id")).trim().to_string();
    // `pack:<slug>` (2026-09-20): a context pack from the library, whole — the chat's @ picker offers them
    if let Some(slug) = kind.strip_prefix("pack:") {
        let body = crate::context_context_library::pack_body(db, slug).unwrap_or_default();
        if body.trim().is_empty() { return Ok(json!({ "ref_type": "source", "id": kind, "source": "context-pack", "rendered_md": format!("### [context pack] {slug}\n(no such pack in the library)") })); }
        return Ok(json!({ "ref_type": "source", "id": kind, "source": "context-pack", "rendered_md": clamp(&format!("### [context pack] {slug}\n{body}")) }));
    }
    if !SOURCE_KINDS.contains(&kind.as_str()) {
        return Err(format!(
            "assemble-context-envelope: references[].id (source kind) must be one of [{}], got '{kind}'",
            SOURCE_KINDS.join(", "),
        ));
    }
    let text_v = crate::cockpit__helpers_context_brief::resolveSourceText(db, &json!(kind), &json!({}));
    let text = str_qq(Some(&text_v));
    if text.trim().is_empty() {
        return Ok(json!({
            "ref_type": "source", "id": kind, "source": "live-gather",
            "rendered_md": format!("### [source] {kind}\n(this source has no rows in this workspace yet)"),
        }));
    }
    Ok(json!({
        "ref_type": "source", "id": kind, "source": "live-gather",
        "rendered_md": clamp(&format!("### [source] {kind}\n{text}")),
    }))
}

fn inputs_from_kv(input_kv: Option<&Value>) -> Value {
    let empty: Vec<Value> = Vec::new();
    let arr = input_kv.filter(|v| !v.is_null()).and_then(|v| v.as_array()).unwrap_or(&empty);
    let mut map = Map::new();
    for item in arr {
        let key_str = tmpl(item.get("key"));
        if let Some(v) = item.get("value") { map.insert(key_str, v.clone()); }
        // a missing 'value' means JS set the prop to `undefined` — JSON.stringify omits it
    }
    Value::Object(map)
}

pub fn assembleContextEnvelope(input: &Value, db: &Connection) -> Result<Value, String> {
    let prompt_name = input.get("prompt_name").cloned().unwrap_or(Value::Null);
    let prompt_version = input.get("prompt_version").cloned().unwrap_or(Value::Null);
    let intent = input.get("intent").cloned().unwrap_or(Value::Null);

    let inputs_v = input.get("inputs").cloned();
    let use_inputs = matches!(&inputs_v, Some(Value::Object(_)) | Some(Value::Array(_)));
    let inputs_map = if use_inputs { inputs_v.unwrap() } else { inputs_from_kv(input.get("input_kv")) };

    let empty: Vec<Value> = Vec::new();
    let references = input.get("references").and_then(|v| v.as_array()).unwrap_or(&empty);
    let attachments_in = input.get("attachments").and_then(|v| v.as_array()).unwrap_or(&empty);

    let mut resolved_refs: Vec<Value> = Vec::new();
    for ref_ in references {
        let ref_type = ref_.get("ref_type").and_then(|v| v.as_str()).unwrap_or("");
        let resolved = match ref_type {
            "card" => resolve_card(db, ref_),
            "knowledge" => resolve_knowledge(db, ref_),
            "db_row" => resolve_db_row(db, ref_)?,
            "code_file" => resolve_code_file(db, ref_),
            "source" => resolve_source(db, ref_)?,
            other => return Err(format!(
                "assemble-context-envelope: references[].ref_type must be one of [card, knowledge, db_row, code_file, source], got '{other}'"
            )),
        };
        resolved_refs.push(resolved);
    }

    let mut resolved_atts: Vec<Value> = Vec::new();
    for att in attachments_in {
        resolved_atts.push(resolve_attachment(db, att));
    }

    let references_md = if resolved_refs.is_empty() { String::new() } else {
        resolved_refs.iter().map(|r| str_qq(r.get("rendered_md"))).collect::<Vec<_>>().join("\n\n")
    };
    let attachments_md = if resolved_atts.is_empty() { String::new() } else {
        resolved_atts.iter().map(|a| str_qq(a.get("rendered_md"))).collect::<Vec<_>>().join("\n\n")
    };

    Ok(json!({
        "envelope_version": ENVELOPE_VERSION,
        "prompt_name": prompt_name,
        "prompt_version": prompt_version,
        "intent": intent,
        "inputs": inputs_map,
        "references": resolved_refs,
        "attachments": resolved_atts,
        "render": { "references_md": references_md, "attachments_md": attachments_md },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE cards (slug TEXT, title TEXT, status TEXT, body_json TEXT, updated_at TEXT, created_at TEXT);
             ATTACH DATABASE ':memory:' AS knowledge;
             ATTACH DATABASE ':memory:' AS memory;
             CREATE TABLE knowledge.documents (id TEXT, vault_path TEXT, doc_type TEXT, summary TEXT, bytes INTEGER);
             CREATE TABLE knowledge.extracts (id INTEGER, document_id TEXT, extract_type TEXT, text TEXT);
             CREATE TABLE memory.memories (id TEXT, type TEXT, title TEXT, situation TEXT, guidance TEXT);",
        ).unwrap();
        db
    }

    #[test]
    fn card_found_and_not_found() {
        let db = setup();
        db.execute("INSERT INTO cards (slug, title, status, body_json) VALUES ('CARD-1','First card','doing','{\"kind\":\"feature\"}')", []).unwrap();
        let out = assembleContextEnvelope(&json!({ "references": [{ "ref_type": "card", "id": "CARD-1" }] }), &db).unwrap();
        let r0 = &out["references"][0];
        assert_eq!(r0["title"], json!("First card"));
        assert!(r0["rendered_md"].as_str().unwrap().contains("CARD-1"));
        assert!(out["render"]["references_md"].as_str().unwrap().contains("First card"));

        let miss = assembleContextEnvelope(&json!({ "references": [{ "ref_type": "card", "id": "NOPE" }] }), &db).unwrap();
        assert!(miss["references"][0]["rendered_md"].as_str().unwrap().contains("(not found)"));
    }

    #[test]
    fn knowledge_and_db_row_and_attachment() {
        let db = setup();
        db.execute("INSERT INTO knowledge.documents (id, vault_path, doc_type, summary, bytes) VALUES ('D1','notes/x.md','note','a summary',42)", []).unwrap();
        db.execute("INSERT INTO knowledge.extracts (id, document_id, extract_type, text) VALUES (1,'D1','fact','some extracted text')", []).unwrap();
        db.execute("INSERT INTO memory.memories (id, type, title, situation, guidance) VALUES ('M1','decision','a decision','sit','guide')", []).unwrap();

        let out = assembleContextEnvelope(&json!({
            "references": [
                { "ref_type": "knowledge", "id": "D1" },
                { "ref_type": "db_row", "table": "memories", "id": "M1" },
            ],
            "attachments": [ { "document_id": "D1" } ],
        }), &db).unwrap();
        assert!(out["references"][0]["rendered_md"].as_str().unwrap().contains("Extracts"));
        assert!(out["references"][1]["rendered_md"].as_str().unwrap().contains("a decision"));
        assert!(out["attachments"][0]["rendered_md"].as_str().unwrap().contains("42 bytes"));
    }

    #[test]
    fn inputs_from_kv_and_empty_envelope() {
        let db = setup();
        let out = assembleContextEnvelope(&json!({ "input_kv": [{ "key": "a", "value": 1 }] }), &db).unwrap();
        assert_eq!(out["inputs"], json!({ "a": 1 }));
        assert_eq!(out["render"], json!({ "references_md": "", "attachments_md": "" }));
        assert_eq!(out["envelope_version"], json!(1));
    }
}
