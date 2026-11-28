//! FUEL, FROM THE RECORD (operator 2026-09-24: *"fuel still not working, also fuel should show complete info
//! about saved tokens and money by using antiloki heimdall"* · *"on fuel add comparations and analyzis"*).
//!
//! It was built on per-pane meter files scraped from each CLI's status line, and after the meters became per-pane
//! a claude agent that had not spoken since showed `— tok · $ —`: the file did not exist yet, so the honest
//! answer was nothing, and nothing is what the operator saw.
//!
//! The workspace already knows better. `brain.ai_calls` has every call it ever made — provider, model, tier,
//! tokens in and out, cost, duration, outcome — written when the call happened. That is the fuel gauge, and it
//! cannot read empty because an agent is idle. The rate-limit WINDOWS still come from the engines, because only
//! the CLI knows how much of a subscription is left; everything else comes from the rows.
use serde_json::{json, Value};

fn n(v: Option<&Value>) -> f64 { v.and_then(Value::as_f64).unwrap_or(0.0) }
fn s(v: Option<&Value>) -> String { v.and_then(Value::as_str).unwrap_or("").to_string() }

/// `claude-code:haiku` → `claude`; `opencode:deepseek-v4-flash` → `opencode`. The three houses the UI groups by,
/// and anything else under its own name rather than silently folded into one of them. Pure — tested.
pub fn house_of(model_id: &str) -> String {
    let head = model_id.split(':').next().unwrap_or("").trim().to_ascii_lowercase();
    match head.as_str() {
        "claude-code" | "claude" | "anthropic" => "claude".into(),
        "codex" | "openai" => "codex".into(),
        "opencode" | "opencode-direct" => "opencode".into(),
        "" => "unknown".into(),
        other => other.to_string(),
    }
}

/// CODEX'S OWN WINDOWS, FROM ITS OWN SESSION FILES (operator 2026-09-30: "on limits at the top switcher, it is not showing that i got the week limit or
/// session limit"). Every Codex turn writes the account's `rate_limits` into its rollout file - `primary` is the 5-hour window, `secondary` the weekly one,
/// each with `used_percent` and `resets_at` - and this reads the newest of them, the way the Claude meter is read. A window whose reset time has passed is
/// not shown: the number is from before the reset and would be the lie. Pure over the text - tested.
pub fn codex_windows_in(text: &str, now_s: i64) -> Vec<Value> {
    for line in text.lines().rev() {
        if !line.contains("\"rate_limits\"") { continue }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let Some(rl) = v.pointer("/payload/rate_limits").or_else(|| v.get("rate_limits")) else { continue };
        let mut out = vec![];
        for key in ["primary", "secondary"] {
            let Some(w) = rl.get(key).filter(|w| !w.is_null()) else { continue };
            let resets = w.get("resets_at").and_then(Value::as_i64);
            if resets.map(|r| r < now_s).unwrap_or(false) { continue }
            // THE WINDOW IS NAMED BY ITS LENGTH (2026-11-21: Codex's meter on this account is a 30-day window, 43200 minutes, and was drawn as "week 16%" from a session of five days before): 5h, week (7 days), month (anything longer)
            let mins = w.get("window_minutes").and_then(Value::as_i64);
            let label = match mins { Some(m) if m <= 360 => "5h", Some(m) if m <= 12_000 => "week", Some(_) => "month", None => if key == "primary" { "5h" } else { "week" } };
            out.push(json!({ "house": "codex", "label": label, "used_pct": w.get("used_percent"), "resets_at": resets }));
        }
        if !out.is_empty() { return out }
    }
    vec![]
}
/// The newest Codex rollout files on this box (today's folders first), read from their tails.
pub fn codex_windows(now_s: i64) -> Vec<Value> {
    use std::io::{Read, Seek, SeekFrom};
    let home = std::env::var("CODEX_HOME").ok().filter(|h| !h.is_empty()).map(std::path::PathBuf::from)
        .or_else(|| std::env::var("HOME").ok().map(|h| std::path::PathBuf::from(h).join(".codex")));
    let Some(root) = home.map(|h| h.join("sessions")) else { return vec![] };
    let newest = |dir: &std::path::Path| -> Vec<std::path::PathBuf> {
        let mut v: Vec<_> = std::fs::read_dir(dir).map(|rd| rd.filter_map(Result::ok).map(|e| e.path()).collect()).unwrap_or_default();
        v.sort(); v.reverse(); v
    };
    let mut files = vec![];
    'outer: for y in newest(&root) { for m in newest(&y) { for d in newest(&m) { for f in newest(&d) { files.push(f); if files.len() >= 12 { break 'outer } } } } }
    for f in files {
        let Ok(mut fh) = std::fs::File::open(&f) else { continue };
        let len = fh.metadata().map(|m| m.len()).unwrap_or(0);
        let from = len.saturating_sub(400_000);
        if fh.seek(SeekFrom::Start(from)).is_err() { continue }
        let mut buf = String::new();
        let mut bytes = vec![]; if fh.read_to_end(&mut bytes).is_err() { continue }
        buf.push_str(&String::from_utf8_lossy(&bytes));
        let w = codex_windows_in(&buf, now_s);
        if !w.is_empty() { return w }
    }
    vec![]
}

