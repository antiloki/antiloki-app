//! learn-insight.rs — twin of skills/ai/learn-insight.mjs (db WRITE lane, attached `memory` db).
//! NAMES ARE CONTRACT: `learnInsight` (default), `findRelated`, `buildInsightPath`. `VALID_MOMENT`
//! is a data Set, not a unit.
//!
//! Cross-module: memoryAdd (skills/entities/memory-add.mjs, CONVERTED), memoryConsolidate
//! (skills/entities/memory-consolidate.mjs, CONVERTED), catalogFind (skills/entities/catalog-find.mjs,
//! PARITY-DEFERRED on the vector-search branch — see that module's doc). `findRelated`'s RAG
//! attempt (`haveEmbeds > 0`) always reaches catalogFind's `useModel` boundary here: the very
//! `COUNT(*) ... WHERE entity_type = 'memory-correction'` guard that gates the call is the SAME
//! condition catalogFind's own `GROUP BY model` auto-derivation needs to resolve a truthy model —
//! so whenever `haveEmbeds > 0`, catalogFind deterministically returns Err in this twin (and the
//! real JS lane's real embed() call deterministically throws in this sandboxed/offline harness —
//! see catalog-find.rs's module doc). Both lanes then fall through to the lexical tag-overlap
//! fallback identically, exactly mirroring the JS `try { ... } catch { /* fall through */ }`. This
//! makes `findRelated` fully exercisable (both `haveEmbeds` branches converge on the same
//! deterministic lexical path) even though the vector-search branch itself is unported.
//!
//! `findRelated` is a named export but its real JS signature is a SINGLE destructured object
//! (`{ db, tags, situation_md, guidance_md, limit }`) — not one of the harness's supported helper
//! call shapes (no-db / db-first / db-last). It is registered `class:"transitive"` (proven via its
//! caller `learnInsight`, which exercises the same logic inline) rather than given its own fixture.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

pub const VALID_MOMENT: [&str; 6] = ["rejection", "success", "drift", "violation", "escalation", "realization"];

/// findRelated({ db, tags, situation_md, guidance_md, limit }) — see module doc for why this
/// helper's real signature doesn't fit a fixture, and why the `haveEmbeds` RAG branch is safe to
/// fold into the same deterministic path as the lexical fallback.
pub fn findRelated(db: &Connection, tags: &Value, situation_md: &Value, guidance_md: &Value, limit: i64) -> Result<Value, String> {
    let have_embeds = crate::db_json::query_rows(
        db,
        "SELECT COUNT(*) AS c FROM memory.catalog_embeddings WHERE entity_type = 'memory-correction'",
        &[],
    )?;
    let have_embeds_c = have_embeds
        .as_array()
        .and_then(|a| a.first())
        .and_then(|r| r.get("c"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    if have_embeds_c > 0 {
        let query = take_chars(&format!("{} {}", tmpl(Some(situation_md)), tmpl(Some(guidance_md))), 400);
        let result = crate::entities_catalog_find::catalogFind(
            &json!({ "query": query, "entity_type": "memory-correction", "limit": limit, "fallback": false }),
            db,
        );
        if let Ok(hits) = result {
            let out: Vec<Value> = hits
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|r| truthy(r.get("entity")))
                .map(|r| {
                    let memory_id = r.get("entity").and_then(|e| e.get("id")).cloned().unwrap_or(Value::Null);
                    let score = nz(r.get("score")).cloned().unwrap_or(json!(0));
                    json!({ "memory_id": memory_id, "score": score, "source": "rag" })
                })
                .collect();
            return Ok(json!(out));
        }
        // JS: `catch { /* fall through */ }` — proceed to the lexical fallback below.
    }

    // Lexical fallback: any correction memory sharing >=1 tag.
    let tags_arr = match tags.as_array() {
        Some(a) if !a.is_empty() => a.clone(),
        _ => return Ok(json!([])),
    };
    let conds = tags_arr.iter().map(|_| "tags_json LIKE ?").collect::<Vec<_>>().join(" OR ");
    let mut params: Vec<Value> = tags_arr.iter().map(|t| json!(format!("%{}%", tmpl(Some(t))))).collect();
    params.push(json!(limit));
    let rows = crate::db_json::query_rows(
        db,
        &format!(
            "SELECT id FROM memory.memories WHERE type = 'correction' AND ({}) ORDER BY id DESC LIMIT ?",
            conds
        ),
        &params,
    )?;
    let out: Vec<Value> = rows
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|r| json!({ "memory_id": r.get("id").cloned().unwrap_or(Value::Null), "score": Value::Null, "source": "lexical-fallback" }))
        .collect();
    Ok(json!(out))
}

