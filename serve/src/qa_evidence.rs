//! QA EVIDENCE - visual QA (operator 2026-09-30: "frontend jobs also should print screen images deterministically to feed the QA and show behavior or
//! changes. If the behavior is static like a new button or a color change just an image. If the behavior is not static the AI should control the preview
//! pane to simulate the behavior and print the image of the result. All that should be selected by the QA but operated deterministically like some
//! deterministic skills for the QA to use").
//!
//! The split is the whole design: the QA judge only CHOOSES - it writes a small JSON plan of steps ("open /cart, click Add to cart, shoot") - and this file
//! EXECUTES it with no model in the loop: one headless Chrome over CDP (the machine's own, found by `browser-capture`), a fixed viewport and scale, animations
//! and transitions switched off, fonts awaited, the page's console errors collected. The same plan on the same app gives the same pixels, which is what makes a
//! before/after comparison mean something. Every seat can look at the resulting PNGs (probed 2026-09-30: claude, codex `-i`, cursor, gemini, grok all read them).
//!
//! Bounded on purpose: a plan is at most `MAX_STEPS` long, waits are capped, and a step that fails is REPORTED (the QA sees "click failed: nothing matches ...")
//! rather than aborting the run - a broken button is exactly the finding QA wants.
use headless_chrome::{Browser, LaunchOptions, Tab};
use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub const MAX_STEPS: usize = 24;
/// What a RECORDED plan can hold: the QA's own steps (`MAX_STEPS`) plus the generated coverage plan (`COVERAGE_MAX_STEPS`) that runs after them in the same browser session. `parse_plan` is the QA's
/// door and stays at `MAX_STEPS`; the saved plan and its replay are the plan that RAN, so they are bounded by the sum (review 2026-10-01: a 28-step plan was recorded and could never be kept).
pub const MAX_RECORDED_STEPS: usize = MAX_STEPS + COVERAGE_MAX_STEPS;
pub const MAX_WAIT_MS: u64 = 5_000;
pub const VIEWPORT: (u32, u32) = (1280, 800);

/// one thing the QA may ask the browser to do
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Goto(String),
    /// a CSS selector, or `text=Add to cart` for the smallest element whose visible text contains it
    Click(String),
    Hover(String),
    Type { target: String, text: String },
    Press(String),
    WaitFor(String),
    Wait(u64),
    Scroll(i64),
    /// take a screenshot and name it (file name safe: letters, digits, `-`, `_`)
    Shot(String),
    /// A DETERMINISTIC CHECK (operator 2026-09-30: *"i asked for a search and didnt test the search ... the AI writes a test that works deterministically like a unit test"*): the
    /// browser itself decides whether the page did what the claim says - no model looks at anything to get this verdict.
    Expect(Expect),
    /// choose an option of a dropdown by its visible label (or value)
    Select { target: String, option: String },
    /// empty a text box
    Clear(String),
    /// remember how the page looks now, so `ExpectChange` can tell whether the next action did anything
    Mark,
    /// the page must look different from the last `Mark` (what the action was, in words)
    ExpectChange(String),
    /// the same observation for a control whose job the ask did not name (a category button, a cart toggle): reported, never failed - "clicking All changed nothing" is right when All was already on
    NoteChange(String),
    /// MEASURE HOW THE PAGE LOOKS (PLAN-THE-DESIGN D1): contrast, spacing grid, type scale, palette, alignment, clipping, tap targets, :hover/:focus, browser-default controls - deterministic arithmetic
    /// over what the browser computed, no model (`design_lint`). The result lands in `Evidence::design`. Never fails a plan: a page that cannot be measured is logged and the plan goes on.
    DesignLint,
}

/// What a check asks of the page. `target` alone = at least `min` visible matches; `text` alone = the page says it; both = that element says it; `url` = the address
/// contains `text`; `not` inverts any of them. Pure data - tested.
#[derive(Clone, Debug, PartialEq)]
pub struct Expect { pub target: String, pub text: String, pub min: u64, pub not: bool, pub url: bool, /// the matches must be ANIMATED (a running CSS animation / transition / Web Animation)
    pub animated: bool }
impl Expect {
    /// the check in words, for the film's caption and the evidence block
    pub fn words(&self) -> String {
        let not = if self.not { "not " } else { "" };
        if self.url { return format!("the address does {not}contain \"{}\"", self.text) }
        if self.animated { return format!("at least {} {} is animated", self.min, pretty_target(&self.target)) }
        match (self.target.is_empty(), self.text.is_empty()) {
            (false, false) => format!("{} does {not}say \"{}\"", pretty_target(&self.target), self.text),
            (false, true) => if self.not { format!("{} is not on the page", pretty_target(&self.target)) } else { format!("at least {} {} on the page", self.min, pretty_target(&self.target)) },
            _ => format!("the page does {not}say \"{}\"", self.text),
        }
    }
}

/// The QA's plan (a JSON array) -> steps, or the reason it is not a plan. Pure - tested. Every field is validated and bounded: a plan is DATA from a model, so
/// it may not name anything but the actions above, may not exceed the step budget, and screenshot names cannot walk out of the evidence folder.
pub fn parse_plan(plan: &Value) -> Result<Vec<Step>, String> { parse_plan_max(plan, MAX_STEPS) }
/// The same, with the step budget the caller's plan is held to (`MAX_STEPS` for what a model writes, `MAX_RECORDED_STEPS` for what ran). Pure - tested.
pub fn parse_plan_max(plan: &Value, max: usize) -> Result<Vec<Step>, String> {
    let arr = plan.as_array().ok_or("a plan is a JSON array of steps")?;
    if arr.is_empty() { return Err("the plan has no steps".into()) }
    if arr.len() > max { return Err(format!("the plan has {} steps; the budget is {max}", arr.len())) }
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::trim).filter(|x| !x.is_empty()).map(String::from);
    let mut out = Vec::with_capacity(arr.len());
    for (i, st) in arr.iter().enumerate() {
        let at = |m: &str| format!("step {}: {m}", i + 1);
        let act = s(st, "do").ok_or_else(|| at("needs a \"do\""))?.to_lowercase();
        let target = || s(st, "target").ok_or_else(|| at(&format!("\"{act}\" needs a \"target\"")));
        out.push(match act.as_str() {
            "goto" | "open" => {
                let u = s(st, "url").or_else(|| s(st, "target")).ok_or_else(|| at("goto needs a \"url\""))?;
                if !nav_shape_ok(&u) { return Err(at("only the box's own app may be opened (localhost, 127.0.0.1, [::1], file:// or a path)")) }
                Step::Goto(u)
            }
            "click" => Step::Click(target()?),
            "hover" => Step::Hover(target()?),
            "type" => Step::Type { target: target()?, text: s(st, "text").ok_or_else(|| at("type needs \"text\""))? },
            "press" => Step::Press(s(st, "key").ok_or_else(|| at("press needs a \"key\""))?),
            "wait_for" | "waitfor" => Step::WaitFor(target()?),
            "wait" => Step::Wait(st.get("ms").and_then(Value::as_u64).unwrap_or(500).min(MAX_WAIT_MS)),
            "scroll" => Step::Scroll(st.get("y").and_then(Value::as_i64).unwrap_or(600).clamp(-20_000, 20_000)),
            "shot" | "screenshot" => {
                let n = s(st, "name").unwrap_or_else(|| format!("shot-{}", i + 1));
                if !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') || n.len() > 60 { return Err(at("a screenshot name is letters, digits, - and _ (60 at most)")) }
                Step::Shot(n)
            }
            "select" | "choose" => Step::Select { target: target()?, option: s(st, "option").or_else(|| s(st, "text")).ok_or_else(|| at("select needs an \"option\" (its visible label)"))? },
            "clear" => Step::Clear(target()?),
            "mark" => Step::Mark,
            "design_lint" | "lint_design" => Step::DesignLint,
            // the observation the generated coverage plan makes (and a SAVED plan carries back): reported, never failed
            "note_change" => Step::NoteChange(s(st, "what").or_else(|| s(st, "text")).unwrap_or_else(|| "the action".into())),
            "expect_change" | "expect_changed" => Step::ExpectChange(s(st, "what").or_else(|| s(st, "text")).unwrap_or_else(|| "the action".into())),
            "expect_animated" => Step::Expect(Expect { target: target()?, text: String::new(), min: st.get("min").and_then(Value::as_u64).unwrap_or(1).clamp(1, 1000), not: false, url: false, animated: true }),
            "expect" | "expect_no" | "expect_not" | "expect_url" => {
                let (tg, tx) = (s(st, "target").unwrap_or_default(), s(st, "text").unwrap_or_default());
                let url = act == "expect_url";
                let not = act != "expect" && !url;
                if url && tx.is_empty() { return Err(at("expect_url needs \"text\" (what the address should contain)")) }
                if !url && tg.is_empty() && tx.is_empty() { return Err(at(&format!("\"{act}\" needs a \"target\" (an element) or \"text\" (what the page should say)"))) }
                Step::Expect(Expect { target: tg, text: tx, min: st.get("min").and_then(Value::as_u64).unwrap_or(1).clamp(1, 1000), not, url, animated: false })
            }
            other => return Err(at(&format!("unknown action \"{other}\" (goto, click, hover, type, select, clear, press, wait_for, wait, scroll, shot, mark, design_lint, expect, expect_animated, expect_change, expect_no, expect_url)"))),
        });
    }
    Ok(out)
}

// ---- WHERE A PLAN MAY GO (audit 2026-10-01, F-08) ------------------------------------------------------------------------------------------------------
//
// A plan is DATA from a model whose prompt includes the diff, and a saved plan lives in the repo (`.antiloki/qa/*.json`), so a cloned repo can carry one. The old gate was a string-prefix test:
// `http://localhost.evil.com/`, `http://localhost@evil.com` and `http://127.0.0.1.evil.net/` passed it, `file://` was open for ANY path (`file:///etc/passwd`, `~/.ssh/id_rsa`: Chrome renders it, `shot`
// writes a PNG that is handed to the judge seat), and any `http://localhost:<port>` was fine, the serve's own included. Now the URL is PARSED (no userinfo, no backslash, an exact host, a numeric port):
// * `parse_plan` (`nav_shape_ok`) holds the SHAPE - it does not know the app yet: a path, an exact loopback name with any port, or a file:// URL;
// * the run (`nav_allowed`) holds the DESTINATION: the app's own host:port (or the other loopback spellings of the SAME port), never the serve's own port unless that IS the app, and a file:// only
//   inside the box (canonicalised, so a symlink inside it that points out is refused).

/// The three names a machine reaches itself by. Exact: not `localhost.` (a trailing dot), not `foo.localhost`, not `127.1` - Chrome would take them to the same place, so they are refused rather than guessed at.
fn is_file_url(u: &str) -> bool { u.get(..7).map_or(false, |p| p.eq_ignore_ascii_case("file://")) }
pub(crate) fn is_loopback_host(h: &str) -> bool { matches!(h, "localhost" | "127.0.0.1" | "[::1]") }

/// `http(s)://host[:port]/rest` -> (scheme, host, port, rest), or None for anything a browser could read differently than this does. Strict: no userinfo, no backslash (Chrome treats `\` as `/`),
/// no whitespace or control characters, a numeric port, a non-empty host. The host is lowercased. Pure - tested.
pub fn parse_http_url(u: &str) -> Option<(String, String, u16, String)> {
    if u.chars().any(|c| c.is_control() || c.is_whitespace() || c == '\\') { return None }
    let (scheme, rest) = u.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default = match scheme.as_str() { "http" => 80, "https" => 443, _ => return None };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    if authority.is_empty() || authority.contains(['@', '%']) { return None }
    let (host, port) = if authority.starts_with('[') {
        let close = authority.find(']')?;
        let after = &authority[close + 1..];
        (&authority[..=close], if after.is_empty() { default } else { after.strip_prefix(':')?.parse::<u16>().ok()? })
    } else {
        match authority.split_once(':') { Some((h, p)) => (h, p.parse::<u16>().ok()?), None => (authority, default) }
    };
    if host.is_empty() || host.ends_with('.') || (!host.starts_with('[') && host.contains(['[', ']', ':'])) { return None }
    Some((scheme, host.to_ascii_lowercase(), port, tail.to_string()))
}

/// Can this `goto` target even be considered, before anyone knows the app? A path ("/cart", not "//host"), a loopback http(s) URL on any port, or a file:// URL. Pure - tested.
pub fn nav_shape_ok(u: &str) -> bool {
    if u.starts_with("//") || u.contains('\\') { return false }
    if u.starts_with('/') { return !u.chars().any(|c| c.is_control()) }
    if is_file_url(u) { return !u.chars().any(|c| c.is_control()) }
    parse_http_url(u).map_or(false, |(_, host, _, _)| is_loopback_host(&host))
}

/// The ports THIS serve answers on: the one it was told, the one it tells its children, and the two defaults. A QA plan has no business at any of them (their routes write: publish, forge tokens, QA replay)
/// unless the app under test is the serve itself.
pub(crate) fn serve_ports() -> Vec<u16> {
    let mut v = vec![41931u16, 41998];
    for k in ["ANTILOKI_SERVE_PORT", "ANTILOKI_BE_PORT"] { if let Some(p) = std::env::var(k).ok().and_then(|x| x.trim().parse::<u16>().ok()) { v.push(p) } }
    v
}

/// `%2e%2e` -> `..`: what Chrome does to a file:// path before it opens it. None on a malformed escape or non-UTF-8 result.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes(); let mut out = Vec::with_capacity(b.len()); let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' { let h = s.get(i + 1..i + 3)?; out.push(u8::from_str_radix(h, 16).ok()?); i += 3 } else { out.push(b[i]); i += 1 }
    }
    String::from_utf8(out).ok()
}

/// The filesystem path a file:// URL names (query and fragment dropped, escapes decoded), or None when it names a remote host or is malformed.
fn file_url_path(u: &str) -> Option<PathBuf> {
    if !is_file_url(u) { return None }
    let rest = &u[7..];
    let rest = &rest[..rest.find(['?', '#']).unwrap_or(rest.len())];
    // `file:///abs/path` and `file://localhost/abs/path`; any other host is a network share
    let path = if rest.starts_with('/') { rest } else if let Some(p) = rest.strip_prefix("localhost").filter(|r| r.starts_with('/')) { p } else if cfg!(windows) && rest.as_bytes().get(1) == Some(&b':') { rest } else { return None };
    let decoded = percent_decode(path)?;
    if decoded.contains('\0') { return None }
    #[cfg(windows)]
    let decoded = decoded.trim_start_matches('/').to_string();
    Some(PathBuf::from(decoded))
}

/// MAY THE BROWSER GO TO `u`? `base` is the app under test (a URL, or a file:// URL for a static page), `box_root` the box the work lives in (file:// targets must resolve inside it). Ok or the reason. Pure
/// over the arguments and the filesystem it resolves symlinks on - tested with every bypass the audit listed.
pub fn nav_allowed(u: &str, base: &str, box_root: Option<&Path>) -> Result<(), String> {
    let no = |why: &str| Err(format!("{u:?} is refused: {why}"));
    if u.starts_with("//") || u.contains('\\') { return no("not an address on the app") }
    if u.starts_with('/') { return Ok(()) }   // joined onto the app's own base by the runner
    if is_file_url(u) {
        let Some(root) = box_root else { return no("a file:// page may be opened only inside the box, and no box was named") };
        let Some(path) = file_url_path(u) else { return no("not a local file address") };
        let (Ok(canon), Ok(root)) = (std::fs::canonicalize(&path), std::fs::canonicalize(root)) else { return no("the file does not exist inside the box") };
        return if canon.starts_with(&root) { Ok(()) } else { no("it resolves outside the box") };
    }
    let Some((_, host, port, _)) = parse_http_url(u) else { return no("only http(s) addresses on the app, paths on it, or files in the box may be opened") };
    // the app's own host:port; when the app is on this machine, its other loopback spellings of the SAME port
    let app = if is_file_url(base) { None } else { parse_http_url(base) };
    let ok = match &app {
        Some((_, bh, bp, _)) => port == *bp && (host == *bh || (is_loopback_host(bh) && is_loopback_host(&host))),
        None => false,
    };
    if !ok { return no("it is not the app under test (the same host and port as the page being checked)") }
    if serve_ports().contains(&port) && app.as_ref().map_or(true, |(_, _, bp, _)| *bp != port) { return no("that is this server's own port") }
    Ok(())
}

/// Chrome's sandbox stays ON unless the process cannot run it: root, a container, or the operator forcing it (`ANTILOKI_CHROME_NO_SANDBOX=1`). Audit 2026-10-01 F-08: `--no-sandbox` was passed always,
/// so a page the plan was steered to had a browser with no sandbox around it. (headless_chrome adds `--no-sandbox` itself when `sandbox(false)`.)
pub fn chrome_needs_no_sandbox() -> bool {
    if std::env::var("ANTILOKI_CHROME_NO_SANDBOX").map_or(false, |v| matches!(v.trim(), "1" | "true" | "yes")) { return true }
    #[cfg(unix)]
    { extern "C" { fn geteuid() -> u32; } if unsafe { geteuid() } == 0 { return true } }
    Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists()
}

/// The inverse of `parse_plan`: the steps as the JSON a plan is written in, so a plan that ran (the QA's own steps AND the generated coverage) can be kept and parsed back into
/// the same steps. Pure - tested by the round trip over every kind of step.
pub fn steps_json(steps: &[Step]) -> Value {
    Value::Array(steps.iter().map(|st| match st {
        Step::Goto(u) => json!({ "do": "goto", "url": u }),
        Step::Click(t) => json!({ "do": "click", "target": t }),
        Step::Hover(t) => json!({ "do": "hover", "target": t }),
        Step::Type { target, text } => json!({ "do": "type", "target": target, "text": text }),
        Step::Press(k) => json!({ "do": "press", "key": k }),
        Step::WaitFor(t) => json!({ "do": "wait_for", "target": t }),
        Step::Wait(ms) => json!({ "do": "wait", "ms": ms }),
        Step::Scroll(y) => json!({ "do": "scroll", "y": y }),
        Step::Shot(n) => json!({ "do": "shot", "name": n }),
        Step::Select { target, option } => json!({ "do": "select", "target": target, "option": option }),
        Step::Clear(t) => json!({ "do": "clear", "target": t }),
        Step::Mark => json!({ "do": "mark" }),
        Step::DesignLint => json!({ "do": "design_lint" }),
        Step::ExpectChange(w) => json!({ "do": "expect_change", "what": w }),
        Step::NoteChange(w) => json!({ "do": "note_change", "what": w }),
        Step::Expect(e) if e.url => json!({ "do": "expect_url", "text": e.text }),
        Step::Expect(e) if e.animated => json!({ "do": "expect_animated", "target": e.target, "min": e.min }),
        Step::Expect(e) => {
            let mut o = json!({ "do": if e.not { "expect_no" } else { "expect" }, "min": e.min });
            if !e.target.is_empty() { o["target"] = json!(e.target); }
            if !e.text.is_empty() { o["text"] = json!(e.text); }
            o
        }
    }).collect())
}

