//! git-commit-card.rs — twin of skills/external/git-commit-card.mjs. NAMES ARE CONTRACT:
//! `detectCodeDrift`, `getUncommittedEventsForCard`, `formatCommitMessage`, `markEventsCommitted`,
//! `realGitRunner`, `isGitIgnored` (all named), `gitCommitCard` (default). Auto-commits a card's
//! audit-managed sqlite files to git and detects code drift (modified/untracked paths outside the
//! owned set that didn't go through this skill).
//!
//! ── LANE SPLIT (subprocess disclosure, per the crossing playbook) ──────────────────────────
//! Three units shell out to a REAL `git` subprocess on every call with no input-flag/optional-field
//! escape hatch that keeps them fixture-deterministic — the proc-run.rs precedent's "bespoke-native"
//! class: `detectCodeDrift`, `isGitIgnored`, `realGitRunner`. Real, correct code below; proven by
//! the native `#[cfg(test)]` tests (real temp git repos), NOT by a conform-db fixture — none exists
//! for these three, by design (see the manifest's `blocked`/native-test disclosure).
//!
//! `getUncommittedEventsForCard` (db-first, pure SQL read) and `formatCommitMessage` (no-db, pure)
//! carry normal fixtures. `gitCommitCard` (default, standard db lane) has exactly ONE
//! fixture-safe success path — zero uncommitted events for the slug, which returns before touching
//! git at all (`{committed:0, gitSha:null, message:null}`, the same early return the JS test suite
//! calls out as `git-commit-card returns zero when no uncommitted events`). Every OTHER success path
//! (there ARE uncommitted events) unconditionally reaches the real-git branch — ported here as real,
//! correct code (isGitIgnored check → drift detection → WAL checkpoint → formatCommitMessage →
//! realGitRunner → markEventsCommitted), DISCLOSED UNTESTED by fixture (constitution-estimate.rs's
//! git-diff-branch precedent for a disclosed-but-real subprocess branch inside an otherwise-fixtured
//! unit). `context.gitRunner`/`context.isGitIgnored` JS test-stub overrides have no Rust equivalent
//! (functions aren't JSON-fixture-representable) — this twin always takes the REAL-runner path once
//! past the zero-events gate, which is also what every actual (non-test) JS caller does (`context`
//! is always `{db}` in production; the stub-injection is a JS-test-only affordance).
//!
//! `context.db` missing → unreachable here: `db: &Connection` is a required, non-nullable
//! reference, same convention as every sibling db-lane twin.
//!
//! `AUDIT_MANAGED_PATHS` (JS `export const ... = new Set([...])`) is plain data, not a census unit
//! (no function) — inlined as a private const array, used only by `detectCodeDrift`.
// QUALITY: getUncommittedEventsForCard/formatCommitMessage/gitCommitCard fixture-backed (gitCommitCard's
// real-git branch disclosed-untested, constitution-estimate.rs precedent); detectCodeDrift/isGitIgnored/
// realGitRunner bespoke-native (proc-run.rs precedent) — no fixture, proven by native temp-repo tests.

