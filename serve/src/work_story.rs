//! THE STORY OF A TURN — what actually happened, in words a person reads once and understands.
//!
//! operator 2026-09-24: *"at the chat must be very clear who is working, the logs of its work, the diff, like
//! stages … even gates like QA when active, tool calling from x-ray and deterministic gates as well. Must use
//! natural human language based on templates … and at the end a final summary of the work done, stages
//! completed, files readed and writen, x-ray features and tools accessed, tokens saved by use heimdall router,
//! tokens and time used, time saved in comparation with raw frontier AI."*
//!
//! Every number below already exists on the goal run — the stage table, the task traces, the router's verdict,
//! the gates, the savings. Nothing here measures anything; it reads the ledger and says it in English. That is
//! the whole point: the run is the record, the story is the reading, and the two can never drift because one is
//! derived from the other. Pure — `story()` takes the run and the merges table and returns JSON.
use serde_json::{json, Map, Value};

fn s(v: Option<&Value>) -> String { v.and_then(Value::as_str).unwrap_or("").to_string() }
fn n(v: Option<&Value>) -> f64 { v.and_then(Value::as_f64).unwrap_or(0.0) }
fn arr(v: Option<&Value>) -> Vec<Value> { v.and_then(Value::as_array).cloned().unwrap_or_default() }

/// `262.7 s` → `4 min 23 s`; short spans stay in seconds, because "263 s" is a number, not a duration.
pub fn human_ms(ms: f64) -> String {
    if ms < 1000.0 { return format!("{} ms", ms.round() as i64) }
    let secs = ms / 1000.0;
    if secs < 90.0 { return format!("{:.0} s", secs) }
    let m = (secs / 60.0).floor() as i64; let r = (secs - (m as f64) * 60.0).round() as i64;
    if r == 0 { format!("{m} min") } else { format!("{m} min {r} s") }
}
/// `307865` → `308k`. Token counts are read at a glance or not at all.
pub fn human_tokens(t: f64) -> String {
    if t >= 1_000_000.0 { format!("{:.1}M", t / 1_000_000.0) } else if t >= 1000.0 { format!("{:.0}k", t / 1000.0) } else { format!("{}", t.round() as i64) }
}
pub fn human_usd(u: f64) -> String { if u >= 0.01 { format!("${u:.2}") } else if u > 0.0 { format!("${u:.4}") } else { "$0".into() } }

/// An engine id as a person would say it: `claude-code:claude-sonnet-5` → `Claude Sonnet`.
pub fn engine_label(engine: &str, model: &str) -> String {
    // the last line of defence: a model whose name says which seat serves it is never printed under another seat's name
    let engine = match crate::agent::engine_of_model_name(model) { Some(own) if own != engine && crate::cli::engine_of(&json!(engine)).is_some() => own, _ => engine };
    // THE SEAT IS ALWAYS NAMED, AND IN ONE VOCABULARY (operator 2026-09-28). This was a third copy of the naming
    // rule — it knew only three engines, so a Gemini, Grok or Cursor side fell through to its raw key, and it
    // joined house and model with a bare space while `cli::side_name` (which the rail and the chips print) uses
    // an explicit separator. The same merge was named two different ways on two screens.
    if matches!(engine, "npm" | "deterministic" | "tsc+vitest") { return pretty_machine(engine) }
    if let Some(e) = crate::cli::engine_of(&json!(engine)) {
        return crate::cli::side_name(e, if model.trim().is_empty() { None } else { Some(model.trim()) });
    }
    let house = match engine {
        "claude-code" | "claude" => "Claude", "codex" => "Codex", "opencode" => "OpenCode",
        other => return other.to_string(),
    };
    // OpenCode model ids carry their provider — `opencode/deepseek-v4-flash` — and the house is already said, so
    // repeating it reads as "OpenCode opencode/DeepSeek v4 flash". Keep the part after the last slash.
    let m = model.trim().rsplit('/').next().unwrap_or("").trim();
    if m.is_empty() || m == "default" { return house.into() }
    let pretty = m.replace('-', " ")
        .replace("claude ", "").replace("sonnet", "Sonnet").replace("opus", "Opus").replace("haiku", "Haiku")
        .replace("deepseek", "DeepSeek").replace("qwen", "Qwen").replace("kimi", "Kimi").replace("gpt", "GPT");
    format!("{house} {}", pretty.trim())
}
fn pretty_machine(engine: &str) -> String {
    match engine {
        "npm" => "the package manager".into(),
        "deterministic" => "antiloki itself".into(),
        "tsc+vitest" => "the compiler and the test runner".into(),
        e if e.starts_with("tsc+vitest") => "the compiler and the test runner".into(),
        other => other.to_string(),
    }
}
/// Is this step a machine's, or a model's? A deterministic gate is worth naming AS deterministic — it is the
/// part of the pipeline that cannot bluff, and the operator asked for it to be visible.
fn is_machine(engine: &str) -> bool { matches!(engine, "npm" | "deterministic") || engine.starts_with("tsc+vitest") }

/// THE TEMPLATES. One sentence per kind of step, written for someone who did not read the code. The stage's own
/// `note` carries the specifics and is appended as the detail line, never rewritten — a template that paraphrased
/// the machine's own words would be a second, lying source.
fn stage_title(name: &str) -> (&'static str, &'static str) {
    let base = name.split(':').next().unwrap_or(name);
    match base {
        "deps" => ("Checked the tools were installed", "deps"),
        "contract" => ("Agreed what to build", "plan"),
        "skeleton" => ("Laid down the empty files", "build"),
        "tests" => ("Wrote the tests first", "test"),
        "gate" => ("Proved the tests fail on empty files", "gate"),
        "check" => ("Checked the work", "gate"),
        "fill" => ("Wrote the code", "build"),
        "join" => ("Put the pieces together", "join"),
        "verify" => ("Verified the change", "gate"),
        "report" => ("Wrote up what changed", "report"),
        "plan" => ("Planned the work", "plan"),
        _ => ("Worked", "build"),
    }
}

