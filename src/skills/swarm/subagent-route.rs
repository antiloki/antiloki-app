//! subagent-route.rs — twin of skills/swarm/subagent-route.mjs (db READ lane). NAMES ARE CONTRACT:
//! `subagentRoute` (default) · `routeFile` (db-first helper). Deterministic file→most-specific-leaf router.
// QUALITY: 82/100 — glob match under a branch, most-specific (literal-char) wins, abstract agents never chosen
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// glob → anchored RegExp: ** (any depth, '**/' optional), * (non-slash run), specials (incl. ?) escaped literal.
fn glob_to_regexp(glob: &str) -> Result<Regex, regex::Error> {
    let chars: Vec<char> = glob.chars().collect();
    let mut re = String::from("^");
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '*' {
            if chars.get(i + 1) == Some(&'*') {
                if chars.get(i + 2) == Some(&'/') { re.push_str("(?:.*/)?"); i += 2; }
                else { re.push_str(".*"); i += 1; }
            } else {
                re.push_str("[^/]*");
            }
        } else if ".+?^${}()|[]\\".contains(c) {
            re.push('\\'); re.push(c);
        } else {
            re.push(c);
        }
        i += 1;
    }
    re.push('$');
    Regex::new(&re)
}
// specificity = literal-char count (glob with '*' removed)
fn specificity(glob: &str) -> i64 { glob.chars().filter(|c| *c != '*').count() as i64 }

fn parent_of(db: &Connection, name: &str) -> Option<String> {
    db.query_row("SELECT parent_agent FROM agents WHERE name = ?", [name], |row| row.get::<_, Option<String>>(0)).ok().flatten()
}
// is `branch` an ancestor of (or equal to) `name`, walking parent_agent up (cycle-guarded, depth-capped)?
fn under_branch(db: &Connection, name: &str, branch: &str, max_depth: i32) -> bool {
    let mut cur = name.to_string();
    let mut depth = 0;
    let mut seen: HashSet<String> = HashSet::new();
    while !cur.is_empty() && depth < max_depth {
        if cur == branch { return true; }
        if seen.contains(&cur) { break; }
        seen.insert(cur.clone());
        cur = parent_of(db, &cur).unwrap_or_default(); // row?.parent_agent ?? null → '' ends the loop
        depth += 1;
    }
    false
}

pub fn routeFile(db: &Connection, file_v: &Value, branch_v: &Value, fallback_v: &Value) -> Value {
    let fb: Value = if fallback_v.is_null() { branch_v.clone() } else { fallback_v.clone() }; // fallback ?? branch
    let file = match file_v.as_str() { Some(f) if !f.is_empty() => f, _ => return json!({ "subagent_id": fb, "matched": false, "candidates": [] }) };
    let branch = branch_v.as_str().unwrap_or("");

    let rows = match crate::db_json::query_rows(db, "SELECT name, applies_to_json FROM agents WHERE is_abstract = 0 AND applies_to_json IS NOT NULL", &[]) {
        Ok(r) => r.as_array().cloned().unwrap_or_default(),
        Err(_) => return json!({ "subagent_id": fb, "matched": false, "candidates": [] }),
    };
    let mut candidates: Vec<Value> = Vec::new();
    for r in &rows {
        let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if !under_branch(db, name, branch, 8) { continue; }
        let applies: Value = r.get("applies_to_json").and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_str::<Value>(s).ok()).filter(|v| !v.is_null()).unwrap_or(json!({}));
        let globs = applies.get("globs").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        for g in &globs {
            if let Some(gs) = g.as_str() {
                if glob_to_regexp(gs).map_or(false, |re| re.is_match(file)) {
                    candidates.push(json!({ "name": name, "glob": gs, "specificity": specificity(gs) }));
                }
            }
        }
    }
    if candidates.is_empty() { return json!({ "subagent_id": fb, "matched": false, "candidates": [] }); }
    candidates.sort_by(|a, b| {
        let (sa, sb) = (a["specificity"].as_i64().unwrap_or(0), b["specificity"].as_i64().unwrap_or(0));
        sb.cmp(&sa).then_with(|| {
            let la = a["glob"].as_str().map_or(0, |s| s.chars().count());
            let lb = b["glob"].as_str().map_or(0, |s| s.chars().count());
            lb.cmp(&la)
        })
    });
    json!({ "subagent_id": candidates[0]["name"].clone(), "matched": true, "candidates": candidates })
}

pub fn subagentRoute(input: &Value, db: &Connection) -> Result<Value, String> {
    let file = input.get("file");
    if !truthy(file) { return Err("subagent-route: 'file' is required".into()); }
    let branch = input.get("branch").filter(|v| !v.is_null()).cloned().unwrap_or(json!("tl.dev"));
    let fallback = input.get("fallback").filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null);
    Ok(routeFile(db, file.unwrap(), &branch, &fallback))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seed(c: &Connection) {
        c.execute_batch("CREATE TABLE agents (name TEXT, parent_agent TEXT, is_abstract INTEGER, applies_to_json TEXT); \
          INSERT INTO agents VALUES \
            ('tl.dev', NULL, 1, NULL), \
            ('tl.dev.style', 'tl.dev', 0, '{\"globs\":[\"**/*.css\"]}'), \
            ('tl.dev.frontend', 'tl.dev', 1, NULL), \
            ('tl.dev.frontend.react', 'tl.dev.frontend', 0, '{\"globs\":[\"**/*.tsx\"]}'), \
            ('tl.dev.backend', 'tl.dev', 1, NULL), \
            ('tl.dev.backend.node', 'tl.dev.backend', 0, '{\"globs\":[\"**/*.ts\"]}'), \
            ('tl.dev.backend.nestjs', 'tl.dev.backend', 0, '{\"globs\":[\"**/*.service.ts\"]}');").unwrap();
    }
    #[test]
    fn routes() {
        let c = Connection::open_in_memory().unwrap(); seed(&c);
        assert_eq!(routeFile(&c, &json!("src/Button.css"), &json!("tl.dev"), &Value::Null)["subagent_id"], json!("tl.dev.style"));
        assert_eq!(routeFile(&c, &json!("src/Card.tsx"), &json!("tl.dev"), &Value::Null)["subagent_id"], json!("tl.dev.frontend.react"));
        // most-specific glob wins: '**/*.service.ts' (spec 12) beats '**/*.ts' (spec 4)
        let svc = routeFile(&c, &json!("src/users.service.ts"), &json!("tl.dev"), &Value::Null);
        assert_eq!(svc["subagent_id"], json!("tl.dev.backend.nestjs"));
        assert_eq!(svc["matched"], json!(true));
        let none = routeFile(&c, &json!("README.md"), &json!("tl.dev"), &Value::Null);
        assert_eq!(none["matched"], json!(false));
        assert_eq!(none["subagent_id"], json!("tl.dev")); // branch fallback
        assert_eq!(routeFile(&c, &json!("data.bin"), &json!("tl.dev"), &json!("tl"))["subagent_id"], json!("tl")); // explicit fallback
        assert_eq!(subagentRoute(&json!({ "file": "src/Button.css" }), &c).unwrap()["subagent_id"], json!("tl.dev.style"));
    }
}
