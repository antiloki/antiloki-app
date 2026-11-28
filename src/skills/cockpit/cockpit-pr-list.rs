//! cockpit-pr-list.rs — twin of skills/cockpit/cockpit-pr-list.mjs (db READ lane). NAMES ARE
//! CONTRACT: `cockpitPrList` (async default → sync twin). EPIC-VIBE-COCKPIT (pull requests). With
//! `{ id }` → one PR's full detail (incl. commits + card/contract). Otherwise → the PR index (no
//! patch, lighter), optionally filtered by session, open-first.
//!
//! GIT COMMITS BRANCH — DISCLOSED UNTESTED (playbook lesson 5's flag-skippable pattern, same as
//! constitution-estimate.rs's `git_diff_paths`): the JS `spawnSync('git', ['log', ...])` call
//! fires ONLY inside the `id`-detail branch (i.e. only when `input.id` is truthy). This twin ports
//! the whole id-branch — the git subprocess AND the card/contract enrichment that follows it — as
//! real, correct code (`git_commits_for_pr` mirrors the JS `git(...)` helper + line-parsing
//! exactly), but no fixture case sets `input.id`: a real `git log <base>..<branch>` depends on
//! actual repo/branch state, which isn't deterministically fixture-controllable the way a seeded
//! table is. The JS wraps the whole git call in try/catch (best-effort: "not a repo / branch
//! missing" → `commits = []`); this twin matches by collapsing ANY spawn failure or non-zero exit
//! to an empty `Vec` (same collapse `constitution-estimate.rs::git_diff_paths` uses).
//!
//! `context.db` missing → JS throws `err.contextMissing(...)`. Not modeled: `db: &Connection` is
//! required/non-nullable here, same "guard-throws are not exercised" convention as every sibling
//! twin (workspace-root.rs, cockpit-session-get.rs, constitution-estimate.rs).
// QUALITY: 76/100 — id-detail branch (git subprocess + card/contract joins) real but disclosed
// fixture-untested; index-listing branch fully fixture-covered (sort, totals, parse defaults)

use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

// JS binds Number(x): an integer-valued number binds as SQLite INTEGER, otherwise REAL (mirrors
// cockpit-session-get.rs's id_param — numeric affinity makes WHERE compare equal either way, but
// we stay faithful to what better-sqlite3 actually sends).
fn id_param(n: f64) -> Value {
    if n.is_finite() && n.fract() == 0.0 && n >= i64::MIN as f64 && n <= i64::MAX as f64 { json!(n as i64) } else { json!(n) }
}

// = JS `parse = (s, d) => { try { return JSON.parse(s ?? 'null') ?? d; } catch { return d; } }`.
fn parse(s: Option<&Value>, d: Value) -> Value {
    let src = match s {
        None | Some(Value::Null) => "null".to_string(),
        Some(Value::String(t)) => t.clone(),
        Some(other) => other.to_string(),
    };
    match serde_json::from_str::<Value>(&src) {
        Ok(v) if !v.is_null() => v,
        _ => d,
    }
}

/// `const git = (args, cwd) => spawnSync('git', args, {...})` applied to the id-branch's
/// `git log --no-merges --pretty=%h<SEP>%an<SEP>%ad<SEP>%s --date=short <base>..<branch>` call,
/// then the `out.split('\n').filter(Boolean).slice(0, 60).map(...)` line-parsing. DISCLOSED
/// UNTESTED BY FIXTURE — see module doc.
fn git_commits_for_pr(db: &Connection, pr: &Map<String, Value>) -> Vec<Value> {
    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root = root_v.as_str().unwrap_or_default();
    const SEP: char = '\u{1f}'; // String.fromCharCode(31)
    let range = format!("{}..{}", tmpl(pr.get("base_branch")), tmpl(pr.get("branch")));
    let output = crate::proc::cmd("git")
        .arg("log")
        .arg("--no-merges")
        .arg(format!("--pretty=%h{SEP}%an{SEP}%ad{SEP}%s"))
        .arg("--date=short")
        .arg(range)
        .current_dir(root)
        .output();
    let text = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => return Vec::new(),
    };
    text.split('\n')
        .filter(|l| !l.is_empty())
        .take(60)
        .map(|line| {
            // `p.split(SEP)`; missing trailing fields are `undefined` in JS → omitted by
            // JSON.stringify, mirrored here by only inserting the keys that were present.
            let parts: Vec<&str> = line.split(SEP).collect();
            let mut m = Map::new();
            if let Some(v) = parts.first() { m.insert("hash".into(), json!(*v)); }
            if let Some(v) = parts.get(1) { m.insert("author".into(), json!(*v)); }
            if let Some(v) = parts.get(2) { m.insert("date".into(), json!(*v)); }
            if let Some(v) = parts.get(3) { m.insert("subject".into(), json!(*v)); }
            Value::Object(m)
        })
        .collect()
}

