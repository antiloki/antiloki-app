//! code-signals-snapshot.rs — twin of skills/codebase/code-signals-snapshot.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `codeSignalsSnapshot` (default, standard db lane). One timestamped,
//! uniquely-id'd sweep over per-file metrics, grouped into FAMILIES (health, coupling, cascade,
//! tested, debt, ownership, mass, churn), persisted APPEND-ONLY into
//! `code.code_signal_snapshot_facts` (+ a `code.code_signal_latest` view over the newest
//! snapshot_uid). Fully deterministic: no AI, no clock read beyond the frozen instant.
//!
//! `input.__metrics`: the JS gates its ONE cross-module call (`atlasMetrics`) behind a TRUTHY
//! check (`!byFile`, not key-presence) — any non-empty-or-not object bypasses it. Ported below as
//! a real, correct call to `crate::codebase_atlas_metrics::atlasMetrics` on the falsy/absent arm
//! (same "flag-skippable, disclosed real-but-untested-by-fixture" shape as the sibling
//! atlas-metrics.rs's own churn branch) — every fixture case here sets `"__metrics"` explicitly
//! to stay deterministic and subprocess-free (atlasMetrics's own churn-autocompute path shells
//! out to git).
//!
//! `datetime('now')` (the table's `created_at` DEFAULT, never actually exercised since every
//! INSERT here supplies an explicit `created_at`) and the skill's own `SELECT datetime('now')`
//! read are both pinned to the frozen instant **`2026-01-01 00:00:00`** per the datetime-skills
//! convention.
//!
//! Table access is qualified `code.` (a real ATTACHed schema on the live multi-db connection,
//! same convention as the sibling `atlas-register.rs` / `atlas-file-snapshot.mjs` family) — the
//! fixture declares `"attach": ["code"]`.
//!
//! `context.db` missing → JS throws `err.contextMissing('code-signals-snapshot', 'db')`. Not
//! modeled: `db: &Connection` is a required, non-nullable reference here — same "guard-throws are
//! not exercised" convention used by every sibling twin.
// QUALITY: real cross-module atlasMetrics call ported faithfully (disclosed real-but-untested-by-
// fixture, flag-skippable via a truthy `__metrics`); the family/card derivation + append-only
// snapshot insert + families rollup is fully fixture-covered.

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
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
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
// JS `Math.round`: floor(x + 0.5) — rounds HALF UP, unlike Rust's half-away-from-zero `.round()`.
fn js_round(x: f64) -> f64 { (x + 0.5).floor() }
// `${v}` for a definite (present, non-undefined) JSON value already known not to be `undefined`.
fn disp(v: &Value) -> String {
    match v { Value::Null => "null".into(), Value::String(s) => s.clone(), Value::Number(n) => n.to_string(), Value::Bool(b) => b.to_string(), other => other.to_string() }
}

// per-file signal families, all read straight off the deterministic atlas metrics
fn families_of(m: &Value) -> Map<String, Value> {
    let mut fam = Map::new();
    if let Some(badness) = nz(m.get("badness")) {
        let health = js_round(100.0 - js_number(Some(badness)) * 100.0);
        fam.insert("health".into(), json!(health as i64));
    }
    let fan_in = js_number(nz(m.get("fan_in")).or(Some(&json!(0))));
    let fan_out = js_number(nz(m.get("fan_out")).or(Some(&json!(0))));
    fam.insert("coupling".into(), json!((fan_in + fan_out) as i64));
    if let Some(cascade) = nz(m.get("cascade")) { fam.insert("cascade".into(), cascade.clone()); }
    fam.insert("tested".into(), json!(if truthy(m.get("covered")) { 1 } else { 0 }));
    if let Some(debt) = nz(m.get("debt")) { fam.insert("debt".into(), debt.clone()); }
    if let Some(authors) = nz(m.get("authors")) { fam.insert("ownership".into(), authors.clone()); }
    if let Some(gravity) = nz(m.get("gravity")) { fam.insert("mass".into(), gravity.clone()); }
    if let Some(churn) = nz(m.get("churn")) {
        if js_number(Some(churn)) > 0.0 { fam.insert("churn".into(), churn.clone()); }
    }
    fam
}

