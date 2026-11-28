//! interviewer-episodic-memory.rs — twin of skills/agents/in/interviewer-episodic-memory.mjs
//! (db READ lane). NAMES ARE CONTRACT: `interviewerEpisodicMemory` (default), `vecFromBlob`
//! (named, no-db), `ageDays` (named, no-db). Cross-session RAG over past interviewer_turns:
//! cosine-rank candidates by user_text_embedding, apply 14d-half-life time-decay, keep hits
//! clearing `threshold`, render a compact "EPISODIC MEMORY" block. Frozen-clock lane: JS
//! `Date.now()` is pinned to 2026-01-01T00:00:00.000Z; the twin hardcodes that literal (both
//! for the SQL `datetime('now', ...)` scan window and for the age-in-days computation).
use rusqlite::types::ValueRef;
use rusqlite::Connection;
use serde_json::{json, Value};

const DEFAULT_THRESHOLD: f64 = 0.78;
const DEFAULT_TOP_K: f64 = 2.0;
const HALF_LIFE_DAYS: f64 = 14.0;
const PREVIEW_CHARS: usize = 100;

// JS truthiness of `v` (used for `!x` and `x && ...`).
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `??` right operand: nz(a).or(nz(b)) — null/undefined fall through.
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
// = JS Number(v)
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
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
// = JS `${v}`
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
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
// JS Math.round: nearest integer, ties toward +Infinity (spec-correct, avoids the x+0.5 float bug).
fn js_round(x: f64) -> f64 {
    if !x.is_finite() { return x; }
    let f = x.floor();
    if x - f < 0.5 { f } else { f + 1.0 }
}
// JS `arr.slice(0, end)`: ToIntegerOrInfinity(end) then clamp into [0, len] (negative counts
// from the array's end).
fn array_slice_end(len: usize, end: f64) -> usize {
    let e = if end.is_nan() { 0.0 } else if end.is_infinite() { end } else { end.trunc() };
    let len_f = len as f64;
    let rel_end = if e < 0.0 { (len_f + e).max(0.0) } else { e.min(len_f) };
    rel_end as usize
}

// days since 1970-01-01 for a proleptic-Gregorian y/m/d (m in 1..=12) — Howard Hinnant's algorithm.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}
// Parse an ISO-8601/SQL UTC instant ('YYYY-MM-DD' or 'YYYY-MM-DD[T ]HH:MM:SS[.sss][Z]') to epoch
// ms — matches `new Date(tsString + (tsString.endsWith('Z') ? '' : 'Z')).getTime()`: the
// space-vs-'T' separator and any trailing 'Z' don't change the parse (both interpreted as UTC).
// A malformed string → None (mirrors JS NaN, where `Number.isFinite(NaN)` is false).
fn parse_iso_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' { return None; }
    let num = |lo: usize, hi: usize| -> Option<i64> {
        let sub = s.get(lo..hi)?;
        if sub.bytes().all(|c| c.is_ascii_digit()) { sub.parse::<i64>().ok() } else { None }
    };
    let y = num(0, 4)?;
    let mo = num(5, 7)?;
    let d = num(8, 10)?;
    let (mut hh, mut mm, mut ss, mut ms) = (0i64, 0i64, 0i64, 0i64);
    if b.len() > 10 {
        if b.len() < 19 || (b[10] != b'T' && b[10] != b' ') || b[13] != b':' || b[16] != b':' { return None; }
        hh = num(11, 13)?;
        mm = num(14, 16)?;
        ss = num(17, 19)?;
        if b.len() > 19 && b[19] == b'.' {
            let mut f3: String = s[20..].chars().take_while(|c| c.is_ascii_digit()).take(3).collect();
            while f3.len() < 3 { f3.push('0'); }
            ms = f3.parse::<i64>().unwrap_or(0);
        }
    }
    Some((((days_from_civil(y, mo, d) * 24 + hh) * 60 + mm) * 60 + ss) * 1000 + ms)
}

