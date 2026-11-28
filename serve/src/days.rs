//! THE OPERATOR'S DAYS — the timeline, consolidated (operator 2026-09-24: "the timeline also is confuse, it is a
//! deterministic log, this is nice, but should be also a consolidation of users episodes and days").
//!
//! The sealed chain is the record and stays exactly as it is: every row, hashed, in order. But nobody reads a
//! chain to remember what they did on Tuesday. This is the same facts folded up one level — a day, the episodes
//! that closed in it, the runs that settled, what they cost — so the question "what happened yesterday" has an
//! answer that is one screen long instead of four hundred rows.
//!
//! Both halves are already written by something else: episodes come from the chat logs (`chat_pane::episodes_of`)
//! and runs from the goal ledger. Nothing here measures anything.
use serde_json::{json, Map, Value};

fn s(v: Option<&Value>) -> String { v.and_then(Value::as_str).unwrap_or("").to_string() }
fn n(v: Option<&Value>) -> i64 { v.and_then(Value::as_i64).unwrap_or(0) }
fn f(v: Option<&Value>) -> f64 { v.and_then(Value::as_f64).unwrap_or(0.0) }

/// `1790080321730` → `2026-09-24`, in LOCAL time — a day is the operator's day, not UTC's. Pure — tested.
pub fn day_of(ms: i64, tz_offset_min: i64) -> String {
    let local = ms + tz_offset_min * 60_000;
    let days = local.div_euclid(86_400_000);
    // 1970-01-01 was a Thursday; civil-from-days, the usual algorithm
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}
/// "today" · "yesterday" · the date. Pure — tested.
pub fn day_label(day: &str, today: &str, yesterday: &str) -> String {
    if day == today { "today".into() } else if day == yesterday { "yesterday".into() } else { day.to_string() }
}

/// THE DAY IN ONE SENTENCE. Written to be true when the day was quiet, which is most days: "nothing closed" is a
/// real answer and better than a row of zeroes. Pure — tested.
pub fn headline(eps: usize, runs: usize, agents: usize, cost: f64, saved: f64) -> String {
    if eps == 0 && runs == 0 { return "nothing was closed or settled — the work that happened is still open".into() }
    let mut bits = vec![];
    if eps > 0 { bits.push(format!("{eps} episode{} closed", if eps == 1 { "" } else { "s" })); }
    if runs > 0 { bits.push(format!("{runs} run{} settled", if runs == 1 { "" } else { "s" })); }
    let who = if agents > 1 { format!(" across {agents} agents") } else { String::new() };
    let money = if cost > 0.0 {
        if saved > 0.0 { format!(" · ${cost:.2} spent, ${saved:.2} the router did not") } else { format!(" · ${cost:.2}") }
    } else { String::new() };
    format!("{}{who}{money}", bits.join(" and "))
}