fn insertLinks(db: &Connection, from_id: &Value, related: &[Value], created_by: &Value) -> Vec<Value> {
    let mut ids = Vec::new();
    for r in related {
        let memory_id = r.get("memory_id").cloned().unwrap_or(Value::Null);
        if memory_id == *from_id {
            continue; // no self-link
        }
        let score = r.get("score").and_then(|s| if s.is_null() { None } else { s.as_f64() }).unwrap_or(0.5);
        let w = score.max(0.0).min(1.0);
        if crate::db_json::run_write(
            db,
            "INSERT INTO memory.memory_links (from_memory_id, to_memory_id, kind, weight, created_by) VALUES (?, ?, 'related', ?, ?)",
            &[from_id.clone(), memory_id.clone(), json!(w), created_by.clone()],
        )
        .is_ok()
        {
            ids.push(memory_id);
        }
    }
    ids
}

/// buildInsightPath(db, focal_id, related_ids) — `db` is accepted (db-first calling shape) but
/// unused in the JS body.
pub fn buildInsightPath(_db: &Connection, focal_id: &Value, related_ids: &Value) -> Value {
    let related = related_ids.as_array().cloned().unwrap_or_default();
    if related.is_empty() {
        return json!([focal_id.clone()]);
    }
    let mut all = vec![focal_id.clone()];
    all.extend(related);
    all.sort_by(|a, b| {
        let af = a.as_f64().unwrap_or(f64::NAN);
        let bf = b.as_f64().unwrap_or(f64::NAN);
        af.partial_cmp(&bf).unwrap_or(std::cmp::Ordering::Equal)
    });
    json!(all)
}

