//! keeper-journey-append.rs — twin of skills/agents/keeper/keeper-journey-append.mjs (db WRITE
//! lane, attached `memory` db). NAMES ARE CONTRACT: `keeperJourneyAppend` (default). Appends a
//! closed card's reflection (the linked type='lesson' memory) to project_journey, then fires a
//! card_phase_events phase='journey_appended' row via the already-converted card-phase-event-record
//! twin. Idempotent: a pre-existing project_journey row for the same (card_slug, memory_id) is a
//! no-op. Pure deterministic — no AI. Card: FEAT-PROJECT-JOURNEY-NARRATIVE.
// The third (and final) post-QA pipeline step; keeper.card.consolidate must have already produced
// the lesson memory this reads.
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
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` left-operand filter

pub fn keeperJourneyAppend(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('keeper-journey-append','db') when no db — the db lane always
    // injects one, so that guard is unreachable here. Likewise JS reads `actor` from
    // `context.actor ?? 'keeper:auto'`, but the db lane only threads `input`+`db` (no context
    // object) — the only value reachable through the harness is the JS default 'keeper:auto'.
    let actor = json!("keeper:auto");

    if !truthy(input.get("card_slug")) {
        return Err("keeper-journey-append: card_slug is required".to_string());
    }
    let card_slug = input.get("card_slug").cloned().unwrap_or(Value::Null);

    let card = crate::db_json::query_rows(db, "SELECT slug, title FROM cards WHERE slug = ?", &[card_slug.clone()])?;
    if card.as_array().map(|a| a.is_empty()).unwrap_or(true) {
        return Err(format!("keeper-journey-append: card '{}' not found", tmpl(Some(&card_slug))));
    }

    let mems = crate::db_json::query_rows(
        db,
        "SELECT id, guidance, situation, tags_json FROM memory.memories
     WHERE source_card = ? AND type = 'lesson'
     ORDER BY id DESC LIMIT 1",
        &[card_slug.clone()],
    )?;
    let mem = match mems.as_array().and_then(|a| a.first()) {
        Some(m) => m.clone(),
        None => {
            return Err(format!(
                "keeper-journey-append: no lesson memory found for card '{}' — run keeper.card.consolidate first",
                tmpl(Some(&card_slug))
            ))
        }
    };
    let mem_id = mem.get("id").cloned().unwrap_or(Value::Null);

    // Idempotent: a journey row already linked to this card+memory means no-op.
    let existing = crate::db_json::query_rows(
        db,
        "SELECT id FROM project_journey WHERE card_slug = ? AND memory_id = ?",
        &[card_slug.clone(), mem_id.clone()],
    )?;
    if let Some(row) = existing.as_array().and_then(|a| a.first()) {
        let existing_id = row.get("id").cloned().unwrap_or(Value::Null);
        let mut out = Map::new();
        out.insert("card_slug".into(), card_slug.clone());
        out.insert("journey_id".into(), existing_id.clone());
        out.insert("memory_id".into(), mem_id.clone());
        out.insert("noop".into(), json!(true));
        out.insert(
            "message".into(),
            json!(format!("{} already in project_journey (id={}); no-op.", tmpl(Some(&card_slug)), tmpl(Some(&existing_id)))),
        );
        return Ok(Value::Object(out));
    }

    // tags = JSON.parse(mem.tags_json ?? '{}'); on ANY failure (parse error, or member access on a
    // non-object like null) JS's catch leaves verdict/lessons/metrics at their pre-declared
    // defaults — which is exactly what `.get(...)` returning None on a non-Object Value already
    // collapses to, so no explicit try/catch branch is needed here.
    let tags_str = match nz(mem.get("tags_json")) {
        Some(Value::String(s)) => s.clone(),
        _ => "{}".to_string(),
    };
    let mut verdict = Value::Null;
    let mut lessons: Vec<Value> = Vec::new();
    let mut metrics = json!({});
    if let Ok(tags) = serde_json::from_str::<Value>(&tags_str) {
        verdict = nz(tags.get("verdict")).cloned().unwrap_or(Value::Null);
        lessons = tags.get("lessons").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        metrics = nz(tags.get("metrics")).cloned().unwrap_or(json!({}));
    }

    let narrative_md = nz(mem.get("guidance")).cloned().unwrap_or(json!("(no narrative)"));
    let lessons_json = serde_json::to_string(&lessons).map_err(|e| e.to_string())?;
    let metrics_json = serde_json::to_string(&metrics).map_err(|e| e.to_string())?;

    let (_changes, journey_id) = crate::db_json::run_write(
        db,
        "INSERT INTO project_journey (card_slug, narrative_md, verdict, lessons_json, metrics_json, memory_id)
     VALUES (?, ?, ?, ?, ?, ?)",
        &[card_slug.clone(), narrative_md, verdict.clone(), json!(lessons_json), json!(metrics_json), mem_id.clone()],
    )?;

    // Swallowed try/catch: a failure here (e.g. the nested guard throwing) leaves phase_event_id null.
    let phase_event_id = match crate::entities_card_phase_event_record::cardPhaseEventRecord(
        &json!({
            "card_slug": card_slug,
            "phase": "journey_appended",
            "actor": actor,
            "evidence": { "journey_id": journey_id, "memory_id": mem_id, "verdict": verdict, "lessons_count": lessons.len() as i64 },
        }),
        db,
    ) {
        Ok(ev) => nz(ev.get("id")).cloned().unwrap_or(Value::Null),
        Err(_) => Value::Null,
    };

    let mut out = Map::new();
    out.insert("card_slug".into(), card_slug.clone());
    out.insert("journey_id".into(), json!(journey_id));
    out.insert("memory_id".into(), mem_id);
    out.insert("verdict".into(), verdict);
    out.insert("lessons_count".into(), json!(lessons.len() as i64));
    out.insert("phase_event_id".into(), phase_event_id);
    out.insert("noop".into(), json!(false));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory;
             CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT);
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, type TEXT, situation TEXT, guidance TEXT, tags_json TEXT, source_card TEXT);
             CREATE TABLE project_journey (id INTEGER PRIMARY KEY, card_slug TEXT, narrative_md TEXT, verdict TEXT, lessons_json TEXT, metrics_json TEXT, memory_id INTEGER);
             CREATE TABLE card_phase_events (id INTEGER PRIMARY KEY, card_slug TEXT, phase TEXT, actor TEXT, evidence_json TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn inserts_journey_row_and_fires_phase_event() {
        let c = db();
        c.execute("INSERT INTO cards VALUES ('EPIC-OK','card')", []).unwrap();
        c.execute(
            "INSERT INTO memory.memories (id,type,situation,guidance,tags_json,source_card) VALUES
             (10,'lesson','sit','clean shipping','{\"verdict\":\"pass\",\"lessons\":[\"lift slug\",\"skip PL\"],\"metrics\":{\"cost_usd\":0.01}}','EPIC-OK')",
            [],
        )
        .unwrap();
        let r = keeperJourneyAppend(&json!({ "card_slug": "EPIC-OK" }), &c).unwrap();
        assert_eq!(r["memory_id"], json!(10));
        assert_eq!(r["verdict"], json!("pass"));
        assert_eq!(r["lessons_count"], json!(2));
        assert_eq!(r["noop"], json!(false));
        assert_eq!(r["phase_event_id"], json!(1));
        let row = crate::db_json::query_rows(&c, "SELECT * FROM project_journey WHERE id = 1", &[]).unwrap()[0].clone();
        assert_eq!(row["narrative_md"], json!("clean shipping"));
        let ev = crate::db_json::query_rows(&c, "SELECT * FROM card_phase_events WHERE card_slug = 'EPIC-OK'", &[]).unwrap()[0].clone();
        assert_eq!(ev["phase"], json!("journey_appended"));
        assert_eq!(ev["actor"], json!("keeper:auto"));
    }

    #[test]
    fn idempotent_reruns_return_noop() {
        let c = db();
        c.execute("INSERT INTO cards VALUES ('EPIC-IDEM','c')", []).unwrap();
        c.execute(
            "INSERT INTO memory.memories (id,type,situation,guidance,tags_json,source_card) VALUES (5,'lesson','s','g','{}','EPIC-IDEM')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO project_journey (id,card_slug,narrative_md,verdict,lessons_json,metrics_json,memory_id) VALUES (42,'EPIC-IDEM','g',NULL,'[]','{}',5)",
            [],
        )
        .unwrap();
        let r = keeperJourneyAppend(&json!({ "card_slug": "EPIC-IDEM" }), &c).unwrap();
        assert_eq!(r, json!({ "card_slug": "EPIC-IDEM", "journey_id": 42, "memory_id": 5, "noop": true, "message": "EPIC-IDEM already in project_journey (id=42); no-op." }));
    }

    #[test]
    fn null_tags_and_null_guidance_fall_back_to_defaults() {
        let c = db();
        c.execute("INSERT INTO cards VALUES ('EPIC-NULL','c')", []).unwrap();
        c.execute(
            "INSERT INTO memory.memories (id,type,situation,guidance,tags_json,source_card) VALUES (7,'lesson','s',NULL,NULL,'EPIC-NULL')",
            [],
        )
        .unwrap();
        let r = keeperJourneyAppend(&json!({ "card_slug": "EPIC-NULL" }), &c).unwrap();
        assert_eq!(r["verdict"], Value::Null);
        assert_eq!(r["lessons_count"], json!(0));
        let row = crate::db_json::query_rows(&c, "SELECT narrative_md, metrics_json FROM project_journey WHERE id = 1", &[]).unwrap()[0].clone();
        assert_eq!(row["narrative_md"], json!("(no narrative)"));
        assert_eq!(row["metrics_json"], json!("{}"));
    }

    #[test]
    fn malformed_tags_json_falls_back_to_defaults() {
        let c = db();
        c.execute("INSERT INTO cards VALUES ('EPIC-BAD','c')", []).unwrap();
        c.execute(
            "INSERT INTO memory.memories (id,type,situation,guidance,tags_json,source_card) VALUES (8,'lesson','s','g8','{not json','EPIC-BAD')",
            [],
        )
        .unwrap();
        let r = keeperJourneyAppend(&json!({ "card_slug": "EPIC-BAD" }), &c).unwrap();
        assert_eq!(r["verdict"], Value::Null);
        assert_eq!(r["lessons_count"], json!(0));
    }

    #[test]
    fn non_array_lessons_field_defaults_to_empty() {
        let c = db();
        c.execute("INSERT INTO cards VALUES ('EPIC-NOARR','c')", []).unwrap();
        c.execute(
            "INSERT INTO memory.memories (id,type,situation,guidance,tags_json,source_card) VALUES (9,'lesson','s','g9','{\"verdict\":\"fail\",\"lessons\":\"notarray\",\"metrics\":{\"x\":1}}','EPIC-NOARR')",
            [],
        )
        .unwrap();
        let r = keeperJourneyAppend(&json!({ "card_slug": "EPIC-NOARR" }), &c).unwrap();
        assert_eq!(r["verdict"], json!("fail"));
        assert_eq!(r["lessons_count"], json!(0));
        let row = crate::db_json::query_rows(&c, "SELECT metrics_json FROM project_journey WHERE id = 1", &[]).unwrap()[0].clone();
        assert_eq!(row["metrics_json"], json!("{\"x\":1}"));
    }

    #[test]
    fn picks_latest_lesson_and_ignores_other_types() {
        let c = db();
        c.execute("INSERT INTO cards VALUES ('EPIC-MULTI','c')", []).unwrap();
        c.execute(
            "INSERT INTO memory.memories (id,type,situation,guidance,tags_json,source_card) VALUES
             (1,'lesson','s','old lesson','{\"verdict\":\"fail\"}','EPIC-MULTI'),
             (2,'correction','s','not a lesson',NULL,'EPIC-MULTI'),
             (3,'lesson','s','new lesson','{\"verdict\":\"pass\"}','EPIC-MULTI')",
            [],
        )
        .unwrap();
        let r = keeperJourneyAppend(&json!({ "card_slug": "EPIC-MULTI" }), &c).unwrap();
        assert_eq!(r["memory_id"], json!(3));
        assert_eq!(r["verdict"], json!("pass"));
    }

    #[test]
    fn missing_card_slug_and_not_found_errors() {
        let c = db();
        assert!(keeperJourneyAppend(&json!({}), &c).unwrap_err().contains("card_slug is required"));
        assert!(keeperJourneyAppend(&json!({ "card_slug": "NOPE" }), &c).unwrap_err().contains("card 'NOPE' not found"));
        c.execute("INSERT INTO cards VALUES ('EPIC-NOLESSON','c')", []).unwrap();
        assert!(keeperJourneyAppend(&json!({ "card_slug": "EPIC-NOLESSON" }), &c).unwrap_err().contains("no lesson memory found"));
    }
}
