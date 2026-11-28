//! A TEAM'S MEMORY IS ITS SAGA (the operator 2026-11-15: "sagas are our memory, in every agent, every tool call, every team's internal saga; this is what was missing for the Leader to remember"): the Leader's terminal is a model with a context window, and
//! a new terminal (a restart, a new thread view, a model switch) knows nothing. What the team has done is in its saga (its episodes, an event for every node and every change and every answer), so a Leader that starts is HANDED that saga, in words,
//! and can ask for it again at any time. Read-only; built from the saga and the run files, never from a model.
use serde_json::Value;
use std::fmt::Write as _;

fn ago(now: i64, t: i64) -> String { let m = ((now - t).max(0) / 60_000) as i64; if m < 1 { "just now".into() } else if m < 60 { format!("{m} min ago") } else if m < 2880 { format!("{} h ago", m / 60) } else { format!("{} days ago", m / 1440) } }
fn clip(s: &str, n: usize) -> String { let t = s.replace('\n', " "); let t = t.trim(); if t.chars().count() > n { format!("{}…", t.chars().take(n - 1).collect::<String>()) } else { t.to_string() } }

/// The memory of a team, as text for its Leader. Blocking.
pub fn recall(root: &str, team: &str) -> String {
    let now = crate::cli::now_ms(); let slug = crate::team_graph::slug(team); let mut o = String::new();
    let g = crate::team_graph_run::load_graph(root, team).ok();
    let _ = writeln!(o, "[team memory · {}] This is what the team has done, read from its saga. Use it to CONTINUE the work: do not start the team over, and do not ask the person what this already answers.", g.as_ref().map(|g| g.team.clone()).unwrap_or_else(|| team.to_string()));
    if let Some(g) = &g { let mut names: Vec<String> = g.nodes.iter().filter(|(_, n)| !matches!(n.kind, crate::team_graph::Kind::Leader)).map(|(k, n)| crate::team_graph::title_of(k, n)).collect(); names.sort(); let _ = writeln!(o, "THE TEAM NOW ({} steps, budget ${:.2}): {}", names.len(), g.budget_usd, names.join(", ")); }
    let runs = crate::team_graph_run::runs_of(root, team);
    if runs.is_empty() { let _ = writeln!(o, "RUNS: none yet."); } else {
        let _ = writeln!(o, "RUNS (newest first):");
        let mut rs = runs.clone(); rs.sort_by(|a, b| b["started_ms"].as_i64().cmp(&a["started_ms"].as_i64()));
        for r in rs.iter().take(8) { let st = if r["status"].is_object() { r["status"]["status"].as_str().unwrap_or("").to_string() } else { r["status"].as_str().unwrap_or("").to_string() }; let why = r["status"]["why"].as_str().map(|w| format!(" ({})", clip(w, 90))).unwrap_or_default();
            let _ = writeln!(o, "- {} · {}{} · ${:.3} · {}{}", clip(r["ask"].as_str().unwrap_or(""), 110), st, why, r["spent_usd"].as_f64().unwrap_or(0.0), ago(now, r["started_ms"].as_i64().unwrap_or(0)), if r["needs_you"] == Value::Bool(true) { " · WAITING FOR THE PERSON" } else { "" }); } }
    // the saga's own events: what the person answered, what failed, what changed: the lines worth remembering (not a start or a finish of every node)
    if let Ok(c) = crate::work::open(root) {
        if let Ok(id) = c.query_row("SELECT id FROM sagas WHERE plan_ref = ?1", [format!("team:{slug}")], |r| r.get::<_, i64>(0)) {
            let rows = antiloki_rs::db_json::query_rows(&c, "SELECT t_ms, actor, kind, text FROM events WHERE saga_id = ?1 AND kind NOT IN ('start','done','loop','resume','tool') ORDER BY id DESC LIMIT 40", &[serde_json::json!(id)]).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
            let pick = |kinds: &[&str]| -> Vec<&Value> { rows.iter().filter(|e| kinds.contains(&e["kind"].as_str().unwrap_or(""))).take(8).collect() };
            let line = |e: &Value| format!("- {} · {}: {}", ago(now, e["t_ms"].as_i64().unwrap_or(0)), e["actor"].as_str().unwrap_or(""), clip(e["text"].as_str().unwrap_or(""), 150));
            let ans = pick(&["answer", "human"]); if !ans.is_empty() { let _ = writeln!(o, "WHAT THE PERSON DECIDED OR WAS ASKED (newest first):"); for e in ans { let _ = writeln!(o, "{}", line(e)); } }
            let bad = pick(&["failed", "timeout", "cap", "budget", "conflict", "land_failed", "restore_failed"]); if !bad.is_empty() { let _ = writeln!(o, "WHAT WENT WRONG:"); for e in bad { let _ = writeln!(o, "{}", line(e)); } }
            let rest: Vec<&Value> = rows.iter().filter(|e| !matches!(e["kind"].as_str().unwrap_or(""), "answer" | "human" | "failed" | "timeout" | "cap" | "budget" | "conflict" | "land_failed" | "restore_failed")).take(10).collect(); if !rest.is_empty() { let _ = writeln!(o, "OTHER THINGS THAT HAPPENED (changes to the team, merges, notes):"); for e in rest { let _ = writeln!(o, "{}", line(e)); } }
        } }
    let _ = writeln!(o, "To read this again at any time: antiloki_team_audit with op `recall` and your team's name."); o
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_team_with_no_history_still_gives_a_memory_that_says_so() { let tmp = std::env::temp_dir().join(format!("antiloki-recall-{}", std::process::id())); let _ = std::fs::remove_dir_all(&tmp); std::fs::create_dir_all(&tmp).unwrap(); let m = recall(&tmp.to_string_lossy(), "Nothing Yet"); assert!(m.contains("[team memory") && m.contains("RUNS: none yet") && m.contains("antiloki_team_audit"), "{m}"); let _ = std::fs::remove_dir_all(&tmp); }
    #[test]
    fn time_is_said_in_words() { assert_eq!(ago(10 * 60_000, 9 * 60_000 + 30_000), "just now"); assert_eq!(ago(10 * 60_000, 4 * 60_000), "6 min ago"); assert_eq!(ago(5 * 3_600_000, 0), "5 h ago"); assert_eq!(clip("a\nb   ", 10), "a b"); assert_eq!(clip("abcdefghij", 5), "abcd…"); }
}