/// The days, newest first. `chats` is what `cockpit/chat/list` returns (one row per chat, `episodes` inside);
/// `runs` is the goal ledger. `tz_offset_min` is the viewer's own offset, so a day breaks where they think it does.
/// The same fold, plus what the repo's own databases say (operator 2026-09-24: "those sqlites gives us abilities
/// to query and generate stats deterministically with all prompts and all responses, this is what timeline can
/// retrieve to show real history sacred as well").
///
/// `filed` is `memory_consolidation`'s episodes — the ones that outlived their box — and `calls` is the per-day
/// roll-up of `brain.ai_calls`. A filed episode WINS over the same episode read from a log: the log is a box's
/// file and the row is the repo's record, so when they disagree the record is the one that was written down.
pub fn days_with(chats: &[Value], runs: &[Value], filed: &[Value], calls: &[Value], tz_offset_min: i64, now_ms: i64) -> Value {
    let mut out = days(chats, runs, tz_offset_min, now_ms);
    // the filed episodes, merged in by (agent, n) so a box still on disk does not produce a second copy
    for e in filed {
        let t = n(e.get("last_ms"));
        let day = if t > 0 { day_of(t, tz_offset_min) } else { s(e.get("last")).split(' ').next().unwrap_or("").to_string() };
        if day.is_empty() { continue }
        let Some(ds) = out.get_mut("days").and_then(Value::as_array_mut) else { break };
        let slot = match ds.iter().position(|d| s(d.get("day")) == day) {
            Some(i) => &mut ds[i],
            None => { ds.push(json!({ "day": day, "episodes": [], "runs": [], "agents": [] })); ds.last_mut().expect("just pushed") }
        };
        let (agent, num_) = (s(e.get("agent")), n(e.get("n")));
        if let Some(a) = slot.get_mut("agents").and_then(Value::as_array_mut) {
            if !a.iter().any(|x| x.as_str().map(|t| t.eq_ignore_ascii_case(&agent)) == Some(true)) { a.push(json!(agent)); }
        }
        if let Some(eps) = slot.get_mut("episodes").and_then(Value::as_array_mut) {
            let same = |x: &Value| n(x.get("n")) == num_ && s(x.get("agent")).eq_ignore_ascii_case(&agent);
            let row = json!({ "agent": agent, "pane_id": e.get("pane_id"), "n": num_, "t": t,
                "title": e.get("title"), "subtitle": e.get("subtitle"), "turns": e.get("turns"),
                "engine": e.get("engine"), "story": e.get("story"), "filed": true,
                "branch": e.get("branch"), "merged_into": e.get("merged_into") });
            match eps.iter().position(same) { Some(i) => eps[i] = row, None => eps.push(row) }
        }
    }
    // the deterministic per-day call record, attached to the day it belongs to
    for c in calls {
        let day = s(c.get("day"));
        let Some(ds) = out.get_mut("days").and_then(Value::as_array_mut) else { break };
        match ds.iter().position(|d| s(d.get("day")) == day) {
            Some(i) => { ds[i]["calls"] = c.clone(); }
            None => { let mut d = json!({ "day": day, "episodes": [], "runs": [], "agents": [] }); d["calls"] = c.clone(); ds.push(d); }
        }
    }
    // the headlines and totals are recomputed, because days may have gained episodes or appeared entirely
    let today = day_of(now_ms, tz_offset_min);
    let yesterday = day_of(now_ms - 86_400_000, tz_offset_min);
    if let Some(ds) = out.get_mut("days").and_then(Value::as_array_mut) {
        for v in ds.iter_mut() {
            for k in ["episodes", "runs"] {
                if let Some(a) = v.get_mut(k).and_then(Value::as_array_mut) { a.sort_by_key(|x| std::cmp::Reverse(n(x.get("t")))); }
            }
            let eps = v.get("episodes").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
            let rs = v.get("runs").and_then(Value::as_array).cloned().unwrap_or_default();
            let agents = v.get("agents").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
            let (cost, saved): (f64, f64) = (rs.iter().map(|x| f(x.get("cost"))).sum(), rs.iter().map(|x| f(x.get("saved"))).sum());
            // when the repo's own call record covers this day, ITS cost is the honest one — every call, not only
            // the ones that happened to be filed as a goal run
            let cost = { let c = f(v.get("calls").and_then(|c| c.get("cost"))); if c > 0.0 { c } else { cost } };
            let day = s(v.get("day"));
            v["label"] = json!(day_label(&day, &today, &yesterday));
            v["headline"] = json!(headline(eps, rs.len(), agents, cost, saved));
            v["totals"] = json!({ "episodes": eps, "runs": rs.len(), "agents": agents, "cost": cost, "saved": saved,
                "tokens": f(v.get("calls").and_then(|c| c.get("tokens_in"))) + f(v.get("calls").and_then(|c| c.get("tokens_out"))),
                "calls": n(v.get("calls").and_then(|c| c.get("calls"))) });
        }
        ds.sort_by(|a, b| s(b.get("day")).cmp(&s(a.get("day"))));
    }
    out
}

