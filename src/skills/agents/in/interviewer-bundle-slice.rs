//! interviewer-bundle-slice.rs — twin of skills/agents/in/interviewer-bundle-slice.mjs (db READ
//! lane, attached `memory` db). NAMES ARE CONTRACT: `interviewerBundleSlice` (default),
//! `detectNamespaces` (named, no-db). Per-tier context bundle for the chat prompt: narratives
//! (cosine-ranked via crate::rag_cosine_rank when a query_vector is present, else recency
//! fallback), active cards, recent turns, and a namespace-sliced action catalog (falling back to
//! crate::agents_in_interviewer_action_catalog's full catalog when slicing yields nothing).
use rusqlite::types::ValueRef;
use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// = JS String(v ?? '')
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// = JS String(v) (no ??)
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
// = JS `${v}`
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
// JS `??` right operand: nz(a).or(nz(b)) — null/undefined fall through.
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

// Decodes a little-endian float32 blob into f64s, truncating any trailing partial 4-byte group —
// mirrors `new Float32Array(buf, off, buf.byteLength / 4)`, whose length arg is coerced via
// ToIndex (truncates toward zero; confirmed against V8 not to throw on a non-multiple-of-4 byte
// length). Copied verbatim from the interviewer-episodic-memory.rs precedent.
fn f32le_to_f64(bytes: &[u8]) -> Vec<f64> {
    let n = bytes.len() / 4;
    (0..n)
        .map(|i| {
            let o = i * 4;
            f32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]) as f64
        })
        .collect()
}

// Mirrors db_json::cell (private to its module): node:sqlite's `.all()` hands back a column's
// native JS type; written locally because this query also needs the RAW `vector` cell (for
// truthiness + float32 decoding), which db_json::query_rows would instead render as a base64
// string.
fn cell(v: ValueRef) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => Value::from(f),
        ValueRef::Text(t) => Value::from(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => json!(b.iter().map(|x| json!(*x as i64)).collect::<Vec<_>>()),
    }
}

// JS truthiness of the raw `e.vector` cell as node:sqlite hands it back: NULL -> null (falsy);
// BLOB -> a Uint8Array object (ALWAYS truthy, even zero-length, since objects are always truthy);
// TEXT -> a string (truthy iff non-empty); INTEGER/REAL -> a number (truthy iff nonzero/non-NaN).
fn cell_truthy(v: ValueRef) -> bool {
    match v {
        ValueRef::Null => false,
        ValueRef::Blob(_) => true,
        ValueRef::Text(t) => !t.is_empty(),
        ValueRef::Integer(i) => i != 0,
        ValueRef::Real(f) => f != 0.0 && !f.is_nan(),
    }
}

// JS `vecFromBlob(blob)`: `if (!blob) return null;` then `Buffer.from(blob)` + a Float32Array
// view, wrapped in try/catch -> null on any throw. Buffer.from() accepts a Uint8Array (BLOB
// storage, copies the bytes) or a string (TEXT storage, UTF-8 encodes it) without throwing; a
// bare number (INTEGER/REAL storage) is NOT a valid Buffer.from() argument and throws, caught to
// null — matching cell_truthy's number case being truthy-but-still-null here for a nonzero cell.
fn vec_from_blob_cell(v: ValueRef) -> Option<Vec<f64>> {
    match v {
        ValueRef::Blob(b) => Some(f32le_to_f64(b)),
        ValueRef::Text(t) if !t.is_empty() => Some(f32le_to_f64(t)),
        _ => None,
    }
}

struct NarrativeRow {
    id: Value,
    title: Value,
    guidance: Value,
    kind: Value, // the `type` column (Rust keyword, renamed field)
    vector_truthy: bool,
    vector_f64: Option<Vec<f64>>,
}