/// what a run of a plan left behind
#[derive(Debug, Default)]
pub struct Evidence {
    /// (name, path of the PNG)
    pub shots: Vec<(String, PathBuf)>,
    /// one line per step: what was done, or why it failed
    pub log: Vec<String>,
    /// `console.error` calls and uncaught errors seen on the pages
    pub console_errors: Vec<String>,
    /// the visible text of the page after the last step (for a seat, or a check, that wants words rather than pixels)
    pub text: String,
    pub failed_steps: usize,
    /// the deterministic checks the plan asked for: (what was asked, passed, what the page actually had)
    pub checks: Vec<(String, bool, String)>,
    /// THE DESIGN LINT (D1) of the page as the plan left it, when the plan had a `DesignLint` step and the page could be measured. Kept OUT of `checks` on purpose: a check is a fact about whether the
    /// FEATURE works and a failed one tells the judge the claim is unmet, but a contrast problem the page already had is not the change's claim. The judge reads it as its own block (`prompt_block_with`).
    pub design: Option<crate::design_lint::DesignLint>,
}
impl Evidence { pub fn checks_failed(&self) -> usize { self.checks.iter().filter(|c| !c.1).count() } }

/// Runs inside every page: the QA must see the errors the page throws, and the run must not depend on motion.
const HOOK_JS: &str = r#"(() => {
  if (window.__qa) return "hooked";
  window.__qa = { errors: [] };
  const push = (m) => { try { window.__qa.errors.push(String(m).slice(0, 300)); } catch (e) {} };
  const ce = console.error; console.error = function () { push(Array.from(arguments).map(String).join(" ")); return ce.apply(console, arguments); };
  window.addEventListener("error", (e) => push("uncaught: " + (e && e.message)));
  window.addEventListener("unhandledrejection", (e) => push("unhandled rejection: " + (e && e.reason)));
  const st = document.createElement("style");
  st.textContent = "*,*::before,*::after{animation:none!important;transition:none!important;caret-color:transparent!important;scroll-behavior:auto!important}";
  document.documentElement.appendChild(st);
  return "hooked";
})()"#;
/// The hook for a change that is ABOUT motion (operator 2026-09-30, "add ... avatars, all animated"): the same error collection, but the page's own animations are left running.
const HOOK_JS_MOTION: &str = r#"(() => {
  if (window.__qa) return "hooked";
  window.__qa = { errors: [] };
  const push = (m) => { try { window.__qa.errors.push(String(m).slice(0, 300)); } catch (e) {} };
  const ce = console.error; console.error = function () { push(Array.from(arguments).map(String).join(" ")); return ce.apply(console, arguments); };
  window.addEventListener("error", (e) => push("uncaught: " + (e && e.message)));
  window.addEventListener("unhandledrejection", (e) => push("unhandled rejection: " + (e && e.reason)));
  // MOTION MODE: the change is ABOUT animation, so the page keeps its animations - killing them and then asking whether anything moves could only ever fail
  return "hooked";
})()"#;

/// find an element by CSS selector or `text=...`; returns JS that evaluates to the element or null
fn finder(target: &str) -> String {
    let t = serde_json::to_string(target).unwrap_or_else(|_| "\"\"".into());
    format!(r#"(() => {{
      const t = {t};
      const isVisible = (el) => {{ const cs = getComputedStyle(el); return el.getClientRects().length > 0 && cs.visibility !== "hidden" && cs.display !== "none"; }};
      if (t.startsWith("text=")) {{
        const want = t.slice(5).trim().toLowerCase();
        let best = null, bestLen = 1e9;
        for (const el of Array.from(document.querySelectorAll("body *"))) {{
          if (!isVisible(el)) continue;
          const own = (el.innerText || el.value || el.getAttribute("aria-label") || el.getAttribute("title") || "").trim().toLowerCase();
          if (own.includes(want) && own.length < bestLen) {{ best = el; bestLen = own.length; }}
        }}
        return best;
      }}
      try {{ return document.querySelector(t); }} catch (e) {{ return null; }}
    }})()"#)
}

fn eval_bool(tab: &Tab, js: &str) -> Result<bool, String> {
    let ro = tab.evaluate(js, false).map_err(|e| e.to_string())?;
    Ok(ro.value.and_then(|v| v.as_bool()).unwrap_or(false))
}

fn settle(tab: &Tab) { let _ = tab.evaluate("document.fonts && document.fonts.ready ? document.fonts.ready.then(()=>true) : true", true); }


/// The JS that decides a check in the page, answering `{"ok":bool,"got":"what the page had"}` as a JSON string. Pure text - the browser test runs it.
fn expect_js(e: &Expect) -> String {
    let j = |x: &str| serde_json::to_string(x).unwrap_or_else(|_| "\"\"".into());
    let finder_expr = if e.target.is_empty() { "null".to_string() } else { finder(&e.target) };
    format!(r#"(() => {{
      const want = {text}.toLowerCase(), target = {target}, min = {min}, not = {not};
      const vis = (el) => {{ if (!el) return false; const cs = getComputedStyle(el); return el.getClientRects().length > 0 && cs.visibility !== "hidden" && cs.display !== "none"; }};
      let ok = false, got = "";
      if ({animated}) {{
        let els = []; try {{ els = Array.from(document.querySelectorAll(target)).filter(vis); }} catch (e) {{ els = []; }}
        const moving = els.filter((el) => (el.getAnimations ? el.getAnimations().length > 0 : false) || (getComputedStyle(el).animationName && getComputedStyle(el).animationName !== "none") || Array.from(el.querySelectorAll("*")).some((c) => (c.getAnimations ? c.getAnimations().length > 0 : false)));
        got = moving.length + " of " + els.length + " visible match(es) are animated"; ok = moving.length >= min;
      }}
      else if ({url}) {{ got = location.href; ok = got.toLowerCase().includes(want); }}
      else if (target && !want) {{
        let n = 0;
        if (target.startsWith("text=")) {{ n = {finder_expr} ? 1 : 0; }}
        else {{ try {{ n = Array.from(document.querySelectorAll(target)).filter(vis).length; }} catch (e) {{ n = 0; }} }}
        got = n + " visible match(es)"; ok = n >= min;
      }} else if (target) {{
        const el = {finder_expr}; const t = el ? String(el.innerText || el.value || el.textContent || "") : "";
        got = el ? JSON.stringify(t.trim().replace(/\s+/g, " ").slice(0, 120)) : "no such element"; ok = !!el && t.toLowerCase().includes(want);
      }} else {{
        const t = document.body ? String(document.body.innerText || "") : "";
        ok = t.toLowerCase().includes(want);
        got = ok ? "the text is there" : "the page said: " + JSON.stringify(t.trim().replace(/\s+/g, " ").slice(0, 160));
      }}
      if (not) ok = !ok;
      return JSON.stringify({{ ok: ok, got: got }});
    }})()"#, text = j(&e.text), target = j(&e.target), min = e.min, not = e.not, url = e.url, animated = e.animated, finder_expr = finder_expr)
}

/// What the page looks like, as one string: its address, its visible text IN ORDER (a reordered list is a changed page), and how many elements it has. The same page always gives the same string.
const FINGERPRINT_JS: &str = r#"(() => { const t = document.body ? String(document.body.innerText || "").replace(/\s+/g, " ").trim() : ""; return location.href + "|" + document.querySelectorAll("*").length + "|" + t; })()"#;

/// THE COVERAGE PLAN - NO MODEL (operator 2026-09-30, after a sorting dropdown was "tested" by one picture: *"any interactable must be interacted and tested"*). From what the probe found on
/// the page, the steps that USE every control: each dropdown option is chosen, each text box is typed into and submitted with a word the page itself shows, each checkbox and button is
/// clicked - and after each the browser checks whether the page changed. It runs after the QA's own plan, from a fresh page, and what it finds goes to the judge as facts. Bounded. Pure - tested.
pub fn coverage_steps(probe: &Value, app: &str, ask: &str, diff: &str) -> Vec<Step> {
    let scoped = scope_controls(probe, ask, diff);
    let arr = |k: &str| probe.get(k).and_then(Value::as_array).cloned().unwrap_or_default();
    let sample = probe.get("sample").and_then(Value::as_str).filter(|x| !x.is_empty()).unwrap_or("a").to_string();
    let mut out: Vec<Step> = vec![];
    for i in arr("inputs") {
        let sel = i.get("sel").and_then(Value::as_str).unwrap_or("").to_string();
        if sel.is_empty() || !scoped.inputs.contains(&sel) { continue }
        let kind = i.get("kind").and_then(Value::as_str).unwrap_or("text");
        let hint = i.get("hint").and_then(Value::as_str).filter(|h| !h.is_empty()).map(|h| format!("\"{h}\"")).unwrap_or_else(|| sel.clone());
        match kind {
            "select" => {
                let cur = i.get("current").and_then(Value::as_str).unwrap_or("");
                for o in i.get("options").and_then(Value::as_array).cloned().unwrap_or_default().iter().filter_map(Value::as_str).filter(|o| !o.is_empty() && *o != cur).take(5) {
                    out.extend([Step::Goto(app.to_string()), Step::Mark, Step::Select { target: sel.clone(), option: o.to_string() }, Step::ExpectChange(format!("choosing \"{o}\" in the dropdown {sel}"))]);
                }
            }
            "checkbox" | "radio" => out.extend([Step::Goto(app.to_string()), Step::Mark, Step::Click(sel.clone()), Step::ExpectChange(format!("toggling {}", hint))]),
            "text" | "search" | "textarea" | "" | "email" | "url" => out.extend([Step::Goto(app.to_string()), Step::Mark, Step::Type { target: sel.clone(), text: sample.clone() }, Step::Press("Enter".into()), Step::ExpectChange(format!("typing \"{sample}\" and pressing Enter in {}", hint))]),
            _ => {}
        }
    }
    for b in arr("buttons").iter().filter_map(Value::as_str).filter(|b| !b.is_empty() && scoped.buttons.iter().any(|x| x == b)).take(6) {
        out.extend([Step::Goto(app.to_string()), Step::Mark, Step::Click(format!("text={b}")), Step::NoteChange(format!("clicking \"{b}\""))]);
    }
    out.truncate(COVERAGE_MAX_STEPS);
    out
}
pub const COVERAGE_MAX_STEPS: usize = 48;

/// Which controls a change is ABOUT - and so which ones the generated plan should use (operator 2026-09-30: *"if i asked just for change the dropdown sorting, why tested so many features?"* - it
/// used every control on the page, 53 actions for one dropdown). The ask and the lines the change ADDED decide:
///  1. a control whose own words (its id, name, hint, option labels, button label) appear in the ask;
///  2. else the KIND the ask names ("dropdown" -> the selects, "search box" -> the text boxes, "checkbox"/"toggle", "button");
///  3. else nothing: the QA's own plan covers it, and the rest of the page is not this change's business.
/// Stopping at the first rule that finds something keeps a sort dropdown's test to the sort dropdown. Pure - tested.
#[derive(Debug, Default, PartialEq)]
pub struct Scoped { pub inputs: Vec<String>, pub buttons: Vec<String> }
pub fn scope_controls(probe: &Value, ask: &str, diff: &str) -> Scoped {
    let words = |t: &str| -> Vec<String> { t.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() >= 3).map(String::from).collect() };
    let related = |a: &str, b: &str| { let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) }; a == b || (short.len() >= 4 && long.contains(short)) || (short.len() == 3 && long.starts_with(short)) };   // sorting ~ sort, filters ~ filter, instock ~ stock
    let added: String = diff.lines().filter(|l| l.starts_with('+') && !l.starts_with("+++")).map(|l| &l[1..]).collect::<Vec<_>>().join("\n");
    let (ask_w, add_w) = (words(ask), words(&added));
    let arr = |k: &str| probe.get(k).and_then(Value::as_array).cloned().unwrap_or_default();
    // (selector / label, kind, the control's own words)
    let mut ctl: Vec<(String, &'static str, Vec<String>)> = vec![];
    for i in arr("inputs") {
        let sel = i.get("sel").and_then(Value::as_str).unwrap_or("").to_string(); if sel.is_empty() { continue }
        let kind = match i.get("kind").and_then(Value::as_str).unwrap_or("text") { "select" => "select", "checkbox" | "radio" => "toggle", _ => "text" };
        let mut w = words(&sel.replace('#', " ").replace('.', " ")); w.extend(words(i.get("hint").and_then(Value::as_str).unwrap_or("")));
        for o in i.get("options").and_then(Value::as_array).cloned().unwrap_or_default().iter().filter_map(Value::as_str) { w.extend(words(o)); }
        w.retain(|x| !["input", "select", "type", "text", "placeholder", "name", "any", "all", "product", "products", "item", "items", "object", "objects", "add", "page", "new"].contains(&x.as_str()));
        ctl.push((sel, kind, w));
    }
    const GENERIC: &[&str] = &["input", "select", "type", "text", "placeholder", "name", "any", "all", "product", "products", "item", "items", "object", "objects", "add", "page", "new"];
    for b in arr("buttons").iter().filter_map(Value::as_str).filter(|b| !b.is_empty()) { let mut w = words(b); w.retain(|x| !GENERIC.contains(&x.as_str())); ctl.push((b.to_string(), "button", w)); }
    let pick = |pool: &[String], own_only_added: bool| -> Vec<usize> {
        let _ = own_only_added;
        ctl.iter().enumerate().filter(|(_, (_, _, w))| w.iter().any(|cw| pool.iter().any(|pw| related(cw, pw)))).map(|(i, _)| i).collect()
    };
    let mut hit = pick(&ask_w, false);
    // (no rule that reads the ADDED LINES: a change that adds avatars to the product list adds the product names to the diff, and every category button is "named" by them - 2026-09-30, run 272)
    let _ = &add_w;
    if hit.is_empty() {
        let has = |ks: &[&str]| ask_w.iter().any(|w| ks.contains(&w.as_str()));
        let kinds: Vec<&str> = [(["dropdown", "select", "sort", "sorting", "order", "menu", "picker"].as_slice(), "select"), (["search", "input", "field", "textbox", "box", "query", "type"].as_slice(), "text"),
            (["checkbox", "toggle", "switch", "radio"].as_slice(), "toggle"), (["button", "click", "cta"].as_slice(), "button")].iter().filter(|(ks, _)| has(ks)).map(|(_, k)| *k).collect();
        hit = ctl.iter().enumerate().filter(|(_, (_, k, _))| kinds.contains(k)).map(|(i, _)| i).collect();
    }
    let mut out = Scoped::default();
    for i in hit { let (name, kind, _) = &ctl[i]; if *kind == "button" { out.buttons.push(name.clone()) } else { out.inputs.push(name.clone()) } }
    out
}

/// WHAT IS ACTUALLY ON THE PAGE (operator 2026-09-30): the QA used to write its steps BLIND - it saw the ask and the diff, never the page - so a search box it had to
/// guess a selector for was a step that failed, and a plan that could not test the behaviour settled for one picture. The browser reads the real controls (inputs, buttons,
/// links, headings), with a selector that works for each, and the planner is handed that list. Deterministic; nothing is judged. Bounded to ~2 000 characters.
pub fn probe_page(base: &str) -> Result<String, String> { probe_raw(base).map(|v| elements_block(&v)) }
pub fn probe_raw(base: &str) -> Result<Value, String> {
    if !base.starts_with("file://") && parse_http_url(base).is_none() { return Err(format!("{base:?} is not an address the browser may open")) }
    let exe = antiloki_rs::codebase_helpers_browser_capture::find_browser()?;
    let opts = LaunchOptions::default_builder().path(Some(exe)).headless(true).sandbox(!chrome_needs_no_sandbox()).window_size(Some(VIEWPORT)).idle_browser_timeout(Duration::from_secs(60))
        .args(vec![std::ffi::OsStr::new("--disable-gpu")]).build().map_err(|e| format!("browser options: {e}"))?;
    let browser = Browser::new(opts).map_err(|e| format!("could not launch the browser: {e}"))?;
    let tab = browser.new_tab().map_err(|e| e.to_string())?;
    tab.set_default_timeout(Duration::from_secs(15));
    tab.navigate_to(base).map_err(|e| e.to_string())?; tab.wait_until_navigated().map_err(|e| e.to_string())?;
    std::thread::sleep(Duration::from_millis(500));
    let ro = tab.evaluate(PROBE_JS, false).map_err(|e| e.to_string())?;
    ro.value.and_then(|v| v.as_str().and_then(|t| serde_json::from_str(t).ok())).ok_or_else(|| "the page gave no elements".to_string())
}
const PROBE_JS: &str = r##"(() => {
  const vis = (el) => { if (!el) return false; const cs = getComputedStyle(el); return el.getClientRects().length > 0 && cs.visibility !== "hidden" && cs.display !== "none"; };
  const esc = (x) => String(x).replace(/"/g, '\\"');
  const sel = (el) => {
    const tag = el.tagName.toLowerCase();
    if (el.id) return "#" + el.id;
    if (el.getAttribute("name")) return tag + '[name="' + esc(el.getAttribute("name")) + '"]';
    if (el.getAttribute("placeholder")) return tag + '[placeholder="' + esc(el.getAttribute("placeholder")) + '"]';
    if (el.getAttribute("aria-label")) return tag + '[aria-label="' + esc(el.getAttribute("aria-label")) + '"]';
    return tag + (el.type ? '[type="' + el.type + '"]' : "");
  };
  const take = (list, f, n) => Array.from(list).filter(vis).slice(0, n).map(f);
  return JSON.stringify({
    title: document.title || "",
    headings: take(document.querySelectorAll("h1,h2"), (e) => String(e.innerText).trim().slice(0, 60), 6),
    inputs: take(document.querySelectorAll("input:not([type=hidden]):not([type=submit]):not([type=button]),textarea,select"), (e) => ({ sel: sel(e), type: e.type || e.tagName.toLowerCase(), kind: e.tagName === "SELECT" ? "select" : (e.type || e.tagName.toLowerCase()), hint: e.getAttribute("placeholder") || e.getAttribute("aria-label") || "",
      options: e.tagName === "SELECT" ? Array.from(e.options).map((o) => o.text.trim()).slice(0, 10) : undefined, current: e.tagName === "SELECT" && e.selectedOptions[0] ? e.selectedOptions[0].text.trim() : undefined }), 12),
    sample: (() => { const items = Array.from(document.querySelectorAll("li,article,tr,[class*=card],[class*=product],[class*=item]")).filter(vis).map((e) => String(e.innerText || "").trim()).filter((t) => t.length > 3 && t.length < 160);
      const words = items.flatMap((t) => t.split(/[^A-Za-z]+/)).filter((w) => w.length >= 4); return (words[0] || "").toLowerCase(); })(),
    buttons: take(document.querySelectorAll("button,[role=button],input[type=submit],input[type=button]"), (e) => String(e.innerText || e.value || e.getAttribute("aria-label") || "").trim().slice(0, 40), 14),
    links: take(document.querySelectorAll("a[href]"), (e) => String(e.innerText || "").trim().slice(0, 40) + " -> " + e.getAttribute("href"), 10),
    lists: Array.from(document.querySelectorAll("ul,ol,table,[class*=result],[class*=list]")).filter(vis).slice(0, 6).map((e) => sel(e) + " (" + e.children.length + " items)"),
  });
})()"##;
/// the probe's answer as the lines a planner reads. Pure - tested.
pub fn elements_block(v: &Value) -> String {
    let arr = |k: &str| v.get(k).and_then(Value::as_array).cloned().unwrap_or_default();
    let mut s = String::new();
    if let Some(t) = v.get("title").and_then(Value::as_str).filter(|t| !t.is_empty()) { s.push_str(&format!("page title: {t}\n")); }
    let hs: Vec<String> = arr("headings").iter().filter_map(|x| x.as_str().map(String::from)).filter(|x| !x.is_empty()).collect();
    if !hs.is_empty() { s.push_str(&format!("headings: {}\n", hs.join(" | "))); }
    for i in arr("inputs") { s.push_str(&format!("input: {} ({}{}{})\n", i.get("sel").and_then(Value::as_str).unwrap_or("?"), i.get("type").and_then(Value::as_str).unwrap_or(""),
        i.get("hint").and_then(Value::as_str).filter(|h| !h.is_empty()).map(|h| format!(", hint \"{h}\"")).unwrap_or_default(),
        i.get("options").and_then(Value::as_array).map(|o| format!(", options: {}", o.iter().filter_map(Value::as_str).map(|x| format!("\"{x}\"")).collect::<Vec<_>>().join(" | "))).unwrap_or_default())); }
    let bs: Vec<String> = arr("buttons").iter().filter_map(|x| x.as_str().map(String::from)).filter(|x| !x.is_empty()).collect();
    if !bs.is_empty() { s.push_str(&format!("buttons (click with text=<label>): {}\n", bs.join(" | "))); }
    let ls: Vec<String> = arr("links").iter().filter_map(|x| x.as_str().map(String::from)).collect();
    if !ls.is_empty() { s.push_str(&format!("links: {}\n", ls.join(" | "))); }
    let li: Vec<String> = arr("lists").iter().filter_map(|x| x.as_str().map(String::from)).collect();
    if !li.is_empty() { s.push_str(&format!("lists/tables: {}\n", li.join(" | "))); }
    s.chars().take(2000).collect()
}