/// THE SEATS WHOSE CLI PUBLISHES NO QUOTA WINDOW (operator 2026-10-01, item 6: "Cursor, Gemini and Grok report no remaining-quota window anywhere antiloki reads"). Investigated 2026-10-01
/// and the answer is no local source for any of the three - see "Limits: what each seat exposes" in HANDOFF-WHEN-CLAUDE-ENDS.md. The only thing antiloki knows about them is what it sent them.
pub const NO_QUOTA_FEED: [&str; 3] = ["cursor", "gemini", "grok"];

/// WHAT WE SENT A SEAT THAT REPORTS NO QUOTA, in the last 5 hours and 7 days, from the serve's audit table (`audit::calls_since_hours` rows: every CLI call and pane turn). One row per seat that made a call in the
/// week, in `limits` shape but with NO `used_pct`: it is a count of what antiloki sent through this serve (not calls made by hand in the CLI or by another tool), not a share of an allowance, and nothing downstream (`cli::set_windows`, the donut, the
/// router) may read it as one. A seat with no call in the week gets no row - "not running" says that better than two zeros. Pure - tested.
pub fn own_use(last_5h: &[Value], last_7d: &[Value]) -> Vec<Value> {
    let sum = |rows: &[Value], house: &str, k: &str| rows.iter().filter(|r| r.get("house").and_then(Value::as_str) == Some(house)).map(|r| n(r.get(k))).sum::<f64>();
    NO_QUOTA_FEED.iter().filter_map(|h| {
        let (c7, t7) = (sum(last_7d, h, "calls"), sum(last_7d, h, "tokens"));
        if c7 <= 0.0 { return None }
        Some(json!({ "house": h, "label": "own", "calls_5h": sum(last_5h, h, "calls"), "tokens_5h": sum(last_5h, h, "tokens"),
            "calls_7d": c7, "tokens_7d": t7, "note": "no quota data from this CLI" }))
    }).collect()
}

/// THE ALLOWANCE FROM ONE NUMBER THE OPERATOR CAN READ OFF THE SEAT'S OWN DASHBOARD (operator 2026-10-09: "I want the less work for the user"): if antiloki sent the seat `spent_usd` (list price, last 7 days)
/// and the dashboard says `used_pct` of the week is gone, the allowance is `spent / (pct / 100)`. `None` when the percentage is not in (0, 100] or antiloki has sent that seat nothing to calibrate on. Pure - tested.
pub fn allowance_from_pct(spent_usd: f64, used_pct: f64) -> Option<f64> {
    if !(used_pct > 0.0 && used_pct <= 100.0) || !(spent_usd > 0.0) || !spent_usd.is_finite() { return None }
    Some(spent_usd / (used_pct / 100.0))
}