/// One stage, told.
fn stage_out(st: &Value, tasks: &[Value]) -> Value {
    let name = s(st.get("name"));
    let (title, kind) = stage_title(&name);
    let engine = s(st.get("engine"));
    let ok = st.get("ok").and_then(Value::as_bool);
    // `check:t2` names the task it checked — say which file, not which id
    let title = match name.split_once(':') {
        Some((_, id)) => {
            let owned = tasks.iter().find(|t| s(t.get("id")) == id)
                .map(|t| arr(t.get("owns")).iter().map(|v| s(Some(v))).collect::<Vec<_>>().join(", "))
                .filter(|f| !f.is_empty());
            match owned { Some(f) => format!("{title} on {f}"), None => title.to_string() }
        }
        None => title.to_string(),
    };
    let tier = s(st.get("tier"));
    let who = if is_machine(&engine) { pretty_machine(&engine) } else {
        let base = engine_label(&engine, "");
        // …and when a TIER is named, "the job's model" is the thing the tier already says — so the seat keeps its
        // name and the redundant half goes (audit 2026-09-28).
        if tier.is_empty() || tier == "-" { base }
        else { format!("{} · {tier} tier", base.trim_end_matches(" · the job's model")) }
    };
    // THE RED GATE IS SUPPOSED TO FAIL. A blueprint writes the tests before the code and then runs them against
    // empty stubs: green there would mean the tests prove nothing. The ledger records it as `ok: false` because
    // the command exited non-zero, which is true — but showing the operator a red cross next to the one step that
    // worked exactly as designed is a lie told with an accurate number. `red_is_good` lets the view say "as
    // intended" without touching what the ledger holds.
    let red_is_good = name == "gate";
    let went_right = match ok { Some(v) => Some(if red_is_good { !v } else { v }), None => None };
    json!({
        "key": name, "title": title, "kind": kind, "who": who, "machine": is_machine(&engine),
        "ok": ok, "went_right": went_right, "red_is_good": red_is_good,
        "detail": s(st.get("note")),
        "ms": n(st.get("ms")), "cost": n(st.get("cost")),
        "tokens": n(st.get("tokens_in")) + n(st.get("tokens_out")),
    })
}

/// THE GATES, named for what they prove rather than for the command they run.
fn gates(run: &Value) -> Vec<Value> {
    let mut out = vec![];
    if let Some(b) = run.get("qa_baseline").filter(|v| !v.is_null()) {
        let passed = b.get("passed").and_then(Value::as_bool).unwrap_or(false);
        let files = arr(b.get("files")).len();
        out.push(json!({ "name": "Before the change", "kind": "deterministic", "passed": passed,
            "detail": if passed { format!("the related tests over {files} file(s) were already green, so anything red afterwards is ours") }
                      else { format!("the related tests over {files} file(s) were ALREADY red before we touched anything — a failure afterwards may not be ours") } }));
    }
    if let Some(q) = run.get("qa").filter(|v| !v.is_null()) {
        let passed = q.get("passed").and_then(Value::as_bool).unwrap_or(false);
        out.push(json!({ "name": "The tests", "kind": "deterministic", "passed": passed,
            "detail": format!("{} · {} · {}", s(q.get("cmd")), if passed { "passed" } else { "failed" }, human_ms(n(q.get("ms")))) }));
    }
    if let Some(r) = run.get("qa_review").filter(|v| !v.is_null()) {
        let passed = r.get("passed").and_then(Value::as_bool).unwrap_or(false);
        let by = s(r.get("by"));
        // IT IS CALLED QA (operator 2026-09-28: "the name is not judge model, is QA"). Every other surface — the
        // rail, the pill, the settings toggle, the run log's `qa-review` lines — calls this gate QA; only the
        // story called it "The judge", so the one screen that explains a turn used a word for it that appears
        // nowhere else the operator looks.
        out.push(json!({ "name": "QA", "kind": if by == "machine" { "deterministic" } else { "model" }, "passed": passed,
            "score": r.get("score").cloned().unwrap_or(Value::Null), "detail": s(r.get("summary")) }));
    }
    out
}

/// `2026-09-24T11:43:10.313Z` → ms. The hook writes ISO-8601 with milliseconds; parsed by hand rather than
/// pulling in a date crate for one field. Returns None on anything that is not that shape. Pure — tested.
pub fn iso_ms(t: &str) -> Option<i64> {
    let b = t.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' { return None }
    let num = |a: usize, z: usize| t.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let ms = t.get(20..23).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
    // days from civil (1970-01-01), the usual algorithm
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 86_400 + h * 3600 + mi * 60 + sec) * 1000) + ms)
}

/// WHAT THE HOOK SAW, inside one run's window.
///
/// operator 2026-09-24: *"it said 0 files writen but is a lie."* The story read the tasks' traces, and an INLINE
/// run — a chat turn the router kept in one place — has no tasks at all, so every file it changed counted as
/// zero. The box's own `.antiloki/reads.jsonl` is the record that does not have that hole: the hook appends every
/// read, write, search and shell call as it happens, which is the same evidence the audit tab is built on.
/// Scoped to the run's window, because the log is the BOX's and other agents share the box.
fn hook_log(cwd: &str, from: i64, to: i64) -> (Vec<Value>, Vec<String>, Vec<String>, i64) {
    let p = std::path::Path::new(cwd).join(".antiloki").join("reads.jsonl");
    let Ok(text) = std::fs::read_to_string(&p) else { return (vec![], vec![], vec![], 0) };
    let (mut reads, mut writes) = (Vec::<String>::new(), Vec::<String>::new());
    let mut by_name: Map<String, Value> = Map::new();
    let mut total = 0i64;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let Some(t) = iso_ms(&s(v.get("t"))) else { continue };
        // a second of slack each side: the run's clock and the hook's are not the same clock
        if t < from - 1000 || (to > 0 && t > to + 1000) { continue }
        let tool = { let x = s(v.get("tool")); if x.is_empty() { s(v.get("op")) } else { x } };
        if tool.is_empty() { continue }
        total += 1;
        let e = by_name.entry(tool).or_insert(json!(0));
        *e = json!(e.as_i64().unwrap_or(0) + 1);
        let path = s(v.get("path"));
        if path.is_empty() { continue }
        match s(v.get("op")).as_str() { "write" => writes.push(path), "read" => reads.push(path), _ => {} }
    }
    for v in [&mut reads, &mut writes] { v.sort(); v.dedup(); }
    reads.retain(|r| !writes.contains(r));
    let tools = by_name.into_iter().map(|(name, count)| json!({ "name": name, "count": count })).collect::<Vec<_>>();
    (tools, reads, writes, total)
}

/// IS THIS RUN THE LAST THING THAT HAPPENED IN ITS BOX — the only case where the box's dirty tree can honestly
/// be read as this run's output. False when another run in the same `cwd` ended later, when this one never ended,
/// or when the box is unknown. Pure over the rows it is given — tested.
pub fn newest_in_box(rows: &[Value], run: &Value) -> bool {
    let cwd = s(run.get("cwd"));
    if cwd.is_empty() { return false }
    let ended = n(run.get("ended"));
    if ended <= 0.0 { return false }                       // still running: the tree is mid-flight, not a result
    let id = n(run.get("id"));
    !rows.iter().any(|r| s(r.get("cwd")) == cwd && n(r.get("id")) != id && n(r.get("ended")) > ended)
}