// ---- THE QA, LIVE (B5) --------------------------------------------------------------------------------------------------------------------------

/// Where a run's play-by-play goes. `sink` gets JSON events (`step`, `frame`, `step-done`, `shot`, `done`); `pace` says how long to pause (ms) around each action - asked before EVERY step, so it can be 0 until somebody starts watching - that exists ONLY so a
/// person can follow it - the captures the judge receives are taken exactly as without it; `film_dir` keeps every ~140 ms frame (capped) and the timeline for a replay.
#[derive(Clone)]
pub struct Live { pub sink: Arc<dyn Fn(Value) + Send + Sync>, pub pace: Arc<dyn Fn() -> u64 + Send + Sync>, pub film_dir: Option<PathBuf>,
    /// set by the operator's STOP: the plan ends at the next step (operator 2026-09-30: "a button to pause or stop an execution, so we dont be locked on a wrong execution")
    pub cancel: Arc<std::sync::atomic::AtomicBool> }

/// (kind, sentence) for a step, in words a person can follow
pub fn describe(st: &Step) -> (&'static str, String) {
    match st {
        Step::Goto(u) => ("goto", format!("opening {u}")),
        Step::Click(t) => ("click", format!("clicking {}", pretty_target(t))),
        Step::Hover(t) => ("hover", format!("hovering over {}", pretty_target(t))),
        Step::Type { target, text } => ("type", format!("typing \"{text}\" into {}", pretty_target(target))),
        Step::Press(k) => ("press", format!("pressing {k}")),
        Step::WaitFor(t) => ("wait", format!("waiting for {}", pretty_target(t))),
        Step::Wait(ms) => ("wait", format!("waiting {ms} ms")),
        Step::Scroll(y) => ("scroll", format!("scrolling {}", if *y >= 0 { "down" } else { "up" })),
        Step::Shot(n) => ("shot", format!("taking the picture \"{n}\"")),
        Step::Expect(e) => ("expect", format!("checking that {}", e.words())),
        Step::Select { target, option } => ("select", format!("choosing \"{option}\" in {}", pretty_target(target))),
        Step::Clear(t) => ("type", format!("clearing {}", pretty_target(t))),
        Step::Mark => ("mark", "noting how the page looks".to_string()),
        Step::ExpectChange(what) => ("expect", format!("checking that {what} changed the page")),
        Step::NoteChange(what) => ("mark", format!("noting whether {what} changed the page")),
        Step::DesignLint => ("mark", "measuring the page's design: contrast, spacing, type, alignment, targets".to_string()),
    }
}
fn pretty_target(t: &str) -> String { t.strip_prefix("text=").map(|x| format!("\"{x}\"")).unwrap_or_else(|| t.to_string()) }

/// where the step's target sits in the viewport, so the viewer can move a cursor there: [x, y, w, h] (scrolls it into view first)
fn rect_of(tab: &Tab, target: &str) -> Option<[f64; 4]> {
    let js = format!("(() => {{ const el = {}; if (!el) return null; el.scrollIntoView({{block:'center'}}); const r = el.getBoundingClientRect(); return JSON.stringify([r.x, r.y, r.width, r.height]); }})()", finder(target));
    let ro = tab.evaluate(&js, false).ok()?;
    let a: Vec<f64> = serde_json::from_str::<Vec<f64>>(ro.value?.as_str()?).ok()?;
    if a.len() == 4 { Some([a[0], a[1], a[2], a[3]]) } else { None }
}

/// Execute a plan against a running app. `base` completes relative `goto` paths (e.g. `http://localhost:5173`). Never panics; a step that cannot be done is
/// logged and counted, and the run goes on to the next one.
pub fn run_plan(base: &str, steps: &[Step], out_dir: &Path) -> Result<Evidence, String> { run_plan_live(base, steps, out_dir, None) }

/// `run_plan` with an optional live viewer (see `Live`). The evidence it returns is identical with or without one.
pub fn run_plan_live(base: &str, steps: &[Step], out_dir: &Path, live: Option<Live>) -> Result<Evidence, String> { run_plan_cfg(base, steps, out_dir, live, false) }
/// THE FILM'S PACE SHRINKS WITH THE PLAN (operator 2026-09-30: a 53-step check took minutes). The pause around each step is for a person watching; a long plan spends it 53 times over, so past 16 steps it is
/// capped at 200 ms and past 40 it is 100 ms. The pictures the judge gets are taken exactly the same. Pure - tested.
pub fn capped_pace(pace_ms: u64, n_steps: usize) -> u64 { if n_steps > 40 { pace_ms.min(100) } else if n_steps > 16 { pace_ms.min(200) } else { pace_ms } }
/// Is the change ABOUT motion - so the page's animations must be left running and can be checked? Pure - tested.
pub fn is_motion_change(ask: &str, diff: &str) -> bool {
    let a = ask.to_lowercase();
    if ["animat", "transition", "motion", "keyframe", "bounce", "wiggle", "pulse", "spin", "float", "fade", "slide", "hover effect", "hover anim", "micro-interaction", "micro interaction"].iter().any(|k| a.contains(k)) { return true }
    // the ask says nothing about it, the added lines introduce animation anyway
    diff.lines().filter(|l| l.starts_with('+') && !l.starts_with("+++")).any(|l| { let l = l.to_lowercase(); l.contains("@keyframes") || l.contains("animation:") || l.contains("animation-name") || l.contains("transition:") || l.contains("framer-motion") })
}
/// THE PLAN THAT RUNS = the QA's steps, ONE design-lint step, then the generated coverage steps (PLAN-THE-DESIGN D1): the page is measured where the QA's own steps left it (the feature in the state the ask is about),
/// before the coverage plan reloads it. A QA plan that already asked for the lint keeps its own and gets no second one. The recorded plan (a replay checks behaviour) is built from the steps WITHOUT this. Pure - tested.
pub fn with_design_lint(qa: &[Step], coverage: &[Step]) -> Vec<Step> {
    let mut out: Vec<Step> = qa.to_vec();
    if !qa.iter().chain(coverage.iter()).any(|s| *s == Step::DesignLint) { out.push(Step::DesignLint); }
    out.extend(coverage.iter().cloned());
    out
}
/// `run_plan_live`, with the page's own animations left alone when `motion` (see `is_motion_change`).
pub fn run_plan_cfg(base: &str, steps: &[Step], out_dir: &Path, live: Option<Live>, motion: bool) -> Result<Evidence, String> { run_plan_in(base, None, steps, out_dir, live, motion) }
/// `run_plan_cfg` with the BOX the plan belongs to: the only place a `file://` step may open (F-08). With none named, a static page's own folder is the box.
pub fn run_plan_in(base: &str, box_root: Option<&Path>, steps: &[Step], out_dir: &Path, live: Option<Live>, motion: bool) -> Result<Evidence, String> {
    let page_dir = file_url_path(base).and_then(|p| p.parent().map(Path::to_path_buf));
    let box_root: Option<PathBuf> = box_root.map(Path::to_path_buf).or(page_dir);
    // the plan is refused whole, before a browser exists, when any step goes somewhere it may not: a step the browser never took cannot leak anything
    for (i, st) in steps.iter().enumerate() {
        if let Step::Goto(u) = st { nav_allowed(u, base.trim_end_matches('/'), box_root.as_deref()).map_err(|e| format!("step {}: {e}", i + 1))?; }
    }
    let exe = antiloki_rs::codebase_helpers_browser_capture::find_browser()?;
    std::fs::create_dir_all(out_dir).map_err(|e| format!("evidence folder: {e}"))?;
    let opts = LaunchOptions::default_builder().path(Some(exe)).headless(true).sandbox(!chrome_needs_no_sandbox()).window_size(Some(VIEWPORT))
        .idle_browser_timeout(Duration::from_secs(180))
        .args(vec![std::ffi::OsStr::new("--hide-scrollbars"), std::ffi::OsStr::new("--disable-gpu"), std::ffi::OsStr::new("--force-device-scale-factor=1"),
                   std::ffi::OsStr::new("--font-render-hinting=none"), std::ffi::OsStr::new("--disable-lcd-text")])
        .build().map_err(|e| format!("browser options: {e}"))?;
    let browser = Browser::new(opts).map_err(|e| format!("could not launch the browser: {e}"))?;
    let tab: Arc<Tab> = browser.new_tab().map_err(|e| format!("could not open a tab: {e}"))?;
    tab.set_default_timeout(Duration::from_secs(20));
    let mut ev = Evidence::default();
    let base = base.trim_end_matches('/');
    // THE PLAY-BY-PLAY: a frame thread (a JPEG of the tab every ~140 ms) and the step events; none of it touches what the judge is given
    let t0 = std::time::Instant::now();
    let timeline: Arc<std::sync::Mutex<Vec<Value>>> = Arc::new(std::sync::Mutex::new(vec![]));
    let emit = {
        let (live, timeline) = (live.clone(), timeline.clone());
        move |mut v: Value| { if let Some(l) = &live { v["t"] = json!(t0.elapsed().as_millis() as u64); if v["type"] != "frame" { timeline.lock().unwrap_or_else(|p| p.into_inner()).push(v.clone()); } (l.sink)(v); } }
    };
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // ONE screenshot call in flight at a time: two overlapping CDP captures on an idle page wait on each other for a repaint that never comes
    let shot_lock: Arc<std::sync::Mutex<()>> = Arc::new(std::sync::Mutex::new(()));
    // no frames while a page is navigating (and none before the first page has loaded): a capture taken mid-navigation can hang Chrome for minutes on a page with nothing on it
    let navigating = Arc::new(std::sync::atomic::AtomicBool::new(true));
    // ONE way to take a frame (the frame thread and the end of every step both use it): a JPEG of the tab, saved to the film (capped) and sent to the viewer
    let kept = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let grab: Arc<dyn Fn(bool) + Send + Sync> = {
        let (tab, shot_lock, emit, film, timeline, kept) = (tab.clone(), shot_lock.clone(), emit.clone(), live.as_ref().and_then(|l| l.film_dir.clone()), timeline.clone(), kept.clone());
        if let Some(d) = &film { let _ = std::fs::create_dir_all(d); }
        Arc::new(move |must: bool| {
            use base64::Engine as _;
            let jpg = match if must { Ok(shot_lock.lock().unwrap_or_else(|p| p.into_inner())) } else { shot_lock.try_lock().map_err(|_| ()) } { Ok(_g) => tab.capture_screenshot(CaptureScreenshotFormatOption::Jpeg, Some(55), None, true), Err(_) => return };
            if let Ok(jpg) = jpg {
                if let Some(d) = &film { let i = kept.load(std::sync::atomic::Ordering::Relaxed); if i < 240 { let f = format!("{:04}.jpg", i); if std::fs::write(d.join(&f), &jpg).is_ok() { kept.store(i + 1, std::sync::atomic::Ordering::Relaxed); timeline.lock().unwrap_or_else(|p| p.into_inner()).push(json!({ "type": "film", "t": t0.elapsed().as_millis() as u64, "file": f })); } } }
                emit(json!({ "type": "frame", "jpeg": base64::engine::general_purpose::STANDARD.encode(&jpg), "w": VIEWPORT.0, "h": VIEWPORT.1 }));
            }
        })
    };
    let frames = live.as_ref().map(|_| {
        let (stop, navigating, grab) = (stop.clone(), navigating.clone(), grab.clone());
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                if navigating.load(std::sync::atomic::Ordering::Relaxed) { std::thread::sleep(Duration::from_millis(60)); continue }
                grab(false);
                std::thread::sleep(Duration::from_millis(140));
            }
        })
    });
    let nap = |ms: u64| if ms > 0 { std::thread::sleep(Duration::from_millis(ms)) };
    let mut marked: Option<u64> = None;
    let fingerprint = |tab: &Tab| -> u64 {
        use std::hash::{Hash, Hasher};
        let ro = tab.evaluate(FINGERPRINT_JS, false).ok().and_then(|r| r.value).and_then(|v| v.as_str().map(String::from)).unwrap_or_default();
        let mut h = std::collections::hash_map::DefaultHasher::new(); ro.hash(&mut h); h.finish()
    };
    for (i, st) in steps.iter().enumerate() {
        let n = i + 1;
        if live.as_ref().map_or(false, |l| l.cancel.load(std::sync::atomic::Ordering::Relaxed)) { ev.log.push(format!("{n}. stopped by the operator - the rest of the plan was not run")); break }
        if live.is_some() {
            let (kind, text) = describe(st);
            let rect = match st { Step::Click(t) | Step::Hover(t) | Step::WaitFor(t) | Step::Clear(t) => rect_of(&tab, t), Step::Type { target, .. } | Step::Select { target, .. } => rect_of(&tab, target), _ => None };
            emit(json!({ "type": "step", "n": n, "of": steps.len(), "kind": kind, "text": text, "rect": rect, "viewport": [VIEWPORT.0, VIEWPORT.1] }));
            nap(live.as_ref().map(|l| capped_pace((l.pace)(), steps.len())).unwrap_or(0));
        }
        let done: Result<String, String> = (|| match st {
            Step::Goto(u) => {
                navigating.store(true, std::sync::atomic::Ordering::Relaxed);
                let full = if u.starts_with('/') { format!("{base}{u}") } else { u.clone() };
                tab.navigate_to(&full).map_err(|e| e.to_string())?; tab.wait_until_navigated().map_err(|e| e.to_string())?;
                let _ = tab.evaluate(if motion { HOOK_JS_MOTION } else { HOOK_JS }, false); settle(&tab); std::thread::sleep(Duration::from_millis(400));
                navigating.store(false, std::sync::atomic::Ordering::Relaxed);
                Ok(format!("opened {full}"))
            }
            Step::Click(t) => {
                if !eval_bool(&tab, &format!("(() => {{ const el = {}; if (!el) return false; el.scrollIntoView({{block:'center'}}); el.click(); return true; }})()", finder(t)))? { return Err(format!("click failed: nothing matches {t:?}")) }
                std::thread::sleep(Duration::from_millis(300)); Ok(format!("clicked {t}"))
            }
            Step::Hover(t) => {
                if !eval_bool(&tab, &format!("(() => {{ const el = {}; if (!el) return false; for (const ty of ['mouseover','mouseenter','mousemove']) el.dispatchEvent(new MouseEvent(ty, {{bubbles:true}})); return true; }})()", finder(t)))? { return Err(format!("hover failed: nothing matches {t:?}")) }
                std::thread::sleep(Duration::from_millis(200)); Ok(format!("hovered {t}"))
            }
            Step::Type { target, text } => {
                if !eval_bool(&tab, &format!("(() => {{ const el = {}; if (!el) return false; el.focus(); return true; }})()", finder(target)))? { return Err(format!("type failed: nothing matches {target:?}")) }
                tab.type_str(text).map_err(|e| e.to_string())?; Ok(format!("typed {} character(s) into {target}", text.chars().count()))
            }
            Step::Press(k) => { tab.press_key(k).map_err(|e| e.to_string())?; std::thread::sleep(Duration::from_millis(200)); Ok(format!("pressed {k}")) }
            Step::WaitFor(t) => {
                for _ in 0..(MAX_WAIT_MS / 100) { if eval_bool(&tab, &format!("!!({})", finder(t)))? { return Ok(format!("{t} appeared")) } std::thread::sleep(Duration::from_millis(100)); }
                Err(format!("wait_for timed out: {t:?} never appeared"))
            }
            Step::Wait(ms) => { std::thread::sleep(Duration::from_millis(*ms)); Ok(format!("waited {ms} ms")) }
            Step::Scroll(y) => { tab.evaluate(&format!("window.scrollBy(0,{y})"), false).map_err(|e| e.to_string())?; std::thread::sleep(Duration::from_millis(200)); Ok(format!("scrolled {y}")) }
            Step::Mark => { marked = Some(fingerprint(&tab)); Ok("noted how the page looks".to_string()) }
            Step::ExpectChange(what) => {
                std::thread::sleep(Duration::from_millis(250));
                let now = fingerprint(&tab);
                let changed = marked.map(|m| m != now).unwrap_or(true);
                ev.checks.push((format!("{what} changes the page"), changed, if changed { "the page changed".into() } else { "the page looked exactly the same afterwards".into() }));
                if changed { Ok(format!("{what}: the page changed")) } else { Err(format!("check FAILED: {what} changed nothing - the page looked exactly the same afterwards")) }
            }
            Step::DesignLint => {
                // optional by nature: a page that cannot be measured is said so in the log, it is not a failed step (that count tells the judge "a control cannot be clicked")
                match crate::design_lint::capture(&tab) {
                    Err(e) => Ok(format!("design lint unavailable: {e}")),
                    Ok(snap) if snap.blank || snap.elements < 2.0 => Ok("design lint skipped: the page is blank".to_string()),
                    Ok(snap) => { let l = crate::design_lint::lint(&snap); let line = l.one_line(); ev.design = Some(l); Ok(line) }
                }
            }
            Step::NoteChange(what) => {
                std::thread::sleep(Duration::from_millis(250));
                let changed = marked.map(|m| m != fingerprint(&tab)).unwrap_or(true);
                Ok(format!("{what}: {}", if changed { "the page changed" } else { "no visible change (informational)" }))
            }
            Step::Select { target, option } => {
                let js = format!("(() => {{ const el = {}; if (!el || el.tagName !== 'SELECT') return false; const want = {}.toLowerCase(); const o = Array.from(el.options).find((x) => x.text.trim().toLowerCase() === want || x.value.toLowerCase() === want); if (!o) return false; \
                    const set = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value').set; set.call(el, o.value); el.dispatchEvent(new Event('input', {{ bubbles: true }})); el.dispatchEvent(new Event('change', {{ bubbles: true }})); return true; }})()", finder(target), serde_json::to_string(option).unwrap_or_default());
                if !eval_bool(&tab, &js)? { return Err(format!("select failed: no dropdown {target:?} with an option {option:?}")) }
                std::thread::sleep(Duration::from_millis(250)); Ok(format!("chose {option:?} in {target}"))
            }
            Step::Clear(t) => {
                let js = format!("(() => {{ const el = {}; if (!el) return false; const proto = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype; Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, ''); el.dispatchEvent(new Event('input', {{ bubbles: true }})); return true; }})()", finder(t));
                if !eval_bool(&tab, &js)? { return Err(format!("clear failed: nothing matches {t:?}")) }
                Ok(format!("cleared {t}"))
            }
            Step::Expect(e) => {
                let ro = tab.evaluate(&expect_js(e), false).map_err(|x| x.to_string())?;
                let v: Value = ro.value.and_then(|v| v.as_str().and_then(|t| serde_json::from_str(t).ok())).unwrap_or(Value::Null);
                let (ok, got) = (v.get("ok").and_then(Value::as_bool).unwrap_or(false), v.get("got").and_then(Value::as_str).unwrap_or("?").to_string());
                ev.checks.push((e.words(), ok, got.clone()));
                if ok { Ok(format!("check passed: {}", e.words())) } else { Err(format!("check FAILED: expected {} - the page had: {got}", e.words())) }
            }
            Step::Shot(name) => {
                settle(&tab);
                let _g = shot_lock.lock().unwrap_or_else(|p| p.into_inner());
                let png = tab.capture_screenshot(CaptureScreenshotFormatOption::Png, None, None, true).map_err(|e| e.to_string())?;
                let path = out_dir.join(format!("{:02}-{name}.png", ev.shots.len() + 1));
                std::fs::write(&path, &png).map_err(|e| e.to_string())?;
                ev.shots.push((name.clone(), path.clone()));
                Ok(format!("shot {name} ({} bytes)", png.len()))
            }
        })();
        match done {
            Ok(m) => { emit(json!({ "type": "step-done", "n": n, "ok": true, "message": m })); ev.log.push(format!("{n}. {m}")) }
            Err(e) => { emit(json!({ "type": "step-done", "n": n, "ok": false, "message": e })); ev.failed_steps += 1; ev.log.push(format!("{n}. FAILED - {e}")) }
        }
        if matches!(st, Step::Goto(_)) { navigating.store(false, std::sync::atomic::Ordering::Relaxed); }   // even when the page failed to load: the viewer still sees what is there
        if let (Step::Shot(name), Some((_, p))) = (st, ev.shots.last()) { emit(json!({ "type": "shot", "name": name, "path": p.display().to_string() })); }
        if live.is_some() && !navigating.load(std::sync::atomic::Ordering::Relaxed) { grab(true); }   // every step leaves a frame, however quick it was
        nap(live.as_ref().map(|l| capped_pace((l.pace)(), steps.len()) / 2).unwrap_or(0));
    }
    // the frame thread stops FIRST: `done` must be the last thing a viewer ever receives
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(h) = frames { let _ = h.join(); }
    emit(json!({ "type": "done", "shots": ev.shots.len(), "failed_steps": ev.failed_steps }));
    if let Some(l) = &live { if let Some(d) = &l.film_dir { let _ = std::fs::write(d.join("timeline.json"), Value::Array(timeline.lock().unwrap_or_else(|p| p.into_inner()).clone()).to_string()); } }
    // (an array does not come back as a value over CDP - it travels as a JSON string)
    if let Ok(ro) = tab.evaluate("JSON.stringify(window.__qa ? window.__qa.errors : [])", false) {
        if let Some(Value::Array(a)) = ro.value.and_then(|v| v.as_str().and_then(|t| serde_json::from_str::<Value>(t).ok())) { ev.console_errors = a.iter().filter_map(|x| x.as_str().map(String::from)).collect(); }
    }
    if let Ok(ro) = tab.evaluate("(document.body ? document.body.innerText : '').slice(0, 4000)", false) { ev.text = ro.value.and_then(|v| v.as_str().map(String::from)).unwrap_or_default(); }
    Ok(ev)
}

