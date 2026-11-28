//! interviewer-action-catalog.rs — twin of skills/agents/in/interviewer-action-catalog.mjs (db READ
//! lane). NAMES ARE CONTRACT: `interviewerActionCatalog` (default), plus the named helpers
//! `buildCatalog`/`namespaceOf` (each its own no-db fixture unit — census counts all 3 fns;
//! `NAMESPACE_PRIORITY` is data, not a unit). Produces a namespace-grouped, budget-capped action
//! catalog string for injection into the chat prompt.
use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const MAX_CATALOG_CHARS: usize = 8000;
const MAX_DESC_CHARS: usize = 70;
// First N actions (across all namespaces, in orderedNs walk order) show full descriptions; the
// rest show name+owner only.
const FULL_DESC_COUNT: usize = 40;

const NAMESPACE_PRIORITY: &[&str] = &[
    "board-keeper.",
    "system.driver.",
    "system.docs.",
    "system.memory.",
    "interviewer.",
    "keeper.",
    "pl.session.",
    "system.consistency.",
    "system.action.",
    "system.skill.",
    "system.agent.",
    "system.law.",
    "system.anatomy.",
    "contract.",
    "tl.",
    "qa.",
    "git-manager.",
    "system.",
    "man.",
    "god.",
    "card.",
];

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
// = JS String(v)
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
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// PROCESS-SCOPE MEMOIZATION: mirrors the JS closure `cache = { hash, catalog }` — the conform-db JS
// lane imports this module ONCE and calls interviewerActionCatalog across every case in one fixture
// file within the same process, so a case that repeats the same action rows sees cache_hit:true. The
// Rust conform binary likewise runs one process per fixture unit and loops its cases in that process,
// so a static cell (instead of a plain module var, which Rust has no direct equivalent of) reproduces
// the identical cross-case behavior. buildCatalog/namespaceOf never touch this — only the default does.
static CACHE: OnceLock<Mutex<(Option<String>, Option<String>)>> = OnceLock::new();
fn cache_cell() -> &'static Mutex<(Option<String>, Option<String>)> {
    CACHE.get_or_init(|| Mutex::new((None, None)))
}

fn hash_actions(rows: &[Value]) -> String {
    let mut hasher = Sha256::new();
    for r in rows {
        let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let owner = r.get("owner_agent").and_then(|v| v.as_str()).unwrap_or("");
        hasher.update(name.as_bytes());
        hasher.update(b"\0");
        hasher.update(owner.as_bytes());
    }
    hex::encode(hasher.finalize())
}

fn namespace_of_str(name: &str) -> String {
    for ns in NAMESPACE_PRIORITY {
        if name.starts_with(ns) {
            return (*ns).to_string();
        }
    }
    match name.find('.') {
        Some(idx) if idx > 0 => name[..idx + 1].to_string(),
        _ => name.to_string(),
    }
}

fn truncate(v: Option<&Value>, n: usize) -> String {
    if matches!(v, None | Some(Value::Null)) {
        return String::new();
    }
    let raw = str_qq(v);
    let first_line = raw.split('\n').next().unwrap_or("").trim().to_string();
    let count = first_line.chars().count();
    if count <= n {
        first_line
    } else {
        let head: String = first_line.chars().take(n.saturating_sub(1)).collect();
        format!("{head}\u{2026}")
    }
}

fn build_catalog_str(rows: &[Value]) -> String {
    // groups: NAMESPACE_PRIORITY keys first (insertion order), then any novel namespace appended
    // the first time it's encountered while walking rows — mirrors the JS Map's insertion order.
    let mut order: Vec<String> = NAMESPACE_PRIORITY.iter().map(|s| s.to_string()).collect();
    let mut groups: HashMap<String, Vec<&Value>> = HashMap::new();
    for ns in &order {
        groups.insert(ns.clone(), Vec::new());
    }
    for r in rows {
        let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let ns = namespace_of_str(name);
        if !groups.contains_key(&ns) {
            groups.insert(ns.clone(), Vec::new());
            order.push(ns.clone());
        }
        groups.get_mut(&ns).unwrap().push(r);
    }

    let mut blocks: Vec<String> = Vec::new();
    let mut shown_with_desc: usize = 0;
    for ns in &order {
        let mut arr: Vec<&Value> = groups.remove(ns).unwrap_or_default();
        if arr.is_empty() {
            continue;
        }
        arr.sort_by(|a, b| {
            let an = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let bn = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
            an.cmp(bn)
        });
        let mut lines = vec![format!("[{ns}*]")];
        for r in &arr {
            let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let owner = tmpl(r.get("owner_agent"));
            if shown_with_desc < FULL_DESC_COUNT {
                lines.push(format!("  {name} ({owner}) \u{2014} {}", truncate(r.get("description"), MAX_DESC_CHARS)));
                shown_with_desc += 1;
            } else {
                lines.push(format!("  {name} ({owner})"));
            }
        }
        blocks.push(lines.join("\n"));
    }

    let mut out = String::new();
    let mut included: usize = 0;
    for b in &blocks {
        let next = if out.is_empty() { b.clone() } else { format!("{out}\n{b}") };
        if next.chars().count() > MAX_CATALOG_CHARS {
            out = format!("{out}\n(\u{2026}{} more actions omitted to fit budget)", rows.len() - included);
            break;
        }
        out = next;
        included += b.split('\n').filter(|l| l.starts_with("  ")).count();
    }
    out
}