// JS: `fetchNarrativesRanked(db, queryVec, max)`.
fn fetch_narratives_ranked(db: &Connection, query_vector: &Value, max: i64) -> Result<String, String> {
    if max == 0 {
        return Ok("  (no consolidated memory needed for this tier)".to_string());
    }
    // FEAT-MEMORY-CORRECTION-WORKFLOW: narrative/correction/lesson types, unsuperseded.
    let sql = "SELECT m.id, m.title, m.guidance, m.type, e.vector \
               FROM memory.memories m \
               LEFT JOIN memory.embeddings e ON e.memory_id = m.id \
               WHERE m.type IN ('narrative','correction','lesson') \
                 AND COALESCE(m.superseded_by,'')=''";
    let rows: Vec<NarrativeRow> = (|| -> rusqlite::Result<Vec<NarrativeRow>> {
        let mut stmt = db.prepare(sql)?;
        let mapped = stmt.query_map([], |row| {
            Ok(NarrativeRow {
                id: cell(row.get_ref(0)?),
                title: cell(row.get_ref(1)?),
                guidance: cell(row.get_ref(2)?),
                kind: cell(row.get_ref(3)?),
                vector_truthy: cell_truthy(row.get_ref(4)?),
                vector_f64: vec_from_blob_cell(row.get_ref(4)?),
            })
        })?;
        mapped.collect::<rusqlite::Result<Vec<_>>>()
    })()
    .unwrap_or_default();

    if rows.is_empty() {
        return Ok("  (no narratives / corrections / lessons yet — substrate is fresh)".to_string());
    }

    // `haveQuery = queryVec && rows.some((r) => r.vector)` — plain JS truthiness on queryVec
    // itself (not a nullish check), so e.g. a truthy-but-empty array would still count.
    let have_query = truthy(Some(query_vector)) && rows.iter().any(|r| r.vector_truthy);

    let picked: Vec<Value> = if have_query {
        // corpus = rows.filter(r=>r.vector).map(r => ({ id, vector: vecFromBlob(r.vector), meta: r }))
        // `meta` only ever has its id/title/guidance/type fields read downstream — the raw
        // `vector` cell itself is never used again, so it's intentionally omitted here.
        let corpus: Vec<Value> = rows
            .iter()
            .filter(|r| r.vector_truthy)
            .map(|r| {
                json!({
                    "id": r.id.clone(),
                    "vector": json!(r.vector_f64.clone()),
                    "meta": { "id": r.id.clone(), "title": r.title.clone(), "guidance": r.guidance.clone(), "type": r.kind.clone() },
                })
            })
            .collect();
        let ranked = crate::rag_cosine_rank::cosineRank(
            &json!({ "query_vector": query_vector, "corpus": corpus, "top_k": max }),
            &Value::Null,
        )?;
        ranked
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|x| x.get("meta").cloned().unwrap_or(Value::Null))
            .collect()
    } else {
        // Fallback: most recent (`rows.sort((a,b) => b.id - a.id).slice(0, max)`).
        let mut sorted: Vec<&NarrativeRow> = rows.iter().collect();
        sorted.sort_by(|a, b| b.id.as_i64().unwrap_or(0).cmp(&a.id.as_i64().unwrap_or(0)));
        let end = max.max(0) as usize;
        sorted
            .into_iter()
            .take(end)
            .map(|r| json!({ "id": r.id.clone(), "title": r.title.clone(), "guidance": r.guidance.clone(), "type": r.kind.clone() }))
            .collect()
    };

    if picked.is_empty() {
        return Ok("  (no memories matched this context)".to_string());
    }

    let mut lines: Vec<String> = Vec::with_capacity(picked.len());
    for (i, meta) in picked.iter().enumerate() {
        let ty = meta.get("type");
        let tag = if truthy(ty) {
            format!("[{}#{}]", tmpl(ty), tmpl(meta.get("id")))
        } else {
            format!("[#{}]", tmpl(meta.get("id")))
        };
        let guidance: String = str_qq(meta.get("guidance")).chars().take(260).collect();
        lines.push(format!("  {}. {} {}\n     \u{2192} {}", i + 1, tag, tmpl(meta.get("title")), guidance));
    }
    Ok(lines.join("\n"))
}

// JS: `fetchActiveCards(db, max)`.
fn fetch_active_cards(db: &Connection, max: i64) -> String {
    if max == 0 {
        return "  (no active-cards context for this tier)".to_string();
    }
    let rows = crate::db_json::query_rows(
        db,
        "SELECT slug, title, status FROM cards WHERE status NOT IN ('done','blocked','archived') ORDER BY updated_at DESC LIMIT ?",
        &[json!(max)],
    )
    .unwrap_or_else(|_| json!([]));
    let rows = rows.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        return "  (board is clear — no active cards)".to_string();
    }
    rows.iter()
        .map(|c| format!("  - {} [{}] \u{2014} {}", tmpl(c.get("slug")), tmpl(c.get("status")), tmpl(c.get("title"))))
        .collect::<Vec<_>>()
        .join("\n")
}

