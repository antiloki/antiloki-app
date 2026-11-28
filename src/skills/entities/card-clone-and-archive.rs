//! card-clone-and-archive.rs — twin of skills/entities/card-clone-and-archive.mjs (db WRITE lane,
//! async default export — converted per the playbook's "async default exports" rule: the JS
//! `await`s two sync-over-sqlite `withAudit` calls, so this twin stays an ordinary sync `pub fn`).
//! NAMES ARE CONTRACT: `cardCloneAndArchive` (default).
//!
//! Operator "reset card": history is immutable, so a reset CLONES the card's authored content
//! into a fresh backlog card (via `card-clone-insert`, sealed `card.add`) and ARCHIVES the
//! original (via `card-status-change`, sealed `card.status.change`) — two sequential audited
//! mutations composed through the already-converted `engine_with_audit::withAudit` twin, which in
//! turn dispatches those two skill names through its own `dispatch_skill` table. This twin's
//! conversion required extending that table with a `"card-clone-insert"` arm (the JS's real
//! `importSkill()` can already resolve it; the Rust dispatch table is the hand-kept mirror the
//! module doc comment on `with-audit.rs` says to extend "arm-by-arm as more twins land").
//!
//! Preview (confirm falsy): computes the clone slug + what carries, mutates nothing. Execute
//! (confirm:true): performs both writes. STALE-SURFACE RESET: an already-archived slug resolves
//! FORWARD to the newest live (non-archived) card in the same `-rN` reset family and resets THAT
//! instead of erroring — `resolved_from` reports the originally-requested (stale) slug.
// QUALITY: twin of FEAT-CARD-RESET-CLONE-ARCHIVE — sequential with-audit composition, stale-surface resolve-forward, reset-chain slug increment
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand

// JS `originalSlug.replace(/-r\d+$/, '')` — strip a trailing reset-generation suffix.
fn strip_r_suffix(s: &str) -> String {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"-r\d+$").unwrap());
    re.replace(s, "").into_owned()
}

// JS `/-r(\d+)$/.exec(x)` → the captured generation number, or None if the slug has no `-rN` tail.
fn r_number(s: &str) -> Option<i64> {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"-r(\d+)$").unwrap());
    re.captures(s).and_then(|c| c.get(1)).and_then(|m| m.as_str().parse::<i64>().ok())
}
// JS `rank(x)` (the stale-surface resolve-forward helper): unmatched → 1 (the original is the
// implicit r1).
fn rank(s: &str) -> i64 { r_number(s).unwrap_or(1) }

// JS `nextCloneSlug(db, originalSlug)`: strip any trailing -rN so a reset-of-a-reset stays in one
// family, then pick the next integer above the highest existing -rN (the original is the implicit
// r1). FOO → FOO-r2 → FOO-r3 …
fn next_clone_slug(db: &Connection, original_slug: &str) -> Result<String, String> {
    let base = strip_r_suffix(original_slug);
    let rows = crate::db_json::query_rows(
        db,
        "SELECT slug FROM cards WHERE slug = ? OR slug LIKE ?",
        &[json!(base.clone()), json!(format!("{base}-r%"))],
    )?;
    let mut max_n: i64 = 1;
    for r in rows.as_array().cloned().unwrap_or_default() {
        if let Some(slug) = r.get("slug").and_then(|v| v.as_str()) {
            if let Some(n) = r_number(slug) {
                if n > max_n { max_n = n; }
            }
        }
    }
    Ok(format!("{base}-r{}", max_n + 1))
}

