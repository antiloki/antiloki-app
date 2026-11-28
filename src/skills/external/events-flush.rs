//! external/events-flush.rs — twin of skills/external/events-flush.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `eventsFlush` (default). Backfills `timeline_commits` rows for
//! sacred_timeline events whose git-commit-card auto-commit was lost (race, hook failure, manual
//! recovery commit, etc.). STRICTLY backfills against real git history; never fabricates a sha.
//!
//! `sacred_timeline` is production-attached (`timeline.sacred_timeline`, see
//! open-db/schemas.mjs's own header note: "Unqualified `sacred_timeline` references across the
//! codebase auto-resolve to this attached copy — it is the only one"). The JS source here writes
//! the table name UNQUALIFIED, and — same precedent as system_substrate_stream.rs's own
//! `sacred_timeline` reads — this twin's fixture defines it plain in the MAIN schema (no `ATTACH`
//! needed): since only one schema exposes the name, unqualified resolution lands on it
//! identically in both lanes, and no cross-db ATTACH machinery is required for behavioral parity.
//!
//! CENSUS NOTE: the JS module also names-exports `findRecentCommitForSlug`/`shaExists` (bottom of
//! the source) purely so its OWN test file can inject a stub `runner` callback into them — neither
//! is the dispatched skill (only the default export is wired into actions), and neither has any
//! real caller with a different signature. Both take a `runner` FUNCTION as their required second
//! parameter — not a JSON `Value`, VET's explicit "takes a callback/function argument" blocked
//! criterion — so they are not independent convertible units. This twin inlines their REAL logic
//! as the `shaExists`/`findRecentCommitForSlug` helpers below (pub, JS `export { ... }` names; real
//! `std::process::Command` git calls; Rust has no closure-typed JSON arg to thread an injectable
//! seam through), used only internally by `eventsFlush`. No frontier entry, no fixture, for either
//! name individually.
//!
//! GIT SUBPROCESS BRANCHES — DISCLOSED UNTESTED (playbook lesson 5's flag-skippable pattern, here
//! gated by DB STATE rather than an input flag): `eventsFlush` only reaches git at all when the
//! slug has at least one uncommitted sacred_timeline event. Both git-touching branches are ported
//! for real, mirroring the JS `execSync` calls exactly:
//!   - explicit `commit_sha` → real `git cat-file -t <sha>` (existence check; refuses to fabricate
//!     if not found)
//!   - else → real `git log --all --grep=<slug> --format=%H|%s -n 50`, first line whose subject
//!     contains the slug wins
//! (`--grep`'s value is passed as a single `Command` arg rather than shell-interpreted — like
//! constitution-estimate.rs's `git_diff_paths`, irrelevant here since slugs never contain shell
//! metacharacters and this branch carries no fixture coverage either way). The fixture instead
//! exercises ONLY the git-free early return (`uncommitted.length === 0`), seeded two ways: no
//! sacred_timeline rows for the slug at all, and rows that are all already present in
//! `timeline_commits` — the sole branch whose result does not depend on live git/repo state.
// QUALITY: disclosed-untested git branches (real Command calls, no fixture coverage — see above)

use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

const UNCOMMITTED_FOR_SLUG: &str = "SELECT t.id, t.operation, t.entity_id, t.ts \
    FROM sacred_timeline t \
    LEFT JOIN timeline_commits c ON c.timeline_id = t.id \
    WHERE t.entity_type = 'card' AND t.entity_id = ? AND c.timeline_id IS NULL \
    ORDER BY t.id ASC";

/// `export { shaExists }` — real `git cat-file -t <sha>`. See module doc:
/// disclosed UNTESTED by fixture (git-state dependent, DB-state-skippable).
pub fn shaExists(sha: &str) -> bool {
    crate::proc::cmd("git")
        .arg("cat-file")
        .arg("-t")
        .arg(sha)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `export { findRecentCommitForSlug }` — real
/// `git log --all --grep=<slug> --format=%H|%s -n 50`, first line whose subject contains the slug
/// wins. See module doc: disclosed UNTESTED by fixture. Passed as discrete `Command` args (no
/// shell) rather than the JS's one unquoted shell string — the JS's own `--format=%H|%s` token is
/// NOT quoted inside its execSync command string, so a real shell would treat the bare `|` as a
/// pipe operator, not literal format text (a latent bug in the source, never exercised outside its
/// stubbed-runner unit tests). This twin's arg-vector form sends `%H|%s` to git as one literal
/// argument, which is what the JS plainly intends — same disclosed-harmless divergence as
/// constitution-estimate.rs's `git_diff_paths` (irrelevant either way: no fixture exercises this
/// branch).
pub fn findRecentCommitForSlug(slug: &str) -> Option<String> {
    let output = crate::proc::cmd("git")
        .arg("log")
        .arg("--all")
        .arg(format!("--grep={slug}"))
        .arg("--format=%H|%s")
        .arg("-n")
        .arg("50")
        .output();
    let out = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => return None,
    };
    for line in out.trim().split('\n') {
        if line.trim().is_empty() { continue; }
        let idx = match line.find('|') { Some(i) => i, None => continue };
        let sha = &line[..idx];
        let subject = &line[idx + 1..];
        if subject.contains(slug) { return Some(sha.to_string()); }
    }
    None
}