/// THE ESTIMATED WEEK OF A SEAT THAT PUBLISHES NO WINDOW (operator 2026-10-09: Cursor was almost at its weekly limit and nothing showed it). For each `(engine, allowance in $)` the operator declared: the list-price
/// cost of what antiloki sent that seat in the last 7 days, as a share of the allowance - a `week` row with `used_pct`, so the preview, the limits window and the router (`cli::seat_limit`: out at
/// `WINDOW_FULL_PCT`, routed around until it falls back) all treat it like a published window. `estimated: true` says it is antiloki's own arithmetic, never the vendor's figure. Pure - tested.
pub fn allowance_rows(last_7d: &[Value], allowances: &[(String, f64)]) -> Vec<Value> {
    allowances.iter().filter(|(_, usd)| *usd > 0.0).map(|(key, usd)| {
        let house = house_of(key);
        let used: f64 = last_7d.iter().filter(|r| r.get("house").and_then(Value::as_str) == Some(house.as_str())).map(|r| n(r.get("cost"))).sum();
        let pct = (used / usd * 100.0).clamp(0.0, 100.0);
        json!({ "house": house, "label": "week", "used_pct": pct, "resets_at": Value::Null, "estimated": true, "used_usd": used, "of_usd": usd,
                "note": format!("antiloki's own estimate: ${used:.2} sent in 7 days of the ${usd:.2} you declared - the seat publishes no figure") })
    }).collect()
}

/// WHAT HEIMDALL SAVED, over the runs that settled. `actual` is what the chosen models cost; `baseline` is the
/// same tokens priced at one high-tier agent doing all of it. Both are API-equivalent — a subscription pays $0
/// metered — and the label says so wherever this appears. Pure — tested.
pub fn savings_of(runs: &[Value]) -> Value {
    let (mut actual, mut baseline, mut tin, mut tout) = (0.0, 0.0, 0.0, 0.0);
    let (mut routed, mut unrouted) = (0usize, 0usize);
    for r in runs {
        let Some(sv) = r.get("savings").filter(|v| !v.is_null()) else { continue };
        if r.get("router").and_then(Value::as_bool).unwrap_or(false) { routed += 1 } else { unrouted += 1 }
        actual += n(sv.get("actual_usd"));
        baseline += n(sv.get("baseline_usd"));
        tin += n(sv.get("tokens_in"));
        tout += n(sv.get("tokens_out"));
    }
    let saved = (baseline - actual).max(0.0);
    let pct = if baseline > 0.0 { saved / baseline * 100.0 } else { 0.0 };
    // THE PROOF, run by run (operator 2026-09-24: "the fuel window most important info is to show comparations
    // how its been save in tokens and money by using heimdall router with real proof based"). A total anybody can
    // assert is not proof; these are the rows it is made of, each naming the ask, what the chosen models actually
    // cost, what one frontier agent doing the same tokens would have cost, and which tiers did the work.
    let mut proof: Vec<Value> = runs.iter().filter_map(|r| {
        let sv = r.get("savings").filter(|v| !v.is_null())?;
        let (a, b) = (n(sv.get("actual_usd")), n(sv.get("baseline_usd")));
        if b <= 0.0 { return None }
        let goal = s(r.get("goal"));
        Some(json!({ "id": r.get("id"), "goal": goal.chars().take(90).collect::<String>(),
            "actual": a, "baseline": b, "saved": (b - a).max(0.0), "pct": (b - a).max(0.0) / b * 100.0,
            "tokens": n(sv.get("tokens_in")) + n(sv.get("tokens_out")),
            "against": s(sv.get("baseline")), "tiers": sv.get("tiers").cloned().unwrap_or(json!([])),
            "ended": r.get("ended").cloned().unwrap_or(Value::Null),
            "router": r.get("router").and_then(Value::as_bool).unwrap_or(false) }))
    }).collect();
    proof.sort_by(|x, y| n(y.get("saved")).partial_cmp(&n(x.get("saved"))).unwrap_or(std::cmp::Ordering::Equal));
    proof.truncate(25);
    json!({ "proof": proof,
        "runs": routed + unrouted, "routed": routed, "unrouted": unrouted,
        "actual": actual, "baseline": baseline, "saved": saved, "pct": pct,
        "tokens_in": tin, "tokens_out": tout, "tokens": tin + tout,
        "line": if baseline <= 0.0 { "nothing routed yet — the router has not had a settled run to price".to_string() }
                else { format!("{} run(s) cost ${actual:.2} where one frontier agent doing the same {} tokens would have cost ${baseline:.2} — Heimdall kept ${saved:.2}, {pct:.0}% of the bill. API-equivalent; a subscription pays $0 metered.",
                               routed + unrouted, human_tokens(tin + tout)) } })
}