/// `export default async function cockpitPrList(input, context)`
pub fn cockpitPrList(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));

    if truthy(input.get("id")) {
        let p = id_param(js_number(input.get("id")));
        let pr_rows = crate::db_json::query_rows(
            db,
            "SELECT pr.*, COALESCE(s.name, pr.ref_slug, 'driver') AS session_name FROM cockpit_pull_requests pr LEFT JOIN cockpit_sessions s ON s.id = pr.session_id WHERE pr.id = ?",
            &[p],
        )?;
        let pr_map = match pr_rows.as_array().and_then(|a| a.first()) {
            Some(r) => r.as_object().cloned().unwrap_or_default(),
            None => {
                let mut out = Map::new();
                out.insert("ok".into(), json!(false));
                out.insert("error".into(), json!(format!("PR {} not found", tmpl(input.get("id")))));
                return Ok(Value::Object(out));
            }
        };

        // git commits on this branch (base..branch), best-effort — see module doc.
        let commits = git_commits_for_pr(db, &pr_map);

        // the card + contract (driver PRs carry a ref_slug)
        let mut card = Value::Null;
        let mut contract = Value::Null;
        if truthy(pr_map.get("ref_slug")) {
            let ref_slug = pr_map.get("ref_slug").cloned().unwrap_or(Value::Null);
            contract = crate::db_json::query_rows(db, "SELECT * FROM contracts WHERE slug = ?", &[ref_slug.clone()])
                .ok()
                .and_then(|v| v.as_array().and_then(|a| a.first()).cloned())
                .unwrap_or(Value::Null);
            let card_slug = if truthy(contract.get("card_slug")) {
                contract.get("card_slug").cloned().unwrap_or(Value::Null)
            } else {
                ref_slug
            };
            card = crate::db_json::query_rows(db, "SELECT * FROM cards WHERE slug = ?", &[card_slug])
                .ok()
                .and_then(|v| v.as_array().and_then(|a| a.first()).cloned())
                .unwrap_or(Value::Null);
        }

        let mut pr_out = pr_map.clone();
        pr_out.insert("conflicts".into(), parse(pr_map.get("conflicts_json"), json!([])));
        pr_out.insert("blast".into(), parse(pr_map.get("blast_json"), Value::Null));
        pr_out.insert("analysis".into(), parse(pr_map.get("analysis_json"), Value::Null));

        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("pr".into(), Value::Object(pr_out));
        out.insert("commits".into(), json!(commits));
        out.insert("card".into(), card);
        out.insert("contract".into(), contract);
        return Ok(Value::Object(out));
    }

    let session_id_present = truthy(input.get("session_id"));
    let where_clause = if session_id_present { " WHERE pr.session_id = ?" } else { "" };
    let mut args: Vec<Value> = Vec::new();
    if session_id_present {
        args.push(id_param(js_number(input.get("session_id"))));
    }
    // card_slug rides along for driver PRs (ref_slug is the CONTRACT slug) — the board's PR
    // column groups by card, and stripping '-CONTRACT-N' client-side would be shape-coupling.
    let sql = format!(
        "SELECT pr.id, pr.session_id, COALESCE(s.name, pr.ref_slug, 'driver') AS session_name, pr.source, pr.ref_slug, c.card_slug, pr.pane_id, pr.idx, pr.agent, pr.branch, pr.base_branch, \
         pr.title, pr.status, pr.changed, pr.mergeable, pr.conflicts_json, pr.summary_md, pr.blast_json, pr.created_at, pr.updated_at \
         FROM cockpit_pull_requests pr LEFT JOIN cockpit_sessions s ON s.id = pr.session_id LEFT JOIN contracts c ON c.slug = pr.ref_slug \
         {where_clause} ORDER BY (pr.status = 'open') DESC, pr.updated_at DESC, pr.id DESC LIMIT 200"
    );
    let rows = crate::db_json::query_rows(db, &sql, &args)?;
    let prs: Vec<Value> = rows
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|row| {
            let mut m = row.as_object().cloned().unwrap_or_default();
            let conflicts = parse(m.get("conflicts_json"), json!([]));
            let has_blast = truthy(m.get("blast_json"));
            let has_summary = truthy(m.get("summary_md"));
            m.insert("conflicts".into(), conflicts);
            m.insert("hasBlast".into(), json!(has_blast));
            m.insert("hasSummary".into(), json!(has_summary));
            Value::Object(m)
        })
        .collect();

    let open = prs.iter().filter(|p| p.get("status").and_then(Value::as_str) == Some("open")).count() as i64;
    let merged = prs.iter().filter(|p| p.get("status").and_then(Value::as_str) == Some("merged")).count() as i64;
    let total = prs.len() as i64;

    let mut totals = Map::new();
    totals.insert("open".into(), json!(open));
    totals.insert("merged".into(), json!(merged));
    totals.insert("total".into(), json!(total));

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("prs".into(), json!(prs));
    out.insert("totals".into(), Value::Object(totals));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cockpit_pull_requests (id INTEGER PRIMARY KEY, session_id INTEGER NOT NULL, pane_id INTEGER, idx INTEGER, agent TEXT, branch TEXT, base_branch TEXT, source TEXT, ref_slug TEXT, title TEXT, status TEXT, changed INTEGER, mergeable INTEGER, conflicts_json TEXT, patch TEXT, summary_md TEXT, analysis_json TEXT, blast_json TEXT, created_at TEXT, updated_at TEXT);
             CREATE TABLE cockpit_sessions (id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE contracts (slug TEXT PRIMARY KEY, card_slug TEXT);
             CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn lists_open_first_then_updated_desc_then_id_desc() {
        let c = db();
        c.execute_batch(
            "INSERT INTO cockpit_sessions (id, name) VALUES (10, 'sess-a');
             INSERT INTO cockpit_pull_requests (id, session_id, branch, base_branch, status, ref_slug, conflicts_json, blast_json, summary_md, updated_at) VALUES
               (1, 10, 'b1', 'main', 'open', 'CT-1', '[\"a.js\"]', NULL, NULL, '2026-01-01 10:00:00'),
               (2, 10, 'b2', 'main', 'merged', NULL, NULL, '{\"impact\":1}', 'did a thing', '2026-01-02 10:00:00'),
               (3, 999, 'b3', 'main', 'open', 'CT-3', NULL, NULL, NULL, '2026-01-01 09:00:00'),
               (4, 999, 'b4', 'main', 'open', NULL, NULL, NULL, NULL, '2026-01-01 09:00:00');
             INSERT INTO contracts (slug, card_slug) VALUES ('CT-1', 'CARD-1');",
        )
        .unwrap();
        let r = cockpitPrList(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        let prs = r["prs"].as_array().unwrap();
        let ids: Vec<i64> = prs.iter().map(|p| p["id"].as_i64().unwrap()).collect();
        // open first (1,3,4 in some order) then merged (2) last; among open ties on updated_at,
        // id DESC breaks the tie (4 before 3); id=1 has a strictly later updated_at so leads.
        assert_eq!(ids, vec![1, 4, 3, 2]);
        assert_eq!(r["totals"], json!({"open": 3, "merged": 1, "total": 4}));

        let pr1 = &prs[0];
        assert_eq!(pr1["session_name"], json!("sess-a"));
        assert_eq!(pr1["card_slug"], json!("CARD-1"));
        assert_eq!(pr1["conflicts"], json!(["a.js"]));
        assert_eq!(pr1["hasBlast"], json!(false));
        assert_eq!(pr1["hasSummary"], json!(false));

        let pr2 = &prs[3];
        assert_eq!(pr2["session_name"], json!("sess-a")); // COALESCE(s.name, ...) — session found
        assert_eq!(pr2["conflicts"], json!([])); // NULL conflicts_json -> parse default []
        assert_eq!(pr2["hasBlast"], json!(true));
        assert_eq!(pr2["hasSummary"], json!(true));

        let pr3 = &prs[2]; // id=3, no session, ref_slug CT-3, no matching contracts row
        assert_eq!(pr3["session_name"], json!("CT-3")); // COALESCE falls to ref_slug
        assert_eq!(pr3["card_slug"], Value::Null); // LEFT JOIN contracts miss

        let pr4 = &prs[1]; // id=4, no session, no ref_slug
        assert_eq!(pr4["session_name"], json!("driver")); // COALESCE falls all the way through

        let keys: Vec<&str> = pr1.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "id", "session_id", "session_name", "source", "ref_slug", "card_slug", "pane_id", "idx", "agent", "branch",
                "base_branch", "title", "status", "changed", "mergeable", "conflicts_json", "summary_md", "blast_json",
                "created_at", "updated_at", "conflicts", "hasBlast", "hasSummary",
            ]
        );
    }

    #[test]
    fn session_id_filter_narrows_the_list() {
        let c = db();
        c.execute_batch(
            "INSERT INTO cockpit_sessions (id, name) VALUES (10, 's1'), (20, 's2');
             INSERT INTO cockpit_pull_requests (id, session_id, branch, base_branch, status, updated_at) VALUES
               (1, 10, 'b1', 'main', 'open', '2026-01-01 10:00:00'),
               (2, 20, 'b2', 'main', 'open', '2026-01-01 11:00:00');",
        )
        .unwrap();
        let r = cockpitPrList(&json!({"session_id": 10}), &c).unwrap();
        let ids: Vec<i64> = r["prs"].as_array().unwrap().iter().map(|p| p["id"].as_i64().unwrap()).collect();
        assert_eq!(ids, vec![1]);
        assert_eq!(r["totals"], json!({"open": 1, "merged": 0, "total": 1}));
    }

    #[test]
    fn empty_table_yields_zero_totals() {
        let c = db();
        let r = cockpitPrList(&json!({}), &c).unwrap();
        assert_eq!(r, json!({"ok": true, "prs": [], "totals": {"open": 0, "merged": 0, "total": 0}}));
    }

    #[test]
    fn id_not_found_reports_the_raw_input_id_via_template_literal() {
        let c = db();
        let r = cockpitPrList(&json!({"id": 999}), &c).unwrap();
        assert_eq!(r, json!({"ok": false, "error": "PR 999 not found"}));
    }

    #[test]
    fn id_zero_is_falsy_and_falls_back_to_the_index_listing() {
        let c = db();
        let r = cockpitPrList(&json!({"id": 0}), &c).unwrap();
        assert_eq!(r["prs"], json!([]));
        assert!(r.get("pr").is_none());
    }

    #[test]
    fn id_found_no_ref_slug_yields_null_card_and_contract() {
        // Exercises the id-branch's success path (found + parse + card/contract null-fallback).
        // The git subprocess runs for real here (see module doc); a nonsense base..branch range
        // reliably yields an unknown-revision failure from real `git`, which this twin collapses
        // to `commits: []` exactly like a not-a-repo/branch-missing failure — deterministic
        // regardless of which real repo happens to sit at the resolved workspace root.
        let c = db();
        c.execute_batch(
            "INSERT INTO cockpit_pull_requests (id, session_id, branch, base_branch, status, ref_slug, conflicts_json, blast_json, analysis_json, updated_at) VALUES
               (5, 999, 'zzz-nonexistent-branch-twin', 'zzz-nonexistent-base-twin', 'open', NULL, '[\"x\"]', '{\"a\":1}', '{\"b\":2}', '2026-01-01 10:00:00');",
        )
        .unwrap();
        let r = cockpitPrList(&json!({"id": 5}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["pr"]["conflicts"], json!(["x"]));
        assert_eq!(r["pr"]["blast"], json!({"a": 1}));
        assert_eq!(r["pr"]["analysis"], json!({"b": 2}));
        assert_eq!(r["card"], Value::Null);
        assert_eq!(r["contract"], Value::Null);
        assert_eq!(r["commits"], json!([])); // unknown revision range -> git exits non-zero -> []
    }
}