pub fn days(chats: &[Value], runs: &[Value], tz_offset_min: i64, now_ms: i64) -> Value {
    let mut by_day: Map<String, Value> = Map::new();
    fn bucket<'a>(m: &'a mut Map<String, Value>, day: &str) -> &'a mut Value {
        m.entry(day.to_string()).or_insert_with(|| json!({ "day": day, "episodes": [], "runs": [], "agents": [] }))
    }
    let push = |v: &mut Value, key: &str, item: Value| { if let Some(a) = v.get_mut(key).and_then(Value::as_array_mut) { a.push(item); } };
    // ONE AGENT, ONE NAME. An episode knows the agent's display title ("Daniboy"); a run knows its own run key
    // ("daniboy", or a timestamp). Matched case-insensitively so the same agent is not counted twice in a day for
    // having been reached two different ways — and the display title wins, because that is what the operator
    // named it. A run key that matches nothing (the generated timestamps) is not an agent and is not counted.
    let note_agent = |v: &mut Value, who: &str, is_title: bool| {
        let who = who.trim();
        if who.is_empty() || (!is_title && who.chars().all(|c| c.is_ascii_digit() || c == '-')) { return }
        let Some(a) = v.get_mut("agents").and_then(Value::as_array_mut) else { return };
        let low = who.to_ascii_lowercase();
        match a.iter().position(|x| x.as_str().map(|t| t.to_ascii_lowercase()) == Some(low.clone())) {
            Some(i) => { if is_title { a[i] = json!(who); } }
            None => a.push(json!(who)),
        }
    };

    for c in chats {
        let agent = { let t = s(c.get("title")); if t.trim().is_empty() { format!("chat #{}", n(c.get("pane_id"))) } else { t } };
        for e in c.get("episodes").and_then(Value::as_array).cloned().unwrap_or_default() {
            // an episode belongs to the day it CLOSED on — that is when the operator finished the thought
            let closed = n(e.get("last_t"));
            if closed <= 0 { continue }
            let day = day_of(closed, tz_offset_min);
            let b = bucket(&mut by_day, &day);
            note_agent(b, &agent, true);
            let title = { let t = s(e.get("title")); if t.trim().is_empty() { s(e.get("first_ask")) } else { t } };
            push(b, "episodes", json!({
                "agent": agent, "pane_id": c.get("pane_id"), "n": e.get("n"), "t": closed,
                "title": title.chars().take(140).collect::<String>(), "subtitle": e.get("subtitle"),
                "turns": e.get("turns"), "engine": e.get("engine"), "story": e.get("story"),
            }));
        }
    }
    for r in runs {
        // a run belongs to the day it ENDED; one still running belongs to nobody's summary yet
        let ended = n(r.get("ended"));
        if ended <= 0 { continue }
        let day = day_of(ended, tz_offset_min);
        let b = bucket(&mut by_day, &day);
        note_agent(b, &s(r.get("parent")), false);
        let sv = r.get("savings").filter(|v| !v.is_null());
        push(b, "runs", json!({
            "id": r.get("id"), "goal": s(r.get("goal")).chars().take(140).collect::<String>(),
            "status": r.get("status"), "t": ended, "ms": ended - n(r.get("started")).max(n(r.get("created"))),
            "cost": sv.map(|x| f(x.get("actual_usd"))).unwrap_or(0.0),
            "saved": sv.map(|x| f(x.get("saved_usd"))).unwrap_or(0.0),
            "tokens": sv.map(|x| f(x.get("tokens_in")) + f(x.get("tokens_out"))).unwrap_or(0.0),
        }));
    }

    let today = day_of(now_ms, tz_offset_min);
    let yesterday = day_of(now_ms - 86_400_000, tz_offset_min);
    let mut out: Vec<Value> = by_day.into_iter().map(|(day, mut v)| {
        for k in ["episodes", "runs"] {
            if let Some(a) = v.get_mut(k).and_then(Value::as_array_mut) { a.sort_by_key(|x| std::cmp::Reverse(n(x.get("t")))); }
        }
        let eps = v.get("episodes").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
        let rs = v.get("runs").and_then(Value::as_array).cloned().unwrap_or_default();
        let agents = v.get("agents").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
        let cost: f64 = rs.iter().map(|x| f(x.get("cost"))).sum();
        let saved: f64 = rs.iter().map(|x| f(x.get("saved"))).sum();
        let tokens: f64 = rs.iter().map(|x| f(x.get("tokens"))).sum();
        v["label"] = json!(day_label(&day, &today, &yesterday));
        v["headline"] = json!(headline(eps, rs.len(), agents, cost, saved));
        v["totals"] = json!({ "episodes": eps, "runs": rs.len(), "agents": agents, "cost": cost, "saved": saved, "tokens": tokens });
        v
    }).collect();
    out.sort_by(|a, b| s(b.get("day")).cmp(&s(a.get("day"))));
    json!({ "days": out, "today": today })
}