fn card(path: &str, fam: &Map<String, Value>) -> String {
    let mut bits: Vec<String> = Vec::new();
    if let Some(h) = fam.get("health") { bits.push(format!("health {}/100", disp(h))); }
    bits.push(if truthy(fam.get("tested")) { "tested".to_string() } else { "UNTESTED".to_string() });
    if let Some(c) = fam.get("cascade") { if truthy(Some(c)) { bits.push(format!("blast-radius {}", disp(c))); } }
    if let Some(c) = fam.get("coupling") { bits.push(format!("coupling {}", disp(c))); }
    if let Some(o) = fam.get("ownership") {
        if o.as_i64() == Some(1) { bits.push("sole-owned (bus-factor)".to_string()); }
        else { bits.push(format!("{} authors", disp(o))); }
    }
    if let Some(d) = fam.get("debt") { if truthy(Some(d)) { bits.push(format!("{} debt markers", disp(d))); } }
    if let Some(c) = fam.get("churn") { if truthy(Some(c)) { bits.push(format!("churn {}", disp(c))); } }
    if let Some(m) = fam.get("mass") { bits.push(format!("mass {}", disp(m))); }
    format!("{} — {}", path, bits.join(" · "))
}

fn ensure(db: &Connection) -> Result<(), String> {
    db.execute_batch(
        &format!("CREATE TABLE IF NOT EXISTS code.code_signal_snapshot_facts (
          id INTEGER PRIMARY KEY AUTOINCREMENT,
          snapshot_uid TEXT NOT NULL,
          created_at   TEXT NOT NULL DEFAULT ({}),
          origin       TEXT NOT NULL,
          path         TEXT NOT NULL,
          families_json TEXT NOT NULL,
          card         TEXT NOT NULL
        )", crate::clock::now_sql()),
    ).map_err(|e| e.to_string())?;
    db.execute_batch(
        "CREATE VIEW IF NOT EXISTS code.code_signal_latest AS
          SELECT * FROM code.code_signal_snapshot_facts
          WHERE snapshot_uid = (SELECT snapshot_uid FROM code.code_signal_snapshot_facts ORDER BY id DESC LIMIT 1)",
    ).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn codeSignalsSnapshot(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin_v = nz(input.get("origin")).cloned().unwrap_or(json!("atlas"));
    let origin = origin_v.as_str().unwrap_or("atlas").to_string();

    let metrics_opt = input.get("__metrics");
    let by_file: Map<String, Value> = if truthy(metrics_opt) {
        metrics_opt.unwrap().as_object().cloned().unwrap_or_default()
    } else {
        let r = crate::codebase_atlas_metrics::atlasMetrics(&json!({ "origin": origin_v.clone() }), db)?;
        r.get("metrics_by_file").and_then(Value::as_object).cloned().unwrap_or_default()
    };

    ensure(db)?;

    let seq: i64 = db
        .query_row("SELECT COUNT(DISTINCT snapshot_uid) AS c FROM code.code_signal_snapshot_facts", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let seq = seq + 1;
    let created_at = crate::clock::now_ts(db);
    let snapshot_uid = format!("sig-{}-{:04}", origin, seq);

    let mut families: Map<String, Value> = Map::new();
    let mut files: i64 = 0;
    for (path, m) in by_file.iter() {
        if m.is_null() || truthy(m.get("is_test")) { continue; }
        let fam = families_of(m);
        if fam.is_empty() { continue; }
        let families_json = serde_json::to_string(&Value::Object(fam.clone())).map_err(|e| e.to_string())?;
        let card_text = card(path, &fam);
        db.execute(
            "INSERT INTO code.code_signal_snapshot_facts (snapshot_uid, created_at, origin, path, families_json, card) VALUES (?,?,?,?,?,?)",
            rusqlite::params![snapshot_uid, created_at, origin, path, families_json, card_text],
        ).map_err(|e| e.to_string())?;
        files += 1;
        for k in fam.keys() {
            let entry = families.entry(k.clone()).or_insert(json!(0));
            let n = entry.as_i64().unwrap_or(0) + 1;
            *entry = json!(n);
        }
    }

    Ok(json!({
        "snapshot_uid": snapshot_uid,
        "created_at": created_at,
        "origin": origin,
        "files": files,
        "families": families,
        "total": files,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use serde_json::json;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH DATABASE ':memory:' AS code").unwrap();
        c
    }

    #[test]
    fn snapshots_families_and_skips_test_and_empty() {
        let c = setup();
        let metrics = json!({
            "src/a.ts": { "badness": 0.2, "fan_in": 3, "fan_out": 1, "cascade": 5, "covered": true, "debt": 2, "authors": 1, "gravity": 120, "churn": 4, "is_test": false },
            "src/b.test.ts": { "badness": 0.9, "fan_in": 0, "fan_out": 0, "covered": false, "is_test": true },
            "src/empty.ts": null,
        });
        let input = json!({ "origin": "atlas", "__metrics": metrics });
        let out = codeSignalsSnapshot(&input, &c).unwrap();

        assert_eq!(out["snapshot_uid"], json!("sig-atlas-0001"));
        assert_eq!(out["created_at"], json!("2026-01-01 00:00:00"));
        assert_eq!(out["origin"], json!("atlas"));
        assert_eq!(out["files"], json!(1));
        assert_eq!(out["total"], json!(1));
        assert_eq!(out["families"], json!({
            "health": 1, "coupling": 1, "cascade": 1, "tested": 1, "debt": 1, "ownership": 1, "mass": 1, "churn": 1
        }));

        let row_card: String = c.query_row("SELECT card FROM code.code_signal_snapshot_facts WHERE path='src/a.ts'", [], |r| r.get(0)).unwrap();
        assert_eq!(row_card, "src/a.ts — health 80/100 · tested · blast-radius 5 · coupling 4 · sole-owned (bus-factor) · 2 debt markers · churn 4 · mass 120");

        let fam_json: String = c.query_row("SELECT families_json FROM code.code_signal_snapshot_facts WHERE path='src/a.ts'", [], |r| r.get(0)).unwrap();
        let fam: Value = serde_json::from_str(&fam_json).unwrap();
        assert_eq!(fam, json!({ "health": 80, "coupling": 4, "cascade": 5, "tested": 1, "debt": 2, "ownership": 1, "mass": 120, "churn": 4 }));

        let latest_count: i64 = c.query_row("SELECT COUNT(*) FROM code.code_signal_latest", [], |r| r.get(0)).unwrap();
        assert_eq!(latest_count, 1);
    }

    #[test]
    fn second_snapshot_increments_seq_and_is_append_only() {
        let c = setup();
        let metrics = json!({ "src/x.ts": { "coupling_stub": true, "fan_in": 0, "fan_out": 0, "covered": true } });
        let input = json!({ "origin": "atlas", "__metrics": metrics });
        let first = codeSignalsSnapshot(&input, &c).unwrap();
        assert_eq!(first["snapshot_uid"], json!("sig-atlas-0001"));
        let second = codeSignalsSnapshot(&input, &c).unwrap();
        assert_eq!(second["snapshot_uid"], json!("sig-atlas-0002"));

        let total_rows: i64 = c.query_row("SELECT COUNT(*) FROM code.code_signal_snapshot_facts", [], |r| r.get(0)).unwrap();
        assert_eq!(total_rows, 2); // append-only across both snapshots

        let latest_count: i64 = c.query_row("SELECT COUNT(*) FROM code.code_signal_latest", [], |r| r.get(0)).unwrap();
        assert_eq!(latest_count, 1); // view only shows the newest snapshot_uid
    }

    #[test]
    fn ownership_plural_and_untested_card() {
        let c = setup();
        let metrics = json!({
            "src/shared.ts": { "authors": 3, "fan_in": 2, "fan_out": 0, "covered": false }
        });
        let input = json!({ "origin": "atlas", "__metrics": metrics });
        let out = codeSignalsSnapshot(&input, &c).unwrap();
        assert_eq!(out["files"], json!(1));
        let row_card: String = c.query_row("SELECT card FROM code.code_signal_snapshot_facts WHERE path='src/shared.ts'", [], |r| r.get(0)).unwrap();
        assert_eq!(row_card, "src/shared.ts — UNTESTED · coupling 2 · 3 authors");
    }
}
