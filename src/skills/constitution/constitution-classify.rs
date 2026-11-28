//! constitution-classify.rs — twin of skills/constitution/constitution-classify.mjs (db lane,
//! cross-db `laws`+`judgments`). NAMES ARE CONTRACT: `constitutionClassify` (default). THE LEDGER
//! (docs/constitution-review-plan.html N5): op:'classify' records a (law_key, path, class) —
//! fixed|dismissed|known; op:'ledger' tallies per law_key + proposes a weight RECOMMENDATION
//! (advisory only — never auto-mutates the constitution).
// QUALITY: 82/100 — dismiss_rate is a genuine JS-number division (whole-valued results must
// render as bare integers, not "1.0" — see js_num); the ledger sort is a two-key JS comparator
// ((recommend truthy) desc, then total desc) that must stay STABLE on ties, matching Array.sort.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

const MIN_SIGNAL: i64 = 5; // don't recommend off a handful of classifications

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// String(v ?? '')
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// String(v) (no ??)
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
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
// JS numbers don't distinguish int/float — a whole-valued division result must render "1", not
// "1.0" (dismiss_rate's SUM/SUM can land on an exact 1.0 or 0.0).
fn js_num(n: f64) -> Value {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 9.007_199_254_740_992e15 { json!(n as i64) } else { json!(n) }
}