#[cfg(test)]
mod tests {
    use super::*;

    const UTC: i64 = 0;
    // 2026-09-24 12:00:00 UTC
    const NOON: i64 = 1_790_251_200_000;

    #[test]
    fn a_day_is_the_operators_day_not_utcs() {
        assert_eq!(day_of(NOON, UTC), "2026-09-24");
        // 01:00 UTC in Brazil (-180 min) is still the 23rd — the operator's own midnight is where the day breaks
        let one_am = NOON - 11 * 3_600_000;
        assert_eq!(day_of(one_am, UTC), "2026-09-24");
        assert_eq!(day_of(one_am, -180), "2026-09-23");
        assert_eq!(day_of(0, UTC), "1970-01-01");
        assert_eq!(day_label("2026-09-24", "2026-09-24", "2026-09-23"), "today");
        assert_eq!(day_label("2026-09-23", "2026-09-24", "2026-09-23"), "yesterday");
        assert_eq!(day_label("2026-09-20", "2026-09-24", "2026-09-23"), "2026-09-20");
    }

    #[test]
    fn a_quiet_day_says_so_rather_than_showing_a_row_of_zeroes() {
        assert_eq!(headline(0, 0, 0, 0.0, 0.0), "nothing was closed or settled — the work that happened is still open");
        assert_eq!(headline(1, 0, 1, 0.0, 0.0), "1 episode closed");
        assert_eq!(headline(3, 2, 2, 1.2, 0.0), "3 episodes closed and 2 runs settled across 2 agents · $1.20");
        assert_eq!(headline(0, 1, 1, 0.16, 2.75), "1 run settled · $0.16 spent, $2.75 the router did not");
    }

    #[test]
    fn the_days_fold_up_the_episodes_and_the_runs_that_actually_finished() {
        let day = 86_400_000i64;
        let chats = vec![json!({ "pane_id": 24, "title": "Daniboy", "episodes": [
            { "n": 1, "last_t": NOON, "title": "The product card", "subtitle": "what it shows", "turns": 6, "engine": "claude-code", "first_ask": "add a product" },
            { "n": 2, "last_t": NOON - day, "title": "", "first_ask": "the cart totals", "turns": 3, "engine": "claude-code" },
            { "n": 3, "last_t": 0, "title": "never closed", "turns": 1 },
        ] })];
        let runs = vec![
            json!({ "id": 75, "parent": "daniboy", "goal": "orders feature", "status": "done", "created": NOON - 1000, "started": NOON - 900, "ended": NOON,
                    "savings": { "actual_usd": 0.16, "saved_usd": 2.75, "tokens_in": 300.0, "tokens_out": 63.0 } }),
            json!({ "id": 76, "parent": "joao", "goal": "still going", "status": "running", "created": NOON, "ended": 0 }),
        ];
        let out = days(&chats, &runs, UTC, NOON);
        let ds = out["days"].as_array().unwrap();
        assert_eq!(ds.len(), 2, "two days have something that finished; the open episode and the running run belong to neither");
        assert_eq!(ds[0]["day"], json!("2026-09-24"));
        assert_eq!(ds[0]["label"], json!("today"));
        assert_eq!(ds[0]["headline"], json!("1 episode closed and 1 run settled · $0.16 spent, $2.75 the router did not"));
        assert_eq!(ds[0]["totals"]["tokens"], json!(363.0));
        assert_eq!(ds[0]["episodes"][0]["title"], json!("The product card"));
        assert_eq!(ds[0]["episodes"][0]["agent"], json!("Daniboy"));
        assert_eq!(ds[0]["runs"][0]["id"], json!(75));

        assert_eq!(ds[1]["label"], json!("yesterday"));
        // an episode with no title of its own is named by the ask that opened it, never left blank
        assert_eq!(ds[1]["episodes"][0]["title"], json!("the cart totals"));
        assert_eq!(ds[1]["headline"], json!("1 episode closed"));
        assert_eq!(out["today"], json!("2026-09-24"));
    }

