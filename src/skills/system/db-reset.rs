//! db-reset.rs — twin of skills/system/db-reset.mjs (standard db lane; default export
//! `dbReset(input, context)`). NAMES ARE CONTRACT: `dbReset`.
//!
//! Archive-then-wipe one substrate DB (or every DB except `state`) so the canonical constitution
//! can re-seed clean. DESTRUCTIVE, guarded by `confirm:true`. Archive-not-delete by default: a
//! clean single-file `VACUUM <schema> INTO` snapshot is written under archive/ BEFORE each DB is
//! wiped; if the snapshot fails, that DB's wipe is SKIPPED (never lose data we couldn't preserve).
//!
//! BESPOKE NATIVE TEST (not conform-db), per this session's lesson 3: this unit does REAL
//! filesystem I/O — `mkdirSync(archiveDir, {recursive:true})` and `VACUUM <schema> INTO '<path>'`
//! write a real snapshot file to disk before every wipe. `context.archiveDir` IS a caller-
//! injectable override (unlike a raw `ANTILOKI_HOME` env read — see roots.mjs), so this twin takes
//! it as a plain `Option<&Path>` parameter rather than the env-var + tempdir indirection the
//! `god-driver-*`/`driver-halt` precedents needed. But the override only changes WHERE the file
//! lands, not WHETHER a real file gets written — and the conform-db harness's `db_after`
//! comparison only inspects SQL table state through the connection, never real files on disk — so
//! this unit is still unverifiable through the normal fixture harness. Tested here instead via
//! `#[cfg(test)]` Rust-native tests that pass a `tempdir` directly as the override (no env-var
//! locking needed, since unlike `HALT_FILE` this path is a genuine call-time parameter in the JS
//! too).
//!
//! GAP DISCLOSED — archive-file timestamp: the JS builds its stamp from LOCAL wall-clock getters
//! (`now.getFullYear()/getMonth()/getDate()/getHours()/getMinutes()/getSeconds()`, NOT
//! `toISOString()`), so the stamp text is OS-timezone-dependent even in the real JS — verified on
//! this dev machine (`America/Sao_Paulo`): the conform-db frozen instant `2026-01-01T00:00:00Z`
//! renders as local stamp `20251231-210000`, not `20260101-000000`. There is no portable JS
//! contract to byte-match, and no timezone crate is in this workspace's Cargo.toml (only
//! serde_json/rusqlite/regex/sha2/hex/base64). This twin stamps from UTC calendar fields instead
//! (a small pure-Rust days-since-epoch→civil-date conversion, Howard Hinnant's well-known
//! algorithm — no crate needed): functionally equivalent (a unique, human-readable, monotonically
//! non-decreasing archive-file discriminator that never collides within the same second) but will
//! not byte-match the JS's local-time string except on a UTC machine. Purely cosmetic — it only
//! affects the archive filename text, never the reset/wipe/archive control flow — which is exactly
//! why this unit needed bespoke rather than conform-db testing in the first place.
//!
//! GAP DISCLOSED — error message truncation: JS `String(e?.message||e).slice(0,80)` is a UTF-16
//! code-unit slice; this twin uses `.chars().take(80)` (the ASCII/BMP approximation — rusqlite
//! error messages are ASCII in practice, per the playbook's gotcha #5).
//!
//! `context.db` missing → JS throws `err.contextMissing('db-reset','db')`
//! (`"db-reset: context.db is required"`). Not modeled: Rust's `db: &Connection` is a required,
//! non-nullable reference, so this guard is unreachable here — same "guard-throws are not
//! exercised" convention used by every sibling twin.
//!
//! `input.db` empty/falsy → JS throws `err.required('db-reset','db')` (`"db-reset: db is
//! required"`) — modeled below as `Err(...)`, the one reachable throw path.
//! `confirm !== true` / unknown `db` name → JS returns `{ ok:false, error }` (NOT a throw) —
//! modeled as `Ok(json!({...}))`.
//!
//! `ALIAS.state = 'main'`: per MULTI_DB_SPLIT_BY_QUADRANT, `state` is the base connection's own
//! `main` schema; every other name is an ATTACHed schema (`nb` for `notebooks`, else same name).

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } }

const ALIAS: &[(&str, &str)] = &[
    ("state", "main"), ("memory", "memory"), ("brain", "brain"), ("laws", "laws"),
    ("knowledge", "knowledge"), ("timeline", "timeline"), ("judgments", "judgments"),
    ("code", "code"), ("notebooks", "nb"),
];
// 'all' = everything EXCEPT state (the operator's board is never nuked by a bulk reset).
const ALL: &[&str] = &["memory", "brain", "laws", "knowledge", "timeline", "judgments", "code", "notebooks"];
// DBs holding canonical constitution → a reseed (BE reopen) restores them after the wipe.
const RESEED: &[&str] = &["state", "brain", "laws"];