// JS: `ageDays(tsString)` core (shared by the standalone unit and the internal per-row call).
fn age_days_core(ts: Option<&Value>) -> f64 {
    if !truthy(ts) { return 0.0; }
    match ts.and_then(Value::as_str).and_then(parse_iso_ms) {
        Some(t) => (crate::clock::now_ms() - t) as f64 / 86_400_000.0,
        None => 0.0, // !Number.isFinite(t)
    }
}
/// JS named export `ageDays(tsString)` — days between the frozen "now" and the parsed instant.
pub fn ageDays(ts_string: &Value) -> Value { json!(age_days_core(Some(ts_string))) }

// Decodes a little-endian float32 blob into f64s, truncating any trailing partial 4-byte group —
// mirrors `new Float32Array(buf, off, buf.byteLength / 4)`, whose length arg is coerced via
// ToIndex (truncates toward zero for a positive/fractional value; the trailing bytes are dropped
// rather than raising, confirmed against V8).
fn f32le_to_f64(bytes: &[u8]) -> Vec<f64> {
    let n = bytes.len() / 4;
    (0..n).map(|i| {
        let o = i * 4;
        f32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]) as f64
    }).collect()
}
/// JS named export `vecFromBlob(blob)` — Buffer/Uint8Array → Float32Array (LE); a falsy blob is
/// `null`; anything that makes `Buffer.from(blob)` throw (e.g. a bare number) is caught → `null`.
/// Fixture convention for a byte-arg is `{ "__bytes_b64": "<base64>" }` (mirrors compare.mjs's
/// reviveArg/normalizeResult markers — the return value round-trips through `{ "__f32": [...] }`).
pub fn vecFromBlob(blob: &Value) -> Value {
    if !truthy(Some(blob)) { return Value::Null; }
    match blob.get("__bytes_b64").and_then(|b| b.as_str()) {
        Some(b64) => {
            use base64::Engine;
            match base64::engine::general_purpose::STANDARD.decode(b64) {
                Ok(bytes) => json!({ "__f32": f32le_to_f64(&bytes) }),
                Err(_) => Value::Null,
            }
        }
        None => Value::Null,
    }
}

// Mirrors db_json::cell (private to its module): node:sqlite's `.all()` hands back a column's
// native JS type. Written locally (not via db_json::query_rows) because the embedding column
// needs its RAW bytes for float32 decoding, not the base64-string rendering query_rows gives a
// BLOB cell — mirrors memory-similar.rs's same local-cell precedent for the identical reason.
fn cell(v: ValueRef) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => Value::from(f),
        ValueRef::Text(t) => Value::from(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => json!(b.iter().map(|x| json!(*x as i64)).collect::<Vec<_>>()),
    }
}
fn to_sql_param(v: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as S;
    match v {
        Value::Null => S::Null,
        Value::Bool(b) => S::Integer(*b as i64),
        Value::Number(n) => n.as_i64().map(S::Integer).unwrap_or_else(|| S::Real(n.as_f64().unwrap())),
        Value::String(s) => S::Text(s.clone()),
        other => S::Text(other.to_string()),
    }
}

struct TurnRow {
    id: Value,
    session_id: Value,
    ts: Value,
    user_text: Value,
    classified_kind: Value,
    // None = vecFromBlob(blob) would return null (falsy blob, or a non-blob/non-text storage
    // class — Integer/Real — which throws inside `Buffer.from(blob)` and gets caught). Some(_)
    // (possibly empty) = a real BLOB or TEXT cell, which `Buffer.from` accepts without throwing.
    blob_bytes: Option<Vec<u8>>,
}