/// The evidence as the block a QA prompt carries: what was done, what failed, the errors the page threw, and the images to open (by path, in the box).
pub fn prompt_block(ev: &Evidence) -> String { prompt_block_with(ev, true) }
/// `with_images` false = the pictures are not handed to the judge (the deterministic checks already decided the behaviour, and a model looking at pixels would only be paid to agree).
pub fn prompt_block_with(ev: &Evidence, with_images: bool) -> String {
    let mut s = String::from(if with_images { "VISUAL EVIDENCE (captured by deterministic tools - no model chose these pixels; open each image with your file tool and LOOK at it):\n" } else { "BROWSER EVIDENCE (captured by deterministic tools - no model chose or judged these):\n" });
    if !ev.checks.is_empty() {
        s.push_str(&format!("  DETERMINISTIC CHECKS run by the browser on the real page - {} passed, {} failed. A failed check is a FACT about the page, not an opinion:\n", ev.checks.len() - ev.checks_failed(), ev.checks_failed()));
        for (what, ok, got) in &ev.checks { s.push_str(&format!("    {} {what}{}\n", if *ok { "PASS" } else { "FAIL" }, if *ok { String::new() } else { format!(" - the page had: {got}") })); }
    }
    if let Some(d) = &ev.design { s.push_str(&d.prompt_block()); }
    for l in &ev.log { s.push_str(&format!("  {l}\n")); }
    if with_images { for (name, p) in &ev.shots { s.push_str(&format!("  IMAGE {name}: {}\n", p.display())); } }
    else if !ev.shots.is_empty() { s.push_str("  (no images attached: the checks above settle the behaviour; the pictures are kept in the QA film)\n"); }
    if !ev.console_errors.is_empty() { s.push_str(&format!("  CONSOLE ERRORS ({}):\n", ev.console_errors.len())); for e in ev.console_errors.iter().take(8) { s.push_str(&format!("    - {e}\n")); } }
    if ev.failed_steps > 0 { s.push_str(&format!("  {} step(s) could not be done - a control that cannot be clicked is itself a finding.\n", ev.failed_steps)); }
    s
}

/// DOES THE JUDGE NEED TO LOOK? (operator 2026-09-30.) Yes when nothing deterministic decided the behaviour (a plan with no check is only pictures), when a check FAILED (the
/// picture shows why), or when the change is to how things look (styles, images, vector art). No when the browser's own checks all passed on a behaviour change: the verdict is
/// already a fact, and pixels would cost tokens to agree with it. Pure - tested.
pub fn needs_eyes(ev: &Evidence, files: &[String]) -> bool {
    if ev.checks.is_empty() || ev.checks_failed() > 0 { return true }
    files.iter().any(|f| { let f = f.to_ascii_lowercase(); [".css", ".scss", ".sass", ".less", ".svg", ".png", ".jpg", ".jpeg", ".webp", ".gif", ".ico"].iter().any(|x| f.ends_with(x)) })
}
/// What one image costs a judge that reads it, in input tokens: width x height / 750, the rule the vendors publish for a picture of this size (a 1280x800 page = ~1 370).
/// Read from the PNG header; 0 for a file that is not one. Pure over the file - tested.
pub fn image_tokens(path: &Path) -> u64 {
    let Ok(b) = std::fs::read(path) else { return 0 };
    if b.len() < 24 || &b[..8] != b"\x89PNG\r\n\x1a\n" { return 0 }
    let (w, h) = (u32::from_be_bytes([b[16], b[17], b[18], b[19]]) as u64, u32::from_be_bytes([b[20], b[21], b[22], b[23]]) as u64);
    (w * h).div_ceil(750)
}

pub fn summary(ev: &Evidence) -> Value {
    let mut v = json!({ "shots": ev.shots.iter().map(|(n, p)| json!({ "name": n, "path": p })).collect::<Vec<_>>(), "failed_steps": ev.failed_steps, "console_errors": ev.console_errors, "log": ev.log });
    if let Some(d) = &ev.design { v["design_lint"] = d.to_json(); }
    v
}


/// `http://localhost:5173/x` -> `localhost:5173` (a default port when the URL names none). Pure - tested.
pub fn host_port(url: &str) -> Option<String> {
    let rest = url.strip_prefix("http://").map(|r| (r, 80)).or_else(|| url.strip_prefix("https://").map(|r| (r, 443)))?;
    let authority = rest.0.split(['/', '?', '#']).next()?;
    if authority.is_empty() { return None }
    Some(if authority.contains(':') { authority.to_string() } else { format!("{authority}:{}", rest.1) })
}

// ---- the box's own dev server ------------------------------------------------------------------------------------------------------------------

/// A dev server this module booted for one capture, and ends when dropped (its whole process group: `npm` + vite + its helpers). The operator's own previews
/// are never touched - `Goals::visual_evidence` prefers a single live one and boots this only when there is none.
#[derive(Debug)]
pub struct DevServer { pid: u32, pub url: String, pub log: Arc<std::sync::Mutex<String>>, child: Option<std::process::Child> }

impl DevServer {
    /// Boot `npm run dev` (or `start`) in `cwd` and wait for it to announce a URL that accepts a connection. Errors say why there is nothing to look at.
    pub fn start(cwd: &str, ready_within: Duration) -> Result<DevServer, String> {
        let pkg_text = std::fs::read_to_string(Path::new(cwd).join("package.json")).map_err(|_| "no package.json in the box".to_string())?;
        let pkg: Value = serde_json::from_str(&pkg_text).map_err(|e| format!("package.json: {e}"))?;
        let script = ["dev", "start"].into_iter().find(|s| pkg["scripts"][*s].is_string()).ok_or("package.json has no dev or start script")?;
        if !Path::new(cwd).join("node_modules").exists() { return Err("dependencies are not installed in the box".into()) }
        let mut child = crate::preview::spawn_shell(&format!("npm run {script}"), Some(cwd), &[("BROWSER".to_string(), "none".to_string())]).ok_or("could not start the dev server")?;
        let pid = child.id();
        let log = Arc::new(std::sync::Mutex::new(String::new()));
        for (i, pipe) in [child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>), child.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>)].into_iter().enumerate() {
            let Some(mut pipe) = pipe else { continue };
            let log = log.clone();
            std::thread::spawn(move || { let _ = i; let mut buf = [0u8; 4096]; while let Ok(n) = pipe.read(&mut buf) { if n == 0 { break } let mut l = log.lock().unwrap_or_else(|p| p.into_inner()); l.push_str(&String::from_utf8_lossy(&buf[..n])); if l.len() > 20_000 { let cut = l.len() - 20_000; let at = l.char_indices().map(|(i, _)| i).find(|&i| i >= cut).unwrap_or(0); *l = l[at..].to_string(); } } });
        }
        let started = std::time::Instant::now();
        let mut me = DevServer { pid, url: String::new(), log: log.clone(), child: Some(child) };   // dropped (killed, reaped) on every early return below
        loop {
            if let Some(url) = crate::preview::sniff_url(&log.lock().unwrap_or_else(|p| p.into_inner())) {
                if let Some(addr) = host_port(&url) {
                    use std::net::ToSocketAddrs;
                    if addr.to_socket_addrs().ok().and_then(|mut a| a.next()).map_or(false, |a| std::net::TcpStream::connect_timeout(&a, Duration::from_millis(300)).is_ok()) { me.url = url; std::thread::sleep(Duration::from_millis(500)); return Ok(me) }
                }
            }
            if let Some(Ok(Some(code))) = me.child.as_mut().map(|c| c.try_wait()) { let tail: String = log.lock().unwrap_or_else(|p| p.into_inner()).chars().rev().take(300).collect::<String>().chars().rev().collect(); me.pid = 0; return Err(format!("the dev server exited ({code}) before it was ready: {}", tail.trim())) }
            if started.elapsed() > ready_within { return Err(format!("the dev server did not announce a reachable URL within {} s", ready_within.as_secs())) }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

impl Drop for DevServer {
    fn drop(&mut self) {
        if self.pid == 0 { if let Some(mut c) = self.child.take() { let _ = c.wait(); } return }
        // the shell's whole process group (same as preview.rs `stop`)
        #[cfg(unix)]
        { let _ = crate::proc::cmd("kill").args(["--", &format!("-{}", self.pid)]).status(); let _ = crate::proc::cmd("kill").arg(self.pid.to_string()).status(); }
        #[cfg(windows)]
        { let _ = crate::proc::cmd("taskkill").args(["/PID", &self.pid.to_string(), "/T", "/F"]).status(); }
        if let Some(mut c) = self.child.take() { let _ = c.wait(); }   // reaped: a killed child is a zombie until somebody waits
    }
}

// ---- BEFORE and AFTER ---------------------------------------------------------------------------------------------------------------------------

/// Put the tree a run started from (a git tree object, `RunBase.tree`) on disk in `dest`, with the box's dependencies linked in, so the SAME plan can be
/// run against the version before the change. Uses a throwaway index, never the box's own.
pub fn materialize_base(cwd: &str, tree: &str, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("base folder: {e}"))?;
    let idx = std::env::temp_dir().join(format!("antiloki-qa-base-index-{}-{}", std::process::id(), tree.chars().take(12).collect::<String>()));
    let git = |args: &[&str]| -> Result<(), String> {
        let o = crate::proc::cmd("git").args(args).current_dir(cwd).env("GIT_INDEX_FILE", &idx).output().map_err(|e| e.to_string())?;
        if o.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().to_string()) }
    };
    let prefix = format!("--prefix={}/", dest.display());
    let r = git(&["read-tree", tree]).and_then(|_| git(&["checkout-index", "-a", "-f", &prefix]));
    let _ = std::fs::remove_file(&idx);
    r?;
    #[cfg(unix)]
    { let nm = Path::new(cwd).join("node_modules"); if nm.exists() && !dest.join("node_modules").exists() { let _ = std::os::unix::fs::symlink(&nm, dest.join("node_modules")); } }
    Ok(())
}

/// the same plan against another app: every goto that pointed at `from` points at `to`
pub fn retarget(steps: &[Step], from: &str, to: &str) -> Vec<Step> {
    steps.iter().map(|s| match s { Step::Goto(u) if u.starts_with(from) => Step::Goto(format!("{to}{}", &u[from.len()..])), o => o.clone() }).collect()
}

/// per screenshot name: does the picture AFTER the change equal the picture BEFORE it? (byte-identical PNGs = the same pixels; the runs are deterministic)
pub fn compare(after: &Evidence, before: &Evidence) -> Vec<(String, bool)> {
    after.shots.iter().filter_map(|(n, p)| {
        let (_, bp) = before.shots.iter().find(|(bn, _)| bn == n)?;
        Some((n.clone(), std::fs::read(p).ok()? == std::fs::read(bp).ok()?))
    }).collect()
}

/// the block that says what the picture looked like BEFORE, so the judge compares instead of guessing
pub fn base_block(before: &Evidence, cmp: &[(String, bool)]) -> String {
    let mut s = String::from("THE SAME STEPS ON THE VERSION BEFORE THE CHANGE (same tools, same viewport):\n");
    for (name, p) in &before.shots { s.push_str(&format!("  IMAGE before-{name}: {}\n", p.display())); }
    for (name, same) in cmp { s.push_str(&format!("  {name}: {}\n", if *same { "IDENTICAL to before - the change did not alter what this looks like" } else { "DIFFERS from before" })); }
    if before.failed_steps > 0 { s.push_str(&format!("  ({} step(s) could not be done on the old version - a control that did not exist yet.)\n", before.failed_steps)); }
    s
}

// ---- the hub the viewers join -------------------------------------------------------------------------------------------------------------------

/// One run's play-by-play as a late viewer needs it: the step timeline so far, the newest frame, whether it is over, and a channel for what comes next.
pub struct LiveRun { pub cancel: Arc<std::sync::atomic::AtomicBool>, pub timeline: Vec<Value>, pub last_frame: Option<Value>, pub done: bool, pub tx: tokio::sync::broadcast::Sender<Value> }
static HUB: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeMap<i64, Arc<std::sync::Mutex<LiveRun>>>>> = std::sync::OnceLock::new();
fn hub() -> &'static std::sync::Mutex<std::collections::BTreeMap<i64, Arc<std::sync::Mutex<LiveRun>>>> { HUB.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new())) }

/// Open (or reopen) a run's live channel and get the `Live` to hand to `run_plan_live`. The pace is 0 until at least one viewer is subscribed: nobody watching, nobody waiting.
pub fn hub_open(run_id: i64, film_dir: Option<PathBuf>, watched_pace_ms: u64) -> Live {
    let (tx, _) = tokio::sync::broadcast::channel(64);
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let run = Arc::new(std::sync::Mutex::new(LiveRun { cancel: cancel.clone(), timeline: vec![], last_frame: None, done: false, tx }));
    { let mut h = hub().lock().unwrap_or_else(|p| p.into_inner()); h.insert(run_id, run.clone()); while h.len() > 20 { let k = *h.keys().next().unwrap(); h.remove(&k); } }
    let (r1, r2) = (run.clone(), run.clone());
    Live {
        sink: Arc::new(move |v: Value| {
            let mut r = r1.lock().unwrap_or_else(|p| p.into_inner());
            if v["type"] == "frame" { r.last_frame = Some(v.clone()); } else { r.timeline.push(v.clone()); if r.timeline.len() > 600 { r.timeline.remove(0); } }
            if v["type"] == "done" { r.done = true; }
            let _ = r.tx.send(v);
        }),
        pace: Arc::new(move || if r2.lock().unwrap_or_else(|p| p.into_inner()).tx.receiver_count() > 0 { watched_pace_ms } else { 0 }),
        film_dir,
        cancel,
    }
}

// ---- STOP (operator 2026-09-30) ---------------------------------------------------------------------------------------------------------------
/// The operator stopped this run: the QA film or server test in flight ends at its next step. (Which runs are stopped is the `Goals`' own record; this only flips the flag on a film that exists.)
pub fn stop_run(run_id: i64) {
    if let Some(r) = hub().lock().unwrap_or_else(|p| p.into_inner()).get(&run_id) { r.lock().unwrap_or_else(|p| p.into_inner()).cancel.store(true, std::sync::atomic::Ordering::Relaxed); }
}

/// What a viewer joining now gets: (the snapshot event, a receiver for the rest - None once the run is over). None when this process never ran that QA.
pub fn hub_join(run_id: i64) -> Option<(Value, Option<tokio::sync::broadcast::Receiver<Value>>)> {
    let run = hub().lock().unwrap_or_else(|p| p.into_inner()).get(&run_id)?.clone();
    let r = run.lock().unwrap_or_else(|p| p.into_inner());
    let snap = json!({ "type": "snapshot", "timeline": r.timeline, "last_frame": r.last_frame, "done": r.done });
    Some((snap, if r.done { None } else { Some(r.tx.subscribe()) }))
}

/// the kept film of a run that this process did not run (a restart) or that is over: its timeline file, if the folder has one
pub fn film_timeline(cwd: &str, run_id: i64) -> Option<Value> {
    let t = std::fs::read_to_string(evidence_dir(cwd, run_id).join("film").join("timeline.json")).ok()?;
    serde_json::from_str::<Value>(&t).ok()
}
/// a film file name: four digits and `.jpg`
pub fn safe_film_name(n: &str) -> bool { n.len() == 8 && n.ends_with(".jpg") && n[..4].chars().all(|c| c.is_ascii_digit()) }

// ---- what the QA gate needs around the runner ------------------------------------------------------------------------------------------------

/// Does this change have anything to LOOK at? Deterministic, from the paths: a component, a style sheet, a page, an image asset.
pub fn touches_frontend(files: &[String]) -> bool {
    const EXT: &[&str] = &["tsx", "jsx", "vue", "svelte", "css", "scss", "sass", "less", "html", "htm", "svg", "astro"];
    files.iter().any(|f| Path::new(f).extension().and_then(|e| e.to_str()).map_or(false, |e| EXT.contains(&e.to_ascii_lowercase().as_str())))
}