// JS: `fetchRecentTurns(db, session_id, max)`.
fn fetch_recent_turns(db: &Connection, session_id: &Value, max: i64) -> String {
    if max == 0 || !truthy(Some(session_id)) {
        return "  (no turn history needed for this tier)".to_string();
    }
    let rows = match crate::db_json::query_rows(
        db,
        "SELECT user_text, classified_kind, interviewer_response_md FROM interviewer_turns WHERE session_id = ? ORDER BY turn_index DESC LIMIT ?",
        &[session_id.clone(), json!(max)],
    ) {
        // JS's catch branch here returns EARLY (unlike the other two fetchers, whose catch just
        // defaults rows=[] and falls through to the "empty" message) — preserved verbatim.
        Err(_) => return "  (session history unavailable)".to_string(),
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
    };
    if rows.is_empty() {
        return "  (this is the first turn of a fresh session)".to_string();
    }
    let mut reversed = rows;
    reversed.reverse();
    let mut lines: Vec<String> = Vec::with_capacity(reversed.len());
    for (i, t) in reversed.iter().enumerate() {
        let user_preview: String = js_string(t.get("user_text").unwrap_or(&Value::Null)).chars().take(180).collect();
        let user_line = format!("  {}. user: \"{}\"", i + 1, user_preview);
        let reply_line = if truthy(t.get("interviewer_response_md")) {
            let reply_preview: String = js_string(t.get("interviewer_response_md").unwrap_or(&Value::Null)).chars().take(220).collect();
            format!("     you: \"{}\"", reply_preview)
        } else {
            "     (no reply recorded)".to_string()
        };
        lines.push(format!("{}\n{}", user_line, reply_line));
    }
    lines.join("\n")
}

// Keyword stems -> action namespaces, transcribed verbatim from KEYWORD_TO_NAMESPACE.
const KEYWORD_TO_NAMESPACE: &[(&[&str], &[&str])] = &[
    (&["card", "cards", "board", "backlog"], &["board-keeper."]),
    (&["contract", "contracts"], &["contract.", "tl."]),
    (
        &["memory", "memories", "memor", "narrative", "consolidate", "consolidation", "keeper"],
        &["keeper.", "system.memory."],
    ),
    (&["drive", "driver", "tick", "autonomous", "autopilot"], &["system.driver."]),
    (&["session", "sessions", "plan", "planning", "pl"], &["pl.session.", "pl."]),
    (&["qa", "accept", "acceptance", "gate"], &["qa."]),
    (&["dev", "implement", "implementation", "impl"], &["git-manager."]),
    (&["agent", "agents"], &["system.agent."]),
    (&["action", "actions"], &["system.action."]),
    (&["law", "laws"], &["system.law.", "system.laws."]),
    (&["doc", "docs", "documentation"], &["system.docs."]),
    (&["anatomy"], &["system.anatomy."]),
    (&["consistency", "consistenc"], &["system.consistency."]),
    (&["workflow", "workflows"], &["system.workflow."]),
];

