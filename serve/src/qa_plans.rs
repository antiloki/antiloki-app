//! SAVED QA PLANS - the QA's test, kept (item 8d, 2026-10-01: *"QA plans as regression tests"*).
//!
//! Every run that touches the UI or the server ends with a deterministic step plan the QA wrote (`qa_evidence.rs`: open the page, use the control, expect a result; `qa_api.rs`: the
//! same with requests). Until now the plan lived for one run: it passed, the run was delivered, and a month later the same screen could break with nothing to notice it.
//! A plan that PASSED, kept next to the code it tested, is a regression test the operator never had to write: `.antiloki/qa/<slug>.json`, replayable by the same deterministic
//! runner - no model in the loop, same browser, same checks.
//!
//! Three things happen here, and only the third needs a browser or a server:
//!   1. `record` - when a run's evidence has been decided, the plan that was run is written beside its pictures (`run-<id>/plan.json`) with how it ended. Nothing is promoted by itself.
//!   2. `save_from_run` - the operator (or the FE's minimal hook) keeps a recorded plan. A plan that did not pass is REFUSED: a failing plan is a bug report, not a regression
//!      test, and one with no deterministic check has nothing a replay could decide.
//!   3. `replay` - the saved plan is run again against the app as it is now (the same app resolution the QA gate uses: the one running preview, else the box's dev server, else a
//!      static page) and judged by `verdict`: no check may fail, and no step may fail that did not fail when the plan was saved.
//!
//! WHO MAY ASK FOR WHAT (audit 2026-10-01, F-14/F-15). The folder (`cwd`) a door works in comes from the request, and a saved plan is data a repository can carry, so: `cwd` must be the active
//! workspace or a worktree of it (`cwd_in_scope`), `.antiloki` and `.antiloki/qa` may not be symlinks, a replay's `app` must be a loopback address that is not this server's own port
//! (`app_ok`: a saved API plan sends GET/POST/DELETE with bodies and returns the answer text in `checks[].had` - to any host that was a request forger with a read-back), a plan is never
//! overwritten without `overwrite: true`, and the verdict of a replay is always recomputed from that replay.
use crate::qa_api::{self, ApiStep};
use crate::qa_evidence::{self as qe, Evidence, Step};
use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const FORMAT: u32 = 1;

/// where a workspace keeps its saved plans: `<cwd>/.antiloki/qa/<slug>.json` (the evidence of single runs sits in `evidence/` beside them)
pub fn plans_dir(cwd: &str) -> PathBuf { Path::new(cwd).join(".antiloki").join("qa") }
pub fn plan_path(cwd: &str, slug: &str) -> PathBuf { plans_dir(cwd).join(format!("{slug}.json")) }
/// a run's own record of the plan it ran, beside its pictures
pub fn record_path(cwd: &str, run_id: i64) -> PathBuf { qe::evidence_dir(cwd, run_id).join("plan.json") }

/// a slug is a plain file stem: lower-case letters, digits and dashes, never a path. Pure - tested.
pub fn safe_slug(s: &str) -> bool { !s.is_empty() && s.len() <= 64 && s.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit()) && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') }

/// A Latin letter folded to ASCII (`á` -> `a`, `ß` -> `ss`); `None` for anything this table does not know. Audit 2026-10-01 F-14: `slug_of` DROPPED every non-ASCII letter, so "página" became "pgina".
fn fold_ascii(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a", 'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c", 'ď' | 'đ' | 'ð' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e", 'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g", 'ĥ' | 'ħ' => "h",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "i", 'ĵ' => "j", 'ķ' => "k", 'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' | 'ŉ' => "n", 'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o", 'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' => "s", 'ţ' | 'ť' | 'ŧ' => "t", 'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'ŵ' => "w", 'ý' | 'ÿ' | 'ŷ' => "y", 'ź' | 'ż' | 'ž' => "z", 'ß' => "ss", 'æ' => "ae", 'œ' => "oe", 'þ' => "th",
        _ => return None,
    })
}

/// The slug's words from the ask, or "" when it has no Latin letter or digit at all (a Cyrillic or Chinese ask): lower-cased, accents folded, at most 48 characters, cut at a word. Pure - tested.
pub fn slug_words(ask: &str) -> String {
    let mut words: Vec<String> = vec![];
    let mut cur = String::new();
    for ch in ask.chars().flat_map(|c| c.to_lowercase()) {
        if ch.is_ascii_alphanumeric() { cur.push(ch) }
        else if let Some(f) = fold_ascii(ch) { cur.push_str(f) }
        else if ch.is_alphanumeric() { /* a letter of a script this table cannot spell: it is not a separator, it is simply not in the name */ }
        else if !cur.is_empty() { words.push(std::mem::take(&mut cur)) }
    }
    if !cur.is_empty() { words.push(cur) }
    let mut out = String::new();
    for w in words {
        if out.len() + w.len() + usize::from(!out.is_empty()) > 48 { break }
        if !out.is_empty() { out.push('-'); }
        out.push_str(&w);
    }
    out
}

/// A slug from the words of the ask: `"Add a sort dropdown to the shop!"` -> `add-a-sort-dropdown-to-the-shop`; `qa-plan` when the ask has no Latin letter. Pure - tested.
pub fn slug_of(ask: &str) -> String { let s = slug_words(ask); if s.is_empty() { "qa-plan".into() } else { s } }

/// The slug a plan from run `run_id` is kept under when the operator named none: the words of the ask, or `qa-plan-<run id>` when it has none (every all-non-Latin ask used to collapse into the
/// one name `qa-plan`, and each save replaced the last). Pure - tested.
pub fn default_slug(ask: &str, run_id: i64) -> String { let s = slug_words(ask); if s.is_empty() { format!("qa-plan-{run_id}") } else { s } }

/// What a run leaves behind about its plan. `checks` is how many deterministic checks it asked, `checks_failed` how many failed, `failed_steps` how many steps could not be done.
pub fn record(cwd: &str, run_id: i64, kind: &str, app: &str, ask: &str, steps: Value, ev: &Evidence) -> Result<PathBuf, String> {
    let rec = json!({ "format": FORMAT, "kind": kind, "run_id": run_id, "ask": ask.chars().take(300).collect::<String>(), "app": app, "recorded_at": crate::cli::now_ms(),
        "steps": steps, "checks": ev.checks.len(), "checks_failed": ev.checks_failed(), "failed_steps": ev.failed_steps, "passed": ev.checks_failed() == 0 });
    checked_plans_dir(cwd)?;
    let p = record_path(cwd, run_id);
    std::fs::create_dir_all(p.parent().unwrap_or(Path::new("."))).map_err(|e| format!("evidence folder: {e}"))?;
    std::fs::write(&p, serde_json::to_string_pretty(&rec).unwrap_or_default()).map_err(|e| format!("could not write the plan record: {e}"))?;
    // what THIS process wrote, remembered: the record sits in the run's box, where the agent that did the work can edit it before anyone keeps it (audit 2026-10-01, F-14/F-15)
    attested().lock().unwrap_or_else(|p| p.into_inner()).insert(p.clone(), digest_of(&rec));
    Ok(p)
}