pub fn human_tokens(t: f64) -> String {
    if t >= 1_000_000.0 { format!("{:.1}M", t / 1_000_000.0) } else if t >= 1000.0 { format!("{:.0}k", t / 1000.0) } else { format!("{}", t.round() as i64) }
}

/// THE COMPARISON the operator asked for: this window against the one before it, per house. A number with no
/// yesterday beside it is a number nobody can act on. Pure — tested.
pub fn compare(now: &[Value], before: &[Value]) -> Vec<Value> {
    // NOTHING BEFORE = NOTHING TO COMPARE (operator 2026-10-09: the preview said "claude $19.98 was $0.00 ▲" for every engine): the call record only reaches back a few days, so the window before this one
    // was simply not recorded - an unmeasured past is not a $0.00 one, and "up 100 %" from nothing is invented growth. No window before = no comparison drawn.
    if before.iter().all(|r| n(r.get("calls")) <= 0.0 && n(r.get("cost")) <= 0.0) { return vec![] }
    let pick = |rows: &[Value], house: &str| -> (f64, f64, f64) {
        rows.iter().filter(|r| s(r.get("house")) == house)
            .fold((0.0, 0.0, 0.0), |(c, t, k), r| (c + n(r.get("cost")), t + n(r.get("tokens")), k + n(r.get("calls"))))
    };
    let mut houses: Vec<String> = now.iter().chain(before.iter()).map(|r| s(r.get("house"))).collect();
    houses.sort(); houses.dedup();
    houses.into_iter().filter(|h| !h.is_empty()).map(|h| {
        let (c0, t0, k0) = pick(now, &h);
        let (c1, t1, _) = pick(before, &h);
        let delta = if c1 > 0.0 { (c0 - c1) / c1 * 100.0 } else if c0 > 0.0 { 100.0 } else { 0.0 };
        json!({ "house": h, "cost": c0, "tokens": t0, "calls": k0, "was": c1, "delta_pct": delta,
                "trend": if c1 <= 0.0 && c0 <= 0.0 { "flat" } else if delta > 15.0 { "up" } else if delta < -15.0 { "down" } else { "flat" } })
    }).collect()
}

