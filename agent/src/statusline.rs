//! `antiloki-agent statusline` — Claude Code's status line for a code-agent pane.
//!
//! USAGE ALWAYS VISIBLE: Claude Code hands the status line the session's numbers on every assistant
//! message (context window, cost, the plan's 5-hour / 7-day windows). This prints one line inside
//! the terminal — `Opus 5 · ctx 12% · $0.42 · 5h 23% · week 41% · antiloki pack 7 files` — and writes
//! `<worktree>/.antiloki/usage.json` so the pane chip (and `antiloki_usage`) shows the same numbers
//! outside the terminal. No network, no env. A port of antiloki-BE/src/mcp/antiloki-statusline.ts.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn find_pack_root(start: &Path) -> Option<PathBuf> {
    let mut dir = std::fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
    for _ in 0..12 {
        if dir.join(".antiloki").exists() { return Some(dir); }
        match dir.parent() { Some(p) => dir = p.to_path_buf(), None => break }
    }
    None
}

/// The engine's status JSON → the one shape the pane reads.
pub fn normalize(input: &Value) -> Value {
    let mut limits = Vec::new();
    let rl = input.get("rate_limits").cloned().unwrap_or(json!({}));
    for (key, label) in [("five_hour", "5h"), ("seven_day", "week"), ("spend_limit", "spend")] {
        if let Some(w) = rl.get(key) {
            if let Some(p) = w.get("used_percentage").and_then(Value::as_f64) {
                limits.push(json!({ "label": label, "used_pct": p, "resets_at": w.get("resets_at").cloned().unwrap_or(Value::Null) }));
            }
        }
    }
    let cw = input.get("context_window").cloned().unwrap_or(json!({}));
    let model = input.get("model").and_then(|m| m.get("display_name").or_else(|| m.get("id"))).cloned().unwrap_or(Value::Null);
    let cost = input.get("cost").cloned().unwrap_or(json!({}));
    json!({
        "engine": "claude-code",
        "t": crate::hook::now_iso(),
        "model": model,
        "context": { "used_pct": cw.get("used_percentage").cloned().unwrap_or(Value::Null), "input_tokens": cw.get("total_input_tokens").cloned().unwrap_or(Value::Null), "output_tokens": cw.get("total_output_tokens").cloned().unwrap_or(Value::Null), "size": cw.get("context_window_size").cloned().unwrap_or(Value::Null) },
        "cost_usd": cost.get("total_cost_usd").cloned().unwrap_or(Value::Null),
        "limits": limits,
        "lines_added": cost.get("total_lines_added").cloned().unwrap_or(Value::Null),
        "lines_removed": cost.get("total_lines_removed").cloned().unwrap_or(Value::Null),
    })
}

fn pct(v: Option<&Value>) -> String { match v.and_then(Value::as_f64) { Some(n) => format!("{}%", n.round() as i64), None => "—".into() } }

pub fn line(u: &Value, pack_files: Option<usize>) -> String {
    let mut bits = vec![u.get("model").and_then(Value::as_str).unwrap_or("claude").to_string(), format!("ctx {}", pct(u.get("context").and_then(|c| c.get("used_pct"))))];
    if let Some(c) = u.get("cost_usd").and_then(Value::as_f64) { bits.push(format!("${c:.2}")); }
    for l in u.get("limits").and_then(Value::as_array).cloned().unwrap_or_default() { bits.push(format!("{} {}", l.get("label").and_then(Value::as_str).unwrap_or(""), pct(l.get("used_pct")))); }
    bits.push(match pack_files { None => "antiloki pack —".into(), Some(n) => format!("antiloki pack {n} file{}", if n == 1 { "" } else { "s" }) });
    bits.join(" · ")
}

pub fn main() -> i32 {
    use std::io::Read;
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    let input: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
    let u = normalize(&input);
    let start = input.get("workspace").and_then(|w| w.get("project_dir")).or_else(|| input.get("cwd")).and_then(Value::as_str).map(PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let mut pack_files: Option<usize> = None;
    let named = std::env::var_os("ANTILOKI_RECORDS").filter(|v| !v.is_empty());
    if let Some(root) = find_pack_root(&start).or_else(|| named.as_ref().map(|_| start.clone())) {
        pack_files = std::fs::read_to_string(root.join(".antiloki/context/manifest.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .map(|m| m.get("items").and_then(Value::as_array).map(|a| a.iter().filter(|i| i.get("written").and_then(Value::as_bool).unwrap_or(false)).count()).unwrap_or(0));
        let _ = std::fs::write(crate::hook::records_in(&root, named).join("usage.json"), u.to_string());
    }
    println!("{}", line(&u, pack_files));
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalize_and_line_match_the_ts() {
        let input = json!({ "model": { "display_name": "Opus 5" }, "context_window": { "used_percentage": 12.4, "total_input_tokens": 1000, "context_window_size": 200000 }, "cost": { "total_cost_usd": 0.4211, "total_lines_added": 3 }, "rate_limits": { "five_hour": { "used_percentage": 23, "resets_at": 1 }, "seven_day": { "used_percentage": 41.4 } } });
        let u = normalize(&input);
        assert_eq!(u["engine"], "claude-code"); assert_eq!(u["limits"].as_array().unwrap().len(), 2); assert_eq!(u["context"]["size"], 200000);
        assert_eq!(line(&u, Some(7)), "Opus 5 · ctx 12% · $0.42 · 5h 23% · week 41% · antiloki pack 7 files");
        assert_eq!(line(&normalize(&json!({})), None), "claude · ctx — · antiloki pack —");
        assert_eq!(line(&normalize(&json!({})), Some(1)), "claude · ctx — · antiloki pack 1 file");
    }
}