/// The fields of a record that decide whether it can be kept: a digest over them, so an edit is seen. `passed` is not one of them - it is derived (`checks_failed == 0`), never read.
fn digest_of(rec: &Value) -> String {
    use sha2::{Digest, Sha256};
    let k = |n: &str| rec.get(n).cloned().unwrap_or(Value::Null);
    hex::encode(Sha256::digest(json!([k("kind"), k("run_id"), k("steps"), k("checks"), k("checks_failed"), k("failed_steps"), k("app")]).to_string().as_bytes()))
}
fn attested() -> &'static std::sync::Mutex<HashMap<PathBuf, String>> {
    static A: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, String>>> = std::sync::OnceLock::new();
    A.get_or_init(Default::default)
}

/// A recorded plan -> the saved plan, or why it is not worth keeping. Pure - tested.
pub fn promote(rec: &Value, slug: &str, now_ms: i64) -> Result<Value, String> {
    if !safe_slug(slug) { return Err(format!("\"{slug}\" is not a plan name: lower-case letters, digits and dashes (64 at most)")) }
    let n = |k: &str| rec.get(k).and_then(Value::as_u64).unwrap_or(0);
    // DERIVED, NOT READ (audit 2026-10-01, F-14/F-15: `passed` was taken from a file the agent that did the work can write): a plan passed when no check failed
    if n("checks_failed") > 0 || rec.get("passed").and_then(Value::as_bool) == Some(false) { return Err("this plan did not pass - a failing plan is a bug report, not a regression test, so it is not kept".into()) }
    if n("checks") == 0 { return Err("this plan asked no deterministic check (it only took pictures), so a replay would have nothing to decide - it is not kept".into()) }
    let steps = rec.get("steps").cloned().unwrap_or(Value::Null);
    let kind = rec.get("kind").and_then(Value::as_str).unwrap_or("");
    // the steps must still be a plan the runner accepts: the file is data, and a hand-edited record must not smuggle in anything the QA's own plan could not say
    match kind { "ui" => { qe::parse_plan_max(&steps, qe::MAX_RECORDED_STEPS)?; } "api" => { qa_api::parse_api_plan(&steps)?; } other => return Err(format!("unknown plan kind \"{other}\"")) };
    Ok(json!({ "format": FORMAT, "slug": slug, "kind": kind, "ask": rec.get("ask").cloned().unwrap_or(json!("")), "run_id": rec.get("run_id").cloned().unwrap_or(json!(0)), "saved_at": now_ms,
        "app": rec.get("app").cloned().unwrap_or(json!("")), "steps": steps, "checks": n("checks"), "failed_steps": n("failed_steps") }))
}

/// WHERE A RUN'S PLANS ARE KEPT (review 2026-10-01). The plan was saved under the run's own `cwd`, which in production is a box - a linked worktree under `~/.antiloki/agents` - or a chat
/// folder. `.antiloki` is deliberately left out of what a box stages (`boxes::OURS`), so a regression test "kept next to the code" went away with the box, and the list/replay/remove doors
/// (which default to the active workspace's root) never saw it. So: the active workspace's root when the run worked in it or under it; else the main checkout a linked worktree belongs to;
/// else the folder itself. `main_checkout` is the git half, passed in so the rule is pure. Pure - tested.
pub fn plan_home_with(cwd: &str, root: &str, main_checkout: impl Fn(&str) -> Option<String>) -> String {
    if crate::agents_http::in_workspace(cwd, root) { return root.to_string() }
    main_checkout(cwd).unwrap_or_else(|| cwd.to_string())
}
pub fn plan_home(cwd: &str, root: &str) -> String { plan_home_with(cwd, root, main_checkout_of) }
/// The main checkout of the repository `cwd` is a LINKED worktree of (`git-common-dir`'s parent), `None` for a plain checkout or a folder that is not a work tree.
pub fn main_checkout_of(cwd: &str) -> Option<String> {
    let git = |a: &[&str]| -> Option<String> {
        let o = crate::proc::cmd("git").args(a).current_dir(cwd).output().ok().filter(|o| o.status.success())?;
        Some(String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|t| !t.is_empty())
    };
    let (dir, common) = (git(&["rev-parse", "--path-format=absolute", "--git-dir"])?, git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?);
    if dir == common { return None }
    Path::new(&common).parent().map(|p| p.to_string_lossy().to_string()).filter(|p| !p.is_empty())
}

/// The plans folder of `cwd`, only when neither `.antiloki` nor `.antiloki/qa` is a symlink (audit 2026-10-01, F-14/F-15: a repository can ship `.antiloki/qa` as a link to anywhere, and list / save /
/// remove / replay would have read, written and DELETED through it). A folder that does not exist yet is fine. Pure over the filesystem - tested.
pub fn checked_plans_dir(cwd: &str) -> Result<PathBuf, String> {
    let dot = Path::new(cwd).join(".antiloki");
    for p in [dot.clone(), dot.join("qa")] {
        if std::fs::symlink_metadata(&p).map_or(false, |m| m.file_type().is_symlink()) { return Err(format!("{} is a symlink - the QA plans are not read or written through it", p.display())) }
    }
    Ok(plans_dir(cwd))
}

/// keep a run's recorded plan as `<home>/.antiloki/qa/<slug>.json` (`home` from `plan_home`; the record is read from the run's own `cwd`); the slug defaults to the words of the ask (or
/// `qa-plan-<run id>`). A plan that is already kept under that name is NOT replaced unless `overwrite` (audit 2026-10-01, F-14: the save replaced it silently - and every ask without Latin letters
/// shared one name). Returns the saved plan; an existing plan is the error "... already exists".
pub fn save_from_run(cwd: &str, home: &str, run_id: i64, slug: Option<&str>, overwrite: bool) -> Result<Value, String> {
    let rp = record_path(cwd, run_id);
    let rec: Value = std::fs::read_to_string(&rp).ok().and_then(|t| serde_json::from_str(&t).ok())
        .ok_or_else(|| format!("run {run_id} left no QA plan to keep (it touched neither the UI nor the server, or the QA gave no plan)"))?;
    // a record this process wrote must be the record it wrote
    if let Some(d) = attested().lock().unwrap_or_else(|p| p.into_inner()).get(&rp) {
        if *d != digest_of(&rec) { return Err("the recorded plan was changed after the run wrote it - it is not kept".into()) }
    }
    let slug = slug.map(str::to_string).unwrap_or_else(|| default_slug(rec.get("ask").and_then(Value::as_str).unwrap_or(""), run_id));
    let saved = promote(&rec, &slug, crate::cli::now_ms())?;
    let dir = checked_plans_dir(home)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not make the QA folder: {e}"))?;
    let dest = plan_path(home, &slug);
    let text = serde_json::to_string_pretty(&saved).unwrap_or_default();
    if overwrite {
        let tmp = dir.join(format!(".{slug}.json.tmp-{}", std::process::id()));
        std::fs::write(&tmp, &text).map_err(|e| format!("could not write the plan: {e}"))?;
        std::fs::rename(&tmp, &dest).map_err(|e| { let _ = std::fs::remove_file(&tmp); format!("could not write the plan: {e}") })?;
    } else {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&dest).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists { format!("a plan named \"{slug}\" already exists in this workspace - send overwrite: true to replace it, or choose another name") } else { format!("could not write the plan: {e}") }
        })?;
        f.write_all(text.as_bytes()).map_err(|e| format!("could not write the plan: {e}"))?;
    }
    Ok(saved)
}