fn alias_schema(name: &str) -> Option<&'static str> {
    ALIAS.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
}

// No exact JS analog: roots.mjs's ARCHIVE_DIR chains ANTILOKI_DATA_DIR (else SUBSTRATE_ROOT/
// .antiloki, else self-derived from the engine module's own file location) + '/archive'. A
// compiled Rust binary has no notion of "its own module file location" — best-effort fallback
// used ONLY when no `archive_dir_override` is passed (every test below passes one explicitly).
fn default_archive_dir() -> PathBuf {
    if let Ok(v) = std::env::var("ANTILOKI_DATA_DIR") {
        if !v.is_empty() { return PathBuf::from(v).join("archive"); }
    }
    if let Ok(v) = std::env::var("ANTILOKI_SUBSTRATE_ROOT") {
        if !v.is_empty() { return PathBuf::from(v).join(".antiloki").join("archive"); }
    }
    std::env::current_dir().unwrap_or_default().join(".antiloki").join("archive")
}

fn truncate80(s: &str) -> String { s.chars().take(80).collect() }

// Mirrors JS `conn.prepare('SELECT name FROM <schema>.sqlite_master ...').all().map(r => r.name)`
// via the crate's shared db-lane helper (same helper `feature-inventory.rs` and siblings use).
fn table_names(db: &Connection, schema: &str) -> Result<Vec<String>, String> {
    let sql = format!("SELECT name FROM {schema}.sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'");
    let rows = crate::db_json::query_rows(db, &sql, &[])?;
    Ok(rows.as_array().cloned().unwrap_or_default().into_iter()
        .filter_map(|r| r.get("name").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .collect())
}

// Howard Hinnant's days-from-civil/civil-from-days algorithm (proleptic Gregorian, days since
// 1970-01-01, UTC). See module doc for why this twin stamps from UTC rather than local time.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn utc_stamp() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = now.as_secs() as i64;
    let days = secs.div_euclid(86400);
    let sod = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}", y, m, d, sod / 3600, (sod % 3600) / 60, sod % 60)
}