pub fn constitutionClassify(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `if (!db) throw err.required(...)` — the db lane always injects a live connection, so
    // that guard is unreachable here (Rust's signature takes `db: &Connection`, never absent).
    let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(db);
    let op_val = nz(input.get("op")).cloned().unwrap_or_else(|| json!("classify"));
    let op = js_string(&op_val);

    if op == "classify" {
        let law_key = str_qq(input.get("law_key")).trim().to_string();
        let cls = str_qq(input.get("class")).trim().to_string();
        if law_key.is_empty() {
            return Err("constitution-classify: law_key is required".to_string());
        }
        if !["fixed", "dismissed", "known"].contains(&cls.as_str()) {
            return Err(format!(
                "constitution-classify: class must be one of: fixed, dismissed, known (got '{cls}')"
            ));
        }
        let path_input = input.get("path");
        let path_value: Value = if truthy(path_input) { json!(js_string(path_input.unwrap())) } else { Value::Null };
        crate::db_json::run_write(
            db,
            "INSERT INTO judgments.review_classifications (law_key, path, class) VALUES (?, ?, ?)",
            &[json!(law_key.clone()), path_value, json!(cls.clone())],
        )?;
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("law_key".into(), json!(law_key));
        out.insert("class".into(), json!(cls));
        return Ok(Value::Object(out));
    }

    if op == "ledger" {
        let rows = crate::db_json::query_rows(
            db,
            "SELECT law_key,
        SUM(class='fixed')     AS fixed,
        SUM(class='dismissed') AS dismissed,
        SUM(class='known')     AS known,
        COUNT(*)               AS total
      FROM judgments.review_classifications GROUP BY law_key",
            &[],
        )?;
        let mut ledger: Vec<Value> = Vec::new();
        for r in rows.as_array().cloned().unwrap_or_default() {
            let fixed_v = r.get("fixed").cloned().unwrap_or(Value::Null);
            let dismissed_v = r.get("dismissed").cloned().unwrap_or(Value::Null);
            let known_v = r.get("known").cloned().unwrap_or(Value::Null);
            let total_v = r.get("total").cloned().unwrap_or(Value::Null);
            let fixed_i = fixed_v.as_i64().unwrap_or(0);
            let dismissed_i = dismissed_v.as_i64().unwrap_or(0);
            let total_i = total_v.as_i64().unwrap_or(0);
            let decided = fixed_i + dismissed_i;
            let dismiss_rate: Option<f64> = if decided != 0 { Some(dismissed_i as f64 / decided as f64) } else { None };
            let mut recommend: Option<&'static str> = None;
            if total_i >= MIN_SIGNAL {
                if let Some(dr) = dismiss_rate {
                    if dr >= 0.8 {
                        recommend = Some("suspend"); // almost always dismissed -> noise
                    } else if dr <= 0.2 && fixed_i >= MIN_SIGNAL {
                        recommend = Some("raise-weight"); // always acted on -> matters
                    }
                }
            }

            let law_key_v = r.get("law_key").cloned().unwrap_or(Value::Null);
            let law_rows = crate::db_json::query_rows(
                db,
                "SELECT title, weight, status FROM laws.review_laws WHERE key=?",
                &[law_key_v.clone()],
            )?;
            let law = law_rows.as_array().and_then(|a| a.first());
            let title = nz(law.and_then(|l| l.get("title"))).cloned().unwrap_or_else(|| law_key_v.clone());
            let weight = nz(law.and_then(|l| l.get("weight"))).cloned().unwrap_or(Value::Null);
            let status = nz(law.and_then(|l| l.get("status"))).cloned().unwrap_or(Value::Null);

            let mut entry = Map::new();
            entry.insert("law_key".into(), law_key_v);
            entry.insert("title".into(), title);
            entry.insert("weight".into(), weight);
            entry.insert("status".into(), status);
            entry.insert("fixed".into(), fixed_v);
            entry.insert("dismissed".into(), dismissed_v);
            entry.insert("known".into(), known_v);
            entry.insert("total".into(), total_v);
            entry.insert("dismiss_rate".into(), dismiss_rate.map(js_num).unwrap_or(Value::Null));
            entry.insert("recommend".into(), recommend.map(|s| json!(s)).unwrap_or(Value::Null));
            ledger.push(Value::Object(entry));
        }
        // JS: (b.recommend?1:0)-(a.recommend?1:0) || b.total-a.total — recommend-bearing rows
        // first, ties by total desc, remaining ties keep GROUP BY's incoming order (stable sort).
        ledger.sort_by(|a, b| {
            let ar = if a.get("recommend").map_or(false, |v| !v.is_null()) { 1 } else { 0 };
            let br = if b.get("recommend").map_or(false, |v| !v.is_null()) { 1 } else { 0 };
            let at = a.get("total").and_then(|v| v.as_i64()).unwrap_or(0);
            let bt = b.get("total").and_then(|v| v.as_i64()).unwrap_or(0);
            br.cmp(&ar).then(bt.cmp(&at))
        });

        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("ledger".into(), json!(ledger));
        return Ok(Value::Object(out));
    }

    Err(format!("constitution-classify: op must be one of: classify, ledger (got '{op}')"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments").unwrap();
        c
    }

    #[test]
    fn classify_records_and_trims() {
        let c = conn();
        let r = constitutionClassify(&json!({ "op": "classify", "law_key": "  NO-SECRETS  ", "class": "fixed" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "law_key": "NO-SECRETS", "class": "fixed" }));
        let row = crate::db_json::query_rows(&c, "SELECT law_key, path, class FROM judgments.review_classifications", &[]).unwrap();
        assert_eq!(row, json!([{ "law_key": "NO-SECRETS", "path": null, "class": "fixed" }]));
    }

    #[test]
    fn classify_path_truthy_string_and_numeric_coercion() {
        let c = conn();
        constitutionClassify(&json!({ "op": "classify", "law_key": "HAS-PATH", "path": "src/auth.ts", "class": "dismissed" }), &c).unwrap();
        constitutionClassify(&json!({ "op": "classify", "law_key": "NUM-PATH", "path": 42, "class": "fixed" }), &c).unwrap();
        let rows = crate::db_json::query_rows(&c, "SELECT law_key, path FROM judgments.review_classifications ORDER BY law_key", &[]).unwrap();
        assert_eq!(rows, json!([{ "law_key": "HAS-PATH", "path": "src/auth.ts" }, { "law_key": "NUM-PATH", "path": "42" }]));
    }

    #[test]
    fn classify_default_op_and_falsy_path_is_null() {
        let c = conn();
        let r = constitutionClassify(&json!({ "law_key": "DEFAULT-OP", "class": "known", "path": "" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "law_key": "DEFAULT-OP", "class": "known" }));
        let row = crate::db_json::query_rows(&c, "SELECT path FROM judgments.review_classifications", &[]).unwrap();
        assert_eq!(row[0]["path"], Value::Null);
    }

    #[test]
    fn classify_rejects_missing_law_key_and_bad_class() {
        let c = conn();
        assert!(constitutionClassify(&json!({ "op": "classify", "class": "fixed" }), &c).unwrap_err().contains("law_key is required"));
        assert!(constitutionClassify(&json!({ "op": "classify", "law_key": "X", "class": "bogus" }), &c).unwrap_err().contains("class must be one of"));
    }

    #[test]
    fn ledger_tallies_dismiss_rate_and_recommends() {
        let c = conn();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, weight, status) VALUES ('project','LAW-SUSPEND','Suspend Candidate Law','r',6,'ratified')", []).unwrap();
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, weight, status) VALUES ('project','LAW-RAISE','Raise Weight Law','r',3,'ratified')", []).unwrap();
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, weight, status) VALUES ('architecture','LAW-NEUTRAL','Neutral Law','r',5,'ratified')", []).unwrap();
        let ins = |law_key: &str, class: &str| {
            c.execute("INSERT INTO judgments.review_classifications (law_key, class) VALUES (?, ?)", rusqlite::params![law_key, class]).unwrap();
        };
        for _ in 0..5 { ins("LAW-SUSPEND", "dismissed"); }
        for _ in 0..5 { ins("LAW-RAISE", "fixed"); }
        ins("LAW-RAISE", "dismissed");
        for _ in 0..2 { ins("LAW-NEUTRAL", "fixed"); }
        for _ in 0..2 { ins("LAW-NEUTRAL", "dismissed"); }
        ins("LAW-NEUTRAL", "known");
        for _ in 0..5 { ins("LAW-ONLYKNOWN", "known"); }
        for _ in 0..2 { ins("LAW-LOWSIGNAL", "dismissed"); }
        for _ in 0..7 { ins("LAW-UNKNOWN-IN-CATALOG", "dismissed"); }

        let r = constitutionClassify(&json!({ "op": "ledger" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        let ledger = r["ledger"].as_array().unwrap();
        let by_key = |k: &str| ledger.iter().find(|l| l["law_key"] == json!(k)).unwrap();

        assert_eq!(by_key("LAW-SUSPEND")["recommend"], json!("suspend"));
        assert_eq!(by_key("LAW-SUSPEND")["dismiss_rate"], json!(1));
        assert_eq!(by_key("LAW-RAISE")["recommend"], json!("raise-weight"));
        assert_eq!(by_key("LAW-RAISE")["dismiss_rate"], json!(1.0 / 6.0));
        assert_eq!(by_key("LAW-NEUTRAL")["recommend"], Value::Null);
        assert_eq!(by_key("LAW-NEUTRAL")["dismiss_rate"], json!(0.5));
        assert_eq!(by_key("LAW-ONLYKNOWN")["recommend"], Value::Null);
        assert_eq!(by_key("LAW-ONLYKNOWN")["dismiss_rate"], Value::Null);
        assert_eq!(by_key("LAW-LOWSIGNAL")["recommend"], Value::Null); // below MIN_SIGNAL despite a 100% dismiss rate
        assert_eq!(by_key("LAW-UNKNOWN-IN-CATALOG")["recommend"], json!("suspend"));
        assert_eq!(by_key("LAW-UNKNOWN-IN-CATALOG")["title"], json!("LAW-UNKNOWN-IN-CATALOG")); // no laws.review_laws row -> falls back to the key
        assert_eq!(by_key("LAW-UNKNOWN-IN-CATALOG")["weight"], Value::Null);

        let order: Vec<&str> = ledger.iter().map(|l| l["law_key"].as_str().unwrap()).collect();
        assert_eq!(order, vec!["LAW-UNKNOWN-IN-CATALOG", "LAW-RAISE", "LAW-SUSPEND", "LAW-NEUTRAL", "LAW-ONLYKNOWN", "LAW-LOWSIGNAL"]);
    }

    #[test]
    fn rejects_unknown_op() {
        let c = conn();
        assert!(constitutionClassify(&json!({ "op": "bogus" }), &c).unwrap_err().contains("op must be one of"));
    }
}