/// A sentence about the window — what dominates it, and whether anything is worth looking at. Pure — tested.
pub fn analysis(rows: &[Value], saved: &Value) -> String {
    if rows.is_empty() { return "nothing has run in this window yet.".into() }
    let total: f64 = rows.iter().map(|r| n(r.get("cost"))).sum();
    let calls: f64 = rows.iter().map(|r| n(r.get("calls"))).sum();
    let failed: f64 = rows.iter().map(|r| n(r.get("failed"))).sum();
    let mut by = rows.to_vec();
    by.sort_by(|a, b| n(b.get("cost")).partial_cmp(&n(a.get("cost"))).unwrap_or(std::cmp::Ordering::Equal));
    let top = &by[0];
    let share = if total > 0.0 { n(top.get("cost")) / total * 100.0 } else { 0.0 };
    let mut out = format!("{} call(s), ${total:.2}. {} is {:.0}% of it.", calls.round() as i64, s(top.get("model")), share);
    if failed > 0.0 { out.push_str(&format!(" {} did not succeed — those cost money and produced nothing.", failed.round() as i64)); }
    let kept = n(saved.get("saved"));
    if kept > 0.0 { out.push_str(&format!(" Against one frontier agent doing the same work: ${kept:.2} kept ({:.0}%).", n(saved.get("pct")))); }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_id_names_its_house_and_never_guesses() {
        assert_eq!(house_of("claude-code:haiku"), "claude");
        assert_eq!(house_of("claude-code:claude-sonnet-5"), "claude");
        assert_eq!(house_of("codex:gpt-5.6-terra"), "codex");
        assert_eq!(house_of("opencode:deepseek-v4-flash"), "opencode");
        assert_eq!(house_of(""), "unknown");
        // something new is itself, not quietly filed under claude
        assert_eq!(house_of("mistral:large"), "mistral");
    }

    #[test]
    fn what_heimdall_saved_is_arithmetic_and_labelled_as_api_equivalent() {
        let run = |actual: f64, baseline: f64, tin: f64, tout: f64, router: bool| json!({
            "router": router, "savings": { "actual_usd": actual, "baseline_usd": baseline, "tokens_in": tin, "tokens_out": tout } });
        let sv = savings_of(&[run(0.25, 1.28, 27_000.0, 30_000.0, true), run(0.16, 2.91, 300_000.0, 55_000.0, true), json!({ "savings": null })]);
        assert_eq!(sv["runs"], json!(2), "a run with no savings row is not a run that saved nothing");
        assert_eq!(sv["routed"], json!(2));
        assert!((sv["saved"].as_f64().unwrap() - 3.78).abs() < 1e-9);
        assert!((sv["pct"].as_f64().unwrap() - 90.21).abs() < 0.01, "{}", sv["pct"]);
        assert_eq!(sv["tokens"], json!(412_000.0));
        // THE PROOF ROWS — the total is made of these, biggest saving first, and each one names its own numbers
        let proof = sv["proof"].as_array().unwrap();
        assert_eq!(proof.len(), 2, "a run with no savings row contributes no proof");
        assert_eq!(proof[0]["saved"], json!(2.75), "the bigger saving leads");
        assert!((proof[0]["pct"].as_f64().unwrap() - 94.50).abs() < 0.01);
        assert_eq!(proof[0]["tokens"], json!(355_000.0));
        assert_eq!(proof[1]["saved"], json!(1.03));
        let line = sv["line"].as_str().unwrap();
        assert!(line.contains("Heimdall kept $3.78"));
        assert!(line.contains("412k tokens"));
        assert!(line.contains("a subscription pays $0 metered"), "the yardstick is always named: {line}");

        // nothing settled yet says so rather than printing a proud zero
        assert!(savings_of(&[])["line"].as_str().unwrap().contains("has not had a settled run"));
        assert_eq!(savings_of(&[])["saved"], json!(0.0));
    }

    #[test]
    fn the_comparison_puts_a_yesterday_beside_every_number() {
        let row = |house: &str, cost: f64, tokens: f64, calls: f64| json!({ "house": house, "cost": cost, "tokens": tokens, "calls": calls });
        let now = vec![row("claude", 2.0, 100.0, 10.0), row("opencode", 0.10, 900.0, 40.0)];
        let before = vec![row("claude", 1.0, 50.0, 5.0), row("codex", 0.50, 10.0, 2.0)];
        let c = compare(&now, &before);
        let get = |h: &str| c.iter().find(|x| s(x.get("house")) == h).cloned().unwrap();

        let cl = get("claude");
        assert_eq!(cl["cost"], json!(2.0)); assert_eq!(cl["was"], json!(1.0));
        assert_eq!(cl["delta_pct"], json!(100.0)); assert_eq!(cl["trend"], json!("up"));
        // a house that ran before and not now is still shown — disappearing is information
        let cx = get("codex");
        assert_eq!(cx["cost"], json!(0.0)); assert_eq!(cx["was"], json!(0.5)); assert_eq!(cx["trend"], json!("down"));
        // and one that is new this window is not a division by zero
        assert_eq!(get("opencode")["delta_pct"], json!(100.0));
        assert_eq!(compare(&[], &[]).len(), 0);
    }

    #[test]
    fn the_analysis_names_what_dominates_and_what_was_wasted() {
        let rows = vec![
            json!({ "model": "claude-code:opus", "cost": 3.0, "calls": 4.0, "failed": 1.0 }),
            json!({ "model": "opencode:flash", "cost": 1.0, "calls": 40.0, "failed": 0.0 }),
        ];
        let sv = savings_of(&[json!({ "router": true, "savings": { "actual_usd": 4.0, "baseline_usd": 10.0, "tokens_in": 1.0, "tokens_out": 1.0 } })]);
        let a = analysis(&rows, &sv);
        assert!(a.contains("44 call(s), $4.00"));
        assert!(a.contains("claude-code:opus is 75% of it"), "{a}");
        assert!(a.contains("1 did not succeed"), "a failed call cost money and produced nothing: {a}");
        assert!(a.contains("$6.00 kept (60%)"));
        assert_eq!(analysis(&[], &sv), "nothing has run in this window yet.");
    }
}