pub fn dbReset(input: &Value, db: &Connection, archive_dir_override: Option<&Path>) -> Result<Value, String> {
    // JS: String(input?.db || '').trim().toLowerCase()
    let raw = input.get("db");
    let which_raw = if truthy(raw) { js_string(raw.unwrap()) } else { String::new() };
    let which = which_raw.trim().to_lowercase();
    if which.is_empty() {
        return Err("db-reset: db is required".to_string());
    }
    let confirmed = matches!(input.get("confirm"), Some(Value::Bool(true)));
    if !confirmed {
        return Ok(json!({ "ok": false, "error": format!("destructive — pass confirm:true to wipe '{}'", which) }));
    }
    let targets: Vec<String> = if which == "all" {
        ALL.iter().map(|s| s.to_string()).collect()
    } else if alias_schema(&which).is_some() {
        vec![which.clone()]
    } else {
        return Ok(json!({ "ok": false, "error": format!("unknown db '{}' (state|memory|brain|laws|knowledge|timeline|judgments|code|notebooks|all)", which) }));
    };

    // JS: input?.archive !== false
    let archive = !matches!(input.get("archive"), Some(Value::Bool(false)));
    let archive_dir = archive_dir_override.map(|p| p.to_path_buf()).unwrap_or_else(default_archive_dir);
    let stamp = utc_stamp();
    if archive {
        let _ = std::fs::create_dir_all(&archive_dir); // best-effort, matches JS's `catch { /* best-effort */ }`
    }

    let mut wiped = Map::new();
    let mut archived = Map::new();
    for name in &targets {
        let schema = alias_schema(name).expect("targets are pre-validated against ALIAS");
        let tables: Vec<String> = match table_names(db, schema) {
            Ok(t) => t,
            Err(_) => {
                wiped.insert(name.clone(), json!("attached-absent"));
                continue;
            }
        };

        // Snapshot BEFORE wiping. If the snapshot fails, SKIP this DB's wipe — never lose data.
        if archive {
            let dest_path = archive_dir.join(format!("{name}-{stamp}.sqlite"));
            let dest = dest_path.to_string_lossy().replace('\\', "/");
            let escaped = dest.replace('\'', "''");
            match crate::db_json::run_write(db, &format!("VACUUM {schema} INTO '{escaped}'"), &[]) {
                Ok(_) => { archived.insert(name.clone(), json!(dest)); }
                Err(e) => {
                    archived.insert(name.clone(), json!(format!("FAILED — wipe skipped (no data lost): {}", truncate80(&e))));
                    continue;
                }
            }
        }

        // FK-safe: retry failed deletes (child-before-parent) until none fail or no progress.
        let mut remaining = tables.clone();
        for _pass in 0..6 {
            if remaining.is_empty() { break; }
            let mut failed = Vec::new();
            for t in &remaining {
                if crate::db_json::run_write(db, &format!("DELETE FROM {schema}.{t}"), &[]).is_err() {
                    failed.push(t.clone());
                }
            }
            if failed.len() == remaining.len() { break; }
            remaining = failed;
        }
        let cleared = tables.len() - remaining.len();
        wiped.insert(name.clone(), json!(format!("{}/{} tables cleared", cleared, tables.len())));
    }

    let needs_reseed = targets.iter().any(|t| RESEED.contains(&t.as_str()));
    let note = format!(
        "{}rows wiped — reopen the substrate (the BE does this via reopenDb) so seedCanonical re-asserts the canonical constitution.",
        if archive { "snapshotted into archive/ then " } else { "" }
    );

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("reset".into(), json!(targets));
    out.insert("wiped".into(), Value::Object(wiped));
    out.insert("archived".into(), if archive { Value::Object(archived) } else { Value::Null });
    out.insert("needs_reseed".into(), json!(needs_reseed));
    out.insert("note".into(), json!(note));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-db-reset-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // Mirrors db-reset.test.mjs's openDb(':memory:') fixture: a `main` schema (== 'state') plus
    // every other substrate DB ATTACHed under its alias name — except `code`, deliberately left
    // unattached so a run over 'all' also exercises the 'attached-absent' branch, same as a real
    // project that hasn't provisioned every DB yet.
    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT, title TEXT, status TEXT);
             CREATE TABLE actions (id INTEGER PRIMARY KEY, name TEXT);
             INSERT INTO actions (name) VALUES ('system.card.add'), ('system.card.update');
             ATTACH DATABASE ':memory:' AS memory;
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, body TEXT);
             INSERT INTO memory.memories (body) VALUES ('m1');
             ATTACH DATABASE ':memory:' AS brain;
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY);
             ATTACH DATABASE ':memory:' AS laws;
             CREATE TABLE laws.laws (id INTEGER PRIMARY KEY);
             ATTACH DATABASE ':memory:' AS knowledge;
             CREATE TABLE knowledge.extracts (id INTEGER PRIMARY KEY);
             ATTACH DATABASE ':memory:' AS timeline;
             CREATE TABLE timeline.sacred_timeline (id INTEGER PRIMARY KEY);
             ATTACH DATABASE ':memory:' AS judgments;
             CREATE TABLE judgments.verdicts (id INTEGER PRIMARY KEY);
             ATTACH DATABASE ':memory:' AS nb;
             CREATE TABLE nb.notebook_items (id INTEGER PRIMARY KEY);
             INSERT INTO nb.notebook_items DEFAULT VALUES;",
        ).unwrap();
        c
    }

    #[test]
    fn confirm_guard_and_unknown_db() {
        let c = db0();
        let r = dbReset(&json!({ "db": "state" }), &c, None).unwrap();
        assert_eq!(r["ok"], json!(false), "no confirm → refused");
        assert_eq!(r["error"], json!("destructive — pass confirm:true to wipe 'state'"));

        let r2 = dbReset(&json!({ "db": "nope", "confirm": true }), &c, None).unwrap();
        assert_eq!(r2["ok"], json!(false), "unknown db → refused");
        assert_eq!(r2["error"], json!("unknown db 'nope' (state|memory|brain|laws|knowledge|timeline|judgments|code|notebooks|all)"));
    }

    #[test]
    fn missing_db_field_throws_required() {
        let c = db0();
        assert_eq!(dbReset(&json!({ "confirm": true }), &c, None), Err("db-reset: db is required".to_string()));
        assert_eq!(dbReset(&json!({ "db": "", "confirm": true }), &c, None), Err("db-reset: db is required".to_string()));
    }

    #[test]
    fn reset_all_excludes_state_reset_state_wipes_board() {
        let c = db0();
        c.execute_batch("INSERT INTO cards (slug, title, status) VALUES ('C1','t','backlog')").unwrap();
        let actions_before: i64 = c.query_row("SELECT count(*) FROM actions", [], |r| r.get(0)).unwrap();
        assert!(actions_before > 0, "constitution seeded");

        let r_all = dbReset(&json!({ "db": "all", "confirm": true, "archive": false }), &c, None).unwrap();
        assert_eq!(r_all["ok"], json!(true));
        let reset_list: Vec<String> = r_all["reset"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert!(!reset_list.contains(&"state".to_string()), "'all' must exclude state");
        assert_eq!(r_all["wiped"]["code"], json!("attached-absent"), "code was never attached in this fixture");
        let cards_count: i64 = c.query_row("SELECT count(*) FROM cards", [], |r| r.get(0)).unwrap();
        assert_eq!(cards_count, 1, "card survives reset-all");
        let actions_after: i64 = c.query_row("SELECT count(*) FROM actions", [], |r| r.get(0)).unwrap();
        assert_eq!(actions_after, actions_before, "constitution survives reset-all");

        let r_state = dbReset(&json!({ "db": "state", "confirm": true, "archive": false }), &c, None).unwrap();
        assert_eq!(r_state["ok"], json!(true));
        assert_eq!(r_state["needs_reseed"], json!(true), "state holds constitution → flags a reseed");
        let cards_after: i64 = c.query_row("SELECT count(*) FROM cards", [], |r| r.get(0)).unwrap();
        assert_eq!(cards_after, 0, "card wiped by reset state");
    }

    #[test]
    fn reset_single_db_wipes_only_it() {
        let c = db0();
        c.execute_batch("INSERT INTO cards (slug, title, status) VALUES ('C2','t','backlog')").unwrap();
        let r = dbReset(&json!({ "db": "notebooks", "confirm": true, "archive": false }), &c, None).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["reset"], json!(["notebooks"]));
        let cards_count: i64 = c.query_row("SELECT count(*) FROM cards", [], |r| r.get(0)).unwrap();
        assert_eq!(cards_count, 1, "state untouched by a notebooks reset");
        let nb_count: i64 = c.query_row("SELECT count(*) FROM nb.notebook_items", [], |r| r.get(0)).unwrap();
        assert_eq!(nb_count, 0, "notebooks wiped");
    }

    #[test]
    fn archive_success_snapshots_before_wiping() {
        let archive_dir = temp_dir("success");
        let c = db0();
        c.execute_batch("INSERT INTO cards (slug, title, status) VALUES ('KEEP','t','backlog')").unwrap();

        let r = dbReset(&json!({ "db": "state", "confirm": true }), &c, Some(archive_dir.as_path())).unwrap();
        assert_eq!(r["ok"], json!(true));
        let snap = r["archived"]["state"].as_str().unwrap().to_string();
        assert!(Path::new(&snap).exists(), "snapshot file written to archive/");

        let snap_conn = Connection::open(&snap).unwrap();
        let kept: i64 = snap_conn.query_row("SELECT count(*) FROM cards WHERE slug='KEEP'", [], |r| r.get(0)).unwrap();
        assert_eq!(kept, 1, "card preserved in the archive snapshot");
        drop(snap_conn);

        let live: i64 = c.query_row("SELECT count(*) FROM cards", [], |r| r.get(0)).unwrap();
        assert_eq!(live, 0, "live board wiped AFTER archiving");
        let _ = fs::remove_dir_all(&archive_dir);
    }

    #[test]
    fn archive_false_writes_no_snapshots() {
        let archive_dir = temp_dir("skip");
        let c = db0();
        let r = dbReset(&json!({ "db": "notebooks", "confirm": true, "archive": false }), &c, Some(archive_dir.as_path())).unwrap();
        assert_eq!(r["archived"], Value::Null, "archive:false → no snapshots");
        let _ = fs::remove_dir_all(&archive_dir);
    }

    #[test]
    fn failed_snapshot_skips_the_wipe_data_survives() {
        // archive_dir points UNDER a plain file (not a directory) → mkdir + VACUUM INTO both fail.
        let parent = temp_dir("failparent");
        let file_as_dir = parent.join("not-a-dir");
        fs::write(&file_as_dir, "x").unwrap();
        let bogus_archive_dir = file_as_dir.join("nested");

        let c = db0();
        c.execute_batch("INSERT INTO cards (slug, title, status) VALUES ('SAFE','t','backlog')").unwrap();
        let r = dbReset(&json!({ "db": "state", "confirm": true }), &c, Some(bogus_archive_dir.as_path())).unwrap();
        assert_eq!(r["ok"], json!(true));
        let archived_state = r["archived"]["state"].as_str().unwrap().to_string();
        assert!(archived_state.contains("FAILED"), "snapshot reported failed: {archived_state}");
        let safe: i64 = c.query_row("SELECT count(*) FROM cards WHERE slug='SAFE'", [], |r| r.get(0)).unwrap();
        assert_eq!(safe, 1, "wipe SKIPPED — data not lost");
        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn utc_stamp_is_well_formed_and_civil_from_days_matches_known_epoch() {
        // 1970-01-01 (day 0) and the frozen-clock reference instant 2026-01-01 (days since epoch
        // = 20454) are well-known fixed points — sanity-check the hand-written conversion.
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20454), (2026, 1, 1));
        let s = utc_stamp();
        assert_eq!(s.len(), 15, "YYYYMMDD-HHMMSS");
        assert_eq!(s.as_bytes()[8], b'-');
    }
}
