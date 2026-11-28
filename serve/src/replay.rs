//! PLAN REPLAY — spend the model on work, not on deciding the same thing twice (HyperAgent's action cache, at the
//! altitude that fits code; queued 2026-09-25 by the operator: "develop them, improve them").
//!
//! HyperAgent records a browser run's XPath steps and replays them with no model call, falling back to the LLM when
//! the page has changed. Copying that literally would be nonsense here: replaying a diff onto a repository that has
//! moved is how you corrupt a branch. What transfers is the ALTITUDE — cache the decision, not the keystrokes.
//!
//! So this caches THE PLAN: the planner's own JSON reply, the single most expensive fixed stage of a run. Every bench
//! since 2026-09-22 says the same thing — the workers are nearly free and the supervision is the bill — so the stage
//! worth not paying for twice is the one that thinks, not the one that types.
//!
//! THREE GUARDS, AND THEY ARE THE FEATURE. A stale plan is worse than no cache, so a replay is only offered when:
//!   1. the ask NORMALISES to the same signature — same words, ignoring case, spacing and punctuation;
//!   2. the repository is at the same COMMIT — a plan is about a tree, and a moved tree deserves a new thought;
//!   3. every path the cached plan claims to own still EXISTS, or was to be created.
//! Their oracle is better than HyperAgent's, too: their replay fails when an element is missing, ours is checked by
//! the gates that already run after every turn — a replayed plan that no longer works fails its tests like any other.
//!
//! AND IT COUNTS ITSELF EITHER WAY. Every lookup is recorded as a hit or a miss with what it would have saved, so
//! "is this worth having on?" is answered by the record instead of by the enthusiasm of whoever built it.
use serde_json::{json, Value};

/// THE SIGNATURE OF AN ASK. Lowercased, punctuation dropped, whitespace collapsed — so "Fix the login." and "fix the
/// login" are one ask, while "fix the logout" is not. Deliberately NOT fuzzy: a near-match would replay a plan made
/// for a different job, which is the one failure this cache must never have. Pure.
pub fn signature(goal: &str, stack: &str, domain: &str) -> String {
    let words: Vec<String> = goal.to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '/' || c == '.' || c == '_' || c == '-'))
        // a trailing full stop is punctuation; a dot INSIDE a word is part of a filename, so only the edges are
        // trimmed — "login." and "login" are one word, `src/a.ts` stays itself
        .map(|w| w.trim_matches(|c: char| matches!(c, '.' | '-' | '_')).to_string())
        .filter(|w| !w.is_empty())
        .collect();
    format!("{}|{}|{}", stack.trim().to_lowercase(), domain.trim().to_lowercase(), words.join(" "))
}

fn open() -> Result<rusqlite::Connection, String> {
    let c = crate::merges::open().map_err(|e| e.to_string())?;
    c.execute_batch("CREATE TABLE IF NOT EXISTS plan_cache (
        sig TEXT NOT NULL, commit_sha TEXT NOT NULL, plan TEXT NOT NULL, owns TEXT NOT NULL DEFAULT '[]',
        usd REAL NOT NULL DEFAULT 0, ms INTEGER NOT NULL DEFAULT 0, n INTEGER NOT NULL DEFAULT 1,
        made INTEGER NOT NULL, UNIQUE (sig, commit_sha));
      CREATE TABLE IF NOT EXISTS plan_cache_log (
        t INTEGER NOT NULL, hit INTEGER NOT NULL, reason TEXT NOT NULL DEFAULT '',
        saved_usd REAL NOT NULL DEFAULT 0, saved_ms INTEGER NOT NULL DEFAULT 0);")
        .map_err(|e| e.to_string())?;
    Ok(c)
}