pub fn interviewerEpisodicMemory(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('interviewer-episodic-memory','db') when no db — the db lane
    // always injects one, so that guard-throw is never exercised by a fixture.
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));

    let query_vector = get("query_vector");
    if !truthy(query_vector) {
        // No query vector → nothing to match; return empty block silently.
        return Ok(json!({ "block_md": "", "hits": [] }));
    }
    let query_vector = query_vector.unwrap().clone();

    let current_session_id = get("current_session_id").cloned().unwrap_or(Value::Null);
    let threshold = match get("threshold") { None => DEFAULT_THRESHOLD, Some(v) => js_number(Some(v)) };
    let top_k = match get("top_k") { None => DEFAULT_TOP_K, Some(v) => js_number(Some(v)) };

    // Pull past turns (excluding the current session). Cap a sane scan window — for now: last
    // 365 days, 5000 rows.
    let session_param = nz(Some(&current_session_id)).cloned().unwrap_or(json!(-1));
    let sql = &format!("SELECT t.id, t.session_id, t.ts, t.user_text, t.classified_kind, \
                      t.user_text_embedding \
               FROM interviewer_turns t \
               WHERE t.session_id IS NOT ? \
                 AND t.user_text_embedding IS NOT NULL \
                 AND t.ts >= datetime({}, '-365 days') \
               ORDER BY t.id DESC LIMIT 5000", crate::clock::now_sql_inner());
    let rows: Vec<TurnRow> = (|| -> rusqlite::Result<Vec<TurnRow>> {
        let mut stmt = db.prepare(sql)?;
        let mapped = stmt.query_map(rusqlite::params![to_sql_param(&session_param)], |row| {
            Ok(TurnRow {
                id: cell(row.get_ref(0)?),
                session_id: cell(row.get_ref(1)?),
                ts: cell(row.get_ref(2)?),
                user_text: cell(row.get_ref(3)?),
                classified_kind: cell(row.get_ref(4)?),
                blob_bytes: match row.get_ref(5)? {
                    ValueRef::Blob(b) => Some(b.to_vec()),
                    ValueRef::Text(t) => Some(t.to_vec()), // Buffer.from(string) UTF-8-encodes; SQLite TEXT storage already IS UTF-8 bytes
                    ValueRef::Null | ValueRef::Integer(_) | ValueRef::Real(_) => None,
                },
            })
        })?;
        mapped.collect::<rusqlite::Result<Vec<_>>>()
    })()
    .unwrap_or_default();

    if rows.is_empty() {
        return Ok(json!({ "block_md": "", "hits": [] }));
    }

    // corpus = rows.map(vecFromBlob).filter(truthy) — id/vector/meta triples for cosineRank.
    let mut corpus: Vec<Value> = Vec::new();
    for r in &rows {
        let Some(bytes) = &r.blob_bytes else { continue };
        corpus.push(json!({
            "id": r.id.clone(),
            "vector": f32le_to_f64(bytes),
            "meta": {
                "ts": r.ts.clone(),
                "session_id": r.session_id.clone(),
                "user_text": r.user_text.clone(),
                "classified_kind": r.classified_kind.clone(),
            },
        }));
    }

    // First pass: cosine top-K * 3 candidates.
    let candidate_k = (top_k * 3.0).max(6.0);
    let candidate_k = if candidate_k.is_finite() { candidate_k.trunc().max(0.0) as i64 } else { 6 };
    let ranked = crate::rag_cosine_rank::cosineRank(
        &json!({ "query_vector": query_vector, "corpus": corpus, "top_k": candidate_k }),
        &Value::Null,
    )?;
    let ranked = ranked.as_array().cloned().unwrap_or_default();

    // Apply decay; re-sort by effective_score.
    let mut decayed: Vec<Value> = Vec::with_capacity(ranked.len());
    for r in &ranked {
        let meta = r.get("meta").cloned().unwrap_or(Value::Null);
        let days = age_days_core(meta.get("ts"));
        let score = r.get("score").and_then(Value::as_f64).unwrap_or(f64::NAN);
        let eff = crate::rag_decay_score::decayScore(
            &json!({ "base_score": score, "age_days": days, "half_life_days": HALF_LIFE_DAYS }),
            &Value::Null,
        )?;
        let effective_score = eff.get("effective_score").and_then(Value::as_f64).unwrap_or(f64::NAN);
        decayed.push(json!({
            "turn_id": r.get("id").cloned().unwrap_or(Value::Null),
            "session_id": meta.get("session_id").cloned().unwrap_or(Value::Null),
            "cosine": score,
            "effective_score": effective_score,
            "age_days": days,
            "user_text": meta.get("user_text").cloned().unwrap_or(Value::Null),
            "classified_kind": meta.get("classified_kind").cloned().unwrap_or(Value::Null),
        }));
    }
    // JS `.sort((a,b) => b.effective_score - a.effective_score)`: stable descending.
    decayed.sort_by(|a, b| {
        let af = a["effective_score"].as_f64().unwrap_or(f64::NAN);
        let bf = b["effective_score"].as_f64().unwrap_or(f64::NAN);
        bf.partial_cmp(&af).unwrap_or(std::cmp::Ordering::Equal)
    });

    let filtered: Vec<Value> = decayed
        .into_iter()
        .filter(|d| d["effective_score"].as_f64().unwrap_or(f64::NAN) >= threshold)
        .collect();
    let end = array_slice_end(filtered.len(), top_k);
    let hits: Vec<Value> = filtered.into_iter().take(end).collect();

    if hits.is_empty() {
        return Ok(json!({ "block_md": "", "hits": [] }));
    }

    let mut lines: Vec<String> = Vec::with_capacity(hits.len());
    for (i, h) in hits.iter().enumerate() {
        let age = h["age_days"].as_f64().unwrap_or(0.0);
        let when = if age < 1.0 {
            "today".to_string()
        } else if age < 7.0 {
            format!("{}d ago", js_round(age) as i64)
        } else {
            format!("{}w ago", js_round(age / 7.0) as i64)
        };
        let preview: String = js_string(&h["user_text"]).chars().take(PREVIEW_CHARS).collect();
        let kind = tmpl(h.get("classified_kind"));
        let score_str = format!("{:.2}", h["effective_score"].as_f64().unwrap_or(f64::NAN));
        lines.push(format!("  {}. {}: \"{}\" (kind={}, score={})", i + 1, when, preview, kind, score_str));
    }

    Ok(json!({ "block_md": lines.join("\n"), "hits": hits }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE interviewer_turns (
               id INTEGER PRIMARY KEY,
               session_id INTEGER NOT NULL,
               ts TEXT NOT NULL,
               user_text TEXT NOT NULL,
               classified_kind TEXT NOT NULL,
               user_text_embedding BLOB
             );",
        )
        .unwrap();
        c
    }

    fn insert_vec(c: &Connection, id: i64, session_id: i64, ts: &str, text: &str, kind: &str, v: &[f32]) {
        let mut bytes = Vec::with_capacity(v.len() * 4);
        for f in v { bytes.extend_from_slice(&f.to_le_bytes()); }
        c.execute(
            "INSERT INTO interviewer_turns (id, session_id, ts, user_text, classified_kind, user_text_embedding) VALUES (?,?,?,?,?,?)",
            rusqlite::params![id, session_id, ts, text, kind, bytes],
        ).unwrap();
    }

    #[test]
    fn no_query_vector_returns_empty_silently() {
        let c = db();
        let r = interviewerEpisodicMemory(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "block_md": "", "hits": [] }));
    }

    #[test]
    fn empty_turn_store_returns_empty() {
        let c = db();
        let r = interviewerEpisodicMemory(&json!({ "query_vector": [1, 0, 0] }), &c).unwrap();
        assert_eq!(r["block_md"], json!(""));
        assert_eq!(r["hits"], json!([]));
    }

    #[test]
    fn similar_recent_turn_clears_threshold_and_excludes_current_session() {
        let c = db();
        insert_vec(&c, 1, 10, "2025-12-31 00:00:00", "how does keeper consolidate memories", "meta", &[1.0, 0.0, 0.0]);
        // Same-session turn must never self-match.
        insert_vec(&c, 2, 20, "2025-12-31 00:00:00", "same session text", "meta", &[1.0, 0.0, 0.0]);

        let r = interviewerEpisodicMemory(
            &json!({ "query_vector": [0.99, 0.1, 0.0], "current_session_id": 20, "threshold": 0.7 }),
            &c,
        ).unwrap();
        let hits = r["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["turn_id"], json!(1));
        assert!(r["block_md"].as_str().unwrap().contains("keeper consolidate"));
    }

    #[test]
    fn old_match_decays_below_threshold() {
        let c = db();
        // ~90 days old: cosine ~1.0 * exp(-90/14) ~ 0.0017, well under any real threshold.
        insert_vec(&c, 1, 10, "2025-10-03 00:00:00", "ancient text", "meta", &[1.0, 0.0, 0.0]);
        let r = interviewerEpisodicMemory(
            &json!({ "query_vector": [1.0, 0.0, 0.0], "current_session_id": 999, "threshold": 0.5 }),
            &c,
        ).unwrap();
        assert_eq!(r["hits"], json!([]));
    }

    #[test]
    fn top_k_caps_results() {
        let c = db();
        for i in 0..5 {
            insert_vec(&c, i, 10, "2025-12-31 00:00:00", &format!("match {i}"), "meta", &[1.0, 0.0, 0.0]);
        }
        let r = interviewerEpisodicMemory(
            &json!({ "query_vector": [1.0, 0.0, 0.0], "current_session_id": 999, "threshold": 0.5, "top_k": 2 }),
            &c,
        ).unwrap();
        assert_eq!(r["hits"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn skips_turns_without_embeddings() {
        let c = db();
        c.execute(
            "INSERT INTO interviewer_turns (id, session_id, ts, user_text, classified_kind) VALUES (1, 10, '2025-12-31 00:00:00', 'no embedding', 'meta')",
            [],
        ).unwrap();
        insert_vec(&c, 2, 10, "2025-12-31 00:00:00", "has embedding", "meta", &[1.0, 0.0, 0.0]);
        let r = interviewerEpisodicMemory(
            &json!({ "query_vector": [1.0, 0.0, 0.0], "current_session_id": 999, "threshold": 0.5 }),
            &c,
        ).unwrap();
        let hits = r["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["user_text"], json!("has embedding"));
    }

    #[test]
    fn vec_from_blob_round_trips_and_handles_null() {
        use base64::Engine;
        let bytes: Vec<u8> = [0.1f32, 0.2, 0.3].iter().flat_map(|f| f.to_le_bytes()).collect();
        let arg = json!({ "__bytes_b64": base64::engine::general_purpose::STANDARD.encode(&bytes) });
        let r = vecFromBlob(&arg);
        let arr = r["__f32"].as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert!((arr[0].as_f64().unwrap() - 0.1).abs() < 1e-6);
        assert_eq!(vecFromBlob(&Value::Null), Value::Null);
    }

    #[test]
    fn vec_from_blob_truncates_trailing_partial_group() {
        use base64::Engine;
        let mut bytes = 0.5f32.to_le_bytes().to_vec();
        bytes.extend_from_slice(&[0xAB, 0xCD]);
        let arg = json!({ "__bytes_b64": base64::engine::general_purpose::STANDARD.encode(&bytes) });
        let r = vecFromBlob(&arg);
        assert_eq!(r["__f32"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn age_days_zero_for_falsy_and_unparseable() {
        assert_eq!(ageDays(&Value::Null), json!(0.0));
        assert_eq!(ageDays(&json!("")), json!(0.0));
        assert_eq!(ageDays(&json!("not-a-date")), json!(0.0));
    }

    #[test]
    fn age_days_matches_frozen_now_delta() {
        assert_eq!(ageDays(&json!("2025-12-31 00:00:00")), json!(1.0));
        assert_eq!(ageDays(&json!("2025-12-25T00:00:00.000Z")), json!(7.0));
        assert_eq!(ageDays(&json!("2026-01-02 00:00:00")), json!(-1.0));
    }
}
