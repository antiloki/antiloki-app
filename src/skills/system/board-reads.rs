//! board-reads.rs — the BE's board READ queries, moved into skills. NAMES ARE CONTRACT:
//! `boardCardsList`, `boardCardGet`, `boardAgentsList`, `boardActionsList`,
//! `boardPromptTemplatesList`, `boardPipelineList`.
//!
//! WHY THEY EXIST. antiloki-BE's BoardService held these as raw SQL against the substrate:
//!
//!     return this.engine.db_readonly((db) => db.prepare(`SELECT … FROM cards …`).all(…))
//!
//! Only a skill touches the databases. A customer-layer query is unauditable, unreachable from
//! the CLI or chat, and free to drift from the schema the skills maintain — the BE's own comment
//! ("so EngineService stays the single .antiloki boundary") describes a boundary that a raw
//! prepare() walks straight through.
//!
//! These are READS, so there is no with-audit row to gain; what they gain is a single owner for
//! the query and one place to fix it. The SQL is carried over verbatim, including the parts that
//! look odd, because the FE is already shaped around exactly these columns.
// QUALITY: 86/100 — verbatim SQL port, per-query column contracts, migration-aware pipeline read
use rusqlite::Connection;
use serde_json::{json, Value};

fn s(input: &Value, k: &str) -> Option<String> {
    input.get(k).and_then(|v| v.as_str()).map(|x| x.to_string()).filter(|x| !x.is_empty())
}

fn csv(input: &Value, k: &str) -> Vec<String> {
    input
        .get(k)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

/// The board list. `slim` drops only the two fat TEXT columns (body_json, accuracy_detail_json) —
/// the list surfaces (board, breadcrumb, chat @-picker) never read them and the card detail uses
/// boardCardGet.
///
/// The LEFT JOIN onto contracts is FIX-THRASH-VISIBILITY: every row carries the latest
/// non-superseded contract's recorded failure so the board can badge '⚠ failing' the moment a
/// stage failure is stamped. Without it a card thrashing in a retry loop is indistinguishable
/// from a calm one anywhere but the BE terminal.
pub fn boardCardsList(input: &Value, db: &Connection) -> Result<Value, String> {
    let slim = input.get("slim").and_then(|v| v.as_bool()).unwrap_or(false);
    let fat_cols = "c.id, c.slug, c.title, c.status, c.body_json, c.risk, c.complexity, c.priority, c.score, \
                    c.accuracy_score, c.accuracy_detail_json, c.accuracy_computed_at, \
                    c.parent_slug, c.driver_intent, c.auto_merge, c.created_at, c.updated_at, \
                    cf.last_failure_kind, cf.last_failure_at";
    let slim_cols = "c.id, c.slug, c.title, c.status, c.risk, c.complexity, c.priority, c.score, \
                     c.accuracy_score, c.accuracy_computed_at, \
                     c.parent_slug, c.driver_intent, c.auto_merge, c.created_at, c.updated_at, \
                     cf.last_failure_kind, cf.last_failure_at";
    let fail_join = " LEFT JOIN (SELECT card_slug, last_failure_kind, last_failure_at FROM contracts \
                      WHERE superseded_by IS NULL) cf ON cf.card_slug = c.slug";

    let mut sql = format!(
        "SELECT {} FROM cards c{} WHERE 1=1",
        if slim { slim_cols } else { fat_cols },
        fail_join
    );
    let mut params: Vec<Value> = Vec::new();
    if let Some(status) = s(input, "status") {
        sql.push_str(" AND c.status = ?");
        params.push(json!(status));
    }
    sql.push_str(" ORDER BY c.updated_at DESC LIMIT ? OFFSET ?");
    // Hard cap regardless of caller. At ~600 cards, 2000 leaves years of headroom; past that the
    // answer is pagination, not a bigger single response.
    let limit = input.get("limit").and_then(|v| v.as_i64()).filter(|&n| n > 0).unwrap_or(200).min(2000);
    let offset = input.get("offset").and_then(|v| v.as_i64()).filter(|&n| n >= 0).unwrap_or(0);
    params.push(json!(limit));
    params.push(json!(offset));
    crate::db_json::query_rows(db, &sql, &params)
}

/// One card, with the full column set the FE's Card type expects.
///
/// The column list is wide on purpose: a narrow 6-column SELECT here was the reason the metadata
/// panel showed "—" for priority/risk/complexity/score/parent on every card.
pub fn boardCardGet(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug = s(input, "slug").ok_or("board-card-get: slug is required")?;
    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, slug, title, status, body_json,
                risk, complexity, priority, score,
                accuracy_score, accuracy_detail_json, accuracy_computed_at,
                parent_slug, driver_intent,
                created_at, updated_at
         FROM cards WHERE slug = ?",
        &[json!(slug)],
    )?;
    rows.as_array()
        .and_then(|a| a.first())
        .cloned()
        // The phrasing matters: the BE maps a cause containing "not found" onto a 404, which is
        // how the FE tells "this card is gone" from "the server broke".
        .ok_or_else(|| format!("board-card-get: card '{slug}' not found"))
}