/// A saved plan is a few KB; a file this big in the QA folder is not one. (Audit 2026-10-01: `list` read every file in the folder whole.)
const MAX_PLAN_BYTES: u64 = 2 * 1024 * 1024;
/// How many plans `list` answers with.
pub const MAX_LISTED: usize = 500;

pub fn load(cwd: &str, slug: &str) -> Result<Value, String> {
    if !safe_slug(slug) { return Err(format!("\"{slug}\" is not a plan name")) }
    checked_plans_dir(cwd)?;
    let p = plan_path(cwd, slug);
    if std::fs::metadata(&p).map_or(false, |m| m.len() > MAX_PLAN_BYTES) { return Err(format!("the saved plan \"{slug}\" is too big to be a plan")) }
    let t = std::fs::read_to_string(&p).map_err(|_| format!("no saved QA plan \"{slug}\" in this workspace"))?;
    serde_json::from_str(&t).map_err(|e| format!("the saved plan \"{slug}\" is not valid JSON: {e}"))
}

/// the saved plans of a workspace, newest first, without their steps - at most `MAX_LISTED`, and a file over `MAX_PLAN_BYTES` is not read
pub fn list(cwd: &str) -> Vec<Value> {
    let Ok(dir) = checked_plans_dir(cwd) else { return vec![] };
    let mut v: Vec<Value> = std::fs::read_dir(dir).map(|rd| rd.flatten().take(MAX_LISTED * 4).filter_map(|e| {
        let name = e.file_name().to_str()?.to_string();
        let slug = name.strip_suffix(".json")?.to_string();
        if !safe_slug(&slug) { return None }
        let md = e.metadata().ok()?;
        if !md.is_file() || md.len() > MAX_PLAN_BYTES { return None }
        let p: Value = serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok()?;
        Some(json!({ "slug": slug, "kind": p["kind"], "ask": p["ask"], "run_id": p["run_id"], "saved_at": p["saved_at"], "checks": p["checks"], "steps": p["steps"].as_array().map_or(0, |a| a.len()) }))
    }).collect()).unwrap_or_default();
    v.sort_by_key(|p| std::cmp::Reverse(p["saved_at"].as_i64().unwrap_or(0)));
    v.truncate(MAX_LISTED);
    v
}

/// Delete a saved plan AND the pictures of its replays (`replays/<slug>/`, which used to be left behind for good).
pub fn remove(cwd: &str, slug: &str) -> Result<(), String> {
    if !safe_slug(slug) { return Err(format!("\"{slug}\" is not a plan name")) }
    let dir = checked_plans_dir(cwd)?;
    std::fs::remove_file(plan_path(cwd, slug)).map_err(|_| format!("no saved QA plan \"{slug}\" in this workspace"))?;
    let replays = dir.join("replays").join(slug);
    if std::fs::symlink_metadata(&replays).map_or(false, |m| m.is_dir()) { let _ = std::fs::remove_dir_all(&replays); }
    Ok(())
}

/// THE VERDICT OF A REPLAY - pure, so the rule is one function with a name. A replay fails when a check fails, or when a step fails that did not fail when the plan was saved (a button
/// that is gone, a page that does not open). A plan with no checks left decides nothing and is not a pass. Returns (passed, the reasons it did not).
pub fn verdict(checks: &[(String, bool, String)], failed_steps: usize, tolerated_failed_steps: usize) -> (bool, Vec<String>) {
    let mut why: Vec<String> = checks.iter().filter(|c| !c.1).map(|c| format!("check failed: {} - the page had: {}", c.0, c.2)).collect();
    if failed_steps > tolerated_failed_steps { why.push(format!("{} step(s) could not be done (it was {tolerated_failed_steps} when the plan was saved)", failed_steps)); }
    if checks.is_empty() { why.push("the plan ran no check".into()); }
    (why.is_empty(), why)
}

/// MAY A REPLAY BE POINTED AT THIS ADDRESS? (audit 2026-10-01, F-14/F-15: `app` was used as given - a UI plan could be retargeted anywhere, and an API plan sent its requests, bodies included, to
/// any host and returned the response text in `checks[].had`). The address of a dev server on THIS machine: http(s), an exact loopback name, a port that is not this server's own. Pure - tested.
pub fn app_ok(app: &str) -> Result<String, String> {
    let a = app.trim();
    let Some((_, host, port, _)) = qe::parse_http_url(a) else { return Err(format!("{a:?} is not an address a replay may open (http://localhost:<port>)")) };
    if !qe::is_loopback_host(&host) { return Err(format!("{a:?} is not on this machine - a replay only goes to localhost, 127.0.0.1 or [::1]")) }
    if qe::serve_ports().contains(&port) { return Err(format!("{a:?} is this server's own port - a saved plan is not run against the server that runs it")) }
    Ok(a.to_string())
}

/// May a door work in `cwd`? Only the active workspace (`root`) or a folder under it, a linked worktree of it (a box), or a folder a run of this server's own worked in (`known`: a chat's
/// folder keeps the plans of its runs where the run was) - the folder came from the request, and a replay boots that folder's dev script. Both sides are canonicalised first, so `..` and a
/// symlink do not pass. Returns the canonical folder. `main_checkout` is the git half, passed in so the rule is testable. Pure over the filesystem.
pub fn cwd_in_scope(cwd: &str, root: &str, known: &[String], main_checkout: impl Fn(&str) -> Option<String>) -> Result<String, String> {
    let canon = |p: &str| std::fs::canonicalize(p).map(|c| c.to_string_lossy().to_string());
    let norm = |p: &str| p.replace('\\', "/").trim_end_matches('/').to_string();
    let c = canon(cwd).map_err(|_| format!("{cwd:?} is not a folder on this machine"))?;
    if known.iter().any(|k| canon(k).map_or(false, |k| norm(&k) == norm(&c))) { return Ok(c) }
    if root.trim().is_empty() { return Err("no workspace is open - there is no folder the QA plans may be read from or run in".into()) }
    let r = canon(root).map_err(|_| "the active workspace's folder is gone".to_string())?;
    if crate::agents_http::in_workspace(&c, &r) { return Ok(c) }
    if main_checkout(&c).and_then(|m| canon(&m).ok()).map_or(false, |m| norm(&m) == norm(&r)) { return Ok(c) }
    Err(format!("{cwd:?} is not the active workspace, a worktree of it or a folder one of its runs worked in - the QA plans of another folder are not read, run or removed from here"))
}

/// Which app a replay opens, the way the QA gate chooses it: the one running preview of this folder, else the folder's own dev server (booted for the replay, ended when it is dropped),
/// else - for a UI plan only - a static page in it. An explicit URL wins, and must pass `app_ok`.
pub(crate) fn resolve_app(cwd: &str, explicit: Option<&str>, kind: &str) -> Result<(String, Option<qe::DevServer>, &'static str), String> {
    if let Some(u) = explicit.filter(|u| !u.trim().is_empty()) { return Ok((app_ok(u)?, None, "the address given")) }
    let live = crate::preview::live_urls_for(cwd);
    if live.len() == 1 { return Ok((live[0].clone(), None, "the preview running on this folder")) }
    match qe::DevServer::start(cwd, Duration::from_secs(90)) {
        Ok(d) => { let u = d.url.clone(); Ok((u, Some(d), "the folder's own dev server, booted for the replay")) }
        Err(boot) => match if kind == "ui" { qe::static_app(Path::new(cwd)) } else { None } {
            Some(s) => Ok((s, None, "a static page in the folder")),
            None => Err(format!("nothing to replay against: no single running preview, no bootable dev server ({boot}){}", if kind == "ui" { " and no static page in the folder" } else { "" })),
        },
    }
}