// JS: `tokens(text)` — `(text ?? '').toLowerCase().split(/[^a-z0-9_-]+/).filter(Boolean)`.
fn tokens_of(text: &Value) -> Vec<String> {
    let base = match nz(Some(text)) {
        Some(Value::String(s)) => s.clone(),
        // A present-but-non-string value (number/bool/object/array) would make JS's
        // `.toLowerCase()` throw TypeError — a guard-throw outside the fixtured success-path
        // domain (playbook gotcha 9); treated here as empty text rather than panicking.
        _ => String::new(),
    };
    base.to_lowercase()
        .split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'))
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// JS named export `detectNamespaces(user_text)` — keyword-stem match over WORD-tokenized input,
/// insertion-ordered de-duped namespace list.
pub fn detectNamespaces(user_text: &Value) -> Value {
    let toks = tokens_of(user_text);
    let mut out: Vec<String> = Vec::new();
    for (kws, nss) in KEYWORD_TO_NAMESPACE {
        if kws.iter().any(|k| toks.iter().any(|t| t == k)) {
            for ns in *nss {
                if !out.iter().any(|o| o == ns) {
                    out.push((*ns).to_string());
                }
            }
        }
    }
    json!(out)
}

// JS: `buildSlicedCatalog(db, user_text, mode)`.
fn build_sliced_catalog(db: &Connection, user_text: &Value, mode: &str) -> Result<String, String> {
    if mode == "full" {
        let r = crate::agents_in_interviewer_action_catalog::interviewerActionCatalog(&json!({}), db)?;
        return Ok(r.get("catalog").and_then(Value::as_str).unwrap_or("").to_string());
    }
    let namespaces_v = detectNamespaces(user_text);
    let namespaces: Vec<String> = namespaces_v
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();
    if namespaces.is_empty() {
        let r = crate::agents_in_interviewer_action_catalog::interviewerActionCatalog(&json!({}), db)?;
        return Ok(r.get("catalog").and_then(Value::as_str).unwrap_or("").to_string());
    }
    let limit = if mode == "one-namespace" { 1 } else { 2 };
    let picked: Vec<String> = namespaces.into_iter().take(limit).collect();
    let placeholders = vec!["name LIKE ?"; picked.len()].join(" OR ");
    let params: Vec<Value> = picked.iter().map(|ns| json!(format!("{ns}%"))).collect();
    let sql = format!("SELECT name, owner_agent, description FROM actions WHERE {placeholders} ORDER BY name");
    let rows = crate::db_json::query_rows(db, &sql, &params)?;
    let rows = rows.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        let r = crate::agents_in_interviewer_action_catalog::interviewerActionCatalog(&json!({}), db)?;
        return Ok(r.get("catalog").and_then(Value::as_str).unwrap_or("").to_string());
    }
    let mut lines = vec![format!("[sliced catalog \u{2014} namespaces: {}]", picked.join(", "))];
    for r in &rows {
        let desc: String = str_qq(r.get("description")).chars().take(80).collect();
        lines.push(format!("  {} ({}) \u{2014} {}", tmpl(r.get("name")), tmpl(r.get("owner_agent")), desc));
    }
    Ok(lines.join("\n"))
}