/// Every tool call across the tasks' traces: how many, of what, and which files they read and wrote.
fn tools_and_files(tasks: &[Value]) -> (Vec<Value>, Vec<String>, Vec<String>, i64) {
    let (mut reads, mut writes) = (Vec::<String>::new(), Vec::<String>::new());
    let mut by_name: Map<String, Value> = Map::new();
    let mut total = 0i64;
    for t in tasks {
        for c in arr(t.get("trace").and_then(|x| x.get("tools"))) {
            let name = s(c.get("name")); if name.is_empty() { continue }
            total += 1;
            let e = by_name.entry(name.clone()).or_insert(json!(0));
            *e = json!(e.as_i64().unwrap_or(0) + 1);
            let target = s(c.get("target"));
            if target.is_empty() { continue }
            let low = name.to_ascii_lowercase();
            if low.contains("read") || low.contains("grep") || low.contains("glob") || low.contains("search") { reads.push(target); }
            else if low.contains("write") || low.contains("edit") || low.contains("patch") { writes.push(target); }
        }
        for f in arr(t.get("changed")) { writes.push(s(Some(&f))); }
    }
    for v in [&mut reads, &mut writes] { v.sort(); v.dedup(); }
    // the writes are the truth about reads too: a file you wrote you also read
    reads.retain(|r| !writes.contains(r));
    let tools = by_name.into_iter().map(|(name, count)| json!({ "name": name, "count": count })).collect::<Vec<_>>();
    (tools, reads, writes, total)
}

/// THE MEASURED RAW BASELINE (operator: "time saved in comparation with raw frontier AI"). Not a model of what a
/// frontier agent *would* take — a row from the bench table where one actually did this shape of job: same job,
/// same size, no worker (`-`), a frontier planner. No such row, no claim: the story says so rather than inventing
/// a number, which is the only version of this figure worth showing anybody.
pub fn raw_baseline(rows: &[Value], job: &str, size: &str, domain: &str) -> Option<Value> {
    let pick = |dom: &str| -> Option<Value> {
        rows.iter().filter(|r| s(r.get("job")) == job && s(r.get("size")) == size && s(r.get("worker")) == "-"
                && (dom.is_empty() || s(r.get("domain")) == dom)
                && n(r.get("n")) > 0.0 && n(r.get("s")) > 0.0)
            // the strongest raw arm, not the luckiest: most samples first, then fastest
            .max_by(|a, b| (n(a.get("n")), -n(a.get("s"))).partial_cmp(&(n(b.get("n")), -n(b.get("s")))).unwrap_or(std::cmp::Ordering::Equal))
            .cloned()
    };
    pick(domain).or_else(|| pick("any")).or_else(|| pick(""))
}

/// WHEN THIS RUN'S WORK ACTUALLY HAPPENED.
///
/// For a planned run the stages bracket it and `ended` is the end. For an INLINE run — a chat turn the router
/// kept in one place — `ended` is stamped when the ROUTE is decided, and the agent then goes and does the work:
/// on run 132 the file was written twelve seconds AFTER `ended`, which is why the story said it wrote nothing.
/// The honest end is the next run filed against the same box, because that is the next turn beginning; with none,
/// now. `siblings` is the ledger; an empty slice falls back to the run's own clock. Pure — tested.
pub fn window_of(run: &Value, siblings: &[Value], now: i64) -> (i64, i64) {
    let from = n(run.get("started")).max(n(run.get("created"))) as i64;
    let ended = n(run.get("ended")) as i64;
    let planned = !arr(run.get("stages")).is_empty() || !arr(run.get("tasks")).is_empty();
    if planned && ended > 0 { return (from, ended) }
    let cwd = s(run.get("cwd"));
    let id = n(run.get("id")) as i64;
    let next = siblings.iter()
        .filter(|r| s(r.get("cwd")) == cwd && (n(r.get("id")) as i64) != id)
        .map(|r| n(r.get("created")) as i64)
        .filter(|t| *t > from)
        .min();
    (from, next.unwrap_or(now).max(ended))
}

/// WHAT THE QA SAID, IN THE OPEN (operator 2026-09-30: *"failed on qa? this should be more explicit, i didnt see anywhere that was happening a second round, also why did it fail? at the screen seemed perfect"*).
/// A run that QA sends back used to look, in the thread, like a run that was still working: the verdict, the reason and the second round lived in a log nobody opens. This is the verdict as data:
/// whether the QA is judging now, the claims it found unmet with its own reason, how many rounds it has spent, and the QA's part of the log in order. Pure - tested.
pub fn qa_of(run: &Value) -> Value {
    let events = arr(run.get("events"));
    let timeline: Vec<Value> = events.iter().filter(|e| { let k = s(e.get("kind")); k.starts_with("qa-") && !matches!(k.as_str(), "qa-evidence" | "qa-review" | "qa-baseline") })
        .map(|e| json!({ "kind": s(e.get("kind")), "text": s(e.get("text")).chars().take(400).collect::<String>(), "t": e.get("t").cloned().unwrap_or(Value::Null) })).collect();
    let gate = run.get("qa_gate").filter(|g| !g.is_null());
    let checks = gate.map(|g| arr(g.get("checks"))).unwrap_or_default();
    let unmet: Vec<Value> = checks.iter().filter(|c| c.get("met") == Some(&json!(false))).map(|c| json!({ "claim": s(c.get("claim")), "why": s(c.get("why")).chars().take(600).collect::<String>(), "by": s(c.get("by")) })).collect();
    let n = |k: &str| gate.and_then(|g| g.get(k)).and_then(Value::as_i64).unwrap_or(0);
    let sent_back = timeline.iter().filter(|e| s(e.get("kind")) == "qa-send-back").count();
    let stopped = timeline.iter().any(|e| s(e.get("kind")) == "qa-stopped");
    let pending = run.get("qa_pending") == Some(&json!(true));
    let state = if stopped { "stopped" } else if pending { "judging" } else if !unmet.is_empty() { "unmet" } else if gate.is_some() { "met" } else { "none" };
    json!({ "state": state, "pending": pending, "rounds": run.get("qa_rounds").cloned().unwrap_or(json!(0)), "sent_back": sent_back,
            "met": n("met"), "unmet_n": n("unmet"), "unproven": n("unproven"), "unmet": unmet, "timeline": timeline })
}
/// Which of Pause / Resume / Stop the thread offers for a run in this state. Stop is offered for anything alive - including a run that is DONE but still being judged, where it stops the QA.
/// Pure - tested.
pub fn controls_of(status: &str, qa_pending: bool) -> Value {
    let alive = matches!(status, "planning" | "awaiting-answers" | "awaiting-approval" | "running" | "joining" | "paused");
    let judging = status == "done" && qa_pending;
    json!({ "can_stop": alive || judging, "can_pause": status == "running", "can_resume": status == "paused", "stop_label": if judging { "stop the QA" } else { "stop" } })
}