pub fn learnInsight(input: &Value, db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));

    let moment_kind = get("moment_kind").cloned().unwrap_or(Value::Null);
    let emitter = get("emitter").cloned().unwrap_or(Value::Null);
    let source_ref = get("source_ref").cloned().unwrap_or(Value::Null);
    let situation_md = get("situation_md").cloned().unwrap_or(Value::Null);
    let naive_md = get("naive_md").cloned().unwrap_or(Value::Null);
    let corrected_md = get("corrected_md").cloned().unwrap_or(Value::Null);
    let tags = match get("tags") { None => json!([]), Some(v) => v.clone() };
    let created_by = get("created_by").cloned().unwrap_or(Value::Null);
    let cluster_threshold = match get("cluster_threshold") { None => json!(5), Some(v) => v.clone() };

    if !truthy(Some(&moment_kind)) {
        return Err("learn-insight: moment_kind is required".into());
    }
    let moment_kind_s = moment_kind.as_str().unwrap_or("");
    if !VALID_MOMENT.contains(&moment_kind_s) {
        return Err(format!(
            "learn-insight: moment_kind must be one of: {} (got '{}')",
            VALID_MOMENT.join(", "),
            tmpl(Some(&moment_kind))
        ));
    }
    if !truthy(Some(&emitter)) {
        return Err("learn-insight: emitter is required".into());
    }
    if !truthy(Some(&situation_md)) {
        return Err("learn-insight: situation_md is required".into());
    }
    if !truthy(Some(&corrected_md)) {
        return Err("learn-insight: corrected_md is required".into());
    }
    let naive_for_memory = if truthy(Some(&naive_md)) {
        naive_md.clone()
    } else {
        json!(format!("(no naive output captured — moment_kind={})", tmpl(Some(&moment_kind))))
    };

    // 1. Persist the correction memory.
    let mut base_tags: Vec<Value> = vec![
        json!("learn.insight"),
        json!(format!("kind:{}", tmpl(Some(&moment_kind)))),
        json!(format!("from:{}", tmpl(Some(&emitter)))),
    ];
    base_tags.extend(tags.as_array().cloned().unwrap_or_default());

    let source_suffix = if truthy(Some(&source_ref)) { format!(" @ {}", tmpl(Some(&source_ref))) } else { String::new() };
    let title = take_chars(&format!("[{}] {}{}", tmpl(Some(&moment_kind)), tmpl(Some(&emitter)), source_suffix), 200);
    let source_card = if source_ref.is_string() { source_ref.clone() } else { Value::Null };
    let created_by_or_emitter = nz(Some(&created_by)).cloned().unwrap_or_else(|| emitter.clone());

    let mem = crate::entities_memory_add::memoryAdd(
        &json!({
            "type": "correction",
            "title": title,
            "situation": situation_md,
            "guidance": corrected_md,
            "naive_response": naive_for_memory,
            "rationale": Value::Null,
            "tags": base_tags,
            "source_card": source_card,
            "created_by": created_by_or_emitter,
            "confidence": 0.85,
        }),
        db,
    )
    .map_err(|e| format!("learn-insight: failed to persist correction memory: {e}"))?;
    let mem_id = mem.get("id").cloned().unwrap_or(Value::Null);

    // 2. Find related + link.
    let related = findRelated(db, &json!(base_tags.clone()), &situation_md, &corrected_md, 5)?;
    let related_arr = related.as_array().cloned().unwrap_or_default();
    let links_added = insertLinks(db, &mem_id, &related_arr, &created_by_or_emitter);

    // 3. Cluster size -> maybe trigger consolidation.
    let mut consolidation_triggered = false;
    let cluster = crate::db_json::query_rows(
        db,
        "SELECT COUNT(DISTINCT m.id) AS c\n     FROM memory.memories m\n     WHERE m.id = ?\n        OR m.id IN (SELECT to_memory_id FROM memory.memory_links WHERE from_memory_id = ?)\n        OR m.id IN (SELECT from_memory_id FROM memory.memory_links WHERE to_memory_id = ?)",
        &[mem_id.clone(), mem_id.clone(), mem_id.clone()],
    )?;
    let cluster_c = cluster.as_array().and_then(|a| a.first()).and_then(|r| r.get("c")).and_then(|v| v.as_i64()).unwrap_or(0);
    let threshold = cluster_threshold.as_i64().unwrap_or(5);
    if cluster_c >= threshold {
        let mut cluster_ids: Vec<Value> = vec![mem_id.clone()];
        cluster_ids.extend(links_added.clone());
        let placeholders = cluster_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        if let Ok(members) = crate::db_json::query_rows(
            db,
            &format!("SELECT id, situation, guidance FROM memory.memories WHERE id IN ({})", placeholders),
            &cluster_ids,
        ) {
            let members_arr = members.as_array().cloned().unwrap_or_default();
            let situations = take_chars(
                &members_arr
                    .iter()
                    .map(|m| format!("- (mem #{}) {}", tmpl(m.get("id")), tmpl(m.get("situation"))))
                    .collect::<Vec<_>>()
                    .join("\n"),
                1500,
            );
            let guidances = take_chars(
                &members_arr
                    .iter()
                    .map(|m| format!("- (mem #{}) {}", tmpl(m.get("id")), tmpl(m.get("guidance"))))
                    .collect::<Vec<_>>()
                    .join("\n"),
                1500,
            );
            let consolidate_title = take_chars(
                &format!("[cluster {}] {} related lessons (auto-narrative)", tmpl(Some(&moment_kind)), cluster_ids.len()),
                200,
            );
            let consolidate_result = crate::entities_memory_consolidate::memoryConsolidate(
                &json!({
                    "title": consolidate_title,
                    "situation": format!("Cluster of {} {} memories sharing tags or semantic content.\n\n{}", cluster_ids.len(), moment_kind_s, situations),
                    "guidance": format!("Distilled across the cluster:\n\n{}", guidances),
                    "rationale": "Auto-synthesized at cluster threshold. Future: AI-generated via system.ai.call.",
                    "constituent_memory_ids": cluster_ids,
                    "tags": ["narrative", "auto-consolidated", format!("kind:{}", moment_kind_s)],
                    "created_by": created_by_or_emitter,
                }),
                db,
            );
            if consolidate_result.is_ok() {
                consolidation_triggered = true;
            }
        }
    }

    // 4. Insight path (focal + chronologically-ordered related).
    let insight_path = buildInsightPath(db, &mem_id, &json!(links_added.clone()));

    let mut out = Map::new();
    out.insert("memory_id".into(), mem_id);
    out.insert("links_added".into(), json!(links_added));
    out.insert("consolidation_triggered".into(), json!(consolidation_triggered));
    out.insert("insight_path".into(), insight_path);
    out.insert(
        "related_source".into(),
        related_arr.first().and_then(|r| r.get("source")).cloned().unwrap_or(json!("none")),
    );
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory;
            CREATE TABLE memory.catalog_embeddings (entity_type TEXT NOT NULL, entity_name TEXT NOT NULL, model TEXT NOT NULL, dims INTEGER NOT NULL, vector BLOB NOT NULL);
            CREATE TABLE memory.memories (
              id              INTEGER PRIMARY KEY AUTOINCREMENT,
              type            TEXT    NOT NULL,
              title           TEXT    NOT NULL,
              situation       TEXT    NOT NULL,
              guidance        TEXT    NOT NULL,
              naive_response  TEXT,
              rationale       TEXT,
              tags_json       TEXT    NOT NULL DEFAULT '[]',
              source_card     TEXT,
              source_event    INTEGER,
              source_session  TEXT,
              confidence      INTEGER NOT NULL DEFAULT 0,
              validated       INTEGER NOT NULL DEFAULT 0,
              superseded_by   INTEGER,
              agent_slug      TEXT,
              created_at      TEXT    NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')),
              created_by      TEXT    NOT NULL,
              CHECK (type IN ('lesson', 'preference', 'decision', 'reference', 'correction', 'narrative')),
              CHECK (confidence >= 0 AND confidence <= 1),
              CHECK (validated IN (0, 1))
            );
            CREATE TABLE memory.memory_links (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              from_memory_id INTEGER NOT NULL,
              to_memory_id INTEGER NOT NULL,
              kind TEXT NOT NULL,
              rationale TEXT,
              weight REAL NOT NULL DEFAULT 1.0,
              created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')),
              created_by TEXT NOT NULL,
              CHECK (kind IN ('contradicts', 'extends', 'derived_from', 'related', 'corrects')),
              CHECK (from_memory_id != to_memory_id),
              CHECK (weight >= 0.0 AND weight <= 1.0),
              UNIQUE (from_memory_id, to_memory_id, kind)
            );",
        )
        .unwrap();
        c
    }

    #[test]
    fn persists_correction_memory_with_no_related() {
        let c = db();
        let r = learnInsight(
            &json!({ "moment_kind": "drift", "emitter": "claude", "situation_md": "s1", "corrected_md": "c1" }),
            &c,
        )
        .unwrap();
        assert_eq!(r["memory_id"], json!(1));
        assert_eq!(r["links_added"], json!([]));
        assert_eq!(r["consolidation_triggered"], json!(false));
        assert_eq!(r["insight_path"], json!([1]));
        // findRelated runs AFTER the new memory is already inserted, and its lexical-fallback
        // query has no self-exclusion — the just-inserted row's own tags_json self-matches its
        // own search tags. insertLinks skips the self-link (verified: links_added is []), but
        // related[0]?.source still reports 'lexical-fallback', not 'none'. Confirmed against the
        // real JS skill (not a Rust-only quirk).
        assert_eq!(r["related_source"], json!("lexical-fallback"));
    }

    #[test]
    fn links_related_by_shared_tag() {
        let c = db();
        // seed a prior correction memory sharing the kind: tag
        c.execute(
            "INSERT INTO memory.memories (type, title, situation, guidance, naive_response, tags_json, created_by) VALUES ('correction','T0','s0','g0','n0','[\"kind:drift\"]','human')",
            [],
        )
        .unwrap();
        let r = learnInsight(
            &json!({ "moment_kind": "drift", "emitter": "claude", "situation_md": "s1", "corrected_md": "c1" }),
            &c,
        )
        .unwrap();
        assert_eq!(r["links_added"], json!([1]));
        assert_eq!(r["related_source"], json!("lexical-fallback"));
        assert_eq!(r["insight_path"], json!([1, 2]));
    }

    #[test]
    fn have_embeds_branch_falls_through_to_lexical() {
        let c = db();
        c.execute(
            "INSERT INTO memory.catalog_embeddings (entity_type, entity_name, model, dims, vector) VALUES ('memory-correction','mem-1','bge-base',768,x'00')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO memory.memories (type, title, situation, guidance, naive_response, tags_json, created_by) VALUES ('correction','T0','s0','g0','n0','[\"kind:drift\"]','human')",
            [],
        )
        .unwrap();
        let r = learnInsight(
            &json!({ "moment_kind": "drift", "emitter": "claude", "situation_md": "s1", "corrected_md": "c1" }),
            &c,
        )
        .unwrap();
        assert_eq!(r["links_added"], json!([1]));
        assert_eq!(r["related_source"], json!("lexical-fallback"));
    }

    #[test]
    fn cluster_threshold_triggers_consolidation() {
        let c = db();
        for i in 0..4 {
            c.execute(
                &format!(
                    "INSERT INTO memory.memories (type, title, situation, guidance, naive_response, tags_json, created_by) VALUES ('correction','T{i}','s{i}','g{i}','n{i}','[\"kind:drift\"]','human')"
                ),
                [],
            )
            .unwrap();
        }
        let r = learnInsight(
            &json!({ "moment_kind": "drift", "emitter": "claude", "situation_md": "s1", "corrected_md": "c1", "cluster_threshold": 5 }),
            &c,
        )
        .unwrap();
        assert_eq!(r["links_added"].as_array().unwrap().len(), 4);
        assert_eq!(r["consolidation_triggered"], json!(true));
    }

    #[test]
    fn find_related_no_tags_returns_empty() {
        let c = db();
        let r = findRelated(&c, &json!([]), &json!("s"), &json!("g"), 5).unwrap();
        assert_eq!(r, json!([]));
    }

    #[test]
    fn build_insight_path_orders_chronologically() {
        let c = db();
        assert_eq!(buildInsightPath(&c, &json!(5), &json!([])), json!([5]));
        assert_eq!(buildInsightPath(&c, &json!(5), &json!([2, 8])), json!([2, 5, 8]));
    }
}