pub fn buildCatalog(rows: &Value) -> Value {
    let empty: Vec<Value> = Vec::new();
    let arr = rows.as_array().unwrap_or(&empty);
    json!(build_catalog_str(arr))
}

pub fn namespaceOf(name: &Value) -> Value {
    json!(namespace_of_str(name.as_str().unwrap_or("")))
}

pub fn interviewerActionCatalog(input: &Value, db: &Connection) -> Result<Value, String> {
    // EPIC-VIBE-CHAT-UNIFICATION M2: a pane session sees only the actions its base agent is
    // GRANTED (agent_actions, non-revoked). This branch never touches the module cache above.
    let for_agent_raw = input.get("for_agent");
    if truthy(for_agent_raw) {
        let for_agent = js_string(for_agent_raw.unwrap());
        let granted_v = crate::db_json::query_rows(
            db,
            "SELECT a.name, a.owner_agent, a.description FROM actions a \
             JOIN agent_actions g ON g.action_name = a.name AND g.agent_name = ? AND g.revoked_at IS NULL \
             ORDER BY a.name",
            &[json!(for_agent)],
        )?;
        let empty: Vec<Value> = Vec::new();
        let granted = granted_v.as_array().cloned().unwrap_or(empty);
        let catalog = build_catalog_str(&granted);
        return Ok(json!({
            "catalog": catalog,
            "action_count": granted.len() as i64,
            "cache_hit": false,
            "for_agent": for_agent,
        }));
    }

    let rows_v = crate::db_json::query_rows(db, "SELECT name, owner_agent, description FROM actions ORDER BY name", &[])?;
    let empty: Vec<Value> = Vec::new();
    let rows = rows_v.as_array().cloned().unwrap_or(empty);

    let h = hash_actions(&rows);
    {
        let cell = cache_cell().lock().unwrap();
        if cell.0.as_deref() == Some(h.as_str()) {
            if let Some(cached) = cell.1.clone() {
                return Ok(json!({ "catalog": cached, "action_count": rows.len() as i64, "cache_hit": true }));
            }
        }
    }

    let catalog = build_catalog_str(&rows);
    {
        let mut cell = cache_cell().lock().unwrap();
        cell.0 = Some(h);
        cell.1 = Some(catalog.clone());
    }
    Ok(json!({ "catalog": catalog, "action_count": rows.len() as i64, "cache_hit": false }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE actions (name TEXT, description TEXT, owner_agent TEXT);
             CREATE TABLE agent_actions (agent_name TEXT, action_name TEXT, revoked_at TEXT);
             INSERT INTO actions VALUES
               ('board-keeper.card.list','List all cards on the board.','board-keeper'),
               ('system.driver.tick','Advance the driver one tick.','system'),
               ('standalone','No dot in this name at all.','sys');
             INSERT INTO agent_actions VALUES ('dev','board-keeper.card.list',NULL);",
        )
        .unwrap();
    }

    #[test]
    fn groups_sorts_and_caches() {
        let conn = Connection::open_in_memory().unwrap();
        seed(&conn);
        let r1 = interviewerActionCatalog(&json!({}), &conn).unwrap();
        assert_eq!(r1["cache_hit"], json!(false));
        assert_eq!(r1["action_count"], json!(3));
        let cat = r1["catalog"].as_str().unwrap();
        assert!(cat.contains("[board-keeper.*]"));
        assert!(cat.contains("[standalone*]"));
        assert!(cat.contains("board-keeper.card.list (board-keeper)"));

        // repeat call, same rows -> module-level cache hits (mirrors the JS test "cached on repeat calls")
        let r2 = interviewerActionCatalog(&json!({}), &conn).unwrap();
        assert_eq!(r2["cache_hit"], json!(true));
        assert_eq!(r1["catalog"], r2["catalog"]);
    }

    #[test]
    fn for_agent_scopes_to_active_grants() {
        let conn = Connection::open_in_memory().unwrap();
        seed(&conn);
        let r = interviewerActionCatalog(&json!({ "for_agent": "dev" }), &conn).unwrap();
        assert_eq!(r["action_count"], json!(1));
        assert_eq!(r["cache_hit"], json!(false));
        assert_eq!(r["for_agent"], json!("dev"));
        let cat = r["catalog"].as_str().unwrap();
        assert!(cat.contains("board-keeper.card.list"));
        assert!(!cat.contains("system.driver.tick"));
    }

    #[test]
    fn namespace_and_truncate_helpers() {
        assert_eq!(namespaceOf(&json!("board-keeper.card.list")), json!("board-keeper."));
        assert_eq!(namespaceOf(&json!("custom.thing")), json!("custom."));
        assert_eq!(namespaceOf(&json!("standalone")), json!("standalone"));
        assert_eq!(namespaceOf(&json!(".leadingdot")), json!(".leadingdot"));

        let long = "x".repeat(80);
        let out = truncate(Some(&json!(long)), MAX_DESC_CHARS);
        assert_eq!(out.chars().count(), MAX_DESC_CHARS);
        assert!(out.ends_with('\u{2026}'));

        let rows = json!([
            { "name": "foo.bar", "owner_agent": "system", "description": "a very long description that exceeds the per-line cap and should get truncated with an ellipsis at the end to keep the catalog within budget across many actions" }
        ]);
        let cat = buildCatalog(&rows);
        assert!(cat.as_str().unwrap().contains("foo.bar"));
        assert!(cat.as_str().unwrap().contains('\u{2026}'));
    }
}