pub fn cardCloneAndArchive(input: &Value, db: &Connection) -> Result<Value, String> {
    let confirm = truthy(input.get("confirm"));
    let mut slug = match input.get("slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => return Err("card-clone-and-archive: slug is required".into()),
    };

    let rows = crate::db_json::query_rows(db, "SELECT * FROM cards WHERE slug = ?", &[json!(slug.clone())])?;
    let mut card = match rows.as_array().and_then(|a| a.first()).cloned() {
        Some(c) => c,
        None => return Err(format!("card-clone-and-archive: card '{slug}' not found")),
    };

    let mut resolved_from = Value::Null;
    if card.get("status").and_then(|v| v.as_str()) == Some("archived") {
        // STALE-SURFACE RESET: resolve FORWARD to the newest non-archived card in the same reset
        // family and reset THAT instead of erroring.
        let base = strip_r_suffix(&slug);
        let live_rows = crate::db_json::query_rows(
            db,
            "SELECT * FROM cards WHERE (slug = ? OR slug LIKE ?) AND status != 'archived'",
            &[json!(base.clone()), json!(format!("{base}-r%"))],
        )?;
        // JS `.sort((a,b) => rank(b.slug) - rank(a.slug))[0]`: stable descending sort by rank,
        // take the first — equivalent to "first row seen with the max rank" over original order.
        let mut best: Option<Value> = None;
        let mut best_rank = i64::MIN;
        for r in live_rows.as_array().cloned().unwrap_or_default() {
            let s = r.get("slug").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let rk = rank(&s);
            if best.is_none() || rk > best_rank {
                best_rank = rk;
                best = Some(r);
            }
        }
        let live = match best {
            Some(l) => l,
            None => return Err(format!(
                "card-clone-and-archive: card '{slug}' is already archived and its reset family has no live clone — nothing to reset"
            )),
        };
        resolved_from = json!(slug.clone());
        slug = live.get("slug").and_then(|v| v.as_str()).unwrap_or("").to_string();
        card = live;
    }

    let clone_slug = next_clone_slug(db, &slug)?;

    let title = card.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
    static RESET_SUFFIX: OnceLock<Regex> = OnceLock::new();
    let reset_suffix = RESET_SUFFIX.get_or_init(|| Regex::new(r"\(reset\)\s*$").unwrap());
    let clone_title = if reset_suffix.is_match(&title) { title.clone() } else { format!("{title} (reset)") };

    let body_json = card.get("body_json").cloned().unwrap_or(Value::Null);
    let acceptance_json = nz(card.get("acceptance_json")).cloned().unwrap_or(json!("[]"));
    let risk = nz(card.get("risk")).cloned().unwrap_or(Value::Null);
    let complexity = nz(card.get("complexity")).cloned().unwrap_or(Value::Null);
    let priority = nz(card.get("priority")).cloned().unwrap_or(Value::Null);
    let score = nz(card.get("score")).cloned().unwrap_or(Value::Null);
    let parent_slug = nz(card.get("parent_slug")).cloned().unwrap_or(Value::Null);

    let mut carries = Map::new();
    carries.insert("title".into(), json!(clone_title));
    carries.insert("body".into(), json!(!body_json.is_null()));
    carries.insert("acceptance_json".into(), acceptance_json.clone());
    carries.insert("risk".into(), risk.clone());
    carries.insert("complexity".into(), complexity.clone());
    carries.insert("priority".into(), priority.clone());
    carries.insert("score".into(), score.clone());
    carries.insert("parent_slug".into(), parent_slug.clone());

    if !confirm {
        let mut out = Map::new();
        out.insert("preview".into(), json!(true));
        out.insert("slug".into(), json!(slug));
        out.insert("resolved_from".into(), resolved_from);
        out.insert("clone_slug".into(), json!(clone_slug));
        out.insert("will_archive".into(), json!(slug));
        out.insert("clone_title".into(), json!(clone_title));
        out.insert("prev_status".into(), card.get("status").cloned().unwrap_or(Value::Null));
        out.insert("carries".into(), Value::Object(carries));
        out.insert("next_step".into(), json!(format!("confirm to create '{clone_slug}' (fresh backlog card) and archive '{slug}'")));
        return Ok(Value::Object(out));
    }

    // ── Execute: two sequential audited card mutations ───────────────────
    // 1) clone INSERT, sealed as 'card.add' on the NEW slug (before=null → after=row).
    let clone_payload = json!({
        "skill": "card-clone-insert",
        "wrappedInput": {
            "slug": clone_slug, "title": clone_title,
            "body_json": body_json, "acceptance_json": acceptance_json,
            "risk": risk, "complexity": complexity, "priority": priority, "score": score,
            "parent_slug": parent_slug, "cloned_from": slug,
        },
        "operation": "card.add", "entity_type": "card", "entity_id": clone_slug,
    });
    let clone = crate::engine_with_audit::withAudit(&clone_payload, db)?;

    // 2) archive the ORIGINAL, sealed as 'card.status.change' on the original slug.
    let archive_payload = json!({
        "skill": "card-status-change",
        "wrappedInput": { "slug": slug, "status": "archived" },
        "operation": "card.status.change", "entity_type": "card", "entity_id": slug,
    });
    crate::engine_with_audit::withAudit(&archive_payload, db)?;

    let mut out = Map::new();
    out.insert("executed".into(), json!(true));
    out.insert("slug".into(), json!(slug));
    out.insert("resolved_from".into(), resolved_from);
    out.insert("clone_slug".into(), json!(clone_slug));
    out.insert("archived".into(), json!(slug));
    out.insert("clone_title".into(), json!(clone_title));
    out.insert("clone".into(), clone);
    out.insert("next_step".into(), json!(format!("'{slug}' archived; work continues on the fresh clone '{clone_slug}' (play it to start)")));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT UNIQUE, title TEXT, status TEXT, \
               body_json TEXT, acceptance_json TEXT, risk INTEGER, complexity INTEGER, priority INTEGER, score INTEGER, \
               parent_slug TEXT, cloned_from TEXT, created_at TEXT, updated_at TEXT);
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, actor TEXT, operation TEXT, \
               entity_type TEXT, entity_id TEXT, before_json TEXT, after_json TEXT, payload TEXT, prev_print TEXT, \
               digital_print TEXT, user_goal_id TEXT, exec_env TEXT, tick_id TEXT);
             INSERT INTO cards (id, slug, title, status, body_json, acceptance_json, risk, complexity, priority, score, parent_slug, cloned_from, created_at, updated_at)
               VALUES (1, 'FOO', 'My card', 'doing', '{\"a\":1}', '[{\"id\":\"ca1\"}]', 3, 4, 2, 24, 'EPIC-1', NULL, '2025-12-01 00:00:00', '2025-12-01 00:00:00');",
        )
        .unwrap();
        c
    }

    #[test]
    fn preview_mutates_nothing_and_reports_what_carries() {
        let c = db();
        let r = cardCloneAndArchive(&json!({ "slug": "FOO" }), &c).unwrap();
        assert_eq!(r["preview"], json!(true));
        assert_eq!(r["clone_slug"], json!("FOO-r2"));
        assert_eq!(r["will_archive"], json!("FOO"));
        assert_eq!(r["clone_title"], json!("My card (reset)"));
        assert_eq!(r["resolved_from"], Value::Null);
        assert_eq!(r["carries"]["risk"], json!(3));
        assert_eq!(r["carries"]["score"], json!(24));
        assert_eq!(r["carries"]["body"], json!(true));
        let cards = crate::db_json::query_rows(&c, "SELECT COUNT(*) n FROM cards", &[]).unwrap();
        assert_eq!(cards[0]["n"], json!(1)); // nothing written
    }

    #[test]
    fn execute_clones_and_archives_with_two_audit_seals() {
        let c = db();
        let r = cardCloneAndArchive(&json!({ "slug": "FOO", "confirm": true }), &c).unwrap();
        assert_eq!(r["executed"], json!(true));
        assert_eq!(r["clone_slug"], json!("FOO-r2"));
        assert_eq!(r["archived"], json!("FOO"));
        assert_eq!(r["clone"]["status"], json!("backlog"));
        assert_eq!(r["clone"]["cloned_from"], json!("FOO"));

        let orig = crate::db_json::query_rows(&c, "SELECT status FROM cards WHERE slug='FOO'", &[]).unwrap();
        assert_eq!(orig[0]["status"], json!("archived"));

        let tl = crate::db_json::query_rows(&c, "SELECT operation, entity_id FROM sacred_timeline ORDER BY id", &[]).unwrap();
        let rows = tl.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["operation"], json!("card.add"));
        assert_eq!(rows[0]["entity_id"], json!("FOO-r2"));
        assert_eq!(rows[1]["operation"], json!("card.status.change"));
        assert_eq!(rows[1]["entity_id"], json!("FOO"));
    }

    #[test]
    fn stale_surface_reset_resolves_forward_to_live_clone() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT UNIQUE, title TEXT, status TEXT, \
               body_json TEXT, acceptance_json TEXT, risk INTEGER, complexity INTEGER, priority INTEGER, score INTEGER, \
               parent_slug TEXT, cloned_from TEXT, created_at TEXT, updated_at TEXT);
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, actor TEXT, operation TEXT, \
               entity_type TEXT, entity_id TEXT, before_json TEXT, after_json TEXT, payload TEXT, prev_print TEXT, \
               digital_print TEXT, user_goal_id TEXT, exec_env TEXT, tick_id TEXT);
             INSERT INTO cards (id, slug, title, status) VALUES
               (1, 'FOO2', 'Foo2 card', 'archived'),
               (2, 'FOO2-r2', 'Foo2 card (reset)', 'archived'),
               (3, 'FOO2-r3', 'Foo2 card (reset)', 'doing');",
        )
        .unwrap();

        let preview = cardCloneAndArchive(&json!({ "slug": "FOO2-r2" }), &c).unwrap();
        assert_eq!(preview["resolved_from"], json!("FOO2-r2"));
        assert_eq!(preview["slug"], json!("FOO2-r3"));
        assert_eq!(preview["clone_slug"], json!("FOO2-r4"));

        let done = cardCloneAndArchive(&json!({ "slug": "FOO2-r2", "confirm": true }), &c).unwrap();
        assert_eq!(done["executed"], json!(true));
        assert_eq!(done["archived"], json!("FOO2-r3"));
        let r3 = crate::db_json::query_rows(&c, "SELECT status FROM cards WHERE slug='FOO2-r3'", &[]).unwrap();
        assert_eq!(r3[0]["status"], json!("archived"));
        let r4 = crate::db_json::query_rows(&c, "SELECT status FROM cards WHERE slug='FOO2-r4'", &[]).unwrap();
        assert_eq!(r4[0]["status"], json!("backlog"));
    }

    #[test]
    fn reset_of_reset_does_not_double_the_title_suffix() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT UNIQUE, title TEXT, status TEXT, \
               body_json TEXT, acceptance_json TEXT, risk INTEGER, complexity INTEGER, priority INTEGER, score INTEGER, \
               parent_slug TEXT, cloned_from TEXT, created_at TEXT, updated_at TEXT);
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, actor TEXT, operation TEXT, \
               entity_type TEXT, entity_id TEXT, before_json TEXT, after_json TEXT, payload TEXT, prev_print TEXT, \
               digital_print TEXT, user_goal_id TEXT, exec_env TEXT, tick_id TEXT);
             INSERT INTO cards (id, slug, title, status, acceptance_json) VALUES (1, 'QUX-r2', 'Qux card (reset)', 'doing', '[]');",
        )
        .unwrap();
        let r = cardCloneAndArchive(&json!({ "slug": "QUX-r2", "confirm": true }), &c).unwrap();
        assert_eq!(r["clone_slug"], json!("QUX-r3"), "strips -rN, picks next above the max");
        assert_eq!(r["clone_title"], json!("Qux card (reset)"), "reset suffix not doubled");
    }
}