/// A page that can be opened without a dev server: the first `index.html` among the places a built or static app keeps it, as a file:// URL.
pub fn static_app(cwd: &Path) -> Option<String> {
    for d in ["", "dist", "build", "out", "public"] {
        let p = cwd.join(d).join("index.html");
        if p.is_file() { return Some(file_url_of(&p)) }
    }
    None
}
/// `/a b/index.html` -> `file:///a%20b/index.html`: the escapes `nav_allowed` decodes, so a path with a space or a `%` in it names the same file on both sides. Pure - tested.
pub fn file_url_of(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://");
    if !s.starts_with('/') { out.push('/') }
    for b in s.bytes() { if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~' | b':') { out.push(b as char) } else { out.push_str(&format!("%{b:02X}")) } }
    out
}

/// EVERY JSON VALUE A REPLY HOLDS, in the order it appears (operator 2026-10-01: grok-high wrote prose around - or instead of - the step array and the QA fell back to a screenshot,
/// "the QA gave no usable plan"). The whole reply; each fenced block; then every balanced `[...]` / `{...}` in the prose, string- and escape-aware, so a stray "[1]" or a brace in a sentence
/// before the real array does not hide it the way "first `[` to last `]`" did. A top-level object that wraps the steps (`{"steps":[...]}`, `{"plan":[...]}`) is offered as its array
/// too. At most 64 values; pure - tested.
pub fn json_candidates(text: &str) -> Vec<Value> {
    let mut out: Vec<Value> = vec![];
    let push = |v: Value, out: &mut Vec<Value>| {
        if let Some(inner) = v.as_object().and_then(|o| ["steps", "plan", "test", "requests"].iter().find_map(|k| o.get(*k)).filter(|x| x.is_array()).cloned()) { out.push(inner); }
        out.push(v);
    };
    let t = text.trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) { push(v, &mut out); }
    // fenced blocks: ```json ... ``` or ``` ... ```
    let mut rest = t;
    while let Some(i) = rest.find("```") {
        let after = &rest[i + 3..];
        let Some(j) = after.find("```") else { break };
        let body = after[..j].trim_start_matches(|c: char| c.is_ascii_alphabetic()).trim();
        if let Ok(v) = serde_json::from_str::<Value>(body) { push(v, &mut out); }
        rest = &after[j + 3..];
    }
    // balanced spans in the prose
    let b: Vec<(usize, char)> = t.char_indices().collect();
    let (mut i, mut tries) = (0, 0);
    while i < b.len() && out.len() < 64 && tries < 2_000 {
        let (at, c) = b[i];
        if c != '[' && c != '{' { i += 1; continue }
        tries += 1;
        let (mut depth, mut in_str, mut esc, mut end) = (0i32, false, false, None);
        for (k, &(_, d)) in b.iter().enumerate().skip(i) {
            if in_str { if esc { esc = false } else if d == '\\' { esc = true } else if d == '"' { in_str = false } continue }
            match d { '"' => in_str = true, '[' | '{' => depth += 1, ']' | '}' => { depth -= 1; if depth == 0 { end = Some(k); break } } _ => {} }
        }
        match end.and_then(|k| { let stop = b.get(k + 1).map(|x| x.0).unwrap_or(t.len()); serde_json::from_str::<Value>(&t[at..stop]).ok().map(|v| (k, v)) }) {
            Some((k, v)) => { push(v, &mut out); i = k + 1; }
            None => i += 1,
        }
    }
    out
}

/// The QA seat's reply -> a plan: the first JSON value in it (whole reply, fenced block, or balanced array/object in prose) that is a valid plan. None when it holds none (the caller then asks once more, then uses `default_plan`).
pub fn plan_from_reply(text: &str) -> Option<Vec<Step>> {
    json_candidates(text).iter().find_map(|v| parse_plan(v).ok())
}

/// WHY a reply is not a plan, in a sentence for the run's log (operator 2026-09-30: two runs said "the QA gave no usable plan" and nothing else - there was no way to tell a model that
/// wrote prose from one that wrote a step the validator refuses). Pure - tested.
pub fn plan_problem(text: &str) -> String {
    let t = text.trim();
    if t.is_empty() { return "the reply was empty".into() }
    let head: String = t.chars().take(120).collect::<String>().replace('\n', " ");
    // the refusal worth reporting is the one of the first candidate that LOOKS like a plan (an array of objects), else the first array
    let cands = json_candidates(t);
    let cand = cands.iter().find(|v| v.as_array().map_or(false, |a| a.iter().any(Value::is_object))).or_else(|| cands.iter().find(|v| v.is_array()));
    match cand { None => format!("the reply held no JSON array of steps (it began: {head})"), Some(v) => match parse_plan(v) { Err(e) => format!("the plan was refused: {e}"), Ok(_) => "the plan is valid".into() } }
}

/// THE STRICTER ASK for the one retry (operator 2026-10-01): the same prompt, plus the reason the first answer was not a plan and the format spelled out once more.
pub fn strict_json_ask(prompt: &str, problem: &str) -> String {
    format!("{prompt}\n\nYOUR PREVIOUS ANSWER WAS NOT USABLE ({problem}).\nAnswer again with ONLY the JSON array of steps - the first character of your reply is `[` and the last is `]`. No prose before or after it, no code fence, no explanation, no wrapping object.")
}

/// When the QA gives no usable plan the evidence is still worth having: the page itself, once.
/// A PURE RESTYLE NEEDS NO MODEL TO CHOOSE ITS PICTURES (deterministic QA step 2, 2026-10-09). Measured on the workspace record: `goal.qa_evidence_plan` was the largest QA line ($10.48 over 95 calls, 43 s each,
/// more than the judging), and for design asks the model's 6-12 steps produced exactly ONE screenshot every time (runs 352, 353, 356) - run 352 re-planned seven times. When the ask is about how the page
/// LOOKS and the change touched only style / markup files, the plan is the standard capture (`default_plan`: open the page, shoot it); the design lint and the coverage steps are added as always. Anything
/// that asks for behaviour, motion or a named control keeps the model's plan. Pure - tested.
pub fn is_static_restyle(ask: &str, files: &[String]) -> bool {
    const STYLE_OR_MARKUP: &[&str] = &["css", "scss", "sass", "less", "html", "htm", "svg", "tsx", "jsx", "vue", "svelte", "astro"];
    if files.is_empty() || !files.iter().all(|f| Path::new(f).extension().and_then(|e| e.to_str()).map_or(false, |e| STYLE_OR_MARKUP.contains(&e.to_ascii_lowercase().as_str()))) { return false }
    let a = ask.to_lowercase();
    let words: Vec<&str> = a.split(|c: char| !c.is_alphanumeric() && c != '-').filter(|w| !w.is_empty()).collect();
    let has = |ks: &[&str]| ks.iter().any(|k| words.iter().any(|w| w.starts_with(k)));
    let looks = ["design", "redesign", "look", "style", "styling", "theme", "colo", "font", "typograph", "layout", "spacing", "padding", "margin", "minimal", "modern", "classic", "dark", "light", "gamer",
                 "aesthetic", "beautif", "pretty", "palette", "visual", "appearance", "elegant", "clean", "sleek", "retro", "playful", "bold", "polish", "professional", "serif", "rounded", "gradient", "glass", "neon"];
    // behaviour, motion, or a control the ask names: the model must choose the steps (the coverage plan does the rest)
    let acts = ["add", "remove", "delete", "fix", "bug", "search", "filter", "sort", "login", "signup", "form", "validat", "submit", "click", "dropdown", "checkbox", "toggle", "modal",
                "menu", "tab", "api", "endpoint", "save", "persist", "load", "fetch", "animat", "transition", "hover", "motion", "scroll", "responsive", "mobile", "phone", "accessib", "keyboard", "focus", "link", "navigat", "route",
                // a part of the page that only EXISTS after an interaction (found 2026-10-10 on the stored plans of runs 360, 362, 313: the model clicked "add" to open the cart before it took the picture the ask was about;
                // the standard capture of the closed page would have shot a page without it): the model must choose the steps that open it
                "cart", "checkout", "panel", "drawer", "sidebar", "popup", "popover", "dialog", "overlay", "tooltip", "accordion", "carousel", "wizard"];
    has(&looks) && !has(&acts)
}
pub fn default_plan(app: &str) -> Vec<Step> { vec![Step::Goto(app.to_string()), Step::Shot("page".into())] }

/// THE VISIBLE COPY OF A MARKUP FILE (deterministic QA, content-survived check, 2026-10-10): the words a page shows, read from its source - JSX / HTML text between a `>` and the next `<` on one line. The model plans
/// of 9 stored restyle runs all carried `expect` steps on exactly this kind of text ("Ministore", "Cart", "Add to cart", the headline) and the standard capture has none, so nothing else said whether a
/// restyle had DELETED content. Anything with braces, an arrow, an entity or an `=` in it is code or an expression, not copy; a text needs three letters in a row to count; at most `max` come back, in file order, each once.
pub fn copy_of(src: &str, max: usize) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for line in src.lines() {
        let mut rest = line;
        while let Some(i) = rest.find('>') {
            let after = &rest[i + 1..];
            let end = after.find('<').unwrap_or(after.len());
            let raw = &after[..end];
            // a `>` that closes a comparison or an arrow (`a > b`, `=>`) opens no text: the character before it must end a tag or attribute list
            let before = rest[..i].trim_end();
            let closes_tag = before.ends_with('"') || before.ends_with('\'') || before.ends_with('}') || before.ends_with('/') || before.chars().last().map_or(false, |c| c.is_alphanumeric() || c == '-');
            // not a tag's end: step over this one `>` only, so the text after an arrow's `>` is still read
            if !closes_tag { rest = after; continue }
            let t = raw.trim();
            let looks_like_text = closes_tag && !t.is_empty() && t.len() <= 60 && !t.chars().any(|c| matches!(c, '{' | '}' | '=' | '&' | ';' | '`' | '(' | ')' | '\\' | '$'))
                && t.chars().collect::<Vec<_>>().windows(3).any(|w| w.iter().all(|c| c.is_alphabetic()));
            if looks_like_text && !out.iter().any(|x| x == t) { out.push(t.to_string()); if out.len() >= max { return out } }
            rest = &after[end..];
        }
    }
    out
}

/// WHAT A RESTYLE DID TO THE COPY: from the changed markup files as they were and as they are -
///   `must_show`: copy the file still carries in its source (not turned into page checks: it may sit in a panel that is not open - see `lost_visible_text`, the browser comparison);
///   `removed`: copy that is gone from the source altogether (reported, never failed: a restyle may reword on purpose - the operator and the judge read the list).
/// Case and spacing do not matter (`text-transform` changes what the browser shows). Pure - tested.
pub fn copy_survival(before: &[String], after_src: &str) -> (Vec<String>, Vec<String>) {
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    let hay = norm(after_src);
    let (mut must_show, mut removed) = (vec![], vec![]);
    for b in before { if hay.contains(&norm(b)) { must_show.push(b.clone()) } else { removed.push(b.clone()) } }
    (must_show, removed)
}

/// The copy a restyle's changed markup files showed BEFORE (read from the tree the run started from, `git show <tree>:<path>`) that the same files, all of them together, no longer carry (text may move between files):
/// the `removed` half of `copy_survival` (the `must_show` half is NOT turned into page checks: copy in a panel that is not open would fail them falsely - the browser comparison `lost_visible_text` is the check). Nothing known about the old tree, or no old version of a file (a new file), contributes nothing - never a guess.
pub fn restyle_copy(cwd: &str, base_tree: Option<&str>, files: &[String]) -> Vec<String> {
    let Some(tree) = base_tree.filter(|t| !t.is_empty()) else { return vec![] };
    const MARKUP: &[&str] = &["tsx", "jsx", "html", "htm", "vue", "svelte", "astro"];
    let marked: Vec<&String> = files.iter().filter(|f| Path::new(f.as_str()).extension().and_then(|e| e.to_str()).map_or(false, |e| MARKUP.contains(&e.to_ascii_lowercase().as_str()))).collect();
    let after: String = marked.iter().filter_map(|f| std::fs::read_to_string(Path::new(cwd).join(f.as_str())).ok()).collect::<Vec<_>>().join("\n");
    let mut before: Vec<String> = vec![];
    for f in &marked {
        let Some(o) = crate::proc::cmd("git").args(["show", &format!("{tree}:{f}")]).current_dir(cwd).output().ok().filter(|o| o.status.success()) else { continue };
        for c in copy_of(&String::from_utf8_lossy(&o.stdout), 12) { if !before.contains(&c) { before.push(c) } }
    }
    copy_survival(&before, &after).1
}

/// The line the judge and the operator read about removed copy; empty when nothing was removed. Pure - tested.
pub fn removed_copy_note(removed: &[String]) -> String {
    if removed.is_empty() { return String::new() }
    let list: Vec<String> = removed.iter().take(8).map(|t| format!("\"{t}\"")).collect();
    format!("\nTEXT THE PAGE HAD BEFORE THIS CHANGE AND THE SOURCE NO LONGER CARRIES: {}{}. A restyle may reword on purpose; if the ask did not call for removing it, treat the removal as a defect.\n", list.join(", "), if removed.len() > 8 { format!(" (+{} more)", removed.len() - 8) } else { String::new() })
}

/// WORDS THE OLD PAGE SHOWED AND THE NEW ONE DOES NOT (2026-10-10): the QA already runs the same steps on the version before the change and keeps each page's visible text (`Evidence.text`); a line that is on
/// the old page and gone from the new one - hidden by CSS, dropped by a condition, clipped, deleted - is listed (at most `max`), compared case- and spacing-insensitively. Both pages are in the SAME state (the
/// plan's own steps), so a panel that was never opened is on neither: no false alarm from unrendered copy. A line needs three letters in a row and at most 60 characters. Pure - tested.
pub fn lost_visible_text(before: &str, after: &str, max: usize) -> Vec<String> {
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    let hay = norm(after);
    let mut out: Vec<String> = vec![];
    for line in before.lines() {
        let t = line.trim();
        // words (three letters in a row) or an amount (a digit with a currency sign or a percent): "$18.00" is content a restyle must not hide, and has no letters
        let words = t.chars().collect::<Vec<_>>().windows(3).any(|w| w.iter().all(|c| c.is_alphabetic()));
        let amount = t.chars().any(|c| c.is_ascii_digit()) && t.chars().any(|c| matches!(c, '$' | '€' | '£' | '¥' | '%'));
        if t.is_empty() || t.len() > 60 || !(words || amount) { continue }
        if !hay.contains(&norm(t)) && !out.iter().any(|x| x == t) { out.push(t.to_string()); if out.len() >= max { break } }
    }
    out
}

/// The line the judge reads about words that vanished from the page; empty when none did. Pure - tested.
pub fn lost_text_note(lost: &[String]) -> String {
    if lost.is_empty() { return String::new() }
    let list: Vec<String> = lost.iter().map(|t| format!("\"{t}\"")).collect();
    format!("TEXT ON THE OLD PAGE AND NOT ON THE NEW ONE (same steps, same state): {}. A restyle may reword on purpose; if the ask did not call for removing or hiding it, this is a defect.\n", list.join(", "))
}

/// a plain PNG file name (letters, digits, `-`, `_`, `.`), never a path - what the evidence endpoint will serve
pub fn safe_image_name(n: &str) -> bool { n.ends_with(".png") && n.len() < 100 && !n.starts_with('.') && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) && !n.contains("..") }

/// where a run's evidence lives: inside its own box, out of the way of the project's files
pub fn evidence_dir(cwd: &str, run_id: i64) -> PathBuf { Path::new(cwd).join(".antiloki").join("qa").join("evidence").join(format!("run-{run_id}")) }

#[cfg(test)]
mod tests {
    #[test]
    fn the_copy_of_a_markup_file_is_its_visible_words_and_nothing_else() {
        let src = "<header><h1>Ministore</h1><nav><a href=\"/c\">Cart</a></nav></header>\n<button className=\"btn\" onClick={() => add(p)}>Add to cart</button>\n{items.length > 3 && <p>Many items</p>}\n<p>{price} each</p>\n<span>OK</span>\n<h2>Ministore</h2>\nif (a > b) { return x }";
        assert_eq!(copy_of(src, 12), vec!["Ministore", "Cart", "Add to cart", "Many items"], "once each, in file order; an expression, a 2-letter label and a comparison are not copy");
        assert_eq!(copy_of(src, 2), vec!["Ministore", "Cart"], "capped");
        assert!(copy_of("const x = a > b ? 1 : 2;", 5).is_empty());
    }
    #[test]
    fn what_a_restyle_did_to_the_copy_splits_into_must_show_and_removed() {
        let before = vec!["Ministore".to_string(), "Add to cart".to_string(), "Free shipping over $50".to_string()];
        let after = "<h1>MINISTORE</h1>\n<button>Add  to cart</button>";
        let (show, gone) = copy_survival(&before, after);
        assert_eq!(show, vec!["Ministore", "Add to cart"], "case and spacing do not matter");
        assert_eq!(gone, vec!["Free shipping over $50"], "gone from the source: reported, not failed");
        assert_eq!(removed_copy_note(&[]), "");
        assert!(removed_copy_note(&gone).contains("\"Free shipping over $50\"") && removed_copy_note(&gone).contains("defect"));
        assert_eq!(restyle_copy("/nonexistent", None, &["src/App.tsx".into()]), Vec::<String>::new(), "no old tree: nothing is claimed");
        // what the browser showed before against after (the real check): same state both sides, so only what vanished is listed
        assert_eq!(lost_visible_text("Ministore\nAdd to cart\n$18.00\nFree shipping over $50\nOK", "MINISTORE\n Add  to cart \n$18.00", 8), vec!["Free shipping over $50"], "case/spacing ignored; a 2-letter label is not text");
        assert_eq!(lost_visible_text("a b c\nHello there", "", 1), vec!["Hello there"], "capped");
        assert_eq!(lost_visible_text("Mug\n$18.00\n12\n50% off", "Mug\n12", 8), vec!["$18.00", "50% off"], "an amount counts, a bare number does not");
        assert!(lost_text_note(&[]).is_empty() && lost_text_note(&["Gone".to_string()]).contains("\"Gone\"") && lost_text_note(&["Gone".to_string()]).contains("defect"));
    }
    use super::*;

    // A SAVED PLAN IS THE SAME PLAN (item 8d, 2026-10-01): a plan kept as JSON and parsed back must be the steps that ran, or a replay would test something else.
    #[test]
    fn a_plan_kept_as_json_parses_back_into_the_same_steps_for_every_kind_of_step() {
        let ex = |target: &str, text: &str, min: u64, not: bool, url: bool, animated: bool| Step::Expect(Expect { target: target.into(), text: text.into(), min, not, url, animated });
        let steps = vec![
            Step::Goto("http://localhost:5173/shop".into()), Step::Click("text=Add to cart".into()), Step::Hover(".menu".into()), Step::Type { target: "input[name=q]".into(), text: "red shoes".into() },
            Step::Press("Enter".into()), Step::WaitFor("#results".into()), Step::Wait(300), Step::Scroll(-600), Step::Shot("after-search".into()),
            Step::Select { target: "select#sort".into(), option: "Price: low to high".into() }, Step::Clear("input[name=q]".into()), Step::Mark, Step::DesignLint, Step::ExpectChange("clicking \"Sort\"".into()), Step::NoteChange("clicking \"All\"".into()),
            ex("", "red shoes", 1, false, false, false), ex(".card", "", 3, false, false, false), ex(".card", "Red shoe", 1, false, false, false), ex("", "zebra", 1, true, false, false), ex(".error", "", 1, true, false, false),
            ex("", "/shop?sort=price", 1, false, true, false), ex(".spinner", "", 2, false, false, true),   // (the grammar has no "address must not contain": `expect_url` cannot be negated, so a plan never holds one)
        ];
        let json = steps_json(&steps);
        assert_eq!(parse_plan(&json).expect("the kept plan is a valid plan"), steps, "{json}");
        // and what is kept is plain JSON a person can read and a model could have written
        assert_eq!(json[0], json!({ "do": "goto", "url": "http://localhost:5173/shop" }));
        assert!(json.to_string().contains("\"expect_no\""), "{json}");
    }