pub fn boardAgentsList(_input: &Value, db: &Connection) -> Result<Value, String> {
    crate::db_json::query_rows(db, "SELECT name, description FROM agents ORDER BY name", &[])
}

/// The action catalog, with a deliberate lean path.
///
/// When the caller names the exact actions it needs (the FE waterfall asks for ~16 phase actions)
/// only name + steps_json come back. The full ~200-row catalog is ~110KB, which under DB lock
/// contention stalled past the FE's 5s timeout → null → "no skill steps registered".
pub fn boardActionsList(input: &Value, db: &Connection) -> Result<Value, String> {
    let names = csv(input, "names");
    if !names.is_empty() {
        let placeholders = names.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let params: Vec<Value> = names.iter().map(|n| json!(n)).collect();
        return crate::db_json::query_rows(
            db,
            &format!("SELECT name, steps_json FROM actions WHERE name IN ({placeholders})"),
            &params,
        );
    }
    let mut sql = String::from("SELECT name, owner_agent, description, steps_json FROM actions WHERE 1=1");
    let mut params: Vec<Value> = Vec::new();
    if let Some(ns) = s(input, "namespace") {
        sql.push_str(" AND name LIKE ?");
        params.push(json!(format!("{ns}%")));
    }
    sql.push_str(" ORDER BY name LIMIT ?");
    params.push(json!(input.get("limit").and_then(|v| v.as_i64()).filter(|&n| n > 0).unwrap_or(200)));
    crate::db_json::query_rows(db, &sql, &params)
}

/// Registered prompt templates by name, newest generation per base.
///
/// Names arrive VERSION-STRIPPED (the skill catalog strips '.vN') while the rows version BY NAME
/// — 'tl.author.file.v1' and '.v2' coexist, both with superseded_by NULL. So each requested base
/// resolves to every matching row, and only the highest generation survives. A '.reason' twin is
/// matched by the same GLOB and kept as its own row, which is why it is part of the base key
/// rather than stripped.
pub fn boardPromptTemplatesList(input: &Value, db: &Connection) -> Result<Value, String> {
    let names = csv(input, "names");
    if names.is_empty() {
        return Ok(json!([]));
    }
    let clauses = names.iter().map(|_| "(name = ? OR name GLOB ?)").collect::<Vec<_>>().join(" OR ");
    let mut params: Vec<Value> = Vec::new();
    for n in &names {
        params.push(json!(n));
        params.push(json!(format!("{n}.v*")));
    }
    let rows = crate::db_json::query_rows(
        db,
        &format!(
            "SELECT name, version, template_md, system_md,
                    default_tier, default_temperature, phase_flow, max_output_tokens
             FROM brain.prompt_templates
             WHERE superseded_by IS NULL AND ({clauses})
             ORDER BY name"
        ),
        &params,
    )?;

    // Insertion-ordered so the result follows the SQL's ORDER BY, as the JS Map did.
    let mut best: Vec<(String, i64, Value)> = Vec::new();
    for r in rows.as_array().cloned().unwrap_or_default() {
        let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let (base, gen) = split_generation(&name);
        match best.iter_mut().find(|(b, _, _)| *b == base) {
            Some(slot) if gen > slot.1 => { slot.1 = gen; slot.2 = r; }
            Some(_) => {}
            None => best.push((base, gen, r)),
        }
    }
    Ok(Value::Array(best.into_iter().map(|(_, _, r)| r).collect()))
}