#[cfg(test)]
mod codex_window_tests {
    use super::*;
    #[test]
    fn codex_reports_its_session_and_weekly_windows_and_a_passed_window_is_not_shown() {
        let line = |p: &str, sec: &str| format!("{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\"rate_limits\":{{\"limit_id\":\"codex\",\"primary\":{p},\"secondary\":{sec}}}}}}}");
        assert_eq!(codex_windows_in(&line("{\"used_percent\":16.0,\"window_minutes\":43200,\"resets_at\":9999999999}", "null"), 1)[0]["label"], "month", "a 30-day window is not a week");
        let text = format!("{}\n{}\n", line("null", "null"), line("{\"used_percent\":100.0,\"window_minutes\":300,\"resets_at\":2000}", "{\"used_percent\":78.0,\"window_minutes\":10080,\"resets_at\":9000}"));
        let w = codex_windows_in(&text, 1000);
        assert_eq!(w.len(), 2, "{w:?}");
        assert_eq!((w[0]["label"].as_str(), w[0]["used_pct"].as_f64()), (Some("5h"), Some(100.0)));
        assert_eq!((w[1]["label"].as_str(), w[1]["used_pct"].as_f64()), (Some("week"), Some(78.0)));
        // the session window has reset by 3000; the weekly one has not
        let w = codex_windows_in(&text, 3000);
        assert_eq!(w.len(), 1); assert_eq!(w[0]["label"], "week");
        assert!(codex_windows_in("not json\n", 0).is_empty());
    }
}

