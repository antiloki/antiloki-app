//! catalog-find.rs — twin of skills/entities/catalog-find.mjs (db READ lane, attached `memory`
//! and `laws` dbs). NAMES ARE CONTRACT: `catalogFind` (default). Catalog search over
//! skill/action/agent/law/memory-correction entities: pick a model (explicit `model` param, else
//! the most-common model already stored for `entity_type` in memory.catalog_embeddings), then
//! EITHER score by cosine similarity against pre-stored embeddings OR fall back to a deterministic
//! LIKE-based substring search over name+description (memory.skills / actions / agents /
//! laws.laws). `memory-correction` has no branch in the JS source's substring if/else chain — it
//! falls through with `rows` left `undefined`, and `rows.map(...)` throws; mirrored exactly here
//! rather than treated as a guard-throw to skip.
//!
//! PARITY-DEFERRED-partial: the `if (useModel) { ... }` vector-search block is not ported.
//! `embed()` (../external/embed.mjs) is a real local ML model call — genuinely unconvertible.
//! IMPORTANT correction to the branch this twin was originally scoped from: embed() runs
//! UNCONDITIONALLY whenever `useModel` is truthy, BEFORE the `stored.length > 0` check — there is
//! NO deterministic sub-branch reachable while useModel is truthy:
//!   - useModel auto-derived (no explicit `model`): the `GROUP BY model ORDER BY c DESC LIMIT 1`
//!     query can only return a row if memory.catalog_embeddings already has >=1 row for
//!     (entity_type, that model) — so the later `stored` query can never come back empty on this
//!     path; embed() always leads straight into real vector scoring.
//!   - useModel passed explicitly, even where memory.catalog_embeddings has ZERO matching rows:
//!     embed() still runs first — confirmed empirically against the real JS source, which attempts
//!     to fetch the Xenova/bge-base-en-v1.5 config from Hugging Face and throws "Unauthorized
//!     access to file..." in this sandboxed/offline environment. So "explicit model but zero
//!     stored embeddings" is NOT a deterministic fixture case either; it needs the same real ML
//!     call as full vector scoring, and its error text is network/environment-dependent.
//! The twin implements useModel RESOLUTION faithfully (the COUNT query) but treats
//! `if useModel truthy` as a hard boundary: it returns a descriptive Err (same "fail loudly on an
//! intentionally-unported branch" convention as with-audit.rs::read_entity_state) instead of
//! silently returning wrong data. This twin's fixtures never pass an explicit `model` and never
//! seed catalog_embeddings, so the boundary is never crossed.
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

const ENTITY_TYPES: [&str; 5] = ["skill", "action", "agent", "law", "memory-correction"];

fn entity_query_sql(entity_type: &str) -> &'static str {
    match entity_type {
        "skill" => "SELECT * FROM memory.skills WHERE name = ?",
        "action" => "SELECT * FROM actions WHERE name = ?",
        "agent" => "SELECT * FROM agents WHERE name = ?",
        "law" => "SELECT * FROM laws.laws WHERE slug = ?",
        _ => "SELECT * FROM memory.memories WHERE id = CAST(REPLACE(?, 'mem-', '') AS INTEGER)",
    }
}