    #[test]
    fn an_agent_is_counted_once_a_day_however_much_it_did() {
        let chats = vec![json!({ "pane_id": 1, "title": "Daniboy", "episodes": [
            { "n": 1, "last_t": NOON, "title": "a", "turns": 1 }, { "n": 2, "last_t": NOON - 60_000, "title": "b", "turns": 1 },
        ] })];
        let runs = vec![
            json!({ "id": 1, "parent": "daniboy", "goal": "g", "status": "done", "created": NOON, "ended": NOON }),
            // a generated run key names no agent and must not inflate the count
            json!({ "id": 2, "parent": "1789948182636-233373452", "goal": "g", "status": "done", "created": NOON, "ended": NOON }),
        ];
        let out = days(&chats, &runs, UTC, NOON);
        assert_eq!(out["days"][0]["totals"]["agents"], json!(1), "one agent, reached by its title and by its run key");
        assert_eq!(out["days"][0]["agents"][0], json!("Daniboy"), "and it keeps the name the operator gave it");
        assert_eq!(out["days"][0]["totals"]["episodes"], json!(2));
        assert_eq!(out["days"][0]["episodes"][0]["title"], json!("a"), "newest first inside the day");
    }

    // operator 2026-09-24: the repo's sqlite is where the real history lives, and the timeline reads it.
    #[test]
    fn a_filed_episode_outlives_its_box_and_beats_the_log_it_came_from() {
        let day_ms = 86_400_000i64;
        // the box is gone, so the chat log has nothing — but the episode was filed in the repo's memory
        let filed = vec![json!({ "agent": "Daniboy", "pane_id": 24, "n": 2, "last_ms": NOON, "title": "The orders feature",
                                 "subtitle": "what it does", "turns": 9, "engine": "claude-code", "branch": "antiloki/daniboy/ep-2", "merged_into": "main" })];
        let out = days_with(&[], &[], &filed, &[], UTC, NOON);
        let d = &out["days"][0];
        assert_eq!(d["episodes"][0]["title"], json!("The orders feature"));
        assert_eq!(d["episodes"][0]["filed"], json!(true));
        assert_eq!(d["episodes"][0]["merged_into"], json!("main"));
        assert_eq!(d["totals"]["agents"], json!(1));

        // the same episode ALSO readable from a log that still exists: one row, the filed one
        let chats = vec![json!({ "pane_id": 24, "title": "Daniboy", "episodes": [
            { "n": 2, "last_t": NOON, "title": "a stale title from the log", "turns": 9 }] })];
        let both = days_with(&chats, &[], &filed, &[], UTC, NOON);
        assert_eq!(both["days"][0]["episodes"].as_array().unwrap().len(), 1, "not two copies of one episode");
        assert_eq!(both["days"][0]["episodes"][0]["title"], json!("The orders feature"), "the record wins over the log");
    }

    #[test]
    fn the_days_money_is_every_call_the_repo_recorded_not_only_the_filed_runs() {
        let calls = vec![json!({ "day": "2026-09-24", "calls": 41, "cost": 3.75, "tokens_in": 120_000, "tokens_out": 8_000, "failed": 2, "models": 4 })];
        // a goal run that knows about $0.16 of it — the record knows about all of it
        let runs = vec![json!({ "id": 1, "parent": "dani", "goal": "g", "status": "done", "created": NOON, "ended": NOON,
                                "savings": { "actual_usd": 0.16, "saved_usd": 2.75, "tokens_in": 300.0, "tokens_out": 63.0 } })];
        let out = days_with(&[], &runs, &[], &calls, UTC, NOON);
        let d = &out["days"][0];
        assert_eq!(d["totals"]["calls"], json!(41));
        assert_eq!(d["totals"]["cost"], json!(3.75), "the deterministic record, not the runs' share of it");
        assert_eq!(d["totals"]["tokens"], json!(128_000.0));
        assert!(d["headline"].as_str().unwrap().contains("$3.75"));
        assert_eq!(d["calls"]["failed"], json!(2));
    }

    #[test]
    fn nothing_at_all_is_an_empty_list_not_a_fabricated_day() {
        assert_eq!(days(&[], &[], UTC, NOON)["days"].as_array().unwrap().len(), 0);
    }
}