#[cfg(test)]
mod own_use_tests {
    use super::*;
    fn row(house: &str, calls: i64, tokens: i64) -> Value { json!({ "model": format!("{house}:x"), "house": house, "calls": calls, "tokens": tokens }) }
    #[test]
    fn a_seat_without_a_quota_feed_shows_what_was_sent_never_a_percentage() {
        let last_5h = vec![row("cursor", 2, 175), row("cursor", 1, 25), row("claude", 9, 999)];
        let last_7d = vec![row("cursor", 5, 4000), row("gemini", 1, 10), row("claude", 90, 9999)];
        let w = own_use(&last_5h, &last_7d);
        assert_eq!(w.iter().map(|x| x["house"].as_str().unwrap()).collect::<Vec<_>>(), ["cursor", "gemini"], "claude has a real window; grok made no call this week");
        assert_eq!((w[0]["calls_5h"].as_f64(), w[0]["tokens_5h"].as_f64(), w[0]["calls_7d"].as_f64(), w[0]["tokens_7d"].as_f64()), (Some(3.0), Some(200.0), Some(5.0), Some(4000.0)));
        assert_eq!((w[1]["calls_5h"].as_f64(), w[1]["calls_7d"].as_f64()), (Some(0.0), Some(1.0)), "called this week, not in the last 5 hours");
        // the guard that matters: no `used_pct`, so `cli::set_windows` drops the row and the router never reads a count as a share of an allowance
        assert!(w.iter().all(|x| x.get("used_pct").is_none() && x["label"] == "own" && x["note"] == "no quota data from this CLI"), "{w:?}");
        assert!(own_use(&[], &[]).is_empty());
    }
    #[test]
    fn no_recorded_window_before_means_no_comparison_not_a_zero_one() {
        let now = vec![json!({ "house": "claude", "cost": 19.98, "tokens": 5.0e6, "calls": 250.0 })];
        assert!(compare(&now, &[]).is_empty(), "the window before was never recorded");
        assert!(compare(&now, &[json!({ "house": "claude", "cost": 0.0, "tokens": 0.0, "calls": 0.0 })]).is_empty(), "rows of nothing are nothing");
        let c = compare(&now, &[json!({ "house": "claude", "cost": 10.0, "tokens": 1.0, "calls": 5.0 })]);
        assert_eq!(c.len(), 1); assert_eq!(c[0]["trend"], "up", "a measured prior window still compares");
    }

    #[test]
    fn one_dashboard_percentage_and_antilokis_own_spend_give_the_allowance() {
        assert!((allowance_from_pct(24.25, 90.0).unwrap() - 26.944).abs() < 0.001, "90 % gone after $24.25 -> $26.94 in all");
        assert_eq!(allowance_from_pct(10.0, 100.0), Some(10.0));
        assert_eq!(allowance_from_pct(10.0, 0.0), None); assert_eq!(allowance_from_pct(10.0, 120.0), None); assert_eq!(allowance_from_pct(10.0, f64::NAN), None);
        assert_eq!(allowance_from_pct(0.0, 50.0), None, "nothing sent, nothing to calibrate on");
    }
    #[test]
    fn a_declared_allowance_turns_antilokis_own_week_into_a_window_row_the_router_can_read() {
        let last7 = vec![json!({ "house": "cursor", "calls": 400, "tokens": 1.0e7, "cost": 23.13 }), json!({ "house": "gemini", "calls": 90, "tokens": 4.0e6, "cost": 3.4 })];
        let rows = allowance_rows(&last7, &[("cursor".to_string(), 25.0), ("grok".to_string(), 10.0)]);
        assert_eq!(rows.len(), 2);
        let c = &rows[0]; assert_eq!((c["house"].as_str(), c["label"].as_str(), c["estimated"].as_bool()), (Some("cursor"), Some("week"), Some(true)));
        assert!((c["used_pct"].as_f64().unwrap() - 92.52).abs() < 0.01, "{c}");
        assert_eq!(rows[1]["used_pct"].as_f64(), Some(0.0), "a seat with an allowance and no spend is at 0, not missing");
        assert_eq!(allowance_rows(&last7, &[("cursor".to_string(), 10.0)])[0]["used_pct"].as_f64(), Some(100.0), "capped at the whole allowance");
        assert!(allowance_rows(&last7, &[]).is_empty() && allowance_rows(&last7, &[("cursor".to_string(), 0.0)]).is_empty(), "nothing declared = nothing tracked");
    }

}