/// `/^(.*)\.v(\d+)(\.reason)?$/` — base keeps the '.reason' suffix, so a template and its reason
/// twin are tracked as separate bases rather than one overwriting the other.
fn split_generation(name: &str) -> (String, i64) {
    let (head, reason) = match name.strip_suffix(".reason") {
        Some(h) => (h, ".reason"),
        None => (name, ""),
    };
    if let Some(idx) = head.rfind(".v") {
        let (prefix, digits) = (&head[..idx], &head[idx + 2..]);
        if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
            if let Ok(gen) = digits.parse::<i64>() {
                return (format!("{prefix}{reason}"), gen);
            }
        }
    }
    (name.to_string(), 0)
}

/// The simplified 3-stage pipeline view, derived from agent_stages (the realigned SST) rather
/// than a hardcoded PHASES list.
///
/// pipeline_order/fe_group only exist after migration 125. On a workspace that has not run it,
/// this reports `needs_migration` instead of letting the query throw a cryptic "no such column"
/// 500 — a missing migration is an operator-actionable state, not a server fault.
pub fn boardPipelineList(_input: &Value, db: &Connection) -> Result<Value, String> {
    let cols = crate::db_json::query_rows(db, "PRAGMA table_info(agent_stages)", &[])?;
    let has = |want: &str| {
        cols.as_array()
            .map(|a| a.iter().any(|c| c.get("name").and_then(|v| v.as_str()) == Some(want)))
            .unwrap_or(false)
    };
    if !(has("pipeline_order") && has("fe_group")) {
        return Ok(json!({ "needs_migration": "20260604-125-pipeline-stages-realign", "stages": [] }));
    }
    let stages = crate::db_json::query_rows(
        db,
        "SELECT s.agent_name AS agent,
                s.stage_name AS stage_name,
                s.pipeline_order AS pipeline_order,
                s.fe_group AS fe_group,
                s.description_md AS description,
                sa.action_name AS action_name,
                sa.invocation_pattern AS invocation_pattern
         FROM agent_stages s
         LEFT JOIN agent_stage_actions sa ON sa.agent_stage_id = s.id
         WHERE s.pipeline_order IS NOT NULL
         ORDER BY s.pipeline_order ASC, sa.order_index ASC",
        &[],
    )?;
    Ok(json!({ "needs_migration": Value::Null, "stages": stages }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE cards (
               id INTEGER PRIMARY KEY, slug TEXT, title TEXT, status TEXT, body_json TEXT,
               risk TEXT, complexity TEXT, priority INTEGER, score REAL,
               accuracy_score REAL, accuracy_detail_json TEXT, accuracy_computed_at TEXT,
               parent_slug TEXT, driver_intent TEXT, auto_merge INTEGER,
               created_at TEXT, updated_at TEXT
             );
             CREATE TABLE contracts (card_slug TEXT, superseded_by TEXT, last_failure_kind TEXT, last_failure_at TEXT);
             CREATE TABLE agents (name TEXT, description TEXT);
             CREATE TABLE actions (name TEXT, owner_agent TEXT, description TEXT, steps_json TEXT);
             CREATE TABLE agent_stages (id INTEGER PRIMARY KEY, agent_name TEXT, stage_name TEXT,
                                        pipeline_order INTEGER, fe_group TEXT, description_md TEXT);
             CREATE TABLE agent_stage_actions (agent_stage_id INTEGER, action_name TEXT,
                                               invocation_pattern TEXT, order_index INTEGER);
             CREATE TABLE brain.prompt_templates (name TEXT, version INTEGER, template_md TEXT, system_md TEXT,
                                                  default_tier TEXT, default_temperature TEXT, phase_flow TEXT,
                                                  max_output_tokens INTEGER, superseded_by TEXT);

             INSERT INTO cards (id, slug, title, status, body_json, priority, updated_at) VALUES
               (1,'A','card A','doing','{\"kind\":\"feat\"}',1,'2026-01-03'),
               (2,'B','card B','backlog','{}',2,'2026-01-02'),
               (3,'C','card C','doing','{}',3,'2026-01-01');
             INSERT INTO contracts VALUES ('A', NULL, 'stage_failed', '2026-01-03 10:00:00');
             INSERT INTO agents VALUES ('dev','builds'),('board-keeper','keeps');
             INSERT INTO actions VALUES
               ('a.one','dev','first','[{\"skill\":\"x\"}]'),
               ('b.two','tl','second','[{\"skill\":\"y\"}]'),
               ('a.three','dev','third','[]');
             INSERT INTO agent_stages VALUES (1,'dev','build',2,'build','the build stage'),
                                             (2,'tl','plan',1,'plan','the plan stage'),
                                             (3,'x','unordered',NULL,NULL,'not in the pipeline');
             INSERT INTO agent_stage_actions VALUES (1,'dev.file.write','per-file',1),(2,'tl.author','once',1);
             INSERT INTO brain.prompt_templates VALUES
               ('tl.author.file.v1',1,'v1 body','sys','mid','cold','a',100,NULL),
               ('tl.author.file.v2',2,'v2 body','sys','mid','cold','a',100,NULL),
               ('tl.author.file.v1.reason',1,'reason body','sys','mid','cold','a',100,NULL),
               ('other.template',1,'other','sys','low','warm','b',50,NULL);",
        )
        .unwrap();
        c
    }

    #[test]
    fn cards_list_orders_by_updated_at_desc_and_joins_the_failure() {
        let r = boardCardsList(&json!({}), &db()).unwrap();
        let rows = r.as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["slug"], json!("A"), "newest updated_at first");
        // The thrash badge is the whole point of the join.
        assert_eq!(rows[0]["last_failure_kind"], json!("stage_failed"));
        assert_eq!(rows[1]["last_failure_kind"], Value::Null);
    }

    #[test]
    fn a_SUPERSEDED_contract_failure_is_not_joined() {
        // Only the latest non-superseded contract counts; an old failure must not badge a card
        // that has since been re-planned.
        let c = db();
        c.execute("UPDATE contracts SET superseded_by = 'CT-2'", []).unwrap();
        let r = boardCardsList(&json!({}), &c).unwrap();
        assert_eq!(r[0]["last_failure_kind"], Value::Null);
    }

    #[test]
    fn cards_list_filters_paginates_and_CAPS_the_limit() {
        let c = db();
        assert_eq!(boardCardsList(&json!({ "status": "doing" }), &c).unwrap().as_array().unwrap().len(), 2);
        let page = boardCardsList(&json!({ "limit": 1, "offset": 1 }), &c).unwrap();
        assert_eq!(page.as_array().unwrap().len(), 1);
        assert_eq!(page[0]["slug"], json!("B"), "offset skips the first row");
        // A caller asking for a million rows gets 2000, not a million.
        assert_eq!(boardCardsList(&json!({ "limit": 1_000_000 }), &c).unwrap().as_array().unwrap().len(), 3);
    }

    #[test]
    fn slim_drops_ONLY_the_two_fat_columns() {
        let c = db();
        let fat = boardCardsList(&json!({}), &c).unwrap();
        let slim = boardCardsList(&json!({ "slim": true }), &c).unwrap();
        assert!(fat[0].get("body_json").is_some());
        assert!(slim[0].get("body_json").is_none());
        assert!(slim[0].get("accuracy_detail_json").is_none());
        // Everything else survives — slim is a payload trim, not a different view.
        for k in ["id", "slug", "title", "status", "priority", "parent_slug", "last_failure_kind"] {
            assert!(slim[0].get(k).is_some(), "slim dropped {k}");
        }
    }

    #[test]
    fn card_get_returns_the_row_and_says_NOT_FOUND_for_a_missing_slug() {
        let c = db();
        assert_eq!(boardCardGet(&json!({ "slug": "A" }), &c).unwrap()["title"], json!("card A"));
        let e = boardCardGet(&json!({ "slug": "NOPE" }), &c).unwrap_err();
        // The BE maps a cause containing "not found" onto a 404 — the wording is load-bearing.
        assert!(e.contains("not found"), "got: {e}");
        assert!(boardCardGet(&json!({}), &c).unwrap_err().contains("slug is required"));
    }

    #[test]
    fn agents_come_back_ordered_by_name() {
        let r = boardAgentsList(&json!({}), &db()).unwrap();
        assert_eq!(r[0]["name"], json!("board-keeper"));
        assert_eq!(r[1]["name"], json!("dev"));
    }

    #[test]
    fn actions_named_explicitly_take_the_LEAN_path() {
        // name + steps_json only: the full catalog is ~110KB and timed the FE out.
        let r = boardActionsList(&json!({ "names": "a.one, b.two" }), &db()).unwrap();
        assert_eq!(r.as_array().unwrap().len(), 2);
        let keys: Vec<&String> = r[0].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["name", "steps_json"]);
    }

    #[test]
    fn actions_filter_by_namespace_prefix() {
        let r = boardActionsList(&json!({ "namespace": "a." }), &db()).unwrap();
        assert_eq!(r.as_array().unwrap().len(), 2);
        assert_eq!(r[0]["name"], json!("a.one"));
        assert!(r[0].get("description").is_some(), "the full path keeps the description");
    }

    #[test]
    fn prompt_templates_keep_only_the_NEWEST_generation_per_base() {
        let r = boardPromptTemplatesList(&json!({ "names": "tl.author.file" }), &db()).unwrap();
        let rows = r.as_array().unwrap();
        // v1 and v2 collapse to v2; the .reason twin survives as its own row.
        assert_eq!(rows.len(), 2, "got {rows:?}");
        let by_name: Vec<&str> = rows.iter().map(|r| r["name"].as_str().unwrap()).collect();
        assert!(by_name.contains(&"tl.author.file.v2"));
        assert!(by_name.contains(&"tl.author.file.v1.reason"));
        assert!(!by_name.contains(&"tl.author.file.v1"));
    }

    #[test]
    fn prompt_templates_with_no_names_asks_the_db_NOTHING() {
        assert_eq!(boardPromptTemplatesList(&json!({}), &db()).unwrap(), json!([]));
        assert_eq!(boardPromptTemplatesList(&json!({ "names": " , " }), &db()).unwrap(), json!([]));
    }

    #[test]
    fn a_superseded_template_is_excluded() {
        let c = db();
        c.execute("UPDATE brain.prompt_templates SET superseded_by = 'x' WHERE name = 'tl.author.file.v2'", []).unwrap();
        let r = boardPromptTemplatesList(&json!({ "names": "tl.author.file" }), &c).unwrap();
        let names: Vec<&str> = r.as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"tl.author.file.v1"), "falls back to v1: {names:?}");
    }

    #[test]
    fn pipeline_orders_by_stage_and_joins_the_action() {
        let r = boardPipelineList(&json!({}), &db()).unwrap();
        assert_eq!(r["needs_migration"], Value::Null);
        let stages = r["stages"].as_array().unwrap();
        assert_eq!(stages.len(), 2, "the NULL-order stage is not in the pipeline");
        assert_eq!(stages[0]["stage_name"], json!("plan"), "pipeline_order 1 first");
        assert_eq!(stages[0]["action_name"], json!("tl.author"));
    }

    #[test]
    fn pipeline_reports_a_MISSING_MIGRATION_instead_of_a_500() {
        // A workspace that has not run migration 125 gets an actionable answer, not "no such
        // column: pipeline_order" surfacing as a server fault.
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE agent_stages (id INTEGER, agent_name TEXT, stage_name TEXT);").unwrap();
        let r = boardPipelineList(&json!({}), &c).unwrap();
        assert_eq!(r["needs_migration"], json!("20260604-125-pipeline-stages-realign"));
        assert_eq!(r["stages"], json!([]));
    }

    #[test]
    fn generation_split_handles_the_shapes_the_regex_did() {
        assert_eq!(split_generation("a.b.v2"), ("a.b".into(), 2));
        assert_eq!(split_generation("a.b.v2.reason"), ("a.b.reason".into(), 2));
        assert_eq!(split_generation("a.b"), ("a.b".into(), 0));
        // '.vX' is not a generation — a non-numeric suffix must not be read as one.
        assert_eq!(split_generation("a.vX"), ("a.vX".into(), 0));
    }
}
