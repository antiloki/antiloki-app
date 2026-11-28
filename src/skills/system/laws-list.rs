//! laws-list.rs — twin of skills/system/laws-list.mjs (db READ lane, cross-db laws via ATTACH).
//! NAMES ARE CONTRACT: `lawsList` (default). Active (non-superseded) laws filtered by
//! kind/domain/enforcement, ORDER BY kind, slug, mapped to the FE Law shape (body_md =
//! statement + optional "**Why:**" rationale, active = enforcement !== 'archived').
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn js_string(v:&Value)->String{ match v{Value::Null=>"null".into(),Value::String(s)=>s.clone(),Value::Number(n)=>n.to_string(),Value::Bool(b)=>b.to_string(),Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","),Value::Object(_)=>"[object Object]".into()} } // = JS String(v)
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>js_string(x) } } // = JS `${v}`

pub fn lawsList(input: &Value, db: &Connection) -> Result<Value, String> {
    let kind = input.get("kind");
    let domain = input.get("domain");
    let enforcement = input.get("enforcement");

    let mut wheres: Vec<&str> = vec!["superseded_at IS NULL"];
    let mut params: Vec<Value> = Vec::new();
    // JS binds named @params; positional ? in the same push order is equivalent.
    if truthy(kind) { wheres.push("kind = ?"); params.push(json!(js_string(kind.unwrap()))); }
    if truthy(domain) { wheres.push("domain = ?"); params.push(json!(js_string(domain.unwrap()))); }
    if truthy(enforcement) { wheres.push("enforcement = ?"); params.push(json!(js_string(enforcement.unwrap()))); }

    let sql = format!(
        "\n      SELECT id, slug, kind, domain, title, statement, rationale,\n             enforcement, enforcement_hook, established_at, established_by\n      FROM laws.laws\n      WHERE {}\n      ORDER BY kind, slug\n    ",
        wheres.join(" AND ")
    );
    let rows = match crate::db_json::query_rows(db, &sql, &params) {
        Ok(v) => v,
        // JS catch {}: laws.sqlite not attached in some test contexts
        Err(_) => return Ok(json!([])),
    };

    let out: Vec<Value> = rows.as_array().cloned().unwrap_or_default().iter().map(|r| {
        let statement = r.get("statement").cloned().unwrap_or(Value::Null);
        let rationale = r.get("rationale");
        let body_md = if truthy(rationale) {
            format!("{}\n\n**Why:** {}", js_string(&statement), tmpl(rationale))
        } else {
            js_string(&statement) // JS `stmt + ''` (null statement renders "null")
        };
        let active = r.get("enforcement").and_then(|v| v.as_str()) != Some("archived");
        let mut o = Map::new();
        o.insert("id".into(), r.get("id").cloned().unwrap_or(Value::Null));
        o.insert("slug".into(), r.get("slug").cloned().unwrap_or(Value::Null));
        o.insert("kind".into(), r.get("kind").cloned().unwrap_or(Value::Null));
        o.insert("domain".into(), r.get("domain").cloned().unwrap_or(Value::Null));
        o.insert("title".into(), r.get("title").cloned().unwrap_or(Value::Null));
        o.insert("enforcement".into(), r.get("enforcement").cloned().unwrap_or(Value::Null));
        o.insert("body_md".into(), json!(body_md));
        o.insert("hook".into(), r.get("enforcement_hook").cloned().unwrap_or(Value::Null));
        o.insert("active".into(), json!(active));
        o.insert("added_at".into(), r.get("established_at").cloned().unwrap_or(Value::Null));
        o.insert("added_by".into(), r.get("established_by").cloned().unwrap_or(Value::Null));
        Value::Object(o)
    }).collect();
    Ok(json!(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; \
          CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT, kind TEXT, domain TEXT, title TEXT, statement TEXT, rationale TEXT, enforcement TEXT, enforcement_hook TEXT, established_at TEXT, established_by TEXT, superseded_at TEXT); \
          INSERT INTO laws.laws (id,slug,kind,domain,title,statement,rationale,enforcement,enforcement_hook,established_at,established_by,superseded_at) VALUES \
            (1,'NEVER_BYPASS_WITH_AUDIT','constitutional','audit','All mutations via with-audit','Direct writes are forbidden','No audit trail otherwise','hard','law-hook-mutation-via-with-audit','2025-12-01 09:00:00','human:dani',NULL), \
            (2,'ASK_BEFORE_DESTRUCTIVE','procedural','security','Confirm destructive ops','Ask before rm -rf',NULL,'soft',NULL,'2025-12-02 09:00:00','system:bootstrap',NULL), \
            (3,'OLD_RULE','decision','audit','Old rule','Was replaced','Old why','soft',NULL,'2025-11-01 09:00:00','human:dani','2025-12-15 00:00:00'), \
            (4,'ANCIENT_RULE','decision','audit','Archived rule','Kept for history','','archived',NULL,'2025-10-01 09:00:00','human:dani',NULL);").unwrap();
        c
    }
    #[test]
    fn lists_filters_and_maps() {
        let c = db();
        // no filters: superseded excluded, ORDER BY kind, slug
        let all = lawsList(&json!({}), &c).unwrap();
        let rows = all.as_array().unwrap();
        assert_eq!(rows.iter().map(|r| r["slug"].as_str().unwrap()).collect::<Vec<_>>(),
            vec!["NEVER_BYPASS_WITH_AUDIT", "ANCIENT_RULE", "ASK_BEFORE_DESTRUCTIVE"]);
        // full mapped shape + key order (rationale present → **Why:** suffix)
        assert_eq!(serde_json::to_string(&rows[0]).unwrap(),
            "{\"id\":1,\"slug\":\"NEVER_BYPASS_WITH_AUDIT\",\"kind\":\"constitutional\",\"domain\":\"audit\",\"title\":\"All mutations via with-audit\",\"enforcement\":\"hard\",\"body_md\":\"Direct writes are forbidden\\n\\n**Why:** No audit trail otherwise\",\"hook\":\"law-hook-mutation-via-with-audit\",\"active\":true,\"added_at\":\"2025-12-01 09:00:00\",\"added_by\":\"human:dani\"}");
        // null rationale → statement only; empty-string rationale also falsy
        assert_eq!(rows[2]["body_md"], json!("Ask before rm -rf"));
        assert_eq!(rows[1]["body_md"], json!("Kept for history"));
        // archived → active:false; others true
        assert_eq!(rows[1]["active"], json!(false));
        assert_eq!(rows[2]["active"], json!(true));
        // filters
        assert_eq!(lawsList(&json!({"kind": "constitutional"}), &c).unwrap().as_array().unwrap().len(), 1);
        assert_eq!(lawsList(&json!({"domain": "audit"}), &c).unwrap().as_array().unwrap().iter().map(|r| r["slug"].as_str().unwrap()).collect::<Vec<_>>(), vec!["NEVER_BYPASS_WITH_AUDIT", "ANCIENT_RULE"]);
        assert_eq!(lawsList(&json!({"enforcement": "soft"}), &c).unwrap()[0]["slug"], json!("ASK_BEFORE_DESTRUCTIVE"));
        assert_eq!(lawsList(&json!({"kind": "constitutional", "domain": "audit", "enforcement": "hard"}), &c).unwrap().as_array().unwrap().len(), 1);
        // falsy filters ignored; null input = JS `input ?? {}`
        assert_eq!(lawsList(&json!({"kind": "", "domain": null}), &c).unwrap().as_array().unwrap().len(), 3);
        assert_eq!(lawsList(&Value::Null, &c).unwrap().as_array().unwrap().len(), 3);
        // no match → []
        assert_eq!(lawsList(&json!({"domain": "nope"}), &c).unwrap(), json!([]));
        // laws not attached → catch branch returns []
        assert_eq!(lawsList(&json!({}), &Connection::open_in_memory().unwrap()).unwrap(), json!([]));
    }
}