/// The steps of a saved UI plan, read back for a replay. The saved plan is the plan that RAN (the QA's steps plus the coverage plan), so it is held to `MAX_RECORDED_STEPS`. Pure - tested.
pub fn ui_steps_of(saved: &Value) -> Result<Vec<Step>, String> { qe::parse_plan_max(&saved["steps"], qe::MAX_RECORDED_STEPS) }

/// Run a saved plan again. Blocking (a browser or a server is involved): call it from `spawn_blocking`. The pictures of a replay go to `.antiloki/qa/replays/<slug>/`.
pub fn replay(cwd: &str, slug: &str, app: Option<&str>) -> Result<Value, String> {
    let saved = load(cwd, slug)?;
    let kind = saved["kind"].as_str().unwrap_or("");
    let tolerated = saved["failed_steps"].as_u64().unwrap_or(0) as usize;
    let out = checked_plans_dir(cwd)?.join("replays").join(slug);
    // refused BEFORE anything is booted: an `app` that is not a loopback address is not a reason to start the folder's dev script first
    if let Some(a) = app.filter(|a| !a.trim().is_empty()) { app_ok(a)?; }
    let t0 = std::time::Instant::now();
    let (ev, app_used, source) = match kind {
        "ui" => {
            let steps = ui_steps_of(&saved)?;
            let (url, _guard, source) = resolve_app(cwd, app, "ui")?;
            // the plan was written against the address of its day; the same steps now point at today's
            let steps = match saved["app"].as_str().filter(|a| !a.is_empty() && *a != url) { Some(was) => qe::retarget(&steps, was, &url), None => steps };
            (qe::run_plan_in(&url, Some(Path::new(cwd)), &steps, &out, None, false)?, url, source)
        }
        "api" => {
            let steps: Vec<ApiStep> = qa_api::parse_api_plan(&saved["steps"])?;
            // the same resolution as a UI replay (audit 2026-10-01: the API replay always booted a dev server, whatever it was given or whatever was already running)
            let (base, _guard, source) = resolve_app(cwd, app, "api")?;
            if !(base.starts_with("http://") || base.starts_with("https://")) { return Err(format!("an API plan needs a server to call, not {base}")) }
            (qa_api::run_api_plan(&base, &steps)?, base, source)
        }
        other => return Err(format!("the saved plan has an unknown kind \"{other}\"")),
    };
    let (passed, why) = verdict(&ev.checks, ev.failed_steps, tolerated);
    Ok(json!({ "ok": true, "slug": slug, "kind": kind, "passed": passed, "why": why, "app": app_used, "source": source, "ms": t0.elapsed().as_millis() as u64,
        "checks_passed": ev.checks.len() - ev.checks_failed(), "checks_failed": ev.checks_failed(), "failed_steps": ev.failed_steps, "console_errors": ev.console_errors,
        "checks": ev.checks.iter().map(|c| json!({ "asked": c.0, "passed": c.1, "had": c.2 })).collect::<Vec<_>>(), "log": ev.log }))
}

// ---- the routes -------------------------------------------------------------------------------------------------------------------------------