    #[test]
    fn a_plan_is_validated_and_bounded_because_it_is_data_from_a_model() {
        let ok = parse_plan(&json!([{ "do": "goto", "url": "/cart" }, { "do": "click", "target": "text=Add to cart" }, { "do": "type", "target": "input[name=q]", "text": "shoes" },
            { "do": "press", "key": "Enter" }, { "do": "wait", "ms": 999_999 }, { "do": "scroll" }, { "do": "shot", "name": "after-add" }])).unwrap();
        assert_eq!(ok[0], Step::Goto("/cart".into()));
        assert_eq!(ok[4], Step::Wait(MAX_WAIT_MS), "a wait is capped");
        assert_eq!(ok[5], Step::Scroll(600));
        assert_eq!(ok[6], Step::Shot("after-add".into()));
        assert!(parse_plan(&json!("click the button")).is_err() && parse_plan(&json!([])).is_err(), "not an array / empty");
        assert!(parse_plan(&json!((0..MAX_STEPS + 1).map(|_| json!({ "do": "wait" })).collect::<Vec<_>>())).is_err(), "over the step budget");
        assert!(parse_plan(&json!([{ "do": "click" }])).unwrap_err().contains("needs a \"target\""));
        assert!(parse_plan(&json!([{ "do": "rm -rf" }])).unwrap_err().contains("unknown action"), "only the listed actions exist");
        assert!(parse_plan(&json!([{ "do": "goto", "url": "https://example.com/x" }])).unwrap_err().contains("only the box's own app"), "no other origins");
        assert!(parse_plan(&json!([{ "do": "shot", "name": "../../etc/passwd" }])).is_err(), "a screenshot name cannot walk out of the folder");
        assert_eq!(parse_plan(&json!([{ "do": "shot" }])).unwrap(), vec![Step::Shot("shot-1".into())], "an unnamed shot is numbered");
    }

