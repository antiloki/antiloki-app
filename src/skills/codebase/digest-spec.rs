//! digest-spec.rs — twin of skills/codebase/digest-spec.mjs (helper module, all no-db). NAMES ARE
//! CONTRACT: kindOf · digestColumnsDdl · sectionsFromRow · groupsOf. The code_digests spec reflector.
// QUALITY: 81/100 — suffix→kind, spec-ordered section reflection, DIGEST_GROUPS-ordered groups
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

const DIGEST_GROUPS: [&str; 8] = ["Overview", "Structure", "Health", "Tests", "Surface", "Smells", "Hotspots", "Narrative"];
const DIGEST_SKIP: [&str; 4] = ["id", "origin", "root", "created_at"];
// (col, group, label) — the ordered code_digests spec.
const DIGEST_COLUMNS: [(&str, &str, &str); 26] = [
    ("total_files_num", "Overview", "Files"), ("total_symbols_num", "Overview", "Symbols"),
    ("total_edges_num", "Overview", "Edges"), ("total_loc_num", "Overview", "Lines of code"),
    ("languages_json", "Overview", "Languages (by extension)"),
    ("avg_fan_in_num", "Structure", "Avg fan-in"), ("avg_fan_out_num", "Structure", "Avg fan-out"),
    ("max_depth_num", "Structure", "Max import depth"), ("cycles_num", "Structure", "Import cycles"),
    ("orphans_json", "Structure", "Orphan files (no imports in/out)"), ("hubs_json", "Structure", "Hubs (most imported)"),
    ("health_score_num", "Health", "Health"), ("risk_score_num", "Health", "Risk"),
    ("top_central_json", "Health", "Most central"), ("top_risky_json", "Health", "Riskiest (depended-on, untested)"),
    ("top_complex_json", "Health", "Most complex"),
    ("coverage_pct", "Tests", "Coverage"), ("tested_files_num", "Tests", "Tested files"),
    ("untested_files_num", "Tests", "Untested files"), ("test_files_num", "Tests", "Test files"),
    ("entry_points_json", "Surface", "Entry points (route/ui/data)"),
    ("smells_total_num", "Smells", "Total smells"), ("smells_by_kind_json", "Smells", "Smells by kind"),
    ("largest_files_json", "Hotspots", "Largest files"), ("top_churn_json", "Hotspots", "Churn hotspots (git)"),
    ("narrative_md", "Narrative", "Architecture narrative"),
];

fn kind_of(col: &str) -> &'static str {
    if col.ends_with("_json") { "json" } else if col.ends_with("_pct") { "pct" }
    else if col.ends_with("_md") { "md" } else if col.ends_with("_num") { "num" } else { "text" }
}
pub fn kindOf(col: &Value) -> Value { json!(kind_of(col.as_str().unwrap_or(""))) }

fn sql_type(col: &str) -> &'static str {
    let k = kind_of(col);
    if k == "json" || k == "md" || k == "text" { "TEXT" } else { "REAL" }
}
pub fn digestColumnsDdl(_args: &Value) -> Value {
    json!(DIGEST_COLUMNS.iter().map(|(col, _, _)| format!("  {col} {}", sql_type(col))).collect::<Vec<_>>().join(",\n"))
}

fn humanize(col: &str) -> String {
    // strip _(json|md|num|pct)$, _→space, uppercase first char
    let base = ["_json", "_md", "_num", "_pct"].iter().find_map(|s| col.strip_suffix(s)).unwrap_or(col);
    let spaced = base.replace('_', " ");
    let mut ch = spaced.chars();
    match ch.next() { Some(f) => f.to_uppercase().collect::<String>() + ch.as_str(), None => spaced }
}
fn safe_parse(s: &str) -> Value { serde_json::from_str(s).unwrap_or(Value::Null) }

pub fn sectionsFromRow(row: &Value) -> Value {
    let obj = match row.as_object() { Some(o) => o, None => return json!([]) };
    let order: HashMap<&str, usize> = DIGEST_COLUMNS.iter().enumerate().map(|(i, (c, _, _))| (*c, i)).collect();
    let spec_of: HashMap<&str, (&str, &str)> = DIGEST_COLUMNS.iter().map(|(c, g, l)| (*c, (*g, *l))).collect();
    let skip: HashSet<&str> = DIGEST_SKIP.iter().copied().collect();
    let mut cols: Vec<String> = obj.keys().filter(|c| !skip.contains(c.as_str())).cloned().collect();
    cols.sort_by(|a, b| {
        let (oa, ob) = (*order.get(a.as_str()).unwrap_or(&1_000_000_000), *order.get(b.as_str()).unwrap_or(&1_000_000_000));
        oa.cmp(&ob).then_with(|| a.cmp(b))
    });
    let sections: Vec<Value> = cols.iter().map(|col| {
        let kind = kind_of(col);
        let raw = obj.get(col).cloned().unwrap_or(Value::Null);
        let (group, label) = match spec_of.get(col.as_str()) { Some((g, l)) => (g.to_string(), l.to_string()), None => ("Other".to_string(), humanize(col)) };
        let value = if kind == "json" { match &raw { Value::String(s) => safe_parse(s), o => o.clone() } } else { raw };
        json!({ "col": col, "label": label, "group": group, "kind": kind, "value": value })
    }).collect();
    json!(sections)
}

pub fn groupsOf(sections: &Value) -> Value {
    let empty = vec![];
    let mut present: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for s in sections.as_array().unwrap_or(&empty) {
        let g = s.get("group").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if seen.insert(g.clone()) { present.push(g); }
    }
    let mut ordered: Vec<Value> = DIGEST_GROUPS.iter().filter(|g| present.iter().any(|p| p == *g)).map(|g| json!(g)).collect();
    for g in &present { if !DIGEST_GROUPS.contains(&g.as_str()) { ordered.push(json!(g)); } }
    json!(ordered)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reflects() {
        assert_eq!(kindOf(&json!("x_json")), json!("json"));
        assert_eq!(kindOf(&json!("y")), json!("text"));
        assert!(digestColumnsDdl(&Value::Null).as_str().unwrap().contains("total_files_num REAL"));
        assert!(digestColumnsDdl(&Value::Null).as_str().unwrap().contains("languages_json TEXT"));
        let row = json!({ "id": 1, "total_files_num": 10, "languages_json": "{\"ts\":5}", "extra_num": 3 });
        let secs = sectionsFromRow(&row);
        let arr = secs.as_array().unwrap();
        assert_eq!(arr[0]["col"], json!("total_files_num")); // spec order first, id skipped
        let langs = arr.iter().find(|s| s["col"] == json!("languages_json")).unwrap();
        assert_eq!(langs["value"], json!({ "ts": 5 })); // json parsed
        let extra = arr.iter().find(|s| s["col"] == json!("extra_num")).unwrap();
        assert_eq!(extra["group"], json!("Other")); // un-spec'd
        assert_eq!(groupsOf(&secs), json!(["Overview", "Other"]));
    }
}