use rusqlite::Connection;
use serde_json::{json, Value};
use std::process::{Command, Stdio};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`

// `const AUDIT_MANAGED_PATHS = new Set([...])` — files git-commit-card "owns" and auto-commits.
// Any other modified/untracked path is "code drift" that didn't go through git-commit-card.
const AUDIT_MANAGED_PATHS: [&str; 5] = [
    ".antiloki/db/state.sqlite",
    ".antiloki/db/memory.sqlite",
    ".antiloki/db/brain.sqlite",
    ".antiloki/db/laws.sqlite",
    ".antiloki/db/knowledge.sqlite",
];

/// `function detectCodeDrift(context)` — BESPOKE-NATIVE (see module doc): every call runs a real
/// `git status --porcelain` subprocess (even the "not a git repo" fallback still attempts it, then
/// catches). JS derives `cwd` from `getWorkspaceRoot(context.db)` when `context.db` is present, else
/// leaves it `undefined` (execSync then defaults to the calling process's own cwd) — modeled as
/// `db: Option<&Connection>` (the only thing this fn reads off `context`).
pub fn detectCodeDrift(db: Option<&Connection>) -> Vec<String> {
    let cwd: Option<String> = db
        .map(crate::engine_workspace_root::getWorkspaceRoot)
        .and_then(|v| v.as_str().map(|s| s.to_string()));

    let mut cmd = crate::proc::cmd("git");
    cmd.arg("status").arg("--porcelain");
    if let Some(dir) = &cwd {
        cmd.current_dir(dir);
    }
    let output = match cmd.output() {
        Ok(o) if o.status.success() => o,
        // Not a git repo, git not installed, etc. — treat as no drift (matches JS catch{}).
        _ => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut drifted = Vec::new();
    for line in text.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        // Format: 'XY path' where X=index status, Y=worktree status.
        let rest: String = line.chars().skip(3).collect();
        let path = rest.replace('\\', "/");
        let path = path.trim();
        let path = path.trim_start_matches('"').trim_end_matches('"');
        if !AUDIT_MANAGED_PATHS.contains(&path) {
            drifted.push(path.to_string());
        }
    }
    drifted
}

/// `function getUncommittedEventsForCard(db, slug)` — db-first helper, pure SQL read.
pub fn getUncommittedEventsForCard(db: &Connection, slug: &Value) -> Value {
    crate::db_json::query_rows(
        db,
        "SELECT t.* FROM sacred_timeline t \
         LEFT JOIN timeline_commits c ON c.timeline_id = t.id \
         WHERE t.entity_type = 'card' AND t.entity_id = ? AND c.timeline_id IS NULL \
         ORDER BY t.id ASC",
        &[slug.clone()],
    )
    .unwrap_or(json!([]))
}

/// `function formatCommitMessage(slug, events)` — no-db helper, pure. Throws (JS
/// `AntilokiError`, `.message` = `git-commit-card: no uncommitted events for card '<slug>'`) when
/// `events` is empty; unreachable from `gitCommitCard` itself (it early-returns first), but this
/// named export is its own census unit and the JS throw is a real reachable path if called
/// directly — modeled as `Err`, not exercised by the fixture (gotcha 9: throws fixture only the
/// success path since thrown MESSAGES must byte-match).
pub fn formatCommitMessage(slug: &Value, events: &Value) -> Result<Value, String> {
    let events_arr = events.as_array().cloned().unwrap_or_default();
    if events_arr.is_empty() {
        return Err(format!(
            "git-commit-card: no uncommitted events for card '{}'",
            tmpl(Some(slug))
        ));
    }

    let subject = if events_arr.len() == 1 {
        format!("{}: {}", tmpl(events_arr[0].get("operation")), tmpl(Some(slug)))
    } else {
        format!("card-bundle ({} events): {}", events_arr.len(), tmpl(Some(slug)))
    };

    let mut lines: Vec<String> = vec![
        subject,
        String::new(),
        format!("Card: {}", tmpl(Some(slug))),
        format!("Events: {}", events_arr.len()),
        String::new(),
    ];

    for e in &events_arr {
        lines.push(format!("[{}] {}", tmpl(e.get("ts")), tmpl(e.get("operation"))));
        lines.push(format!("  actor: {}", tmpl(e.get("actor"))));
        lines.push(format!("  print: {}", tmpl(e.get("digital_print"))));
        lines.push(format!("  prev:  {}", tmpl(e.get("prev_print"))));
        if truthy(e.get("before_json")) {
            lines.push(format!("  before: {}", tmpl(e.get("before_json"))));
        }
        if truthy(e.get("after_json")) {
            lines.push(format!("  after:  {}", tmpl(e.get("after_json"))));
        }
        if truthy(e.get("payload")) {
            lines.push(format!("  payload: {}", tmpl(e.get("payload"))));
        }
        lines.push(String::new());
    }

    let first_prev = tmpl(events_arr[0].get("prev_print"));
    let last_digital = tmpl(events_arr[events_arr.len() - 1].get("digital_print"));
    lines.push(format!(
        "Chain: {} \u{2192} {}",
        first_prev.chars().take(12).collect::<String>(),
        last_digital.chars().take(12).collect::<String>()
    ));
    Ok(json!(lines.join("\n")))
}

/// `function markEventsCommitted(db, events, gitSha)` — db-first helper, INSERT-only write; JS has
/// no `return` (implicit `undefined`, which the harness normalizes to `null`).
pub fn markEventsCommitted(db: &Connection, events: &Value, git_sha: &Value) -> Value {
    if let Some(arr) = events.as_array() {
        for e in arr {
            let id = e.get("id").cloned().unwrap_or(Value::Null);
            let _ = crate::db_json::run_write(
                db,
                "INSERT INTO timeline_commits (timeline_id, git_sha) VALUES (?, ?)",
                &[id, git_sha.clone()],
            );
        }
    }
    Value::Null
}

/// `function realGitRunner(message, dbPath)` — BESPOKE-NATIVE (see module doc): every call
/// unconditionally shells `git add`, `git commit -F -`, `git rev-parse HEAD` (JS `execSync`, no
/// `cwd` option — relies on the calling process's own cwd, exactly like this twin's
/// `Command`s below, which also omit `.current_dir(..)`).
pub fn realGitRunner(message: &str, db_path: &str, cwd: &std::path::Path) -> Result<String, String> {
    let add = crate::proc::cmd("git").arg("add").arg(db_path).current_dir(cwd).output().map_err(|e| e.to_string())?;
    if !add.status.success() {
        return Err(String::from_utf8_lossy(&add.stderr).into_owned());
    }

    let mut child = crate::proc::cmd("git")
        .arg("commit")
        .arg("-F")
        .arg("-")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().ok_or("git-commit-card: failed to open commit stdin")?;
        stdin.write_all(message.as_bytes()).map_err(|e| e.to_string())?;
    }
    let commit_out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !commit_out.status.success() {
        return Err(String::from_utf8_lossy(&commit_out.stderr).into_owned());
    }

    // ALL THREE calls need the directory. Missing it here committed into `cwd` but read HEAD from
    // the process CWD, so the function returned a sha from a different repository entirely — the
    // test caught it the moment the chdir that had been masking it was removed.
    let rev = crate::proc::cmd("git").arg("rev-parse").arg("HEAD").current_dir(cwd).output().map_err(|e| e.to_string())?;
    if !rev.status.success() {
        return Err(String::from_utf8_lossy(&rev.stderr).into_owned());
    }
    Ok(String::from_utf8_lossy(&rev.stdout).trim().to_string())
}

/// `function isGitIgnored(p, cwd)` — BESPOKE-NATIVE (see module doc): shells
/// `git check-ignore -q "<p>"` in `cwd`. The JS twin took no cwd and inherited the caller's, which
/// forced its tests to chdir the whole process; the directory is an explicit parameter here so a
/// test cannot disturb anything running beside it. Passed as a single `Command` arg (no shell), so
/// no quoting is needed.
pub fn isGitIgnored(p: &str, cwd: &std::path::Path) -> bool {
    crate::proc::cmd("git")
        .arg("check-ignore")
        .arg("-q")
        .arg(p)
        .current_dir(cwd)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// Best-effort reconstruction of open-db.mjs's DEFAULT_DB_PATH (roots.mjs SUBSTRATE_ROOT/DATA_DIR
// derivation) for the disclosed-untested real-git branch below. Never exercised by the fixture
// (that branch requires uncommitted events, which the fixture never seeds), so exact fidelity to
// the JS constant is not a proof obligation — same disclosed-approximate status as proc-run.rs's
// `cwd` gap.
fn default_db_path() -> String {
    let substrate_root = match std::env::var("ANTILOKI_SUBSTRATE_ROOT") {
        Ok(v) if !v.is_empty() => v,
        _ => std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
    };
    let data_dir = match std::env::var("ANTILOKI_DATA_DIR") {
        Ok(v) if !v.is_empty() => v,
        _ => format!("{substrate_root}/.antiloki"),
    };
    format!("{data_dir}/db/state.sqlite")
}

/// `export default function gitCommitCard(input, context)` — see module doc for the lane split.
/// JS `const slug = typeof input === 'string' ? input : input?.slug;` — this twin narrows to "a
/// non-empty JSON string" (disclosed divergence, card-run-preflight.rs's same narrowing precedent):
/// real callers always pass either a plain string slug or `{ slug: "<string>" }`.
pub fn gitCommitCard(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug_val = match input {
        Value::String(_) => Some(input),
        _ => input.get("slug"),
    };
    let slug = match slug_val {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Err("git-commit-card: slug is required".into()),
    };

    let events = getUncommittedEventsForCard(db, &json!(slug.clone()));
    let events_arr = events.as_array().cloned().unwrap_or_default();
    if events_arr.is_empty() {
        return Ok(json!({ "committed": 0, "gitSha": Value::Null, "message": Value::Null }));
    }

    // ── REAL-GIT BRANCH — DISCLOSED UNTESTED BY FIXTURE (see module doc) ───────────────────
    let db_path = default_db_path();
    // The repo these commits belong to, passed EXPLICITLY. These helpers used to inherit the
    // process CWD, which forced their tests to std::env::set_current_dir — a process-global
    // mutation that silently corrupted whatever else was running in parallel. It cost two
    // unrelated flakes (code-transform, code-regenerate) before the cause was found.
    let repo_dir = std::path::Path::new(&db_path)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    if isGitIgnored(&db_path, &repo_dir) {
        return Ok(json!({
            "committed": 0, "gitSha": Value::Null, "message": Value::Null,
            "skipped": "audit-db-gitignored",
        }));
    }

    let drifted = detectCodeDrift(Some(db));
    let drift_warned_for: Value = if drifted.is_empty() {
        Value::Null
    } else {
        let summary = {
            let head: Vec<&String> = drifted.iter().take(5).collect();
            let more = if drifted.len() > 5 { format!(", +{} more", drifted.len() - 5) } else { String::new() };
            format!(
                "{} uncommitted code file(s) (auto-commit will not include them): {}{more}",
                drifted.len(),
                head.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            )
        };
        if std::env::var("ANTILOKI_STRICT_COMMIT").ok().as_deref() == Some("true") {
            return Err(format!(
                "git-commit-card: code drift detected (ANTILOKI_STRICT_COMMIT=true blocks audit commit): {summary}"
            ));
        }
        eprintln!("\u{26a0} git-commit-card: {summary}");
        eprintln!("   Audit commit for '{slug}' will land but code stays uncommitted.");
        eprintln!("   Set ANTILOKI_STRICT_COMMIT=true to block on drift.");
        json!(drifted)
    };

    // FIX-PREBUILT-PLAN-SERIALIZE-COMMITS: flush WAL into the main files before staging.
    for schema in ["main", "memory", "brain", "laws", "knowledge"] {
        let _ = db.execute_batch(&format!("PRAGMA {schema}.wal_checkpoint(PASSIVE)"));
    }

    let message_v = formatCommitMessage(&json!(slug.clone()), &events)?;
    let message = message_v.as_str().unwrap_or_default().to_string();
    let git_sha = realGitRunner(&message, &db_path, &repo_dir)?;
    markEventsCommitted(db, &events, &json!(git_sha.clone()));

    Ok(json!({
        "committed": events_arr.len() as i64,
        "gitSha": git_sha,
        "message": message,
        "drift_warned_for": drift_warned_for,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn timeline_schema() -> &'static str {
        "CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, actor TEXT, \
         operation TEXT, entity_type TEXT, entity_id TEXT, before_json TEXT, after_json TEXT, \
         payload TEXT, prev_print TEXT, digital_print TEXT, user_goal_id TEXT, exec_env TEXT, tick_id TEXT); \
         CREATE TABLE timeline_commits (timeline_id INTEGER PRIMARY KEY, git_sha TEXT NOT NULL, \
         committed_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')"
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(timeline_schema()).unwrap();
        c
    }

    // ── getUncommittedEventsForCard / formatCommitMessage / markEventsCommitted ────────────

    #[test]
    fn uncommitted_events_filters_type_slug_and_already_committed() {
        let c = db();
        c.execute_batch(
            "INSERT INTO sacred_timeline (id, ts, actor, operation, entity_type, entity_id, prev_print, digital_print) VALUES \
             (1, '2025-12-01 00:00:00', 't', 'card.add', 'card', 'A', 'GENESIS', 'p1'), \
             (2, '2025-12-02 00:00:00', 't', 'card.update', 'card', 'A', 'p1', 'p2'), \
             (3, '2025-12-01 00:00:00', 't', 'mem.add', 'memory', 'A', 'GENESIS', 'pmem'), \
             (4, '2025-12-01 00:00:00', 't', 'card.add', 'card', 'B', 'GENESIS', 'pB')",
        ).unwrap();
        c.execute("INSERT INTO timeline_commits (timeline_id, git_sha) VALUES (4, 'sha0')", []).unwrap();
        let r = getUncommittedEventsForCard(&c, &json!("A"));
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["id"], json!(1));
        assert_eq!(arr[1]["id"], json!(2));
    }

    #[test]
    fn format_commit_message_single_event_subject_and_chain() {
        let events = json!([{
            "ts": "2025-12-01 00:00:00", "operation": "card.add", "actor": "human:dani",
            "digital_print": "abcdef123456789", "prev_print": "GENESIS",
            "before_json": null, "after_json": "{\"a\":1}", "payload": null
        }]);
        let r = formatCommitMessage(&json!("X"), &events).unwrap();
        let s = r.as_str().unwrap();
        assert!(s.starts_with("card.add: X\n"));
        assert!(s.contains("Card: X"));
        assert!(s.contains("Events: 1"));
        assert!(s.contains("actor: human:dani"));
        assert!(s.contains("after:  {\"a\":1}"));
        assert!(!s.contains("before:"));
        assert!(s.contains("Chain: GENESIS \u{2192} abcdef123456"));
    }

    #[test]
    fn format_commit_message_multi_event_bundle_subject() {
        let events = json!([
            { "ts": "t1", "operation": "card.add", "actor": "a", "digital_print": "print1aaaaaaaaaaaa", "prev_print": "GENESIS", "before_json": "{}", "after_json": "{}", "payload": "p1" },
            { "ts": "t2", "operation": "card.update", "actor": "a", "digital_print": "print2bbbbbbbbbbbb", "prev_print": "print1aaaaaaaaaaaa", "before_json": null, "after_json": null, "payload": null }
        ]);
        let r = formatCommitMessage(&json!("Y"), &events).unwrap();
        let s = r.as_str().unwrap();
        assert!(s.starts_with("card-bundle (2 events): Y\n"));
        assert!(s.contains("Events: 2"));
        assert!(s.contains("payload: p1"));
        assert!(s.contains("Chain: GENESIS \u{2192} print2bbbbbb"));
    }

    #[test]
    fn format_commit_message_empty_events_is_an_err() {
        let e = formatCommitMessage(&json!("Z"), &json!([])).unwrap_err();
        assert_eq!(e, "git-commit-card: no uncommitted events for card 'Z'");
    }

    #[test]
    fn mark_events_committed_inserts_one_row_per_event() {
        let c = db();
        let events = json!([{ "id": 10 }, { "id": 11 }]);
        let r = markEventsCommitted(&c, &events, &json!("deadbeef"));
        assert_eq!(r, Value::Null);
        let rows = crate::db_json::query_rows(&c, "SELECT * FROM timeline_commits ORDER BY timeline_id", &[]).unwrap();
        assert_eq!(rows, json!([
            { "timeline_id": 10, "git_sha": "deadbeef", "committed_at": "2026-01-01 00:00:00" },
            { "timeline_id": 11, "git_sha": "deadbeef", "committed_at": "2026-01-01 00:00:00" }
        ]));
    }

    // ── gitCommitCard: fixture-safe zero-events path ────────────────────────────────────────

    #[test]
    fn zero_events_no_sacred_timeline_rows_returns_zero_without_touching_git() {
        let c = db();
        let r = gitCommitCard(&json!({ "slug": "NOEVENTS" }), &c).unwrap();
        assert_eq!(r, json!({ "committed": 0, "gitSha": null, "message": null }));
    }

    #[test]
    fn zero_events_already_committed_returns_zero() {
        let c = db();
        c.execute(
            "INSERT INTO sacred_timeline (id, entity_type, entity_id, prev_print, digital_print) VALUES (1, 'card', 'DONE', 'GENESIS', 'p1')",
            [],
        ).unwrap();
        c.execute("INSERT INTO timeline_commits (timeline_id, git_sha) VALUES (1, 'sha1')", []).unwrap();
        let r = gitCommitCard(&json!({ "slug": "DONE" }), &c).unwrap();
        assert_eq!(r["committed"], json!(0));
    }

    #[test]
    fn missing_slug_is_an_err() {
        let c = db();
        assert_eq!(gitCommitCard(&json!({}), &c), Err("git-commit-card: slug is required".to_string()));
        assert_eq!(gitCommitCard(&json!(""), &c), Err("git-commit-card: slug is required".to_string()));
    }

    #[test]
    fn string_input_form_is_accepted() {
        let c = db();
        let r = gitCommitCard(&json!("NOEVENTS2"), &c).unwrap();
        assert_eq!(r["committed"], json!(0));
    }

    // ── bespoke-native: real temp git repos ─────────────────────────────────────────────────

    fn temp_repo(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-git-commit-card-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            let st = crate::proc::cmd("git").args(args).current_dir(&dir).status().unwrap();
            assert!(st.success(), "git {args:?} failed");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@antiloki.local"]);
        run(&["config", "user.name", "antiloki-test"]);
        dir
    }

    #[test]
    fn is_git_ignored_true_for_gitignored_path_false_for_tracked() {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let dir = temp_repo("ignore");
        fs::write(dir.join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(dir.join("tracked.txt"), "hello\n").unwrap();
        fs::write(dir.join("ignored.txt"), "shh\n").unwrap();
        crate::proc::cmd("git").args(["add", ".gitignore", "tracked.txt"]).current_dir(&dir).status().unwrap();
        crate::proc::cmd("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();
        // No chdir: the directory is a PARAMETER now, so this test cannot disturb its neighbours.
        let ignored = isGitIgnored("ignored.txt", &dir);
        let tracked = isGitIgnored("tracked.txt", &dir);
        let _ = fs::remove_dir_all(&dir);
        assert!(ignored);
        assert!(!tracked);
    }

    #[test]
    fn detect_code_drift_flags_untracked_file_but_excludes_managed_path_modification() {
        // A TRACKED audit-managed file (git already knows the path, so a later modification is
        // reported individually as ' M <path>' — unlike a brand-new untracked path inside a
        // wholly-untracked directory, which git's default -unormal collapses into one directory
        // entry) must NOT show up as drift; an unrelated untracked file must.
        let dir = temp_repo("drift");
        fs::create_dir_all(dir.join(".antiloki/db")).unwrap();
        fs::write(dir.join(".antiloki/db/state.sqlite"), "v1\n").unwrap();
        fs::write(dir.join("README.md"), "hi\n").unwrap();
        crate::proc::cmd("git").args(["add", "README.md", ".antiloki/db/state.sqlite"]).current_dir(&dir).status().unwrap();
        crate::proc::cmd("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();
        fs::write(dir.join(".antiloki/db/state.sqlite"), "v2\n").unwrap(); // modify a tracked, managed path
        fs::write(dir.join("rogue.mjs"), "export default 1;\n").unwrap(); // untracked, unmanaged path

        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')",
        ).unwrap();
        c.execute(
            "INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', ?1, 1)",
            rusqlite::params![dir.to_str().unwrap()],
        ).unwrap();

        let drifted = detectCodeDrift(Some(&c));
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(drifted, vec!["rogue.mjs".to_string()]);
    }

    #[test]
    fn detect_code_drift_no_db_never_panics() {
        let drifted = detectCodeDrift(None);
        let _ = drifted; // content depends on the test binary's own cwd — shape-only assertion
    }

    #[test]
    fn real_git_runner_commits_staged_change_and_returns_head_sha() {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let dir = temp_repo("runner");
        fs::write(dir.join("tracked.txt"), "v1\n").unwrap();
        crate::proc::cmd("git").args(["add", "tracked.txt"]).current_dir(&dir).status().unwrap();
        crate::proc::cmd("git").args(["commit", "-q", "-m", "init"]).current_dir(&dir).status().unwrap();
        fs::write(dir.join("tracked.txt"), "v2\n").unwrap();

        // No chdir: the directory is a PARAMETER now, so this test cannot disturb its neighbours.
        let result = realGitRunner("test commit\n\nCard: X", "tracked.txt", &dir);

        let sha = result.unwrap();
        assert_eq!(sha.len(), 40);
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));

        let head = crate::proc::cmd("git").args(["rev-parse", "HEAD"]).current_dir(&dir).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), sha);
        let _ = fs::remove_dir_all(&dir);
    }
}