fn bad(m: &str) -> Response { (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": { "message": m } }))).into_response() }
fn body_json(b: &[u8]) -> Value { serde_json::from_slice(b).unwrap_or(Value::Null) }

/// The folder a door works in: the one the request names (or the active workspace), and only when it is the active workspace or a worktree of it (`cwd_in_scope`).
async fn scoped_cwd(state: &std::sync::Arc<crate::dispatch::AppState>, goals: &std::sync::Arc<crate::goals::Goals>, asked: Option<&str>) -> Result<String, String> {
    let root = crate::agents_http::active_root(state).await;
    let cwd = match asked.map(str::trim).filter(|c| !c.is_empty()) { Some(c) => c.to_string(), None => root.clone() };
    if cwd.is_empty() { return Err("no workspace is open and no cwd was given".into()) }
    let runs: Vec<String> = goals.list(None).into_iter().map(|r| r.cwd).collect();
    tokio::task::spawn_blocking(move || {
        // the folders this server's own runs worked in, and the checkout a box outside the workspace belongs to
        let mut known: Vec<String> = runs.clone();
        known.extend(runs.iter().filter(|r| !crate::agents_http::in_workspace(r, &root)).filter_map(|r| main_checkout_of(r)));
        cwd_in_scope(&cwd, &root, &known, main_checkout_of)
    }).await.map_err(|e| e.to_string())?
}
fn refused(m: &str) -> Response { (StatusCode::FORBIDDEN, Json(json!({ "ok": false, "error": { "message": m } }))).into_response() }

/// `GET /api/v1/cockpit/qa/plans?cwd=<folder>` - the plans saved in a folder (default: the active workspace; any other folder than that workspace or one of its worktrees is refused, 403)
pub async fn plans_get(state: std::sync::Arc<crate::dispatch::AppState>, goals: std::sync::Arc<crate::goals::Goals>, q: HashMap<String, String>) -> Response {
    let cwd = match scoped_cwd(&state, &goals, q.get("cwd").map(String::as_str)).await { Ok(c) => c, Err(e) => return if e.starts_with("no workspace") { bad(&e) } else { refused(&e) } };
    Json(json!({ "ok": true, "cwd": cwd, "plans": list(&cwd) })).into_response()
}

/// `POST /api/v1/cockpit/qa/plans {run_id, slug?, overwrite?}` keeps the plan a run ran (answers `cwd`, the folder it is kept in; 409 when that name is already kept and `overwrite` is not true) · `{slug, cwd?, remove: true}` deletes a saved one (and its replays' pictures)
pub async fn plans_post(state: std::sync::Arc<crate::dispatch::AppState>, goals: std::sync::Arc<crate::goals::Goals>, body: axum::body::Bytes) -> Response {
    let b = body_json(&body);
    let slug = b.get("slug").and_then(Value::as_str).map(str::to_string);
    if b.get("remove").and_then(Value::as_bool) == Some(true) {
        let cwd = match scoped_cwd(&state, &goals, b.get("cwd").and_then(Value::as_str)).await { Ok(c) => c, Err(e) => return if e.starts_with("no workspace") { bad(&e) } else { refused(&e) } };
        return match remove(&cwd, slug.as_deref().unwrap_or("")) { Ok(()) => Json(json!({ "ok": true })).into_response(), Err(e) => (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": { "message": e } }))).into_response() };
    }
    let Some(id) = b.get("run_id").and_then(Value::as_i64).filter(|i| *i > 0) else { return bad("saving a plan needs the run_id of the run that ran it") };
    let Some(run) = goals.get(id) else { return (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": { "message": "no such run" } }))).into_response() };
    // the folder the plan is KEPT in rides back, so a client lists, replays and removes it where it is
    let home = { let root = crate::agents_http::active_root(&state).await; let cwd = run.cwd.clone(); tokio::task::spawn_blocking(move || plan_home(&cwd, &root)).await.unwrap_or_else(|_| run.cwd.clone()) };
    let overwrite = b.get("overwrite").and_then(Value::as_bool) == Some(true);
    match save_from_run(&run.cwd, &home, id, slug.as_deref(), overwrite) {
        Ok(saved) => (StatusCode::CREATED, Json(json!({ "ok": true, "plan": saved, "cwd": home }))).into_response(),
        Err(e) if e.contains("already exists") => (StatusCode::CONFLICT, Json(json!({ "ok": false, "error": { "message": e }, "exists": true }))).into_response(),
        Err(e) => bad(&e),
    }
}

/// `POST /api/v1/cockpit/qa/replay {slug, cwd?, app?}` - run a saved plan again, now, and say whether it still passes. `cwd` must be the active workspace or a worktree of it (403 otherwise);
/// `app`, when given, must be a loopback address (`http://localhost:<port>`) that is not this server's own port (400 otherwise).
pub async fn replay_post(state: std::sync::Arc<crate::dispatch::AppState>, goals: std::sync::Arc<crate::goals::Goals>, body: axum::body::Bytes) -> Response {
    let b = body_json(&body);
    let Some(slug) = b.get("slug").and_then(Value::as_str).map(str::to_string) else { return bad("a replay needs the slug of a saved plan") };
    let cwd = match scoped_cwd(&state, &goals, b.get("cwd").and_then(Value::as_str)).await { Ok(c) => c, Err(e) => return if e.starts_with("no workspace") { bad(&e) } else { refused(&e) } };
    let app = b.get("app").and_then(Value::as_str).map(str::to_string);
    match tokio::task::spawn_blocking(move || replay(&cwd, &slug, app.as_deref())).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => bad(&e),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "ok": false, "error": { "message": format!("the replay thread failed: {e}") } }))).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("antiloki-qa-plans-{tag}-{}-{}", std::process::id(), crate::cli::now_ms()));
        std::fs::create_dir_all(&d).unwrap(); d
    }
    fn ev(checks: &[(&str, bool)], failed_steps: usize) -> Evidence {
        Evidence { checks: checks.iter().map(|(w, p)| (w.to_string(), *p, "the page had it".to_string())).collect(), failed_steps, ..Default::default() }
    }
    fn ui_steps() -> Value { json!([{ "do": "goto", "url": "http://localhost:5173/" }, { "do": "expect", "text": "Add to cart" }, { "do": "shot", "name": "page" }]) }

    #[test]
    fn a_slug_comes_from_the_words_of_the_ask_and_is_always_a_plain_file_name() {
        assert_eq!(slug_of("Add a sort dropdown to the shop!"), "add-a-sort-dropdown-to-the-shop");
        assert_eq!(slug_of("  ../../etc/passwd  "), "etc-passwd");
        assert_eq!(slug_of("???"), "qa-plan");
        assert_eq!(slug_of("Crème brûlée recipe page"), "creme-brulee-recipe-page", "accents are folded, not dropped (they used to leave \"crme-brle\")");
        let long = slug_of("make the checkout page show the order summary with every line item and the delivery estimate for the chosen country");
        assert!(long.len() <= 48 && !long.ends_with('-') && safe_slug(&long), "{long}");
        for ask in ["a/b", "x y", "UPPER", "", "../x"] { assert!(safe_slug(&slug_of(ask)), "{ask}"); }
        for bad in ["", "../x", "a/b", "A", "-a", "a.json", &"a".repeat(65)] { assert!(!safe_slug(bad), "{bad}"); }
    }

    #[test]
    fn only_a_plan_that_passed_and_decided_something_is_promoted() {
        let rec = |passed: bool, checks: u64, kind: &str, steps: Value| json!({ "format": 1, "kind": kind, "run_id": 7, "ask": "a sort dropdown", "app": "http://localhost:5173/", "steps": steps, "checks": checks, "checks_failed": u64::from(!passed), "failed_steps": 1, "passed": passed });
        let ok = promote(&rec(true, 2, "ui", ui_steps()), "sort-dropdown", 42).unwrap();
        assert_eq!((ok["slug"].as_str(), ok["kind"].as_str(), ok["checks"].as_u64(), ok["failed_steps"].as_u64(), ok["saved_at"].as_i64()), (Some("sort-dropdown"), Some("ui"), Some(2), Some(1), Some(42)), "{ok}");
        assert!(promote(&rec(false, 2, "ui", ui_steps()), "x", 0).unwrap_err().contains("did not pass"));
        assert!(promote(&rec(true, 0, "ui", ui_steps()), "x", 0).unwrap_err().contains("no deterministic check"));
        assert!(promote(&rec(true, 2, "ui", ui_steps()), "../evil", 0).unwrap_err().contains("not a plan name"));
        // the steps of a record are data again: a hand-edited one that says something the runner refuses is refused here, not at replay time
        assert!(promote(&rec(true, 2, "ui", json!([{ "do": "goto", "url": "https://example.com/" }])), "x", 0).unwrap_err().contains("only the box's own app"));
        assert!(promote(&rec(true, 2, "ui", json!([])), "x", 0).is_err());
        assert!(promote(&rec(true, 2, "api", json!([{ "do": "request", "path": "https://example.com/" }])), "x", 0).is_err());
        assert!(promote(&rec(true, 2, "sql", ui_steps()), "x", 0).unwrap_err().contains("unknown plan kind"));
        assert!(promote(&json!({}), "x", 0).is_err());
    }

    #[test]
    fn a_replay_fails_on_a_failed_check_or_a_step_that_used_to_work_and_passes_otherwise() {
        let c = |p: bool| ("the page does say \"Add to cart\"".to_string(), p, "a page about shoes".to_string());
        assert_eq!(verdict(&[c(true), c(true)], 0, 0), (true, vec![]));
        let (ok, why) = verdict(&[c(true), c(false)], 0, 0);
        assert!(!ok && why.len() == 1 && why[0].contains("check failed") && why[0].contains("a page about shoes"), "{why:?}");
        // a click that failed when the plan was saved (a control the page never had) is tolerated; one MORE failure is the regression
        assert!(verdict(&[c(true)], 1, 1).0);
        let (ok, why) = verdict(&[c(true)], 2, 1);
        assert!(!ok && why[0].contains("2 step(s) could not be done (it was 1"), "{why:?}");
        // a plan that ran no check decided nothing - it is not a pass
        let (ok, why) = verdict(&[], 0, 0);
        assert!(!ok && why == vec!["the plan ran no check".to_string()], "{why:?}");
    }

    #[test]
    fn a_run_records_its_plan_and_the_operator_keeps_it_next_to_the_code() {
        let cwd = tmp("save"); let cwd_s = cwd.to_string_lossy().to_string();
        assert!(save_from_run(&cwd_s, &cwd_s, 3, None, false).unwrap_err().contains("left no QA plan"), "nothing recorded, nothing to keep");
        record(&cwd_s, 3, "ui", "http://localhost:5173/", "Add a sort dropdown to the shop", ui_steps(), &ev(&[("the page does say \"Add to cart\"", true)], 0)).unwrap();
        record(&cwd_s, 4, "ui", "http://localhost:5173/", "a failing one", ui_steps(), &ev(&[("x", false)], 0)).unwrap();
        record(&cwd_s, 5, "ui", "http://localhost:5173/", "pictures only", ui_steps(), &ev(&[], 0)).unwrap();
        assert!(save_from_run(&cwd_s, &cwd_s, 4, None, false).unwrap_err().contains("did not pass"));
        assert!(save_from_run(&cwd_s, &cwd_s, 5, None, false).unwrap_err().contains("no deterministic check"));
        let saved = save_from_run(&cwd_s, &cwd_s, 3, None, false).unwrap();
        assert_eq!(saved["slug"], json!("add-a-sort-dropdown-to-the-shop"));
        // it is exactly where the operator was promised: .antiloki/qa/<slug>.json, and a person can read it
        let file = cwd.join(".antiloki/qa/add-a-sort-dropdown-to-the-shop.json");
        assert!(file.is_file(), "{}", file.display());
        assert_eq!(load(&cwd_s, "add-a-sort-dropdown-to-the-shop").unwrap(), saved);
        // a second plan under a chosen name; the list is newest first and carries no steps
        record(&cwd_s, 6, "api", "http://localhost:3000", "hello api", json!([{ "do": "get", "path": "/api/hello" }, { "do": "expect", "status": 200 }]), &ev(&[("the status is 200", true)], 0)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(3));
        save_from_run(&cwd_s, &cwd_s, 6, Some("hello-api"), false).unwrap();
        let l = list(&cwd_s);
        assert_eq!(l.iter().map(|p| p["slug"].as_str().unwrap()).collect::<Vec<_>>(), vec!["hello-api", "add-a-sort-dropdown-to-the-shop"], "{l:?}");
        assert_eq!((l[0]["kind"].as_str(), l[0]["steps"].as_u64(), l[0]["checks"].as_u64()), (Some("api"), Some(2), Some(1)));
        // names are file stems: nothing walks out of the folder, and the evidence folder is not a plan
        assert!(load(&cwd_s, "../../etc/passwd").is_err() && remove(&cwd_s, "../x").is_err());
        remove(&cwd_s, "hello-api").unwrap();
        assert!(load(&cwd_s, "hello-api").unwrap_err().contains("no saved QA plan") && remove(&cwd_s, "hello-api").is_err());
        assert_eq!(list(&cwd_s).len(), 1);
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn a_corrupt_or_foreign_file_in_the_qa_folder_is_not_listed_and_does_not_break_the_list() {
        let cwd = tmp("junk"); let cwd_s = cwd.to_string_lossy().to_string();
        std::fs::create_dir_all(plans_dir(&cwd_s).join("evidence")).unwrap();
        std::fs::write(plans_dir(&cwd_s).join("broken.json"), "{ not json").unwrap();
        std::fs::write(plans_dir(&cwd_s).join("Has Spaces.json"), "{}").unwrap();
        std::fs::write(plans_dir(&cwd_s).join("notes.txt"), "hi").unwrap();
        assert!(list(&cwd_s).is_empty());
        assert!(load(&cwd_s, "broken").unwrap_err().contains("not valid JSON"));
        let _ = std::fs::remove_dir_all(&cwd);
    }

    /// A PLAN THAT RAN IS LONGER THAN A PLAN THE QA WROTE (review 2026-10-01). The recorded plan is the QA's steps (24 at most) followed by the generated coverage plan (48 at most), but
    /// `promote` and `replay` re-parsed it with the QA's own 24-step door: an 8-step plan beside one dropdown of five options was recorded and could never be kept or replayed.
    #[test]
    fn a_plan_of_the_qas_steps_plus_the_coverage_steps_can_be_kept_and_read_back_for_a_replay() {
        let cwd = tmp("long"); let cwd_s = cwd.to_string_lossy().to_string();
        let long = |n: usize| -> Value { let mut v: Vec<Value> = (0..n - 1).map(|_| json!({ "do": "wait", "ms": 10 })).collect(); v.push(json!({ "do": "expect", "text": "Add to cart" })); json!(v) };
        let full = qe::MAX_STEPS + qe::COVERAGE_MAX_STEPS;
        assert_eq!(qe::MAX_RECORDED_STEPS, full);
        record(&cwd_s, 1, "ui", "http://localhost:5173/", "a long one", long(full), &ev(&[("the page does say \"Add to cart\"", true)], 0)).unwrap();
        let saved = save_from_run(&cwd_s, &cwd_s, 1, None, false).expect("24 QA steps + 48 coverage steps are a plan that ran, so it can be kept");
        assert_eq!(saved["steps"].as_array().unwrap().len(), full);
        // …and the replay's own read of the file accepts it, where the QA's door would not
        assert_eq!(ui_steps_of(&load(&cwd_s, "a-long-one").unwrap()).unwrap().len(), full);
        assert!(qe::parse_plan(&long(full)).is_err(), "what a model WRITES is still held to its 24");
        // one over the sum is refused, as a hand-edited file must be
        record(&cwd_s, 2, "ui", "http://localhost:5173/", "too long", long(full + 1), &ev(&[("x", true)], 0)).unwrap();
        assert!(save_from_run(&cwd_s, &cwd_s, 2, None, false).unwrap_err().contains("the budget is 72"));
        let _ = std::fs::remove_dir_all(&cwd);
    }

    /// WHERE THE PLAN IS KEPT (review 2026-10-01): a run in a box or a sub-folder saved under ITS cwd, and the doors that default to the workspace root never saw it (and a box's `.antiloki`
    /// is not staged, so the file went away with the box). The plan goes to the workspace root, or the main checkout of a linked worktree, and the answer says where.
    #[test]
    fn a_plan_from_a_sub_folder_or_a_box_is_kept_where_the_default_list_looks() {
        let root = tmp("home"); let root_s = root.to_string_lossy().to_string();
        let never = |_: &str| -> Option<String> { panic!("a run inside the workspace needs no git") };
        assert_eq!(plan_home_with(&format!("{root_s}/packages/shop"), &root_s, never), root_s, "a sub-folder of the workspace");
        assert_eq!(plan_home_with(&root_s, &root_s, never), root_s);
        assert_eq!(plan_home_with("/elsewhere/box", &root_s, |c| Some(format!("{c}-main"))), "/elsewhere/box-main", "a box outside the root: its main checkout");
        assert_eq!(plan_home_with("/elsewhere/plain", &root_s, |_| None), "/elsewhere/plain", "no checkout to go to: the folder itself, as before");
        // the whole path, a run in a sub-folder: recorded there, kept at the root, listed with no cwd
        let sub = root.join("packages/shop"); std::fs::create_dir_all(&sub).unwrap(); let sub_s = sub.to_string_lossy().to_string();
        record(&sub_s, 9, "ui", "http://localhost:5173/", "a sort dropdown", ui_steps(), &ev(&[("the page does say \"Add to cart\"", true)], 0)).unwrap();
        let home = plan_home(&sub_s, &root_s);
        save_from_run(&sub_s, &home, 9, None, false).unwrap();
        assert_eq!(list(&root_s).iter().map(|p| p["slug"].as_str().unwrap().to_string()).collect::<Vec<_>>(), vec!["a-sort-dropdown"], "listed where the default (the workspace root) looks");
        assert!(list(&sub_s).is_empty(), "…and not left behind in the sub-folder");
        // a real linked worktree: the box. Its plan is kept in the main checkout, which outlives the box
        let git = |d: &Path, a: &[&str]| { let o = crate::proc::cmd("git").args(a).current_dir(d).output().unwrap(); assert!(o.status.success(), "git {a:?}: {}", String::from_utf8_lossy(&o.stderr)); };
        let main = tmp("main"); let boxd = tmp("box").join("agent");
        git(&main, &["init", "-q"]); git(&main, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "init"]);
        git(&main, &["worktree", "add", "-q", boxd.to_str().unwrap()]);
        let main_s = std::fs::canonicalize(&main).unwrap().to_string_lossy().to_string();
        assert_eq!(main_checkout_of(boxd.to_str().unwrap()).as_deref(), Some(main_s.as_str()), "a linked worktree finds the checkout it belongs to");
        assert_eq!(main_checkout_of(&main_s), None, "a plain checkout is its own home");
        assert_eq!(plan_home(boxd.to_str().unwrap(), "/some/other/workspace"), main_s);
        let _ = (std::fs::remove_dir_all(&root), std::fs::remove_dir_all(&main), std::fs::remove_dir_all(boxd.parent().unwrap()));
    }

    // ── F-14 / F-15 (audit 2026-10-01) ────────────────────────────────────────────────────────────────────────────────────────────────────────────────
    fn kept_one(cwd_s: &str, run: i64, ask: &str) {
        record(cwd_s, run, "ui", "http://localhost:5173/", ask, ui_steps(), &ev(&[("the page does say \"Add to cart\"", true)], 0)).unwrap();
    }

    #[test]
    fn saving_a_plan_never_replaces_a_kept_one_without_overwrite() {
        let cwd = tmp("exists"); let cwd_s = cwd.to_string_lossy().to_string();
        kept_one(&cwd_s, 1, "sort dropdown"); kept_one(&cwd_s, 2, "sort dropdown");
        let first = save_from_run(&cwd_s, &cwd_s, 1, None, false).unwrap();
        let before = std::fs::read_to_string(plan_path(&cwd_s, "sort-dropdown")).unwrap();
        let e = save_from_run(&cwd_s, &cwd_s, 2, None, false).unwrap_err();
        assert!(e.contains("already exists") && e.contains("overwrite"), "{e}");
        assert_eq!(std::fs::read_to_string(plan_path(&cwd_s, "sort-dropdown")).unwrap(), before, "the kept plan is untouched");
        assert_eq!(load(&cwd_s, "sort-dropdown").unwrap()["run_id"], first["run_id"]);
        let second = save_from_run(&cwd_s, &cwd_s, 2, None, true).unwrap();
        assert_eq!(load(&cwd_s, "sort-dropdown").unwrap()["run_id"], json!(2), "overwrite: true replaces it");
        assert_eq!(second["run_id"], json!(2));
        let leftovers: Vec<_> = std::fs::read_dir(plans_dir(&cwd_s)).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains("tmp")).collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn a_slug_keeps_the_letters_of_accented_words_and_an_ask_without_latin_letters_gets_the_run_id() {
        assert_eq!(default_slug("Añadir una página nueva", 7), "anadir-una-pagina-nueva");
        assert_eq!(default_slug("Straße and Œuvre and Łódź", 7), "strasse-and-oeuvre-and-lodz");
        // Cyrillic / Chinese / Greek asks: nothing to spell, so the run id names it - and two of them do not share one name
        assert_eq!(default_slug("Добавить кнопку", 7), "qa-plan-7");
        assert_eq!(default_slug("添加按钮", 8), "qa-plan-8");
        assert_eq!(default_slug("???", 9), "qa-plan-9");
        assert_ne!(default_slug("Добавить кнопку", 7), default_slug("Другая кнопка", 8));
        // mixed: the Latin words still make the name
        assert_eq!(default_slug("кнопка Add to cart", 7), "add-to-cart");
        for ask in ["página", "Добавить", "a/b", "../x", "Ünïcödé"] { assert!(safe_slug(&default_slug(ask, 3)), "{ask}"); }
        let cwd = tmp("cyr"); let cwd_s = cwd.to_string_lossy().to_string();
        kept_one(&cwd_s, 11, "Добавить кнопку"); kept_one(&cwd_s, 12, "Другая кнопка");
        assert_eq!(save_from_run(&cwd_s, &cwd_s, 11, None, false).unwrap()["slug"], json!("qa-plan-11"));
        assert_eq!(save_from_run(&cwd_s, &cwd_s, 12, None, false).unwrap()["slug"], json!("qa-plan-12"), "the second does not collide with the first");
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn a_replay_goes_only_to_a_loopback_address_that_is_not_this_servers_own_port() {
        for ok in ["http://localhost:5173", "http://127.0.0.1:3000/", "http://[::1]:8080", " http://localhost:4000 "] { assert!(app_ok(ok).is_ok(), "{ok}"); }
        for bad in ["http://evil.com", "http://localhost.evil.com:80", "http://localhost@evil.com", "http://127.0.0.1.evil.net/", "https://example.com/x", "file:///etc/passwd", "//evil.com", "localhost:5173",
                    "javascript:alert(1)", "http://169.254.169.254/latest/meta-data", "http://10.0.0.5:8080", "http://localhost:41931", "http://127.0.0.1:41998/api", ""] {
            assert!(app_ok(bad).is_err(), "{bad}");
        }
        // refused BEFORE a dev server is booted or a browser opened: the replay of a saved plan with a hostile app
        let cwd = tmp("hostile-app"); let cwd_s = cwd.to_string_lossy().to_string();
        kept_one(&cwd_s, 1, "x"); save_from_run(&cwd_s, &cwd_s, 1, None, false).unwrap();
        let e = replay(&cwd_s, "x", Some("http://evil.example/")).unwrap_err();
        assert!(e.contains("not on this machine"), "{e}");
        let _ = std::fs::remove_dir_all(&cwd);
    }

    /// An API plan used to boot the folder's dev server whatever it was given (the doc said an explicit address wins); now it resolves the app like a UI replay and a given address is used as it is.
    #[test]
    fn an_api_replay_uses_the_given_server_and_boots_nothing() {
        use std::io::{Read, Write};
        let cwd = tmp("api-replay"); let cwd_s = cwd.to_string_lossy().to_string();      // no package.json: booting a dev server here is impossible
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || { if let Ok((mut c, _)) = listener.accept() { let mut b = [0u8; 2048]; let _ = c.read(&mut b); let body = "{\"hello\":\"world\"}"; let _ = write!(c, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}", body.len(), body); } });
        record(&cwd_s, 1, "api", "http://localhost:3000", "hello", json!([{ "do": "get", "path": "/api/hello" }, { "do": "expect", "status": 200 }]), &ev(&[("the status is 200", true)], 0)).unwrap();
        save_from_run(&cwd_s, &cwd_s, 1, Some("hello"), false).unwrap();
        let r = replay(&cwd_s, "hello", Some(&format!("http://127.0.0.1:{port}"))).expect("the given server is called");
        assert_eq!(r["passed"], json!(true), "{r}");
        assert_eq!(r["source"], json!("the address given"));
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn a_folder_outside_the_active_workspace_is_not_one_the_doors_work_in() {
        let root = tmp("scope-root"); let root_s = root.to_string_lossy().to_string();
        let sibling = PathBuf::from(format!("{root_s}2")); std::fs::create_dir_all(&sibling).unwrap();
        let sub = root.join("packages/shop"); std::fs::create_dir_all(&sub).unwrap();
        let none = |_: &str| -> Option<String> { None };
        assert!(cwd_in_scope(&root_s, &root_s, &[], none).is_ok());
        assert!(cwd_in_scope(&sub.to_string_lossy(), &root_s, &[], none).is_ok(), "a sub-folder");
        assert!(cwd_in_scope(&format!("{root_s}/packages/../packages/shop"), &root_s, &[], none).is_ok(), "a path that still resolves inside");
        assert!(cwd_in_scope("/", &root_s, &[], none).is_err() && cwd_in_scope("/etc", &root_s, &[], none).is_err() && cwd_in_scope(&std::env::temp_dir().to_string_lossy(), &root_s, &[], none).is_err());
        assert!(cwd_in_scope(&sibling.to_string_lossy(), &root_s, &[], none).is_err(), "root2 starts like root and is not under it");
        assert!(cwd_in_scope(&format!("{root_s}/../{}", sibling.file_name().unwrap().to_string_lossy()), &root_s, &[], none).is_err(), ".. out of the root");
        assert!(cwd_in_scope(&format!("{root_s}/does-not-exist"), &root_s, &[], none).is_err());
        assert!(cwd_in_scope(&root_s, "", &[], none).unwrap_err().contains("no workspace is open"));
        #[cfg(unix)] {
            std::os::unix::fs::symlink(&sibling, root.join("link-out")).unwrap();
            assert!(cwd_in_scope(&format!("{root_s}/link-out"), &root_s, &[], none).is_err(), "a symlink inside the root that points out");
        }
        // a linked worktree (a box) of the workspace is in scope; one of another repository is not
        let boxd = tmp("scope-box").to_string_lossy().to_string();
        let canon_root = std::fs::canonicalize(&root).unwrap().to_string_lossy().to_string();
        assert!(cwd_in_scope(&boxd, &root_s, &[], |_| Some(canon_root.clone())).is_ok());
        assert!(cwd_in_scope(&boxd, &root_s, &[], |_| Some("/some/other/repo".into())).is_err());
        // a folder one of this server's own runs worked in (a chat's folder) is a place its plans live; a folder nobody ran anything in is not
        assert!(cwd_in_scope(&boxd, &root_s, &[boxd.clone()], none).is_ok());
        assert!(cwd_in_scope(&boxd, "", &[boxd.clone()], none).is_ok(), "even with no workspace open");
        assert!(cwd_in_scope("/etc", &root_s, &[boxd.clone()], none).is_err());
        let _ = (std::fs::remove_dir_all(&root), std::fs::remove_dir_all(&sibling), std::fs::remove_dir_all(&boxd));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_qa_folder_is_neither_read_nor_written_nor_deleted_through() {
        let cwd = tmp("qa-link"); let cwd_s = cwd.to_string_lossy().to_string();
        let outside = tmp("qa-link-out");
        std::fs::write(outside.join("precious.json"), "{\"kind\":\"ui\"}").unwrap();
        std::fs::create_dir_all(cwd.join(".antiloki")).unwrap();
        std::os::unix::fs::symlink(&outside, cwd.join(".antiloki/qa")).unwrap();
        assert!(checked_plans_dir(&cwd_s).unwrap_err().contains("symlink"));
        assert!(list(&cwd_s).is_empty(), "nothing is listed through the link");
        assert!(load(&cwd_s, "precious").is_err());
        assert!(remove(&cwd_s, "precious").is_err());
        assert!(outside.join("precious.json").exists(), "and nothing was deleted");
        // the run's own record is not written through the link either, and there is nothing to keep from it
        assert!(record(&cwd_s, 1, "ui", "http://localhost:5173/", "x", ui_steps(), &ev(&[("the page does say \"Add to cart\"", true)], 0)).unwrap_err().contains("symlink"));
        assert!(save_from_run(&cwd_s, &cwd_s, 1, None, true).is_err());
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1, "nothing was written into the folder the link points at");
        // ...and `.antiloki` itself as a link
        let cwd2 = tmp("qa-link2"); let cwd2_s = cwd2.to_string_lossy().to_string();
        std::os::unix::fs::symlink(&outside, cwd2.join(".antiloki")).unwrap();
        assert!(checked_plans_dir(&cwd2_s).is_err());
        let _ = (std::fs::remove_dir_all(&cwd), std::fs::remove_dir_all(&cwd2), std::fs::remove_dir_all(&outside));
    }

    #[test]
    fn removing_a_plan_removes_its_replays_and_the_list_is_capped() {
        let cwd = tmp("remove"); let cwd_s = cwd.to_string_lossy().to_string();
        kept_one(&cwd_s, 1, "sort dropdown"); save_from_run(&cwd_s, &cwd_s, 1, None, false).unwrap();
        let replays = plans_dir(&cwd_s).join("replays").join("sort-dropdown"); std::fs::create_dir_all(&replays).unwrap(); std::fs::write(replays.join("page.png"), "png").unwrap();
        let other = plans_dir(&cwd_s).join("replays").join("other-plan"); std::fs::create_dir_all(&other).unwrap();
        remove(&cwd_s, "sort-dropdown").unwrap();
        assert!(!replays.exists(), "the pictures of the plan's replays go with it");
        assert!(other.exists(), "another plan's replays stay");
        // the list: capped, and a file that is not the size of a plan is not read
        for i in 0..(MAX_LISTED + 30) { std::fs::write(plans_dir(&cwd_s).join(format!("p{i}.json")), format!("{{\"kind\":\"ui\",\"saved_at\":{i},\"steps\":[]}}")).unwrap(); }
        std::fs::write(plans_dir(&cwd_s).join("huge.json"), vec![b' '; (MAX_PLAN_BYTES + 1) as usize]).unwrap();
        let l = list(&cwd_s);
        assert_eq!(l.len(), MAX_LISTED);
        assert!(l.iter().all(|p| p["slug"] != json!("huge")));
        assert!(load(&cwd_s, "huge").unwrap_err().contains("too big"));
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn a_stored_passed_flag_is_not_believed_and_an_edited_record_is_not_kept() {
        let cwd = tmp("tamper"); let cwd_s = cwd.to_string_lossy().to_string();
        let rec = |checks_failed: u64, passed: bool| json!({ "format": 1, "kind": "ui", "run_id": 7, "ask": "x", "app": "http://localhost:5173/", "steps": ui_steps(), "checks": 2, "checks_failed": checks_failed, "failed_steps": 0, "passed": passed });
        // `passed: true` beside a failed check is a lie the file can tell; `passed: false` with nothing failed is also honoured as a refusal
        assert!(promote(&rec(1, true), "x", 0).unwrap_err().contains("did not pass"));
        assert!(promote(&rec(0, false), "x", 0).is_err());
        assert!(promote(&rec(0, true), "x", 0).is_ok());
        // a record this process wrote and the agent then edited (the file sits in the agent's box): its steps, its counts
        kept_one(&cwd_s, 3, "tamper");
        let p = record_path(&cwd_s, 3);
        let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        doc["steps"] = json!([{ "do": "goto", "url": "http://localhost:5173/admin" }, { "do": "expect", "text": "ok" }]);
        std::fs::write(&p, doc.to_string()).unwrap();
        assert!(save_from_run(&cwd_s, &cwd_s, 3, None, false).unwrap_err().contains("changed after the run"));
        kept_one(&cwd_s, 4, "untouched");
        assert!(save_from_run(&cwd_s, &cwd_s, 4, None, false).is_ok(), "an unedited record is kept as before");
        let _ = std::fs::remove_dir_all(&cwd);
    }
}