/// JS default export `interviewerBundleSlice(input, context)`. `context.db` missing -> guard
/// throw, never exercised by a fixture (the db lane always injects one).
pub fn interviewerBundleSlice(input: &Value, db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));

    let user_text = get("user_text");
    if !truthy(user_text) {
        return Err("interviewer-bundle-slice: user_text is required".into());
    }
    let user_text_v = user_text.unwrap().clone();

    // Destructuring defaults apply only when the key is ABSENT (JS `undefined` default rule) —
    // an explicit null/other value present in input passes through untouched.
    let session_id_v = match get("session_id") { None => Value::Null, Some(v) => v.clone() };
    let tier_v = match get("tier") { None => json!("mid"), Some(v) => v.clone() };
    let query_vector_v = match get("query_vector") { None => Value::Null, Some(v) => v.clone() };

    // `TIER_CONFIG[tier] ?? TIER_CONFIG.mid` — bracket lookup only matches the literal string
    // keys "low"/"mid"/"high"; any other type or value (including null, numbers, "mid" itself
    // falling through the same way) resolves to TIER_CONFIG.mid. `tier_used` in the returned
    // bundle always echoes the ORIGINAL `tier_v`, not the resolved config key.
    let (narratives, cards, turns, catalog_mode) = match tier_v.as_str() {
        Some("low") => (2i64, 5i64, 2i64, "one-namespace"),
        Some("high") => (5i64, 10i64, 4i64, "full"),
        _ => (3i64, 7i64, 3i64, "two-namespace"),
    };

    let memory_context = fetch_narratives_ranked(db, &query_vector_v, narratives)?;
    let active_cards = fetch_active_cards(db, cards);
    let recent_turns = fetch_recent_turns(db, &session_id_v, turns);
    let action_catalog = build_sliced_catalog(db, &user_text_v, catalog_mode)?;

    Ok(json!({
        "memory_context": memory_context,
        "active_cards": active_cards,
        "recent_turns": recent_turns,
        "action_catalog": action_catalog,
        "tier_used": tier_v,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory;
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, title TEXT NOT NULL, guidance TEXT NOT NULL, type TEXT NOT NULL, superseded_by INTEGER);
             CREATE TABLE memory.embeddings (id INTEGER PRIMARY KEY AUTOINCREMENT, memory_id INTEGER NOT NULL, vector BLOB);
             CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT, updated_at TEXT);
             CREATE TABLE interviewer_turns (id INTEGER PRIMARY KEY, session_id TEXT, turn_index INTEGER, user_text TEXT, classified_kind TEXT, interviewer_response_md TEXT);
             CREATE TABLE actions (name TEXT, owner_agent TEXT, description TEXT);
             INSERT INTO memory.memories (id,title,guidance,type,superseded_by) VALUES
               (1,'Narrative One','Always check the board first.', 'narrative', NULL),
               (2,'Correction Two','Do not skip QA gates.', 'correction', NULL),
               (3,'Lesson Superseded','old', 'lesson', 999);
             INSERT INTO memory.embeddings (memory_id, vector) VALUES (1, X'0000803f0000000000000000');
             INSERT INTO memory.embeddings (memory_id, vector) VALUES (2, X'000000000000803f00000000');
             INSERT INTO cards (slug,title,status,updated_at) VALUES
               ('C1','Card One','doing','2025-12-01 10:00:00'),
               ('C2','Card Two','done','2025-12-05 10:00:00');
             INSERT INTO actions (name,owner_agent,description) VALUES
               ('board-keeper.card.add','board-keeper','Add a card.'),
               ('system.driver.tick','system','Advance the driver.');",
        )
        .unwrap();
        c
    }

    #[test]
    fn detect_namespaces_matches_and_dedups() {
        assert_eq!(detectNamespaces(&json!("check the cards on the board")), json!(["board-keeper."]));
        assert_eq!(detectNamespaces(&json!("AUTONOMOUS DRIVER TICK")), json!(["system.driver."]));
        assert_eq!(detectNamespaces(&json!("nothing matches here")), json!([]));
        assert_eq!(detectNamespaces(&Value::Null), json!([]));
    }

    #[test]
    fn bundle_slice_mid_tier_default_and_namespace_slicing() {
        let c = db();
        let r = interviewerBundleSlice(&json!({ "user_text": "check the cards on the board please" }), &c).unwrap();
        assert_eq!(r["tier_used"], json!("mid"));
        let cat = r["action_catalog"].as_str().unwrap();
        assert!(cat.contains("[sliced catalog"));
        assert!(cat.contains("board-keeper.card.add"));
        assert!(!cat.contains("system.driver.tick"));
        let cards = r["active_cards"].as_str().unwrap();
        assert!(cards.contains("C1"));
        assert!(!cards.contains("C2")); // done excluded
        assert_eq!(r["recent_turns"], json!("  (no turn history needed for this tier)"));
    }

    #[test]
    fn bundle_slice_ranks_narratives_by_cosine_when_query_vector_present() {
        let c = db();
        let r = interviewerBundleSlice(
            &json!({ "user_text": "irrelevant", "tier": "low", "query_vector": [1, 0, 0] }),
            &c,
        )
        .unwrap();
        let mc = r["memory_context"].as_str().unwrap();
        // memory 1 ([1,0,0], score 1.0) ranks ahead of memory 2 ([0,1,0], score 0.0)
        assert!(mc.find("Narrative One").unwrap() < mc.find("Correction Two").unwrap());
    }

    #[test]
    fn bundle_slice_falls_back_to_full_catalog_when_no_namespace_detected() {
        let c = db();
        let r = interviewerBundleSlice(&json!({ "user_text": "zzz completely unrelated gibberish" }), &c).unwrap();
        let cat = r["action_catalog"].as_str().unwrap();
        assert!(!cat.contains("[sliced catalog"));
        assert!(cat.contains("board-keeper.card.add"));
        assert!(cat.contains("system.driver.tick"));
    }

    #[test]
    fn bundle_slice_requires_user_text() {
        let c = db();
        assert!(interviewerBundleSlice(&json!({}), &c).is_err());
    }
}