/// The run, told. `rows` is the merges table (`merges::table()`), used only for the raw time baseline.
/// `siblings` is the rest of the ledger, used only to bound an inline turn's window (see `window_of`).
pub fn story_with(run: &Value, rows: &[Value], siblings: &[Value], now: i64) -> Value {
    story_inner(run, rows, window_of(run, siblings, now))
}
pub fn story(run: &Value, rows: &[Value]) -> Value {
    story_inner(run, rows, window_of(run, &[], crate::cli::now_ms()))
}
fn story_inner(run: &Value, rows: &[Value], window: (i64, i64)) -> Value {
    let tasks = arr(run.get("tasks"));
    let stage_rows = arr(run.get("stages"));
    let stages: Vec<Value> = stage_rows.iter().map(|st| stage_out(st, &tasks)).collect();
    let done = stages.iter().filter(|x| x.get("went_right") == Some(&json!(true))).count();
    let failed = stages.iter().filter(|x| x.get("went_right") == Some(&json!(false))).count();

    // WHO WORKED. The brain is the run's own engine — the one that decided; the hands are the workers it sent the
    // writing to. With no workers it is one model doing both, and the story says that instead of pretending.
    let pair = run.get("pair").filter(|v| !v.is_null());
    let brain_model = pair.map(|p| s(p.get("brain_model"))).unwrap_or_default();
    // A MODEL BELONGS TO ITS OWN SEAT (operator 2026-09-30, a screenshot: "Claude · gpt-5.6-terra · medium" - "it is not claude gpt, it is codex gpt"). The run's engine is the
    // PANE's engine; the model comes from the routed PAIR. Gluing the pane's engine to the pair's model named a GPT model with Anthropic's name. When the pair names a brain model,
    // the pair's brain is the seat that model runs on.
    let brain_engine = match pair.map(|p| s(p.get("brain"))).filter(|b| !b.is_empty() && !brain_model.is_empty()) { Some(b) => b, None => s(run.get("engine")) };
    let hands_engine = pair.map(|p| s(p.get("hands"))).filter(|x| !x.is_empty()).unwrap_or_else(|| s(run.get("workers")));
    let hands_model = pair.map(|p| s(p.get("hands_model"))).unwrap_or_default();
    let configured = !hands_engine.is_empty() && !(hands_engine == brain_engine && hands_model == brain_model);

    // CONFIGURED IS NOT THE SAME AS RAN (operator 2026-09-24: "Also said that open code wrote, but was all
    // sonnet"). The pane's merge says who WOULD write if the work were split; whether it was split is a fact
    // about this run, and the evidence for it is a task that actually executed. A router that decided "no
    // delegation — one job for the terminal that asked" leaves no such task, and the brain did all of it. Saying
    // otherwise credits a model that never ran, which is the one thing a report of what happened must not do.
    let ran_on_hands = tasks.iter().any(|t| {
        let e = t.get("trace").map(|tr| s(tr.get("engine"))).unwrap_or_default();
        !e.is_empty() && (e == hands_engine || e != brain_engine)
    });
    let split = configured && ran_on_hands;
    let who = json!({
        "brain": { "engine": brain_engine, "label": engine_label(&brain_engine, &brain_model),
                   "did": if split { "decided the plan, split the work and judged what came back" } else { "decided and wrote — this turn was not split" } },
        "hands": if split { json!({ "engine": hands_engine, "label": engine_label(&hands_engine, &hands_model), "did": "wrote the code, one file at a time" }) } else { Value::Null },
        "solo": !split,
        // a merge that did not split this time is worth saying, because the roster shows the agent AS a merge and
        // the operator would otherwise wonder which of the two they are reading about
        "configured": if configured { json!(format!("{} × {}", engine_label(&brain_engine, &brain_model), engine_label(&hands_engine, &hands_model))) } else { Value::Null },
        "note": if configured && !split {
            json!(format!("this agent is a merge, but the router kept the whole turn in one place — {} did all of it", engine_label(&brain_engine, &brain_model)))
        } else { Value::Null },
    });

    // THE ROUTER'S VERDICT, in its own words — `why` is already a sentence.
    let router = run.get("router").and_then(Value::as_bool).unwrap_or(false);
    let route = run.get("route").filter(|v| !v.is_null());
    let router_line = match (router, route) {
        (true, Some(r)) => {
            let tier = s(r.get("tier"));
            let why = s(r.get("why"));
            let sure = s(r.get("confidence"));
            format!("Heimdall sent this to the {tier} tier{}{}", if sure.is_empty() { String::new() } else { format!(" with {sure} confidence") },
                    if why.is_empty() { String::new() } else { format!(" — {why}") })
        }
        (true, None) => "Heimdall routed this turn".into(),
        _ => "The router was off — the pane's own tier did the work".into(),
    };

    let (mut tools, mut reads, mut writes, mut tool_calls) = tools_and_files(&tasks);
    if tool_calls == 0 && writes.is_empty() {
        // no task traces (an inline turn), so ask the box what the hook actually saw in this run's window
        let (t2, r2, w2, c2) = hook_log(&s(run.get("cwd")), window.0, window.1);
        tools = t2; reads = r2; writes = w2; tool_calls = c2;
    }
    // …AND IF NOTHING SAW A WRITE, ASK GIT — BUT ONLY FOR A RUN THAT IS STILL THE LAST WORD IN ITS BOX.
    //
    // The hole this fills is real (operator 2026-09-28: "0 writen … im starting to think the agent is read-only"):
    // an inline turn has no task traces, and the hook's log had recorded no write since 2026-09-24, so two silent
    // sources became a confident "0 files written" about a file the run had just edited.
    //
    // The first version of this fill was worse than the hole. `git status` is the dirty tree AT RENDER TIME, with
    // no author and no clock on it: a box shared by several agents lent one agent's edits to another's story, the
    // operator's own uncommitted work became the run's, and re-opening an old run re-attributed whatever happened
    // to be dirty that day. It also overwrote `reads` on the same evidence.
    //
    // So it speaks only where the attribution can be right: this run is the NEWEST in its box, it is finished,
    // and nothing else has run there since. Anything else keeps the honest empty list — a story that says nothing
    // is a gap in the bookkeeping; a story that says the wrong thing is a lie the operator cannot detect.
    if writes.is_empty() && tool_calls == 0 && newest_in_box(rows, run) {
        let cwd = s(run.get("cwd"));
        if !cwd.is_empty() {
            let changed = crate::agent::changed_files(&cwd);
            // reads are left exactly as they were: a file this run only READ, that someone else later touched,
            // is not a file this run wrote
            if !changed.is_empty() { writes = changed; }
        }
    }
    let skills: Vec<String> = { let mut v: Vec<String> = tasks.iter().flat_map(|t| arr(t.get("skills"))).map(|x| s(Some(&x))).collect(); v.sort(); v.dedup(); v };
    let specialists: Vec<String> = { let mut v: Vec<String> = tasks.iter().flat_map(|t| arr(t.get("specialists"))).map(|x| s(Some(&x))).collect(); v.sort(); v.dedup(); v };

    let ms_total = stage_rows.iter().map(|x| n(x.get("ms"))).sum::<f64>()
        .max(n(run.get("ended")) - n(run.get("started")).max(n(run.get("created"))));
    let sv = run.get("savings").filter(|v| !v.is_null());
    let (tin, tout) = (sv.map(|m| n(m.get("tokens_in"))).unwrap_or(0.0), sv.map(|m| n(m.get("tokens_out"))).unwrap_or(0.0));

    // TIME AGAINST A MEASURED RAW ARM — or an honest blank.
    let size = crate::merges::size_of(tasks.len(), writes.len() <= 1, s(run.get("goal")).len());
    let base = raw_baseline(rows, "chat", size, "any");
    let time = match base {
        Some(b) => {
            let raw_ms = n(b.get("s")) * 1000.0;
            let saved = raw_ms - ms_total;
            json!({ "ms": ms_total, "human": human_ms(ms_total), "baseline_ms": raw_ms, "baseline_human": human_ms(raw_ms),
                    "saved_ms": saved, "saved_human": human_ms(saved.abs()), "faster": saved > 0.0, "samples": b.get("n").cloned().unwrap_or(json!(0)),
                    "note": format!("against {} measured raw run(s) of this size — no merge, one frontier model doing all of it", n(b.get("n")).round() as i64) })
        }
        None => json!({ "ms": ms_total, "human": human_ms(ms_total), "baseline_ms": Value::Null,
                        "note": "no raw frontier run of this shape has been measured yet, so there is nothing honest to compare the clock against" }),
    };

    let money = sv.map(|m| json!({
        "spent": n(m.get("actual_usd")), "spent_human": human_usd(n(m.get("actual_usd"))),
        "baseline": n(m.get("baseline_usd")), "baseline_human": human_usd(n(m.get("baseline_usd"))),
        "saved": n(m.get("saved_usd")), "saved_human": human_usd(n(m.get("saved_usd"))),
        "saved_pct": n(m.get("saved_pct")), "baseline_name": s(m.get("baseline")),
        "line": format!("{} instead of {} — {:.0}% of the bill, gone, because the router put the writing on the cheap tier",
                        human_usd(n(m.get("actual_usd"))), human_usd(n(m.get("baseline_usd"))), n(m.get("saved_pct"))),
    })).unwrap_or(Value::Null);

    let status = s(run.get("status"));
    let gs = gates(run);
    let failed_gate = gs.iter().any(|g| g.get("passed") == Some(&json!(false)));
    let headline = {
        let what = s(run.get("goal"));
        let what = if what.chars().count() > 90 { format!("{}…", what.chars().take(90).collect::<String>()) } else { what };
        match (status.as_str(), failed_gate) {
            ("done", false) => format!("Done — {what}"),
            ("done", true) => format!("Done, but a gate says look again — {what}"),
            ("failed", _) => format!("Could not finish — {what}"),
            ("cancelled", _) => format!("Stopped — {what}"),
            ("awaiting-answers", _) => format!("Waiting for your answer — {what}"),
            // AN INLINE RUN IS ALREADY OVER (operator 2026-09-28: "the stats detail appears 'working', it never
            // appears finished"). `Inline` is what the router calls a turn it decided NOT to delegate — one job,
            // done in the pane's own session — and `goals.rs` constructs it already settled, with `started` and
            // `ended` both set and `delivered` true. It was simply missing from this match, so every such turn fell
            // to the catch-all and said "Working" forever. Nothing polls it back into line either: the pane treats
            // `inline` as settled and stops asking, so the word never changed on its own.
            ("inline", false) => format!("Done — {what}"),
            ("inline", true) => format!("Done, but a gate says look again — {what}"),
            _ => format!("Working — {what}"),
        }
    };

    let qa = qa_of(run);
    let controls = controls_of(&status, qa["pending"] == json!(true));
    json!({
        "run": run.get("id").cloned().unwrap_or(Value::Null),
        "qa": qa, "controls": controls, "confidence": run.get("confidence").cloned().unwrap_or(Value::Null),
        "goal": s(run.get("goal")), "status": status, "headline": headline,
        "who": who, "router": { "on": router, "line": router_line },
        "stages": stages, "gates": gs,
        "summary": {
            "stages": { "done": done, "failed": failed, "total": stages.len() },
            "tasks": { "total": tasks.len(), "done": tasks.iter().filter(|t| s(t.get("status")) == "done").count() },
            "files": { "read": reads.len(), "written": writes, "read_list": reads },
            "xray": { "skills": skills, "specialists": specialists, "tools": tools, "tool_calls": tool_calls },
            "tokens": { "in": tin, "out": tout, "total": tin + tout, "human": human_tokens(tin + tout) },
            "money": money, "time": time,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run75() -> Value { serde_json::from_str(include_str!("../fixtures/run75.json")).expect("the fixture parses") }

    /// AN INLINE RUN IS ALREADY OVER (operator 2026-09-28: "the stats detail appears 'working', it never appears
    /// finished"). `Inline` is the router's verdict that a turn did NOT need delegating — one job, done in the
    /// pane's own session — and `goals.rs` builds it already settled: `started` and `ended` both set, `delivered`
    /// true. It was simply missing from the headline's match, so it fell to the catch-all and said "Working".
    ///
    /// Nothing corrected it later, either: the pane counts `inline` as settled and stops polling the story, so the
    /// word the operator saw first was the word they kept.
    #[test]
    fn an_inline_turn_reads_as_finished_because_it_is() {
        // the gates come from `qa` / `qa_baseline` / `qa_review`, so they are what the test sets — the fixture
        // carries a failing judge of its own and would decide the answer for us
        let inline = |gate_passed: Option<bool>| {
            let mut r = run75();
            r["status"] = json!("inline");
            r["goal"] = json!("add a Clear button to the cart drawer");
            r["qa_baseline"] = Value::Null;
            r["qa_review"] = Value::Null;
            r["qa"] = match gate_passed {
                Some(p) => json!({ "passed": p, "score": if p { 1.0 } else { 0.0 }, "summary": "the claim", "by": "opus" }),
                None => Value::Null,
            };
            story(&r, &[])
        };
        let h = |v: &Value| v["headline"].as_str().unwrap_or("").to_string();

        assert!(h(&inline(None)).starts_with("Done — "), "an inline turn is finished: {:?}", h(&inline(None)));
        assert!(h(&inline(Some(true))).starts_with("Done — "), "{:?}", h(&inline(Some(true))));
        // …and a failing gate is still not hidden, exactly as it is not for a delegated run
        assert!(h(&inline(Some(false))).starts_with("Done, but a gate says look again"), "{:?}", h(&inline(Some(false))));
        // the word that was wrong must not come back
        for g in [None, Some(true), Some(false)] { assert!(!h(&inline(g)).starts_with("Working"), "{:?}", h(&inline(g))); }

        // a run that really IS still going keeps saying so
        let mut going = run75(); going["status"] = json!("running");
        assert!(story(&going, &[])["headline"].as_str().unwrap().starts_with("Working — "));
        let mut asking = run75();
        asking["status"] = json!("awaiting-answers");
        asking["goal"] = json!("pick the REST layer");
        assert!(story(&asking, &[])["headline"].as_str().unwrap().starts_with("Waiting for your answer — "), "{}", story(&asking, &[])["headline"]);
    }

    #[test]
    fn a_real_blueprint_run_reads_like_english() {
        let st = story(&run75(), &[]);
        assert_eq!(st["status"], json!("done"));
        // the judge failed this run, and the headline must not hide that behind "Done"
        assert!(st["headline"].as_str().unwrap().starts_with("Done, but a gate says look again"), "{}", st["headline"]);

        // WHO. The run decided on claude-code and sent the writing to opencode — two different houses, so not solo.
        assert_eq!(st["who"]["solo"], json!(false));
        assert_eq!(st["who"]["brain"]["label"], json!("Claude · the job's model"));
        assert_eq!(st["who"]["hands"]["label"], json!("OpenCode · the job's model"));

        // STAGES, in the operator's language rather than the pipeline's
        let stages = st["stages"].as_array().unwrap();
        assert_eq!(stages.len(), 15);
        assert_eq!(stages[0]["title"], json!("Checked the tools were installed"));
        assert_eq!(stages[0]["who"], json!("the package manager"));
        assert_eq!(stages[0]["machine"], json!(true));
        assert_eq!(stages[1]["title"], json!("Agreed what to build"));
        assert_eq!(stages[1]["who"], json!("Claude · mid tier"));
        assert_eq!(stages[4]["title"], json!("Proved the tests fail on empty files"));
        assert_eq!(stages[4]["ok"], json!(false), "the ledger's number is untouched");
        assert_eq!(stages[4]["went_right"], json!(true), "…but red IS the point of this one, and the story says so");
        assert_eq!(stages[4]["red_is_good"], json!(true));
        // a per-task check names the FILE it checked, not the task id
        assert_eq!(stages[5]["title"], json!("Checked the work on src/orders/totals.ts"));
        assert_eq!(stages[5]["who"], json!("the compiler and the test runner"));

        // GATES. Three of them here: the before-baseline, the tests, the judge.
        let gates = st["gates"].as_array().unwrap();
        assert_eq!(gates.len(), 3);
        assert_eq!(gates[0]["name"], json!("Before the change"));
        assert_eq!(gates[0]["passed"], json!(false));
        assert!(gates[0]["detail"].as_str().unwrap().contains("ALREADY red"), "a red baseline must be said out loud");
        assert_eq!(gates[1]["name"], json!("The tests"));
        assert_eq!(gates[1]["passed"], json!(true));
        assert_eq!(gates[2]["name"], json!("QA"), "the operator calls it QA, and so does every other screen");
        assert_eq!(gates[2]["passed"], json!(false));
        assert_eq!(gates[2]["score"], json!(60));

        // THE SUMMARY the operator asked for, item by item
        let sum = &st["summary"];
        assert_eq!(sum["stages"], json!({ "done": 14, "failed": 1, "total": 15 }), "one real failure — the first attempt at t2 — not two");
        assert_eq!(sum["tasks"], json!({ "total": 9, "done": 9 }));
        assert!(sum["files"]["written"].as_array().unwrap().len() >= 1, "the files it wrote");
        assert!(sum["xray"]["tool_calls"].as_i64().unwrap() > 0, "the tools it called");
        assert_eq!(sum["tokens"]["total"], json!(307865.0 + 54803.0));
        assert_eq!(sum["tokens"]["human"], json!("363k"));
        assert_eq!(sum["money"]["saved_human"], json!("$2.75"));
        assert!(sum["money"]["line"].as_str().unwrap().contains("95% of the bill"), "{}", sum["money"]["line"]);
        // no bench rows were handed in, so there is NO time claim — and the story says why
        assert_eq!(sum["time"]["baseline_ms"], json!(null));
        assert!(sum["time"]["note"].as_str().unwrap().contains("nothing honest to compare"));
    }

    #[test]
    fn the_raw_baseline_is_a_measured_row_or_nothing() {
        let rows = vec![
            json!({ "job": "chat", "size": "xxl", "planner": "claude-code:opus", "worker": "-", "n": 3, "s": 900.0, "domain": "any" }),
            json!({ "job": "chat", "size": "xxl", "planner": "claude-code:opus", "worker": "opencode:q", "n": 9, "s": 100.0, "domain": "any" }),
            json!({ "job": "chat", "size": "xxl", "planner": "codex:gpt", "worker": "-", "n": 1, "s": 300.0, "domain": "any" }),
        ];
        // a merge row is never the RAW baseline, however many samples it has
        let b = raw_baseline(&rows, "chat", "xxl", "any").unwrap();
        assert_eq!(b["n"], json!(3), "the raw arm with the most samples wins, not the fastest single run");
        assert!(raw_baseline(&rows, "chat", "s", "any").is_none(), "no row of that size — no claim");

        let mut run = run75();
        run["tasks"] = json!([{}, {}]);              // 2 tasks → size xxl
        let st = story(&run, &rows);
        assert_eq!(st["summary"]["time"]["baseline_ms"], json!(900_000.0));
        assert!(st["summary"]["time"]["note"].as_str().unwrap().contains("3 measured raw run(s)"));
        // THE COMPARISON IS ALLOWED TO LOSE. This run took longer than the measured raw arm, and the story says
        // `faster: false` rather than quietly dropping the row — a saving you only report when it is positive is
        // not a measurement, it is an advertisement.
        assert_eq!(st["summary"]["time"]["faster"], json!(false));
        let slower_raw = vec![json!({ "job": "chat", "size": "xxl", "planner": "claude-code:opus", "worker": "-", "n": 2, "s": 3600.0, "domain": "any" })];
        let st2 = story(&run, &slower_raw);
        assert_eq!(st2["summary"]["time"]["faster"], json!(true));
        assert_eq!(st2["summary"]["time"]["baseline_human"], json!("60 min"));
    }

    #[test]
    fn one_model_doing_both_is_told_as_one_model() {
        let mut run = run75();
        run["workers"] = json!("");
        let st = story(&run, &[]);
        assert_eq!(st["who"]["solo"], json!(true));
        assert_eq!(st["who"]["hands"], json!(null));
        assert_eq!(st["who"]["configured"], json!(null), "nothing was configured, so there is nothing to explain");
        assert_eq!(st["who"]["note"], json!(null));
        assert!(st["who"]["brain"]["did"].as_str().unwrap().contains("this turn was not split"));
    }

    // operator 2026-09-24: "Also said that open code wrote, but was all sonnet. The info must be real and match
    // what really happened."
    #[test]
    fn a_merge_that_did_not_split_does_not_get_to_claim_the_worker_ran() {
        // run 132's shape: the pane IS a merge, the router said "no delegation — one job for the terminal that
        // asked", and there is not a single task. OpenCode never started.
        let run = json!({
            "id": 132, "status": "inline", "engine": "claude-code", "cwd": "/nope", "goal": "add a product",
            "created": 1_790_000_000_000i64, "started": 1_790_000_000_000i64, "ended": 1_790_000_030_000i64,
            "tasks": [], "stages": [], "router": true,
            "route": { "tier": "low", "delegate": false, "why": "one job for the terminal that asked", "confidence": "high" },
            "pair": { "brain": "claude-code", "hands": "opencode", "brain_model": "sonnet", "hands_model": "opencode/deepseek-v4-flash" },
            "savings": { "tokens_in": 34773, "tokens_out": 502, "actual_usd": 0.037, "baseline_usd": 0.186, "saved_usd": 0.149, "saved_pct": 80.0, "baseline": "claude-code · opus", "tiers": ["inline:low"] },
        });
        let st = story(&run, &[]);
        assert_eq!(st["who"]["solo"], json!(true), "nothing ran on the hands, so nothing is credited to them");
        assert_eq!(st["who"]["hands"], json!(null));
        assert!(st["who"]["brain"]["did"].as_str().unwrap().contains("not split"));
        // …but the roster shows this agent AS a merge, so the story says why only one of the two appears
        assert_eq!(st["who"]["configured"], json!("Claude · Sonnet × OpenCode · DeepSeek v4 Flash"));
        assert!(st["who"]["note"].as_str().unwrap().contains("the router kept the whole turn in one place"));
        assert!(st["who"]["note"].as_str().unwrap().contains("Claude · Sonnet did all of it"));

        // and with a task that really ran on the worker, the split IS claimed
        let mut split = run.clone();
        split["tasks"] = json!([{ "id": "t1", "owns": ["a.ts"], "trace": { "engine": "opencode", "tools": [] } }]);
        let st2 = story(&split, &[]);
        assert_eq!(st2["who"]["solo"], json!(false));
        assert_eq!(st2["who"]["hands"]["label"], json!("OpenCode · DeepSeek v4 Flash"));
        assert_eq!(st2["who"]["note"], json!(null));
    }

    // operator 2026-09-24: "it said 0 files writen but is a lie."
    #[test]
    fn an_inline_turn_counts_the_files_the_hook_actually_saw() {
        let d = std::env::temp_dir().join(format!("antiloki-story-hook-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".antiloki")).unwrap();
        // 2026-09-24T11:43:10.313Z — the shape the hook writes
        let at = |t: &str, op: &str, tool: &str, path: &str| format!(r#"{{"t":"{t}","tool":"{tool}","path":"{path}","op":"{op}"}}"#);
        std::fs::write(d.join(".antiloki/reads.jsonl"), [
            at("2026-09-24T11:00:00.000Z", "write", "Edit", "src/before-the-run.ts"),   // another agent, earlier
            at("2026-09-24T11:43:05.000Z", "read", "Read", "src/products.ts"),
            at("2026-09-24T11:43:10.313Z", "write", "Edit", "src/products.ts"),
            at("2026-09-24T11:43:10.391Z", "write", "Edit", "src/products.ts"),          // the same file twice
            at("2026-09-24T11:43:06.249Z", "exec", "Bash", ""),
            at("2026-09-24T14:26:18.386Z", "write", "Edit", "src/products.ts"),          // a later turn, not ours
        ].join("\n")).unwrap();

        let from = iso_ms("2026-09-24T11:42:00.000Z").unwrap();
        let to = iso_ms("2026-09-24T11:44:00.000Z").unwrap();
        let run = json!({ "id": 132, "status": "inline", "engine": "claude-code", "cwd": d.to_string_lossy(),
            "goal": "add a product", "created": from, "started": from, "ended": to, "tasks": [], "stages": [] });
        // the next turn on the same box closes this one's window — without it the window runs to now and would
        // sweep in the 14:26 edit, which belongs to a later turn
        let next = json!({ "id": 133, "cwd": d.to_string_lossy(), "created": to });
        let sum = &story_with(&run, &[], &[next], to + 600_000)["summary"];
        assert_eq!(sum["files"]["written"], json!(["src/products.ts"]), "written once, however many edits it took");
        assert_eq!(sum["files"]["read"], json!(0), "a file it also wrote is not counted again as a read");
        assert_eq!(sum["xray"]["tool_calls"], json!(4), "only what happened inside this run's window");
        let names: Vec<String> = sum["xray"]["tools"].as_array().unwrap().iter().map(|t| s(t.get("name"))).collect();
        assert!(names.contains(&"Edit".to_string()) && names.contains(&"Bash".to_string()), "{names:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    // operator 2026-09-24: "it said 0 files writen but is a lie." The deeper half of that bug: an inline run's
    // `ended` is stamped when the ROUTE is decided, and the agent does the work AFTER it — on run 132 the file
    // landed twelve seconds past `ended`, outside every window the story knew how to ask about.
    #[test]
    fn an_inline_turns_window_runs_to_the_next_turn_not_to_its_own_ended() {
        let t0 = 1_790_259_951_906i64;
        let inline = json!({ "id": 132, "cwd": "/box", "created": t0, "ended": t0 + 14_000, "tasks": [], "stages": [] });
        // nothing else on the box: the window stays open to now, because the work may still be landing
        assert_eq!(window_of(&inline, &[], t0 + 600_000), (t0, t0 + 600_000));
        // the next run on the SAME box is the next turn beginning — that is where this one's window ends
        let siblings = vec![
            json!({ "id": 133, "cwd": "/box", "created": t0 + 90_000 }),
            json!({ "id": 134, "cwd": "/box", "created": t0 + 300_000 }),
            json!({ "id": 999, "cwd": "/somewhere/else", "created": t0 + 20_000 }),
            json!({ "id": 132, "cwd": "/box", "created": t0 }),
        ];
        assert_eq!(window_of(&inline, &siblings, t0 + 600_000), (t0, t0 + 90_000), "the nearest later run on this box, not another box's and not its own row");

        // a PLANNED run brackets its own work — its stages say when it ended, and it is not extended
        let planned = json!({ "id": 75, "cwd": "/box", "created": t0, "started": t0, "ended": t0 + 40_000,
                              "stages": [{ "name": "contract" }], "tasks": [{ "id": "t1" }] });
        assert_eq!(window_of(&planned, &siblings, t0 + 600_000), (t0, t0 + 40_000));
    }

    #[test]
    fn the_hooks_timestamps_parse_without_a_date_crate() {
        assert_eq!(iso_ms("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(iso_ms("2026-09-24T11:43:10.313Z"), Some(1_790_250_190_313));
        assert_eq!(iso_ms("2026-03-01T00:00:00.000Z").unwrap() - iso_ms("2026-02-28T00:00:00.000Z").unwrap(), 86_400_000, "2026 is not a leap year");
        assert_eq!(iso_ms("2024-03-01T00:00:00.000Z").unwrap() - iso_ms("2024-02-28T00:00:00.000Z").unwrap(), 2 * 86_400_000, "2024 is");
        assert_eq!(iso_ms("not a time"), None);
        assert_eq!(iso_ms(""), None);
    }

    #[test]
    fn durations_and_counts_are_read_at_a_glance() {
        assert_eq!(human_ms(730.0), "730 ms");
        assert_eq!(human_ms(43_200.0), "43 s");
        assert_eq!(human_ms(262_707.0), "4 min 23 s");
        assert_eq!(human_ms(120_000.0), "2 min");
        assert_eq!(human_tokens(362_668.0), "363k");
        assert_eq!(human_tokens(1_500_000.0), "1.5M");
        assert_eq!(human_usd(0.156), "$0.16");
        assert_eq!(human_usd(0.0009), "$0.0009");
        assert_eq!(engine_label("opencode", "deepseek-v4-flash"), "OpenCode · DeepSeek v4 Flash");
        // an OpenCode id carries its provider; the house is already said, so "OpenCode opencode/…" is wrong
        assert_eq!(engine_label("opencode", "opencode/deepseek-v4-flash"), "OpenCode · DeepSeek v4 Flash");
        assert_eq!(engine_label("opencode", "opencode/"), "OpenCode · the job's model");
        assert_eq!(engine_label("claude-code", "claude-sonnet-5"), "Claude · Sonnet 5");
        // the operator's screenshot (2026-09-30): the pane is Claude's, the model is GPT's - it is Codex's GPT
        assert!(engine_label("claude-code", "gpt-5.6-terra@medium").starts_with("Codex"), "{}", engine_label("claude-code", "gpt-5.6-terra@medium"));
        assert!(!engine_label("claude-code", "gpt-5.6-terra@medium").contains("Claude"));
        assert_eq!(engine_label("claude-code", ""), "Claude · the job's model", "a side whose model the job picks still names its seat");
    }
}

#[cfg(test)]
mod newest_in_box_tests {
    use super::*;
    fn r(id: i64, cwd: &str, ended: i64) -> Value { json!({ "id": id, "cwd": cwd, "ended": ended }) }
    #[test]
    fn the_boxs_dirty_tree_is_only_this_runs_when_nothing_ran_after_it() {
        let rows = vec![r(1, "/box", 100), r(2, "/box", 200), r(3, "/other", 900)];
        assert!(newest_in_box(&rows, &rows[1]), "the last run in its box may read the tree");
        assert!(!newest_in_box(&rows, &rows[0]), "an older run must not claim what came after it");
        assert!(newest_in_box(&rows, &rows[2]), "a different box is not competition");
        assert!(!newest_in_box(&rows, &json!({ "id": 9, "cwd": "/box", "ended": 0 })), "still running: the tree is mid-flight");
        assert!(!newest_in_box(&rows, &json!({ "id": 9, "cwd": "", "ended": 500 })), "no box, no claim");
    }
}

#[cfg(test)]
mod qa_in_the_open_tests {
    use super::*;
    #[test]
    fn a_run_the_qa_sent_back_says_so_with_the_reason_and_the_round() {
        let run = json!({ "status": "running", "qa_pending": true, "qa_rounds": 1,
            "qa_gate": { "met": 0, "unmet": 1, "unproven": 0, "checks": [{ "task": "t1", "claim": "the ask is done as written", "met": false, "why": "6 avatars exist but 5 checks on them failed", "by": "opus" }] },
            "events": [ { "kind": "qa-evidence", "text": "x", "t": 1 }, { "kind": "qa-gate-fail", "text": "0 met · 1 unmet", "t": 2 }, { "kind": "qa-send-back", "text": "[Q272.1] QA sent 1 task(s) back", "t": 3 }, { "kind": "work", "text": "ignored", "t": 4 } ] });
        let q = qa_of(&run);
        assert_eq!(q["state"], "judging"); assert_eq!(q["sent_back"], 1); assert_eq!(q["rounds"], 1);
        assert_eq!(q["unmet"][0]["why"], "6 avatars exist but 5 checks on them failed");
        assert_eq!(q["timeline"].as_array().unwrap().len(), 2, "the QA's own lines only, in order: {q}");
        assert_eq!(qa_of(&json!({ "status": "done", "events": [] }))["state"], "none");
        assert_eq!(qa_of(&json!({ "status": "done", "qa_gate": { "met": 1, "unmet": 0, "unproven": 0, "checks": [{ "met": true, "claim": "c", "why": "w" }] } }))["state"], "met");
        assert_eq!(qa_of(&json!({ "status": "done", "qa_pending": false, "events": [{ "kind": "qa-stopped", "text": "the operator stopped the QA", "t": 1 }] }))["state"], "stopped");
    }
    #[test]
    fn the_thread_offers_stop_for_anything_alive_and_pause_only_between_tasks() {
        assert_eq!(controls_of("running", false), json!({ "can_stop": true, "can_pause": true, "can_resume": false, "stop_label": "stop" }));
        assert_eq!(controls_of("paused", false)["can_resume"], json!(true));
        assert_eq!(controls_of("done", true), json!({ "can_stop": true, "can_pause": false, "can_resume": false, "stop_label": "stop the QA" }), "a run that is done but still judged can be stopped, and it says what it stops");
        assert_eq!(controls_of("done", false)["can_stop"], json!(false));
        assert_eq!(controls_of("failed", false)["can_stop"], json!(false));
        assert_eq!(controls_of("planning", false)["can_stop"], json!(true));
    }
}