/// `export default function eventsFlush(input, context = {})`
pub fn eventsFlush(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `if (!db) throw err.contextMissing(...)` — unreachable, `db: &Connection` is required.
    // JS: `if (!slug || typeof slug !== 'string') throw err.required(...)` — a falsy OR
    // non-string slug both collapse into this one guard (as_str() is None for either shape,
    // and the emptiness check catches the falsy-empty-string case as_str() alone would miss).
    let slug = match input.get("slug").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s,
        _ => return Err("events-flush: slug is required".into()),
    };
    let commit_sha = nz(input.get("commit_sha"));

    let rows = crate::db_json::query_rows(db, UNCOMMITTED_FOR_SLUG, &[json!(slug)])?;
    let uncommitted: Vec<Value> = rows.as_array().cloned().unwrap_or_default();

    if uncommitted.is_empty() {
        return Ok(json!({
            "flushed": 0i64,
            "slug": slug,
            "commit_sha": Value::Null,
            "reason": "no uncommitted events for this slug — nothing to flush",
        }));
    }

    let sha: Value = if truthy(commit_sha) {
        let sha_str = tmpl(commit_sha);
        if !shaExists(&sha_str) {
            return Err(format!(
                "events-flush: commit '{sha_str}' not found in git history; refusing to fabricate timeline_commits rows"
            ));
        }
        commit_sha.cloned().unwrap()
    } else {
        match findRecentCommitForSlug(slug) {
            Some(s) => json!(s),
            None => {
                return Ok(json!({
                    "flushed": 0i64,
                    "slug": slug,
                    "commit_sha": Value::Null,
                    "reason": format!("no git commit found referencing slug '{slug}'; pass commit_sha explicitly if a matching commit exists"),
                }));
            }
        }
    };

    let mut event_ids: Vec<Value> = Vec::with_capacity(uncommitted.len());
    for e in &uncommitted {
        let id = e.get("id").cloned().unwrap_or(Value::Null);
        crate::db_json::run_write(
            db,
            "INSERT INTO timeline_commits (timeline_id, git_sha) VALUES (?, ?)",
            &[id.clone(), sha.clone()],
        )?;
        event_ids.push(id);
    }

    Ok(json!({
        "flushed": uncommitted.len() as i64,
        "slug": slug,
        "commit_sha": sha,
        "event_ids": event_ids,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT, entity_id TEXT, before_json TEXT, after_json TEXT, payload TEXT, prev_print TEXT, digital_print TEXT); \
             CREATE TABLE timeline_commits (timeline_id INTEGER PRIMARY KEY, git_sha TEXT NOT NULL, committed_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');",
        )
        .unwrap();
        c
    }

    #[test]
    fn no_events_at_all_returns_flushed_zero() {
        let c = db();
        let r = eventsFlush(&json!({ "slug": "NEVER-TOUCHED" }), &c).unwrap();
        assert_eq!(r, json!({ "flushed": 0, "slug": "NEVER-TOUCHED", "commit_sha": null, "reason": "no uncommitted events for this slug — nothing to flush" }));
    }

    #[test]
    fn all_events_already_committed_returns_flushed_zero() {
        let c = db();
        c.execute("INSERT INTO sacred_timeline (id, ts, actor, operation, entity_type, entity_id, prev_print, digital_print) VALUES (1, '2025-12-01 00:00:00', 'test', 'card.add', 'card', 'ALREADY-CLEAN', 'p0', 'd1')", []).unwrap();
        c.execute("INSERT INTO timeline_commits (timeline_id, git_sha) VALUES (1, 'fake-sha')", []).unwrap();
        let r = eventsFlush(&json!({ "slug": "ALREADY-CLEAN" }), &c).unwrap();
        assert_eq!(r, json!({ "flushed": 0, "slug": "ALREADY-CLEAN", "commit_sha": null, "reason": "no uncommitted events for this slug — nothing to flush" }));
    }

    #[test]
    fn missing_slug_is_a_guard_throw() {
        let c = db();
        assert_eq!(eventsFlush(&json!({}), &c).unwrap_err(), "events-flush: slug is required");
    }
}