    #[test]
    fn the_gate_looks_only_at_changes_that_have_something_to_look_at() {
        assert!(touches_frontend(&["src/Cart.tsx".into()]) && touches_frontend(&["a/b.CSS".into()]) && touches_frontend(&["index.html".into()]));
        assert!(!touches_frontend(&["src/lib.rs".into(), "README.md".into(), "server/api.ts".into()]), "a backend change has nothing to screenshot");
        let d = std::env::temp_dir().join(format!("antiloki-qa-static-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("dist")).unwrap();
        assert!(static_app(&d).is_none(), "no page, no evidence");
        std::fs::write(d.join("dist/index.html"), "<p>hi</p>").unwrap();
        assert!(static_app(&d).unwrap().ends_with("/dist/index.html"));
        std::fs::write(d.join("index.html"), "<p>root</p>").unwrap();
        assert!(static_app(&d).unwrap().ends_with(&format!("{}/index.html", d.display())), "the box's own index.html wins over a built one");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_plan_is_found_in_prose_and_a_bad_reply_falls_back_to_one_look_at_the_page() {
        let p = plan_from_reply("Sure! Here you go:\n```json\n[{\"do\":\"goto\",\"url\":\"/x\"},{\"do\":\"shot\"}]\n```").unwrap();
        assert_eq!(p.len(), 2);
        assert!(plan_from_reply("I would open the page and click things").is_none());
        assert!(plan_from_reply("[{\"do\":\"nuke\"}]").is_none(), "an invalid plan is not a plan");
        assert_eq!(default_plan("http://localhost:5173/"), vec![Step::Goto("http://localhost:5173/".into()), Step::Shot("page".into())]);
        // grok-high, 2026-10-01: prose around - or instead of - the array. The bracket in a sentence before the plan, a wrapping object, a fence with no language tag, braces inside strings.
        let plan = "[{\"do\":\"goto\",\"url\":\"/\"},{\"do\":\"expect\",\"text\":\"a ] and a } in a string\"},{\"do\":\"shot\"}]";
        assert_eq!(plan_from_reply(&format!("I checked the list [see item 1] and wrote this:\n{plan}\nThat covers it [done].")).map(|p| p.len()), Some(3), "a stray [1] before the plan must not hide it");
        assert_eq!(plan_from_reply(&format!("Here is the plan:\n```\n{plan}\n```\nand some words [with brackets] after")).map(|p| p.len()), Some(3), "a fence with no language tag");
        assert_eq!(plan_from_reply(&format!("{{\"steps\": {plan}}}")).map(|p| p.len()), Some(3), "a wrapping object");
        assert_eq!(plan_from_reply(&format!("Plan -> {{\"plan\": {plan}, \"note\": \"x\"}} done")).map(|p| p.len()), Some(3), "a wrapping object inside prose");
        assert!(plan_from_reply("steps: [first open the page], [then click] {sort}").is_none(), "brackets that are not JSON are not a plan");
        assert!(json_candidates(&"[".repeat(5000)).len() <= 64, "an unbalanced flood is bounded");
        assert!(plan_problem("the order [1] matters, {see below}").contains("held no JSON array") || plan_problem("the order [1] matters, {see below}").contains("refused"));
        assert!(strict_json_ask("P", "why").starts_with("P\n\n") && strict_json_ask("P", "why").contains("(why)") && strict_json_ask("P", "why").contains("ONLY the JSON array"));
    }


    /// THE PLAY-BY-PLAY, in a real browser: every step announces itself (with where its target sits), frames stream while it works, the film and timeline are kept,
    /// and the pictures the judge gets are BYTE-IDENTICAL to a run with nobody watching (the pacing is for people, not for evidence)
    #[test]
    fn a_watched_run_streams_steps_and_frames_and_leaves_the_evidence_untouched() {
        if antiloki_rs::codebase_helpers_browser_capture::find_browser().is_err() { eprintln!("no Chrome - skipped"); return }
        let dir = std::env::temp_dir().join(format!("antiloki-qa-live-{}", std::process::id())); let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
        let page = dir.join("app.html");
        std::fs::write(&page, r#"<!doctype html><meta charset=utf-8><body style="margin:0;font:32px sans-serif">
          <button id=b style="margin:120px 300px;padding:20px 40px;background:#eee" onclick="document.body.style.background='#0a7';document.getElementById('m').textContent='Added to cart'">Add to cart</button>
          <div id=m style="margin:40px 300px">Nothing yet</div></body>"#).unwrap();
        let url = format!("file://{}", page.display());
        let plan = parse_plan(&json!([{ "do": "goto", "url": url }, { "do": "shot", "name": "before" }, { "do": "click", "target": "text=Add to cart" },
            { "do": "wait_for", "target": "text=Added to cart" }, { "do": "shot", "name": "after" }, { "do": "click", "target": "text=No such button" }])).unwrap();
        let seen: Arc<std::sync::Mutex<Vec<Value>>> = Arc::new(std::sync::Mutex::new(vec![]));
        let sink = { let s = seen.clone(); Arc::new(move |v: Value| s.lock().unwrap().push(v)) as Arc<dyn Fn(Value) + Send + Sync> };
        let watched = run_plan_in("", Some(&dir), &plan, &dir.join("watched"), Some(Live { sink, pace: Arc::new(|| 250), film_dir: Some(dir.join("film")), cancel: Default::default() }), false).unwrap();
        let quiet = run_plan_in("", Some(&dir), &plan, &dir.join("quiet"), None, false).unwrap();
        let ev = seen.lock().unwrap().clone();
        let of = |t: &str| ev.iter().filter(|e| e["type"] == t).collect::<Vec<_>>();
        assert_eq!(of("step").len(), 6, "one announcement per step");
        let click = of("step").into_iter().find(|e| e["kind"] == "click" && e["text"].as_str().unwrap().contains("Add to cart")).expect("the click is announced");
        let r = &click["rect"]; assert!(r.is_array() && r[2].as_f64().unwrap() > 50.0 && r[3].as_f64().unwrap() > 20.0 && r[0].as_f64().unwrap() >= 300.0, "the click says where its target is: {click}");
        assert_eq!(of("step-done").iter().filter(|e| e["ok"] == true).count(), 5); assert_eq!(of("step-done").iter().filter(|e| e["ok"] == false).count(), 1, "the missing button is reported, not fatal");
        assert_eq!(of("shot").len(), 2); assert_eq!(ev.last().unwrap()["type"], "done", "the stream ends with done");
        // announcements come in order, each before its own result
        let order: Vec<(String, u64)> = ev.iter().filter(|e| e["type"] == "step" || e["type"] == "step-done").map(|e| (e["type"].as_str().unwrap().to_string(), e["n"].as_u64().unwrap())).collect();
        for n in 1..=6u64 { let a = order.iter().position(|x| *x == ("step".to_string(), n)).unwrap(); let b = order.iter().position(|x| *x == ("step-done".to_string(), n)).unwrap(); assert!(a < b, "step {n}: announced before done"); }
        let frames = of("frame"); assert!(frames.len() >= 4, "frames streamed while it worked, got {}", frames.len());
        use base64::Engine as _;
        let first = base64::engine::general_purpose::STANDARD.decode(frames[0]["jpeg"].as_str().unwrap()).unwrap();
        assert_eq!(&first[..2], &[0xFF, 0xD8], "a JPEG");
        // the film for a replay: frames on disk and a timeline that mentions them and the steps
        let film: Vec<_> = std::fs::read_dir(dir.join("film")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        assert!(film.iter().filter(|f| f.ends_with(".jpg")).count() >= 4 && film.contains(&"timeline.json".to_string()), "{film:?}");
        let tl: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("film/timeline.json")).unwrap()).unwrap();
        assert!(tl.as_array().unwrap().iter().any(|e| e["type"] == "film") && tl.as_array().unwrap().iter().any(|e| e["type"] == "step"), "the timeline has both");
        assert!(tl.as_array().unwrap().iter().all(|e| e["type"] != "frame"), "the base64 frames are not in the timeline");
        // determinism: what the judge is given is the same with or without a viewer
        assert_eq!(std::fs::read(&watched.shots[0].1).unwrap(), std::fs::read(&quiet.shots[0].1).unwrap(), "the first picture is identical");
        assert_eq!(std::fs::read(&watched.shots[1].1).unwrap(), std::fs::read(&quiet.shots[1].1).unwrap(), "and so is the second");
        let _ = std::fs::remove_dir_all(&dir);
    }


    /// a page with nothing on it but text (what a worker's first draft often is) must not stall the frame thread: the run ends promptly
    #[test]
    fn a_watched_run_on_a_page_with_only_text_ends_promptly() {
        if antiloki_rs::codebase_helpers_browser_capture::find_browser().is_err() { eprintln!("no Chrome - skipped"); return }
        let dir = std::env::temp_dir().join(format!("antiloki-qa-text-{}", std::process::id())); let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), "written by the fake seat (0)\n").unwrap();
        let plan = parse_plan(&json!([{ "do": "goto", "url": format!("file://{}", dir.join("index.html").display()) }, { "do": "shot", "name": "page" }])).unwrap();
        let sink: Arc<dyn Fn(Value) + Send + Sync> = Arc::new(|_| {});
        let t = std::time::Instant::now();
        let ev = run_plan_in("", Some(&dir), &plan, &dir.join("evidence"), Some(Live { sink, pace: Arc::new(|| 0), film_dir: Some(dir.join("film")), cancel: Default::default() }), false).unwrap();
        assert_eq!(ev.shots.len(), 1);
        assert!(t.elapsed() < Duration::from_secs(20), "the run took {:?} - the frame thread stalled it", t.elapsed());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// F-08 (audit 2026-10-01): the old gate was `starts_with("http://localhost")`; every one of these passed it.
    #[test]
    fn a_goto_that_only_looks_like_localhost_is_refused_by_the_plan_and_by_the_run() {
        for evil in ["http://localhost.evil.com/", "http://localhost@evil.com/", "http://127.0.0.1.evil.net/", "http://localhost:80@evil.com", "http://127.0.0.1@evil.com:5173/x", "http://localhost\\@evil.com/",
                     "http://localhost.:5173/", "http://foo.localhost:5173/", "http://127.1:5173/", "http://2130706433:5173/", "http://localhost:5173.evil.com/", "//evil.com/x", "http://localhost%2eevil.com/", "javascript:alert(1)",
                     "data:text/html,hi", "chrome://settings", "ftp://localhost/x", "https://example.com", "http://[::1]evil.com/"] {
            assert!(parse_plan(&json!([{ "do": "goto", "url": evil }])).is_err(), "the plan must refuse {evil}");
            assert!(nav_allowed(evil, "http://localhost:5173", Some(Path::new("/tmp"))).is_err(), "the run must refuse {evil}");
        }
        // the spellings of the app that ARE the app
        for ok in ["/cart", "http://localhost:5173/cart", "http://127.0.0.1:5173/", "http://[::1]:5173/x?y=1#z", "HTTP://LOCALHOST:5173"] {
            assert!(parse_plan(&json!([{ "do": "goto", "url": ok }])).is_ok(), "the plan accepts {ok}");
            assert!(nav_allowed(ok, "http://localhost:5173", None).is_ok(), "the run accepts {ok}");
        }
        // ...but only the SAME port: another local server is not the app under test
        assert!(nav_allowed("http://localhost:3000/", "http://localhost:5173", None).is_err());
        assert!(nav_allowed("http://127.0.0.1/", "http://localhost:5173", None).is_err(), "port 80 is not 5173");
    }

    /// F-08: this serve's own port is where publish / forge tokens / QA replay live - a plan may not drive a browser at it unless it IS the app.
    #[test]
    fn the_serves_own_port_is_not_a_page_to_open_unless_it_is_the_app_under_test() {
        assert!(nav_allowed("http://localhost:41931/api/v1/cockpit/deliver/preflight?pane_id=1", "http://localhost:5173", None).is_err());
        assert!(nav_allowed("http://127.0.0.1:41998/", "http://localhost:5173", None).is_err());
        // (a same-port spelling of the app is the app: that case is decided by the app test above, not by this one)
        assert!(nav_allowed("http://localhost:41931/", "http://127.0.0.1:41931", None).is_ok(), "when the serve IS the app under test");
    }

    /// F-08: file:// only inside the box, after symlinks are resolved - /etc/passwd, ../ escapes, %2e%2e, a sibling-prefix folder and a symlink out all fail.
    #[test]
    fn a_file_url_may_name_only_a_file_inside_the_box() {
        let d = std::env::temp_dir().join(format!("antiloki-qa-nav-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d);
        let boxd = d.join("box"); let sibling = d.join("box2"); std::fs::create_dir_all(boxd.join("dist")).unwrap(); std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(boxd.join("dist/index.html"), "<h1>hi</h1>").unwrap(); std::fs::write(sibling.join("secret.txt"), "s").unwrap();
        let outside = d.join("outside.txt"); std::fs::write(&outside, "do not show").unwrap();
        #[cfg(unix)] std::os::unix::fs::symlink(&outside, boxd.join("link.txt")).unwrap();
        #[cfg(unix)] std::os::unix::fs::symlink("/etc", boxd.join("etc-link")).unwrap();
        let url = |p: &Path| format!("file://{}", p.display());
        assert!(nav_allowed(&url(&boxd.join("dist/index.html")), "http://localhost:5173", Some(&boxd)).is_ok(), "the box's own page");
        assert!(nav_allowed(&format!("{}#/route?x=1", url(&boxd.join("dist/index.html"))), "http://localhost:5173", Some(&boxd)).is_ok(), "a fragment/query is not part of the path");
        assert!(nav_allowed(&file_url_of(&boxd.join("dist/index.html")), "http://localhost:5173", Some(&boxd)).is_ok(), "the encoder and the checker agree");
        assert!(nav_allowed("file:///etc/passwd", "http://localhost:5173", Some(&boxd)).is_err(), "any other path");
        assert!(nav_allowed("file:///etc/passwd", "http://localhost:5173", None).is_err(), "no box named: no file at all");
        assert!(nav_allowed(&url(&boxd.join("../outside.txt")), "http://localhost:5173", Some(&boxd)).is_err(), "../ out of the box");
        assert!(nav_allowed(&format!("{}/%2e%2e/outside.txt", url(&boxd)), "http://localhost:5173", Some(&boxd)).is_err(), "%2e%2e is ..");
        assert!(nav_allowed(&url(&sibling.join("secret.txt")), "http://localhost:5173", Some(&boxd)).is_err(), "box2 starts with box but is not in it");
        assert!(nav_allowed(&format!("file://evil.example{}", boxd.join("dist/index.html").display()), "http://localhost:5173", Some(&boxd)).is_err(), "a network host");
        assert!(nav_allowed(&url(&boxd.join("missing.html")), "http://localhost:5173", Some(&boxd)).is_err(), "a file that is not there");
        #[cfg(unix)] {
            assert!(nav_allowed(&url(&boxd.join("link.txt")), "http://localhost:5173", Some(&boxd)).is_err(), "a symlink in the box that points out");
            assert!(nav_allowed(&url(&boxd.join("etc-link/passwd")), "http://localhost:5173", Some(&boxd)).is_err(), "a symlinked folder that points out");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// F-08: the plan is refused whole BEFORE a browser is launched - no Chrome needed to prove it, and no step ran.
    #[test]
    fn a_run_whose_plan_goes_somewhere_forbidden_never_starts_a_browser() {
        let d = std::env::temp_dir().join(format!("antiloki-qa-refuse-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        let steps = vec![Step::Goto("/".into()), Step::Goto("file:///etc/passwd".into()), Step::Shot("x".into())];
        let e = run_plan_in("http://localhost:5173", Some(&d), &steps, &d.join("out"), None, false).unwrap_err();
        assert!(e.contains("step 2") && e.contains("refused"), "{e}");
        assert!(!d.join("out").exists(), "nothing was created");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn urls_parse_strictly_and_a_file_url_round_trips_a_path_with_a_space_and_a_percent() {
        assert_eq!(parse_http_url("http://LocalHost:5173/a?b#c"), Some(("http".into(), "localhost".into(), 5173, "/a?b#c".into())));
        assert_eq!(parse_http_url("https://[::1]/x").map(|u| (u.1, u.2)), Some(("[::1]".into(), 443)));
        assert_eq!(parse_http_url("http://localhost").map(|u| u.2), Some(80));
        for bad in ["http://a b/", "http://u:p@h/", "http:///x", "http://h:99999/", "http://h:x/", "localhost:5173", "http://h\\x/"] { assert!(parse_http_url(bad).is_none(), "{bad}"); }
        let d = std::env::temp_dir().join(format!("antiloki qa 100%-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("index.html"), "x").unwrap();
        let u = static_app(&d).unwrap();
        assert!(u.contains("%20") && u.contains("%25"), "{u}");
        assert!(nav_allowed(&u, &u, Some(&d)).is_ok(), "the static page the runner itself opens passes its own gate: {u}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn chrome_keeps_its_sandbox_unless_the_process_cannot_run_it() {
        std::env::set_var("ANTILOKI_CHROME_NO_SANDBOX", "1");
        assert!(chrome_needs_no_sandbox(), "the operator's override");
        std::env::remove_var("ANTILOKI_CHROME_NO_SANDBOX");
        // not root and not in a container (this suite does not run as root; in a container the sandbox is legitimately off)
        let is_root = { #[cfg(unix)] { extern "C" { fn geteuid() -> u32; } unsafe { geteuid() == 0 } } #[cfg(not(unix))] { false } };
        let expect = Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists() || is_root;
        assert_eq!(chrome_needs_no_sandbox(), expect);
        let src = include_str!("qa_evidence.rs");
        assert_eq!(src.matches(concat!("\"--no", "-sandbox\"")).count(), 0, "the flag is never written into the launch args: headless_chrome adds it when sandbox(false)");
    }

    #[test]
    fn the_same_plan_is_pointed_at_the_old_version_and_the_pictures_are_compared() {
        let s = vec![Step::Goto("http://localhost:5173/cart".into()), Step::Click("text=Buy".into()), Step::Shot("after".into())];
        assert_eq!(retarget(&s, "http://localhost:5173", "http://localhost:5199")[0], Step::Goto("http://localhost:5199/cart".into()));
        assert_eq!(retarget(&s, "http://localhost:5173", "http://localhost:5199")[1], Step::Click("text=Buy".into()), "only URLs move");
        let d = std::env::temp_dir().join(format!("antiloki-qa-compare-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        for (n, bytes) in [("a1", b"same".as_slice()), ("b1", b"same"), ("a2", b"one"), ("b2", b"two")] { std::fs::write(d.join(format!("{n}.png")), bytes).unwrap(); }
        let ev = |x: &str, names: &[&str]| Evidence { shots: names.iter().enumerate().map(|(i, n)| (n.to_string(), d.join(format!("{x}{}.png", i + 1)))).collect(), ..Default::default() };
        let cmp = compare(&ev("a", &["home", "cart"]), &ev("b", &["home", "cart"]));
        assert_eq!(cmp, vec![("home".to_string(), true), ("cart".to_string(), false)]);
        let b = base_block(&ev("b", &["home", "cart"]), &cmp);
        assert!(b.contains("home: IDENTICAL to before") && b.contains("cart: DIFFERS from before") && b.contains("IMAGE before-home:"), "{b}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_base_tree_is_put_on_disk_and_the_old_version_differs_from_the_new() {
        let d = std::env::temp_dir().join(format!("antiloki-qa-base-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        let cwd = d.to_string_lossy().to_string();
        let git = |a: &[&str]| { let o = crate::proc::cmd("git").args(["-c", "user.email=q@x", "-c", "user.name=q"]).args(a).current_dir(&d).output().unwrap(); String::from_utf8_lossy(&o.stdout).trim().to_string() };
        git(&["init", "-q"]); std::fs::write(d.join("index.html"), "<p>old</p>").unwrap(); git(&["add", "-A"]); git(&["commit", "-qm", "old"]);
        let tree = git(&["rev-parse", "HEAD^{tree}"]);
        std::fs::write(d.join("index.html"), "<p>new</p>").unwrap();
        let dest = d.join("base-copy");
        materialize_base(&cwd, &tree, &dest).unwrap();
        assert_eq!(std::fs::read_to_string(dest.join("index.html")).unwrap(), "<p>old</p>", "the OLD version, while the box holds the new one");
        assert_eq!(std::fs::read_to_string(d.join("index.html")).unwrap(), "<p>new</p>", "the box itself is untouched");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_late_viewer_gets_the_timeline_and_the_latest_frame_and_then_the_rest() {
        let live = hub_open(90_001, None, 300);
        assert_eq!((live.pace)(), 0, "nobody is watching, nobody waits");
        (live.sink)(json!({ "type": "step", "n": 1 })); (live.sink)(json!({ "type": "frame", "jpeg": "AAAA" })); (live.sink)(json!({ "type": "frame", "jpeg": "BBBB" }));
        let (snap, rx) = hub_join(90_001).expect("the run is known");
        assert_eq!(snap["timeline"].as_array().unwrap().len(), 1, "steps are kept"); assert_eq!(snap["last_frame"]["jpeg"], "BBBB", "only the newest frame is kept"); assert_eq!(snap["done"], false);
        let mut rx = rx.expect("still running: a receiver for the rest");
        assert_eq!((live.pace)(), 300, "somebody is watching now: pace the steps so they can be followed");
        (live.sink)(json!({ "type": "step", "n": 2 }));
        assert_eq!(rx.try_recv().unwrap()["n"], 2);
        (live.sink)(json!({ "type": "done" }));
        assert_eq!(rx.try_recv().unwrap()["type"], "done");
        let (snap2, rx2) = hub_join(90_001).unwrap(); assert_eq!(snap2["done"], true); assert!(rx2.is_none(), "a finished run has nothing more to stream");
        assert!(hub_join(90_002).is_none(), "a run this process never ran");
        assert!(safe_film_name("0007.jpg") && !safe_film_name("../a.jpg") && !safe_film_name("7.jpg") && !safe_film_name("00071.jpg"));
    }

    #[test]
    fn only_plain_png_names_can_be_served_from_the_evidence_folder() {
        assert!(safe_image_name("01-after-add.png") && safe_image_name("02_page.png"));
        assert!(!safe_image_name("../../etc/passwd") && !safe_image_name("a/b.png") && !safe_image_name("..png") && !safe_image_name(".hidden.png") && !safe_image_name("shot.txt") && !safe_image_name("a\\b.png"));
    }

    #[test]
    fn a_dev_servers_announced_url_becomes_an_address_to_connect_to() {
        assert_eq!(host_port("http://localhost:5173/").as_deref(), Some("localhost:5173"));
        assert_eq!(host_port("http://127.0.0.1:3000/app?x=1").as_deref(), Some("127.0.0.1:3000"));
        assert_eq!(host_port("http://localhost/").as_deref(), Some("localhost:80"));
        assert_eq!(host_port("https://example.test").as_deref(), Some("example.test:443"));
        assert!(host_port("file:///x/index.html").is_none() && host_port("http:///").is_none());
    }

    #[test]
    fn a_box_without_a_dev_script_or_dependencies_says_why_there_is_nothing_to_boot() {
        let d = std::env::temp_dir().join(format!("antiloki-devserver-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        let cwd = d.to_string_lossy().to_string();
        assert!(DevServer::start(&cwd, Duration::from_secs(1)).unwrap_err().contains("no package.json"));
        std::fs::write(d.join("package.json"), r#"{"scripts":{"build":"x"}}"#).unwrap();
        assert!(DevServer::start(&cwd, Duration::from_secs(1)).unwrap_err().contains("no dev or start script"));
        std::fs::write(d.join("package.json"), r#"{"scripts":{"dev":"vite"}}"#).unwrap();
        assert!(DevServer::start(&cwd, Duration::from_secs(1)).unwrap_err().contains("dependencies are not installed"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// a REAL server: a tiny Node http server announces its URL like Vite does; the module boots it, the capture reads it, and dropping it ends the process
    #[test]
    fn a_dev_server_is_booted_captured_and_shut_down() {
        if antiloki_rs::codebase_helpers_browser_capture::find_browser().is_err() || crate::proc::which("node").is_none() || crate::proc::which("npm").is_none() || !crate::proc::cmd("npm").arg("--version").output().map_or(false, |o| o.status.success()) /* a version-manager shim with no version set is on PATH but cannot run */ { eprintln!("no Chrome/node/npm - skipped"); return }
        let d = std::env::temp_dir().join(format!("antiloki-devserver-live-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(d.join("node_modules")).unwrap();
        std::fs::write(d.join("package.json"), r#"{"scripts":{"dev":"node server.js"}}"#).unwrap();
        std::fs::write(d.join("server.js"), r#"const http=require("http");const s=http.createServer((q,r)=>{r.setHeader("content-type","text/html");r.end('<h1 id=t style="font:48px sans-serif">served by the box</h1>')});s.listen(0,"127.0.0.1",()=>console.log("  Local:   http://127.0.0.1:"+s.address().port+"/"));"#).unwrap();
        let cwd = d.to_string_lossy().to_string();
        let pid;
        {
            let dev = DevServer::start(&cwd, Duration::from_secs(30)).expect("the dev server boots and announces its URL");
            pid = dev.pid;
            assert!(dev.url.starts_with("http://127.0.0.1:"), "{}", dev.url);
            let ev = run_plan(&dev.url, &parse_plan(&json!([{ "do": "goto", "url": dev.url }, { "do": "shot", "name": "home" }])).unwrap(), &d.join("evidence")).unwrap();
            assert_eq!(ev.shots.len(), 1, "{:?}", ev.log);
            assert!(ev.text.contains("served by the box"), "{}", ev.text);
        }
        std::thread::sleep(Duration::from_millis(800));
        #[cfg(unix)]
        assert!(!Path::new(&format!("/proc/{pid}")).exists(), "dropping the DevServer ends its process");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_prompt_block_names_the_images_the_errors_and_the_steps_that_failed() {
        let ev = Evidence { shots: vec![("after".into(), PathBuf::from("/box/.antiloki/qa/01-after.png"))], log: vec!["1. opened /cart".into(), "2. FAILED - click failed: nothing matches \"text=Buy\"".into()],
            console_errors: vec!["TypeError: x is undefined".into()], text: String::new(), failed_steps: 1, checks: vec![], design: None };
        let b = prompt_block(&ev);
        assert!(b.contains("IMAGE after: /box/.antiloki/qa/01-after.png") && b.contains("CONSOLE ERRORS (1)") && b.contains("TypeError") && b.contains("1 step(s) could not be done"), "{b}");
    }

    #[test]
    fn a_reply_that_is_not_a_plan_says_why() {
        assert_eq!(plan_problem("  "), "the reply was empty");
        assert!(plan_problem("I would open the page and look at it.").contains("held no JSON array"));
        assert!(plan_problem("[{\"do\":\"fly\"}]").starts_with("the plan was refused: step 1: unknown action"), "{}", plan_problem("[{\"do\":\"fly\"}]"));
        assert!(plan_problem("[{\"do\":\"goto\",\"url\":\"https://evil.example\"}]").contains("only the box's own app"));
        assert_eq!(plan_problem("[{\"do\":\"goto\",\"url\":\"/\"},{\"do\":\"shot\"}]"), "the plan is valid");
    }
    #[test]
    fn a_plan_may_contain_deterministic_checks_and_they_are_validated_like_any_step() {
        let ok = parse_plan(&json!([{ "do": "expect", "text": "shoes" }, { "do": "expect", "target": ".result", "min": 3 }, { "do": "expect", "target": "#cart", "text": "1 item" },
            { "do": "expect_no", "text": "error" }, { "do": "expect_url", "text": "/cart" }])).unwrap();
        assert_eq!(ok[0], Step::Expect(Expect { target: "".into(), text: "shoes".into(), min: 1, not: false, url: false, animated: false }));
        assert_eq!(ok[1], Step::Expect(Expect { target: ".result".into(), text: "".into(), min: 3, not: false, url: false, animated: false }));
        assert!(matches!(&ok[3], Step::Expect(e) if e.not) && matches!(&ok[4], Step::Expect(e) if e.url));
        assert!(parse_plan(&json!([{ "do": "expect" }])).is_err(), "a check must ask for something");
        assert!(parse_plan(&json!([{ "do": "expect_url" }])).is_err());
        assert_eq!(describe(&ok[1]).1, "checking that at least 3 .result on the page");
        assert_eq!(describe(&ok[2]).1, "checking that #cart does say \"1 item\"");
    }

    #[test]
    fn the_judge_looks_only_when_the_browser_could_not_decide_or_the_change_is_visual() {
        let mk = |checks: Vec<(&str, bool)>| Evidence { checks: checks.into_iter().map(|(w, ok)| (w.to_string(), ok, String::new())).collect(), ..Default::default() };
        let js = vec!["src/Search.tsx".to_string()]; let css = vec!["src/Search.tsx".to_string(), "src/search.css".to_string()];
        assert!(needs_eyes(&mk(vec![]), &js), "a plan with no check is only pictures");
        assert!(needs_eyes(&mk(vec![("a", true), ("b", false)]), &js), "a failed check: the picture shows why");
        assert!(!needs_eyes(&mk(vec![("a", true), ("b", true)]), &js), "every check passed on a behaviour change: the browser already decided");
        assert!(needs_eyes(&mk(vec![("a", true)]), &css), "a stylesheet changed: how it looks is the claim");
    }
    #[test]
    fn an_image_costs_what_its_size_says() {
        let dir = std::env::temp_dir().join(format!("antiloki-qa-img-{}", std::process::id())); std::fs::create_dir_all(&dir).unwrap();
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec(); png.extend_from_slice(&[0, 0, 0, 13]); png.extend_from_slice(b"IHDR"); png.extend_from_slice(&1280u32.to_be_bytes()); png.extend_from_slice(&800u32.to_be_bytes());
        let p = dir.join("a.png"); std::fs::write(&p, &png).unwrap();
        assert_eq!(image_tokens(&p), 1366, "1280 x 800 / 750");
        std::fs::write(dir.join("b.png"), b"not a png at all, just bytes").unwrap();
        assert_eq!(image_tokens(&dir.join("b.png")), 0); assert_eq!(image_tokens(&dir.join("missing.png")), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn the_planner_is_told_what_is_really_on_the_page() {
        let v = json!({ "title": "Scratch shop", "headings": ["Scratch shop"], "inputs": [{ "sel": "#q", "type": "search", "hint": "Search products" }], "buttons": ["Search", "Add to cart"],
            "links": ["Cart -> /cart"], "lists": ["#results (0 items)"] });
        let b = elements_block(&v);
        assert!(b.contains("input: #q (search, hint \"Search products\")") && b.contains("buttons (click with text=<label>): Search | Add to cart") && b.contains("#results (0 items)"), "{b}");
        assert!(elements_block(&json!({})).is_empty());
    }

    #[test]
    fn the_evidence_block_leads_with_the_checks_and_leaves_the_images_out_when_asked() {
        let ev = Evidence { shots: vec![("page".into(), PathBuf::from("/box/01-page.png"))], log: vec!["1. opened /".into()], checks: vec![("the page does say \"shoes\"".into(), true, "the text is there".into()),
            ("at least 1 .result on the page".into(), false, "0 visible match(es)".into())], ..Default::default() };
        let with = prompt_block_with(&ev, true); let without = prompt_block_with(&ev, false);
        assert!(with.contains("1 passed, 1 failed") && with.contains("PASS the page does say \"shoes\"") && with.contains("FAIL at least 1 .result on the page - the page had: 0 visible match(es)"), "{with}");
        assert!(with.contains("IMAGE page:") && !without.contains("IMAGE page:") && without.contains("no images attached"), "{without}");
        assert_eq!(ev.checks_failed(), 1);
    }

    #[test]
    fn only_the_controls_the_change_is_about_are_used() {
        let probe = json!({ "sample": "shoes", "inputs": [
            { "sel": "#sort", "kind": "select", "options": ["Featured", "Price: low to high", "Name"], "current": "Featured" },
            { "sel": "input[placeholder=\"Search products\"]", "kind": "search", "hint": "Search products" },
            { "sel": "#instock", "kind": "checkbox", "hint": "In stock" }], "buttons": ["Add to cart", "Filters"] });
        let names = |s: Scoped| { let mut v = s.inputs; v.extend(s.buttons); v };
        // the operator's ask: the dropdown, nothing else - even though the diff also moves the search row around
        let diff = "+  <div className=\"search-row\">\n+    <select id=\"sort\" value={sort}>\n+      <option>Price: low to high</option>";
        assert_eq!(names(scope_controls(&probe, "change the dropdown sorting", diff)), vec!["#sort"], "the ask names the sort dropdown (sorting ~ sort): only it");
        // the ask names no control and no kind: nothing is generated - the lines the change added are NOT a reason (they name every product and every category)
        assert!(names(scope_controls(&probe, "make it nicer", "+ <input id=\"instock\" type=\"checkbox\" />\n  context line mentioning search")).is_empty());
        assert!(names(scope_controls(&probe, "add pictures to the products", "+ {name:'Lamp', category:'Light'}\n+ <button>Add to cart</button>")).is_empty(), "an ask about pictures uses no control (run 272: it clicked every category button)");
        // the ask names only a KIND: every control of that kind
        assert_eq!(names(scope_controls(&probe, "redo the checkbox", "")), vec!["#instock"]);
        assert_eq!(names(scope_controls(&probe, "a dropdown somewhere", "")), vec!["#sort"]);
        // nothing names anything: nothing is generated (the QA's own plan covers it)
        assert!(names(scope_controls(&probe, "make it faster", "")).is_empty());
        // a one-control change is a short test: 4 options besides the default minus none = well under the old 53
        let steps = coverage_steps(&probe, "http://x/", "change the dropdown sorting", diff);
        assert!(steps.len() <= 4 * 2 + 4, "{}", steps.len());
        assert!(!steps.iter().any(|x| matches!(x, Step::Type { .. } | Step::Press(_))), "the search box is not touched");
    }
    #[test]
    fn the_coverage_plan_uses_every_control_the_page_shows() {
        let probe = json!({ "sample": "shoes", "inputs": [
            { "sel": "#sort", "kind": "select", "options": ["Featured", "Price: low to high", "Price: high to low", "Name"], "current": "Featured" },
            { "sel": "input[placeholder=\"Search products\"]", "kind": "search", "hint": "Search products" },
            { "sel": "#instock", "kind": "checkbox", "hint": "In stock" }], "buttons": ["Add to cart", "Filters"] });
        let steps = coverage_steps(&probe, "http://localhost:5177/", "add sorting, search, the in stock toggle, add to cart and filters", "");
        let selects: Vec<&Step> = steps.iter().filter(|x| matches!(x, Step::Select { .. })).collect();
        assert_eq!(selects.len(), 3, "every option that is not the current one is chosen: {selects:?}");
        assert!(steps.iter().any(|x| matches!(x, Step::Type { text, .. } if text == "shoes")), "the search is typed with a word the page itself shows");
        assert!(steps.iter().any(|x| matches!(x, Step::Press(k) if k == "Enter")));
        assert!(steps.iter().any(|x| matches!(x, Step::Click(t) if t == "#instock")) && steps.iter().any(|x| matches!(x, Step::Click(t) if t == "text=Add to cart")));
        // every action is bracketed: a fresh page, a mark, the action, the verdict
        for (i, st) in steps.iter().enumerate() { if let Step::ExpectChange(_) = st { assert!(matches!(steps[i - 3], Step::Goto(_)) || matches!(steps[i - 4], Step::Goto(_)), "a fresh page before each: {i}"); } }
        assert!(steps.len() <= COVERAGE_MAX_STEPS);
        assert!(coverage_steps(&json!({}), "http://x/", "anything", "").is_empty(), "a page with no controls has nothing to use");
    }

    /// THE SORTING DROPDOWN, TESTED (operator 2026-09-30: *"add a sorting drop down besides the search ... again just took a picture instead of click on the dropdown, see the options, choose one"*).
    /// A page with a dropdown that really sorts, one that does nothing, a search box and a checkbox: the explorer - no model - proves the ones that work and flags the one that does not.
    #[test]
    fn every_control_is_used_and_the_one_that_changes_nothing_is_caught() {
        if antiloki_rs::codebase_helpers_browser_capture::find_browser().is_err() { eprintln!("no Chrome/Chromium/Edge here - the browser test is skipped"); return }
        let dir = std::env::temp_dir().join(format!("antiloki-qa-explore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
        let page = dir.join("app.html");
        std::fs::write(&page, r#"<!doctype html><meta charset=utf-8><title>Shop</title><body><h1>Shop</h1>
          <input id=q type=search placeholder="Search products">
          <select id=sort><option>Featured</option><option>Price: low to high</option><option>Price: high to low</option></select>
          <select id=broken><option>Any colour</option><option>Red</option><option>Blue</option></select>
          <label><input id=instock type=checkbox> In stock</label>
          <ul id=list></ul>
          <script>
            const P=[{n:'Sneakers',p:90,s:1},{n:'Socks',p:5,s:1},{n:'Shoes',p:60,s:0},{n:'Shirt',p:25,s:1}];
            function draw(){ let l=P.filter(x=>x.n.toLowerCase().includes(q.value.toLowerCase())&&(!instock.checked||x.s));
              if(sort.value==='Price: low to high') l=l.sort((a,b)=>a.p-b.p); if(sort.value==='Price: high to low') l=l.sort((a,b)=>b.p-a.p);
              list.innerHTML=l.map(x=>'<li>'+x.n+' $'+x.p+'</li>').join(''); }
            q.addEventListener('input',draw); sort.addEventListener('change',draw); instock.addEventListener('change',draw); draw();
          </script></body>"#).unwrap();
        let url = format!("file://{}", page.display());
        let probe = probe_raw(&url).unwrap();
        assert_eq!(probe["sample"], "sneakers", "a word the page itself shows: {probe}");
        let block = elements_block(&probe);
        assert!(block.contains("#sort (select") && block.contains("\"Price: low to high\""), "the planner is told the options: {block}");
        let steps = coverage_steps(&probe, &url, "check sorting, search, the in stock filter and the colour menu", "");
        let ev = run_plan_in("", Some(&dir), &steps, &dir.join("evidence"), None, false).unwrap();
        let by = |needle: &str| ev.checks.iter().find(|c| c.0.contains(needle)).unwrap_or_else(|| panic!("no check for {needle}: {:?}", ev.checks)).clone();
        assert!(by("\"Price: low to high\" in the dropdown #sort").1, "choosing a sort order reorders the list: {:?}", ev.checks);
        assert!(by("\"Price: high to low\" in the dropdown #sort").1);
        assert!(by("typing \"sneakers\"").1, "the search filters the list: {:?}", ev.checks);
        assert!(by("toggling").1, "the in-stock filter changes the list");
        let red = by("\"Red\" in the dropdown #broken");
        assert!(!red.1 && red.2.contains("exactly the same"), "the dropdown that does nothing is CAUGHT: {red:?}");
        assert!(!by("\"Blue\" in the dropdown #broken").1);
        assert_eq!(ev.checks_failed(), 2, "only the broken dropdown fails: {:?}", ev.checks);
        // and the judge is told, as a fact
        let b = prompt_block_with(&ev, false);
        assert!(b.contains("FAIL choosing \"Red\" in the dropdown #broken changes the page - the page had: the page looked exactly the same afterwards"), "{b}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn motion_is_recognised_and_the_film_pace_shrinks_with_the_plan() {
        assert!(is_motion_change("add pictures carismatic avatars to the products, all animated", ""));
        assert!(is_motion_change("make it nicer", "+ .avatar { animation: float 3s infinite; }"), "the ask says nothing, the added lines introduce an animation");
        assert!(is_motion_change("a hover effect on the cards", ""));
        assert!(!is_motion_change("add a sorting dropdown", "+ <select id=\"sort\">"));
        assert_eq!((capped_pace(700, 10), capped_pace(700, 24), capped_pace(700, 53), capped_pace(0, 53)), (700, 200, 100, 0));
    }

    /// RUN 272 (operator 2026-09-30, "add pictures carismatic avatars to the products, all animated"): 11 checks failed on a page whose avatars the judge could SEE - an SVG has no offsetParent, so every
    /// SVG counted as invisible; the QA switched animations off and then asked whether anything moved; and clicking a category that was already selected "changed nothing". The judge said UNMET and
    /// the worker was sent back for a second round. All three, in a real page.
    #[test]
    fn svg_is_visible_animation_can_be_checked_and_an_unrelated_button_is_only_noted() {
        if antiloki_rs::codebase_helpers_browser_capture::find_browser().is_err() { eprintln!("no Chrome/Chromium/Edge here - the browser test is skipped"); return }
        let dir = std::env::temp_dir().join(format!("antiloki-qa-truth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
        let page = dir.join("app.html");
        std::fs::write(&page, r##"<!doctype html><meta charset=utf-8><title>Shop</title>
          <style>@keyframes bob{from{transform:translateY(0)}to{transform:translateY(-8px)}} .avatar{animation:bob 1s ease-in-out infinite alternate} .still{display:inline-block;width:20px;height:20px}</style>
          <body><ul>
            <li class=card><svg class="product-avatar-svg avatar" width=40 height=40 viewBox="0 0 40 40"><circle cx=20 cy=20 r=18 fill="#c33"/></svg> Lamp</li>
            <li class=card><svg class="product-avatar-svg avatar" width=40 height=40 viewBox="0 0 40 40"><rect width=40 height=40 fill="#36c"/></svg> Mug</li>
            <li class=card><svg class="product-avatar-svg" width=40 height=40 viewBox="0 0 40 40"><rect width=40 height=40 fill="#3a6"/></svg> Chair</li>
          </ul><button id=all>All</button></body>"##).unwrap();
        let url = format!("file://{}", page.display());
        let plan = parse_plan(&json!([{ "do": "goto", "url": url },
            { "do": "expect", "target": ".card svg.product-avatar-svg", "min": 3 },
            { "do": "expect_animated", "target": ".avatar", "min": 2 },
            { "do": "expect_animated", "target": ".card svg.product-avatar-svg", "min": 3 },
            { "do": "mark" }, { "do": "click", "target": "text=All" }])).unwrap();
        // MOTION MODE: the page keeps its animations, the SVGs count as visible, and the check that not every avatar moves is honest (2 of 3 do)
        let ev = run_plan_in("", Some(&dir), &plan, &dir.join("e1"), None, true).unwrap();
        let by: Vec<(bool, String)> = ev.checks.iter().map(|c| (c.1, c.2.clone())).collect();
        assert!(by[0].0, "three SVG avatars are visible and counted: {by:?}");
        assert!(by[1].0, "two avatars are animated: {by:?}");
        assert!(!by[2].0 && by[2].1.contains("2 of 3"), "the third is not animated, and the check says so: {by:?}");
        // DEFAULT MODE kills animations for deterministic pictures, so the same check reports it plainly (this is why motion mode exists)
        let ev2 = run_plan_in("", Some(&dir), &plan, &dir.join("e2"), None, false).unwrap();
        assert!(ev2.checks.iter().any(|c| !c.1 && c.2.contains("0 of 3")), "with animations switched off nothing moves: {:?}", ev2.checks);
        // an unrelated button is only NOTED: clicking "All" changes nothing and that is not a failure
        let note = parse_plan(&json!([{ "do": "goto", "url": url }, { "do": "mark" }])).unwrap();
        let mut steps = note; steps.extend([Step::Click("text=All".into()), Step::NoteChange("clicking \"All\"".into())]);
        let ev3 = run_plan_in("", Some(&dir), &steps, &dir.join("e3"), None, false).unwrap();
        assert_eq!(ev3.failed_steps, 0, "{:?}", ev3.log);
        assert!(ev3.log.iter().any(|l| l.contains("no visible change (informational)")), "{:?}", ev3.log);
        assert!(ev3.checks.is_empty(), "an informational note is not a check");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE SEARCH, TESTED (operator 2026-09-30: *"i asked for a search and didnt test the search, didnt write anything, didnt see the result on writting"*): type into the box, press
    /// Enter, and the browser itself decides whether results appeared - a passing check, a failing one, and a check on something that is not there. No model in the loop.
    #[test]
    fn a_search_is_typed_submitted_and_its_result_checked_by_the_browser_itself() {
        if antiloki_rs::codebase_helpers_browser_capture::find_browser().is_err() { eprintln!("no Chrome/Chromium/Edge here - the browser test is skipped"); return }
        let dir = std::env::temp_dir().join(format!("antiloki-qa-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
        let page = dir.join("app.html");
        std::fs::write(&page, r#"<!doctype html><meta charset=utf-8><title>Shop</title><body><h1>Shop</h1>
          <input id=q placeholder="Search products" onkeydown="if(event.key==='Enter'){const v=this.value.toLowerCase();const all=['red shoes','blue shoes','green hat'];const hits=all.filter(x=>x.includes(v));
            document.getElementById('r').innerHTML=hits.map(h=>'<li class=result>'+h+'</li>').join('')||'<li>nothing found</li>';}">
          <ul id=r></ul></body>"#).unwrap();
        let url = format!("file://{}", page.display());
        // the probe sees the real search box
        let probe = probe_page(&url).unwrap();
        assert!(probe.contains("input: #q (text, hint \"Search products\")"), "{probe}");
        let plan = parse_plan(&json!([{ "do": "goto", "url": url }, { "do": "expect", "target": ".result", "min": 1 }, { "do": "type", "target": "#q", "text": "shoes" }, { "do": "press", "key": "Enter" },
            { "do": "expect", "target": ".result", "min": 2 }, { "do": "expect", "text": "blue shoes" }, { "do": "expect_no", "text": "green hat" }, { "do": "expect", "target": "#r", "text": "red shoes" }])).unwrap();
        let ev = run_plan_in("", Some(&dir), &plan, &dir.join("evidence"), None, false).unwrap();
        let by: Vec<(bool, String)> = ev.checks.iter().map(|c| (c.1, c.0.clone())).collect();
        assert_eq!(by.len(), 5, "{by:?}");
        assert!(!by[0].0, "before searching there are no results, and the browser says so: {by:?}");
        assert!(by[1..].iter().all(|c| c.0), "after typing shoes + Enter the four checks pass: {by:?} {:?}", ev.log);
        assert_eq!(ev.checks_failed(), 1);
        assert_eq!(ev.failed_steps, 1, "a failed check counts as a failed step, so the judge is told: {:?}", ev.log);
        assert!(ev.log.iter().any(|l| l.contains("check FAILED") && l.contains("0 visible match(es)")), "{:?}", ev.log);
        // and a feature that does NOT work is caught: searching for something the page cannot find still shows "nothing found", never results
        let broken = parse_plan(&json!([{ "do": "goto", "url": url }, { "do": "type", "target": "#q", "text": "zebra" }, { "do": "press", "key": "Enter" }, { "do": "expect", "target": ".result" }])).unwrap();
        let ev2 = run_plan_in("", Some(&dir), &broken, &dir.join("evidence2"), None, false).unwrap();
        assert_eq!(ev2.checks_failed(), 1, "{:?}", ev2.log);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// the whole thing against a real page in the machine's own Chrome: a static shot, a click that changes the page, a shot after, and a page error - all deterministic
    #[test]
    fn a_plan_runs_in_a_real_browser_and_leaves_two_different_images_and_the_pages_errors() {
        if antiloki_rs::codebase_helpers_browser_capture::find_browser().is_err() { eprintln!("no Chrome/Chromium/Edge here - the browser test is skipped"); return }
        let dir = std::env::temp_dir().join(format!("antiloki-qa-evidence-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
        let page = dir.join("app.html");
        std::fs::write(&page, r#"<!doctype html><meta charset=utf-8><body style="margin:0;font:32px sans-serif">
          <button id=b style="margin:40px;padding:20px 40px;background:#eee" onclick="document.body.style.background='#0a7';document.getElementById('m').textContent='Added to cart';console.error('boom after click')">Add to cart</button>
          <div id=m style="margin:40px">Nothing yet</div></body>"#).unwrap();
        let url = format!("file://{}", page.display());
        let plan = parse_plan(&json!([{ "do": "goto", "url": url }, { "do": "shot", "name": "before" }, { "do": "click", "target": "text=Add to cart" },
            { "do": "wait_for", "target": "text=Added to cart" }, { "do": "shot", "name": "after" }, { "do": "click", "target": "text=No such button" }])).unwrap();
        let ev = run_plan_in("", Some(&dir), &plan, &dir.join("evidence"), None, false).unwrap();
        assert_eq!(ev.shots.len(), 2, "{:?}", ev.log);
        let (a, b) = (std::fs::read(&ev.shots[0].1).unwrap(), std::fs::read(&ev.shots[1].1).unwrap());
        assert_eq!(&a[..8], b"\x89PNG\r\n\x1a\n", "a real PNG");
        assert_ne!(a, b, "the click changed what the page looks like, and the images show it");
        assert!(ev.text.contains("Added to cart"), "{}", ev.text);
        assert!(ev.console_errors.iter().any(|e| e.contains("boom after click")), "the page's console error reaches the QA: {:?}", ev.console_errors);
        assert_eq!(ev.failed_steps, 1, "the missing button is reported, not fatal: {:?}", ev.log);
        assert!(ev.log.last().unwrap().contains("nothing matches"), "{:?}", ev.log);
        // deterministic: the same plan gives the same pixels
        let again = run_plan_in("", Some(&dir), &plan, &dir.join("evidence2"), None, false).unwrap();
        assert_eq!(std::fs::read(&again.shots[0].1).unwrap(), a, "the same page, the same plan, the same first image");
        let _ = std::fs::remove_dir_all(&dir);
    }
    // PLAN-THE-DESIGN S1: the design lint is a step of the plan, its result is evidence of its own, and it is never mistaken for a check of the feature.
    #[test]
    fn the_design_lint_is_its_own_block_of_the_evidence_and_not_one_of_the_checks() {
        let full: Value = serde_json::from_str(include_str!("../../bench/design/fixtures/bad-contrast-defaults.snapshot.json")).unwrap();
        let lint = crate::design_lint::lint(&serde_json::from_value(full["raw"].clone()).unwrap());
        let ev = Evidence { shots: vec![("page".into(), PathBuf::from("/box/01-page.png"))], log: vec!["1. opened /".into()], design: Some(lint), ..Default::default() };
        assert_eq!(ev.checks_failed(), 0, "a contrast problem the page had is not a failed check of the change's claim");
        assert!(!needs_eyes(&Evidence { design: ev.design.clone(), checks: vec![("x".into(), true, String::new())], ..Default::default() }, &["a.ts".to_string()]), "and it does not by itself send pictures to the judge");
        let b = prompt_block_with(&ev, false);
        assert!(b.contains("DESIGN LINT measured on the real page") && b.contains("FAIL text_contrast") && b.contains("#a3a3a3"), "{b}");
        assert!(!b.contains("DETERMINISTIC CHECKS"), "no behaviour check was run: {b}");
        let sm = summary(&ev);
        assert_eq!(sm["design_lint"]["checks"].as_array().map(|a| a.len()), Some(9));
        assert!(summary(&Evidence::default()).get("design_lint").is_none());
        assert!(!prompt_block_with(&Evidence::default(), false).contains("DESIGN LINT"), "no lint, no block");
    }

    #[test]
    fn the_design_lint_step_sits_between_the_qa_steps_and_the_coverage_steps_once() {
        let (qa, cov) = (vec![Step::Goto("/".into()), Step::Click("text=Add".into())], vec![Step::Goto("/".into()), Step::Mark]);
        assert_eq!(with_design_lint(&qa, &cov), vec![Step::Goto("/".into()), Step::Click("text=Add".into()), Step::DesignLint, Step::Goto("/".into()), Step::Mark]);
        assert_eq!(with_design_lint(&qa, &[]).last(), Some(&Step::DesignLint), "no coverage plan: the lint is the last step");
        let own = vec![Step::Goto("/".into()), Step::DesignLint, Step::Shot("a".into())];
        assert_eq!(with_design_lint(&own, &cov).iter().filter(|s| **s == Step::DesignLint).count(), 1, "a QA plan that asked for the lint is not given a second");
    }

    #[test]
    fn a_plan_can_ask_for_the_design_lint_and_the_step_is_described_and_kept() {
        let steps = parse_plan(&json!([{ "do": "goto", "url": "/" }, { "do": "design_lint" }])).unwrap();
        assert_eq!(steps[1], Step::DesignLint);
        assert_eq!(steps_json(&steps)[1], json!({ "do": "design_lint" }));
        assert_eq!(describe(&steps[1]).0, "mark");
        assert!(describe(&steps[1]).1.contains("design"));
    }

    /// THE REAL WIRING: a plan with a design_lint step, in the real headless Chrome, on a page with a known contrast problem: the evidence carries a lint that names the node, no step failed, and the
    /// behaviour checks are untouched. Skips without a browser.
    #[test]
    fn a_plan_with_a_design_lint_step_measures_the_real_page() {
        if antiloki_rs::codebase_helpers_browser_capture::find_browser().is_err() { eprintln!("no Chrome/Chromium/Edge here - the browser test is skipped"); return }
        let dir = std::env::temp_dir().join(format!("antiloki-qa-design-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
        let page = dir.join("app.html");
        std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/../bench/design/fixtures/bad-contrast-defaults.html"), &page).unwrap();
        let url = format!("file://{}", page.display());
        let steps = vec![Step::Goto(url), Step::DesignLint, Step::Shot("page".into())];
        let ev = run_plan_in("", Some(&dir), &steps, &dir.join("evidence"), None, false).unwrap();
        assert_eq!(ev.failed_steps, 0, "{:?}", ev.log);
        let d = ev.design.as_ref().unwrap_or_else(|| panic!("no design lint: {:?}", ev.log));
        assert!(!d.check("text_contrast").unwrap().ok && d.check("text_contrast").unwrap().sure, "{}", d.one_line());
        assert!(ev.log.iter().any(|l| l.contains("design lint ") && l.contains("text_contrast")), "{:?}", ev.log);
        assert!(ev.checks.is_empty() && ev.shots.len() == 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn a_restyle_ask_on_style_and_markup_files_needs_no_model_to_choose_its_pictures() {
        let f = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        for ask in ["make the design more minimalist", "make now more gamer like design", "Shift the ministore UI from the current minimalist modern dark look to a more classical design", "use a serif typography and a restrained palette", "change the design of the page to a better looking, more modern design", "Modernize the ministore page with a better-looking, more modern design"] {
            assert!(is_static_restyle(ask, &f(&["src/App.tsx", "src/App.css"])), "{ask}");
        }
    }
    #[test]
    fn behaviour_motion_a_named_control_or_a_logic_file_keeps_the_models_plan() {
        let f = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(!is_static_restyle("add a dark theme toggle", &f(&["src/App.tsx"])), "a toggle is behaviour");
        assert!(!is_static_restyle("make the sort dropdown look better", &f(&["src/App.css"])), "a named control");
        assert!(!is_static_restyle("add a hover animation to the cards", &f(&["src/App.css"])), "motion");
        assert!(!is_static_restyle("make the checkout form look modern", &f(&["src/App.css"])), "a form is a flow");
        // a panel that only exists once opened (runs 360 / 362 of 2026-10-10: the model had to click to open the cart before it could shoot it)
        assert!(!is_static_restyle("Redesign the cart panel and sticky checkout bar so they look polished", &f(&["src/App.css"])), "the cart panel is not on the loaded page");
        assert!(!is_static_restyle("restyle the side drawer to look cleaner", &f(&["src/App.css"])), "a drawer is opened by an interaction");
        assert!(!is_static_restyle("the cart looks ugly", &f(&["src/App.css"])), "the cart is only on the page once something is in it");
        assert!(is_static_restyle("the buttons look flat", &f(&["src/App.css"])), "how buttons LOOK is a restyle");
        assert!(is_static_restyle("Give primary buttons more depth: gradients, inner highlight, a resting glow, pressed states", &f(&["src/App.css"])), "the rewritten ask of run #363");
        assert!(!is_static_restyle("add a button that clears the cart", &f(&["src/App.tsx"])), "adding a button is behaviour (add, clears)");
        assert!(!is_static_restyle("make the buttons open a modal", &f(&["src/App.tsx"])), "opening a modal is behaviour");
        assert!(!is_static_restyle("make the design more minimalist", &f(&["src/App.tsx", "src/lib/cart.ts"])), "a logic file changed");
        assert!(!is_static_restyle("make the design more minimalist", &[]), "nothing changed: nothing to shoot");
        assert!(!is_static_restyle("rename the retry constant", &f(&["src/App.css"])), "not about looks");
    }
    #[test]
    fn the_standard_capture_is_open_the_page_and_shoot_it() {
        assert_eq!(default_plan("http://localhost:5174/"), vec![Step::Goto("http://localhost:5174/".into()), Step::Shot("page".into())]);
    }

}