/// KEEP A PLAN THAT WORKED. Called after a run plans, with what the planner actually replied and what that reply cost
/// — the cost is kept so a later hit can say what it saved instead of claiming a saving in the abstract.
pub fn remember(sig: &str, commit: &str, plan: &str, owns: &[String], usd: f64, ms: i64) -> Result<(), String> {
    if plan.trim().is_empty() { return Ok(()) }
    let c = open()?;
    c.execute("INSERT INTO plan_cache (sig, commit_sha, plan, owns, usd, ms, n, made) VALUES (?1,?2,?3,?4,?5,?6,1,?7)
               ON CONFLICT(sig, commit_sha) DO UPDATE SET plan = ?3, owns = ?4, usd = ?5, ms = ?6, n = n + 1, made = ?7",
        rusqlite::params![sig, commit, plan, serde_json::to_string(owns).unwrap_or_else(|_| "[]".into()), usd, ms, crate::cli::now_ms()])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// WHAT A LOOKUP FOUND — and, when it found nothing, why. The reason is reported rather than swallowed, because "the
/// commit moved" and "nobody has asked this before" are different facts and only one of them means the cache is
/// useless for your way of working.
#[derive(Clone, Debug, PartialEq)]
pub enum Recall {
    Hit { plan: String, usd: f64, ms: i64 },
    Miss(&'static str),
}

/// Look for a usable plan. `exists` answers "is this path in the box?", injected so the decision is pure and testable.
pub fn recall<F: Fn(&str) -> bool>(sig: &str, commit: &str, exists: F) -> Recall {
    let Ok(c) = open() else { return Recall::Miss("the cache could not be opened") };
    let row: Option<(String, String, f64, i64)> = c.query_row(
        "SELECT plan, owns, usd, ms FROM plan_cache WHERE sig = ?1 AND commit_sha = ?2",
        rusqlite::params![sig, commit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).ok();
    // A SIGNATURE THAT MATCHES AT ANOTHER COMMIT IS NOT A HIT. It is the interesting miss, though — it means this ask
    // repeats and the tree moved — so it is named as itself.
    let Some((plan, owns, usd, ms)) = row else {
        let elsewhere: i64 = c.query_row("SELECT count(*) FROM plan_cache WHERE sig = ?1", rusqlite::params![sig], |r| r.get(0)).unwrap_or(0);
        return Recall::Miss(if elsewhere > 0 { "the same ask, but the repository has moved since" } else { "this ask has not been planned before" });
    };
    // THE GUARD THAT BLOCKED THE PLANS MOST WORTH REPLAYING (removed 2026-09-26, after watching it work).
    //
    // It required every fixed path the plan owns to exist. That sounds careful and is backwards: a plan whose tasks
    // CREATE their files — a scaffold, a feature laid out across new modules, the most repeatable ask there is — owns
    // paths that by definition do not exist before it runs. Five of the night's ten lookups missed for exactly this
    // reason, every one of them a creation plan.
    //
    // What it was protecting against is already covered, and covered better: the COMMIT pins the tree. A file removed
    // in a commit changes the sha and misses; a file removed in the working tree without a commit would break the
    // plan's task, and the gates that run after every turn are what catch that — they are a better oracle than an
    // existence check, which cannot tell a deleted file from a file not yet written.
    //
    // `owns` is still stored: it is what the miss reasons and any future guard would read, and throwing away evidence
    // to save a column is how the next question becomes unanswerable.
    let _ = (&owns, &exists);
    Recall::Hit { plan, usd, ms }
}

/// count a lookup, whichever way it went
pub fn note(hit: bool, reason: &str, saved_usd: f64, saved_ms: i64) {
    let Ok(c) = open() else { return };
    let _ = c.execute("INSERT INTO plan_cache_log (t, hit, reason, saved_usd, saved_ms) VALUES (?1,?2,?3,?4,?5)",
        rusqlite::params![crate::cli::now_ms(), i64::from(hit), reason, saved_usd, saved_ms]);
}

/// WHAT THE CACHE HAS ACTUALLY DONE — hits, misses, and the money and minutes it did not spend. The verdict says
/// plainly when the answer is "nothing yet", because a cache that has never hit is a cache nobody needs, and that is
/// worth knowing before anyone tunes it.
pub fn report() -> Result<Value, String> {
    let c = open()?;
    let (hits, misses, usd, ms): (i64, i64, f64, i64) = c.query_row(
        "SELECT COALESCE(SUM(hit),0), COALESCE(SUM(1-hit),0), COALESCE(SUM(saved_usd),0), COALESCE(SUM(saved_ms),0) FROM plan_cache_log",
        [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap_or((0, 0, 0.0, 0));
    let plans: i64 = c.query_row("SELECT count(*) FROM plan_cache", [], |r| r.get(0)).unwrap_or(0);
    let total = hits + misses;
    let verdict = if total == 0 {
        "no run has looked for a cached plan yet".to_string()
    } else if hits == 0 {
        format!("{total} lookups, no hit yet — {plans} plan(s) kept; this way of working may simply not repeat")
    } else {
        format!("{hits} of {total} runs replayed their plan, saving ${:.2} and {} minutes of planning", usd, (ms as f64 / 60_000.0).round())
    };
    let mut by_reason: Vec<Value> = vec![];
    let mut st = c.prepare("SELECT reason, count(*) FROM plan_cache_log WHERE hit = 0 GROUP BY reason ORDER BY count(*) DESC").map_err(|e| e.to_string())?;
    for r in st.query_map([], |r| Ok(json!({ "reason": r.get::<_, String>(0)?, "n": r.get::<_, i64>(1)? }))).map_err(|e| e.to_string())?.filter_map(Result::ok) { by_reason.push(r); }
    Ok(json!({ "ok": true, "hits": hits, "misses": misses, "plans": plans,
               "saved_usd": (usd * 100.0).round() / 100.0, "saved_ms": ms, "misses_by_reason": by_reason, "verdict": verdict }))
}

pub async fn http_report() -> axum::response::Response {
    use axum::response::IntoResponse;
    match tokio::task::spawn_blocking(report).await.unwrap_or_else(|e| Err(e.to_string())) {
        Ok(v) => axum::Json(v).into_response(),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, axum::Json(json!({ "ok": false, "error": e }))).into_response(),
    }
}

/// A RUN'S PLAN, WRITTEN BACK AS THE PLANNER WOULD HAVE SAID IT. Re-serialised from the route and the tasks rather
/// than stored raw: what the model wrapped around its JSON is noise, the sanitised plan is what actually ran, and a
/// replay should hand the next run the plan that WORKED rather than the prose that carried it.
pub fn plan_json(tier: &str, delegate: bool, why: &str, confidence: &str, tasks: &[Value]) -> String {
    json!({ "tier": tier, "delegate": delegate, "why": why, "confidence": confidence, "tasks": tasks }).to_string()
}

/// the box's current commit, or "" when it is not a git worktree — an empty commit never matches a stored one, so a
/// box without git simply never replays rather than replaying wrongly.
pub fn commit_of(cwd: &str) -> String {
    let out = crate::proc::cmd("git").args(["rev-parse", "HEAD"]).current_dir(cwd).output();
    match out { Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(), _ => String::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn all(_: &str) -> bool { true }
    fn none(_: &str) -> bool { false }

    #[test]
    fn the_signature_ignores_spelling_noise_and_nothing_else() {
        let a = signature("Fix the login.", "rust", "backend");
        assert_eq!(a, signature("fix   the    login", "rust", "backend"));
        assert_eq!(a, signature("FIX THE LOGIN!", "RUST", "Backend"));
        // a different ask, a different language, a different kind of work: three different signatures
        assert_ne!(a, signature("fix the logout", "rust", "backend"));
        assert_ne!(a, signature("fix the login", "python", "backend"));
        assert_ne!(a, signature("fix the login", "rust", "frontend"));
        // a path is part of the ask, not punctuation to be dropped
        assert_ne!(signature("tidy src/a.ts", "web", "any"), signature("tidy src/b.ts", "web", "any"));
    }

    #[test]
    fn an_ask_nobody_has_planned_is_a_miss_that_says_so() {
        let _ = std::fs::remove_file(crate::merges::path());
        assert_eq!(recall("nope", "abc123", all), Recall::Miss("this ask has not been planned before"));
    }

    #[test]
    fn the_same_ask_at_the_same_commit_replays_and_carries_what_it_saved() {
        let _ = std::fs::remove_file(crate::merges::path());
        let sig = signature("rebuild the parser", "rust", "backend");
        remember(&sig, "abc123", r#"{"tier":"mid","delegate":false,"tasks":[]}"#, &["src/lib.rs".into()], 0.42, 61_000).unwrap();
        match recall(&sig, "abc123", all) {
            Recall::Hit { plan, usd, ms } => { assert!(plan.contains("\"tier\"")); assert!((usd - 0.42).abs() < 1e-9); assert_eq!(ms, 61_000); }
            m => panic!("expected a hit, got {m:?}"),
        }
    }

    #[test]
    fn a_moved_repository_is_a_miss_and_it_is_named_as_the_interesting_one() {
        let _ = std::fs::remove_file(crate::merges::path());
        let sig = signature("rebuild the parser", "rust", "backend");
        remember(&sig, "abc123", "{}", &[], 0.4, 1000).unwrap();
        assert_eq!(recall(&sig, "def456", all), Recall::Miss("the same ask, but the repository has moved since"),
            "a plan is about a tree; a moved tree deserves a new thought");
    }

    /// THE PLAN MOST WORTH REPLAYING OWNS FILES THAT DO NOT EXIST YET (2026-09-26). A scaffold — "add eight modules
    /// under src/features/" — owns paths it is about to create, and an existence check refuses exactly those. Five of
    /// the night's ten real lookups missed for this reason. The commit is the guard; this is not.
    #[test]
    fn a_plan_that_creates_its_files_still_replays() {
        let _ = std::fs::remove_file(crate::merges::path());
        let sig = signature("add eight modules under src/features", "web", "frontend");
        remember(&sig, "abc123", "{}", &["src/features/search.ts".into()], 0.4, 1000).unwrap();
        assert!(matches!(recall(&sig, "abc123", none), Recall::Hit { .. }),
            "a plan whose files do not exist yet is the commonest repeatable ask, not a stale plan");
        // and the commit still decides: another tree, another thought
        assert_eq!(recall(&sig, "def456", all), Recall::Miss("the same ask, but the repository has moved since"));
    }

    #[test]
    fn the_report_says_plainly_when_the_cache_has_never_helped() {
        let _ = std::fs::remove_file(crate::merges::path());
        let r = report().unwrap();
        assert!(r["verdict"].as_str().unwrap().contains("no run has looked"), "{}", r["verdict"]);

        note(false, "this ask has not been planned before", 0.0, 0);
        note(false, "the same ask, but the repository has moved since", 0.0, 0);
        let r2 = report().unwrap();
        assert!(r2["verdict"].as_str().unwrap().contains("no hit yet"), "{}", r2["verdict"]);
        assert_eq!(r2["misses_by_reason"].as_array().unwrap().len(), 2, "and why it missed is kept, not swallowed");

        note(true, "", 0.42, 61_000);
        let r3 = report().unwrap();
        assert_eq!(r3["hits"], 1);
        assert!(r3["verdict"].as_str().unwrap().contains("saving $0.42"), "{}", r3["verdict"]);
    }
}