pub fn catalogFind(input: &Value, db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));

    let query = match get("query") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Err("catalog-find: query is required".into()),
    };
    let entity_type_v = get("entity_type").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&entity_type_v)) {
        return Err("catalog-find: entity_type is required".into());
    }
    let entity_type = match entity_type_v.as_str() {
        Some(s) if ENTITY_TYPES.contains(&s) => s,
        _ => {
            return Err(format!(
                "catalog-find: entity_type must be one of: {} (got '{}')",
                ENTITY_TYPES.join(", "),
                tmpl(Some(&entity_type_v))
            ));
        }
    };

    let limit = get("limit").cloned().unwrap_or(json!(10));
    let fallback = match get("fallback") {
        None => true,
        Some(v) => truthy(Some(v)),
    };

    // Pick model: explicit > most-used model in catalog_embeddings.
    let mut use_model = get("model").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&use_model)) {
        let row = crate::db_json::query_rows(
            db,
            "SELECT model, COUNT(*) AS c FROM memory.catalog_embeddings WHERE entity_type = ? GROUP BY model ORDER BY c DESC LIMIT 1",
            &[json!(entity_type)],
        )?;
        use_model = row
            .as_array()
            .and_then(|a| a.first())
            .and_then(|r| r.get("model"))
            .cloned()
            .unwrap_or(Value::Null);
    }

    // Vector-search path — see module doc: unconvertible (needs the real embed() ML call),
    // unconditionally entered whenever useModel is truthy. Never reached by this twin's fixtures.
    if truthy(Some(&use_model)) {
        return Err(format!(
            "catalog-find twin: vector-search branch requires embed() (real local ML model call, ../external/embed.mjs) — not ported. useModel resolved to '{}'; pass no explicit model and leave memory.catalog_embeddings empty for entity_type '{}' to stay on the deterministic substring-fallback path.",
            tmpl(Some(&use_model)),
            entity_type
        ));
    }

    // Fallback: substring search over name + description.
    if fallback {
        let like = json!(format!("%{}%", query.to_lowercase()));
        let rows: Option<Value> = match entity_type {
            "skill" => Some(crate::db_json::query_rows(
                db,
                "SELECT name, description FROM memory.skills WHERE LOWER(name) LIKE ? OR LOWER(COALESCE(description, '')) LIKE ? LIMIT ?",
                &[like.clone(), like.clone(), limit.clone()],
            )?),
            "action" => Some(crate::db_json::query_rows(
                db,
                "SELECT name, description FROM actions WHERE LOWER(name) LIKE ? OR LOWER(description) LIKE ? LIMIT ?",
                &[like.clone(), like.clone(), limit.clone()],
            )?),
            "agent" => Some(crate::db_json::query_rows(
                db,
                "SELECT name, description FROM agents WHERE LOWER(name) LIKE ? OR LOWER(description) LIKE ? LIMIT ?",
                &[like.clone(), like.clone(), limit.clone()],
            )?),
            "law" => Some(crate::db_json::query_rows(
                db,
                "SELECT slug AS name, title AS description FROM laws.laws WHERE superseded_by IS NULL AND (LOWER(slug) LIKE ? OR LOWER(title) LIKE ? OR LOWER(statement) LIKE ? OR LOWER(rationale) LIKE ?) LIMIT ?",
                &[like.clone(), like.clone(), like.clone(), like.clone(), limit.clone()],
            )?),
            // 'memory-correction' has no branch in the JS if/else chain — `rows` stays
            // `undefined`; `rows.map(...)` below throws. Mirrored exactly (not a guard-throw).
            _ => None,
        };
        // JS: `const entityQ = db.prepare(ENTITY_QUERIES[entity_type]);` runs unconditionally
        // BEFORE `rows.map` — irrelevant to a well-formed fixture (the table always exists), so
        // only the `rows === undefined` consequence is reproduced here.
        let rows = match rows {
            Some(r) => r,
            None => return Err("Cannot read properties of undefined (reading 'map')".into()),
        };
        let entity_sql = entity_query_sql(entity_type);
        let mut out: Vec<Value> = Vec::new();
        for r in rows.as_array().cloned().unwrap_or_default() {
            let name = r.get("name").cloned().unwrap_or(Value::Null);
            let entity_rows = crate::db_json::query_rows(db, entity_sql, &[name.clone()])?;
            let entity = entity_rows.as_array().and_then(|a| a.first()).cloned();
            let mut o = Map::new();
            o.insert("entity_name".into(), name);
            o.insert("score".into(), Value::Null);
            o.insert("source".into(), json!("substring"));
            // JS `{ ..., entity: entityQ.get(r.name) }`: an undefined `.get()` result is a key
            // set to `undefined`, which JSON.stringify OMITS — mirrored by not inserting the key.
            if let Some(e) = entity {
                o.insert("entity".into(), e);
            }
            out.push(Value::Object(o));
        }
        return Ok(json!(out));
    }

    Ok(json!([]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory; ATTACH ':memory:' AS laws;
             CREATE TABLE memory.catalog_embeddings (entity_type TEXT NOT NULL, entity_name TEXT NOT NULL, model TEXT NOT NULL, dims INTEGER NOT NULL, vector BLOB NOT NULL);
             CREATE TABLE memory.skills (name TEXT, description TEXT);
             CREATE TABLE actions (name TEXT, description TEXT);
             CREATE TABLE agents (name TEXT, description TEXT);
             CREATE TABLE laws.laws (slug TEXT, title TEXT, statement TEXT, rationale TEXT, superseded_by INTEGER);
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, title TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn skill_substring_matches_name_and_description() {
        let c = db();
        c.execute_batch(
            "INSERT INTO memory.skills (name, description) VALUES
             ('card-add', 'adds a card to the board'),
             ('audit-log', 'writes an audit entry'),
             ('trace-tool', 'used for audit trail review');",
        )
        .unwrap();
        let r = catalogFind(&json!({ "query": "audit", "entity_type": "skill" }), &c).unwrap();
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["entity_name"], json!("audit-log"));
        assert_eq!(arr[0]["source"], json!("substring"));
        assert!(arr[0]["score"].is_null());
        assert_eq!(arr[0]["entity"]["description"], json!("writes an audit entry"));
        assert_eq!(arr[1]["entity_name"], json!("trace-tool"));
    }

    #[test]
    fn skill_substring_no_matches_returns_empty() {
        let c = db();
        c.execute("INSERT INTO memory.skills (name, description) VALUES ('card-add', 'adds a card')", []).unwrap();
        let r = catalogFind(&json!({ "query": "zzznomatch", "entity_type": "skill" }), &c).unwrap();
        assert_eq!(r, json!([]));
    }

    #[test]
    fn action_and_agent_substring_search() {
        let c = db();
        c.execute("INSERT INTO actions (name, description) VALUES ('embed-all', 'embed catalog')", []).unwrap();
        let r = catalogFind(&json!({ "query": "embed", "entity_type": "action" }), &c).unwrap();
        assert_eq!(r.as_array().unwrap().len(), 1);

        c.execute("INSERT INTO agents (name, description) VALUES ('dev', 'development agent')", []).unwrap();
        let r2 = catalogFind(&json!({ "query": "agent", "entity_type": "agent" }), &c).unwrap();
        assert_eq!(r2[0]["entity_name"], json!("dev"));
    }

    #[test]
    fn law_substring_excludes_superseded_and_aliases_slug_title() {
        let c = db();
        c.execute_batch(
            "INSERT INTO laws.laws (slug, title, superseded_by) VALUES
             ('NEVER_BYPASS_WITH_AUDIT', 'All mutations go through with-audit', NULL),
             ('OLD_LAW_SUPERSEDED', 'old audit rule', 1);",
        )
        .unwrap();
        let r = catalogFind(&json!({ "query": "audit", "entity_type": "law" }), &c).unwrap();
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 1, "superseded law excluded even though its title matches");
        assert_eq!(arr[0]["entity_name"], json!("NEVER_BYPASS_WITH_AUDIT"));
        assert_eq!(arr[0]["entity"]["slug"], json!("NEVER_BYPASS_WITH_AUDIT"));
    }

    #[test]
    fn memory_correction_has_no_substring_branch_and_throws() {
        let c = db();
        let e = catalogFind(&json!({ "query": "x", "entity_type": "memory-correction" }), &c).unwrap_err();
        assert_eq!(e, "Cannot read properties of undefined (reading 'map')");
    }

    #[test]
    fn bad_entity_type_throws_enum_violation() {
        let c = db();
        let e = catalogFind(&json!({ "query": "x", "entity_type": "bogus" }), &c).unwrap_err();
        assert_eq!(e, "catalog-find: entity_type must be one of: skill, action, agent, law, memory-correction (got 'bogus')");
    }

    #[test]
    fn fallback_false_with_no_stored_embeddings_returns_empty() {
        let c = db();
        let r = catalogFind(&json!({ "query": "whatever", "entity_type": "skill", "fallback": false }), &c).unwrap();
        assert_eq!(r, json!([]));
    }

    #[test]
    fn missing_query_or_entity_type_throws_required() {
        let c = db();
        assert_eq!(catalogFind(&json!({ "entity_type": "skill" }), &c).unwrap_err(), "catalog-find: query is required");
        assert_eq!(catalogFind(&json!({ "query": "x" }), &c).unwrap_err(), "catalog-find: entity_type is required");
    }
}
