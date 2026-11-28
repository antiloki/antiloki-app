//! review-markdown.rs — the measured view of a file / folder / the laws, rendered as compact,
//! self-contained markdown for a code agent to read. NO JS TWIN.
//!
//! Added 2026-09-14 for the pane context pack. antiIDE renders its reviews in the FE from the JSON
//! `constitutionFile` returns; a Claude/Codex pane has no FE, so the same JSON is rendered here —
//! deterministically, $0, no AI — into what the agent needs to *act*: the weak axes and what
//! drives them, the violated laws with line numbers and the rule they broke, who imports the
//! file, whether a test reaches it, and whether the last change made it worse. The same functions
//! back the `antiloki` MCP tools (`format: "md"`), so a pack file and a live tool answer are the
//! same bytes for the same snapshot.
//!
//! Every renderer starts with a STAMP comment (`<!-- antiloki context · kind=… · path=… ·
//! engineering_version=… · snapshot=… -->`) so staleness is visible in the file and greppable.
use serde_json::{json, Map, Value};

fn s(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Null) | None => String::new(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(o) => o.to_string(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn num(v: Option<&Value>) -> Option<f64> { nz(v).and_then(Value::as_f64) }
fn int(v: Option<&Value>) -> String { num(v).map(|f| format!("{}", f.round() as i64)).unwrap_or_else(|| "—".into()) }
fn pct(v: Option<&Value>) -> String { num(v).map(|f| format!("{}th pct", (f * 100.0).round() as i64)).unwrap_or_else(|| "—".into()) }
fn first_line(md: &str) -> String { md.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string() }
fn take(sv: &str, n: usize) -> String { sv.chars().take(n).collect() }

/// The stamp every pack file and tool answer carries first.
pub fn stamp(kind: &str, path: &str, review: &Value, generated: &str) -> String {
    format!(
        "<!-- antiloki context · kind={kind} · path={path} · engineering_version={} · snapshot={} · generated={generated} -->",
        s(review.get("engineering_version")),
        int(review.get("snapshot_id")),
    )
}

const AXIS_ORDER: [(&str, f64); 7] = [("compliance", 0.25), ("defects", 0.18), ("tests", 0.16), ("structure", 0.15), ("coupling", 0.12), ("cohesion", 0.06), ("change", 0.08)];

/// One line on what drives an axis, from `axes[ax].parts` — the same vocabulary the FE paper uses.
pub fn axis_because(ax: &str, parts: &Value) -> String {
    let p = |k: &str| parts.get(k);
    match ax {
        "structure" => {
            if s(p("basis")) == "cognitive" {
                let mut bits = vec![format!("worst block `{}` L{} (cognitive {}, nesting {})", s(p("worst_block")), int(p("worst_line")), int(p("worst_cognitive")), int(p("max_nesting")))];
                if num(p("nested_loops")).unwrap_or(0.0) > 0.0 { bits.push(format!("{} nested loops", int(p("nested_loops")))); }
                if num(p("god_file")).unwrap_or(0.0) > 0.0 { bits.push("god-file".into()); }
                bits.join(", ")
            } else {
                let mut bits = vec![format!("complexity at the {} of the repo", pct(p("pct_complexity")))];
                if num(p("god_file")).unwrap_or(0.0) > 0.0 { bits.push("god-file".into()); }
                bits.join(", ")
            }
        }
        "coupling" => {
            let mut bits = vec![format!("fan-in {} · fan-out {}", pct(p("pct_fan_in")), pct(p("pct_fan_out")))];
            if num(p("cycle")).unwrap_or(0.0) > 0.0 { bits.push("in an import cycle".into()); }
            bits.join(", ")
        }
        "tests" => match nz(p("covered")) {
            None => format!("unmeasured — {}", if s(p("basis")).is_empty() { "no test files in the atlas".to_string() } else { s(p("basis")) }),
            Some(c) if c.as_f64().unwrap_or(0.0) > 0.0 => format!("a test reaches it (basis {})", s(p("basis"))),
            Some(_) => format!("no test reaches it (basis {}); fan-in {}", s(p("basis")), pct(p("pct_fan_in"))),
        },
        "compliance" => format!("law score {}", int(p("law_score"))),
        "defects" => format!("{} findings ({} high)", int(p("findings")), int(p("high"))),
        "change" => match nz(p("note")) {
            Some(n) => s(Some(n)),
            None => format!("churn {} ({}), complexity {}", int(p("churn")), pct(p("pct_churn")), pct(p("pct_complexity"))),
        },
        "cohesion" => match nz(p("note")) {
            Some(n) => s(Some(n)),
            None => format!("{}% of symbols connected", num(p("connected_ratio")).map(|r| (r * 100.0).round() as i64).unwrap_or(0)),
        },
        _ => String::new(),
    }
}

/// The one move that lifts an axis — mechanical, derived from the same parts.
pub fn axis_fix(ax: &str, parts: &Value, violations: usize) -> String {
    let p = |k: &str| parts.get(k);
    match ax {
        "structure" if s(p("basis")) == "cognitive" => format!("split `{}` (L{}) into named steps; each nesting level you remove lowers cognitive {}", s(p("worst_block")), int(p("worst_line")), int(p("worst_cognitive"))),
        "structure" => "extract the largest function into smaller named ones".into(),
        "coupling" if num(p("cycle")).unwrap_or(0.0) > 0.0 => "break the import cycle first (move the shared piece to a leaf module)".into(),
        "coupling" => "reduce what this file imports; prefer one façade over many siblings".into(),
        "tests" if nz(p("covered")).is_none() => "no test instrument here — add the first test file and the axis becomes measurable".into(),
        "tests" if num(p("covered")).unwrap_or(0.0) == 0.0 => "write one test that imports this file (a render or a call of its main export)".into(),
        "compliance" if violations > 0 => format!("fix the {violations} violation(s) listed below — line numbers are exact"),
        "defects" if num(p("findings")).unwrap_or(0.0) > 0.0 => "resolve the bugfinder findings (anchors below)".into(),
        _ => String::new(),
    }
}

fn axes_table(axes: &Value, violations: usize, out: &mut Vec<String>) {
    out.push("| axis | score | weight | because | fix |".into());
    out.push("|---|---|---|---|---|".into());
    for (ax, w) in AXIS_ORDER {
        let a = match nz(axes.get(ax)) { Some(a) => a, None => continue };
        let score = num(a.get("score"));
        let parts = a.get("parts").cloned().unwrap_or(json!({}));
        let because = axis_because(ax, &parts);
        let fix = if score.unwrap_or(100.0) < 80.0 { axis_fix(ax, &parts, violations) } else { String::new() };
        out.push(format!("| {ax} | {} | {} | {} | {} |", score.map(|v| format!("{}", v.round() as i64)).unwrap_or_else(|| "—".into()), w, because.replace('|', "/"), fix.replace('|', "/")));
    }
}

fn violations_block(violations: &[Value], with_path: bool, out: &mut Vec<String>) {
    if violations.is_empty() {
        out.push("none — obeys every ratified law".into());
        return;
    }
    // group by law, keep the row order (weight desc) inside each group
    let mut order: Vec<String> = Vec::new();
    let mut groups: Map<String, Value> = Map::new();
    for v in violations {
        let k = s(v.get("law_key"));
        if !groups.contains_key(&k) { order.push(k.clone()); groups.insert(k.clone(), json!([])); }
        groups.get_mut(&k).unwrap().as_array_mut().unwrap().push(v.clone());
    }
    for k in order {
        let rows = groups[&k].as_array().cloned().unwrap_or_default();
        let head = &rows[0];
        let rule = first_line(&s(head.get("rule_md")));
        out.push(format!("- **{}** `{k}` ×{} (weight {}, {}) — {}", s(head.get("title")), rows.len(), int(head.get("weight")), s(head.get("severity")), take(&rule, 200)));
        for r in rows.iter().take(12) {
            let line = match nz(r.get("line")) { Some(l) => format!("L{}", int(Some(l))), None => "file".into() };
            let ev = take(&first_line(&s(r.get("evidence"))), 140);
            let sym = s(r.get("symbol"));
            let path_part = if with_path { format!("`{}` ", s(r.get("path"))) } else { String::new() };
            let sym_part = if sym.is_empty() { String::new() } else { format!(" in `{sym}`") };
            out.push(format!("  - {path_part}{line}{sym_part}{}", if ev.is_empty() { String::new() } else { format!(": `{ev}`") }));
        }
        if rows.len() > 12 { out.push(format!("  - … {} more", rows.len() - 12)); }
        let fix = s(head.get("fix"));
        if !fix.is_empty() { out.push(format!("  - fix: {}", take(&first_line(&fix), 200))); }
    }
}

fn regression_block(reg: &Value, out: &mut Vec<String>) {
    if reg.is_null() { out.push("first snapshot — nothing to compare yet".into()); return; }
    let comparable = reg.get("comparable").and_then(Value::as_bool).unwrap_or(true);
    if !comparable {
        out.push(format!("baseline reset — the previous snapshot #{} was judged by engineering_version {} (instrument changed, deltas are not the code moving)", int(reg.get("prev_snapshot_id")), s(reg.get("prev_engineering_version"))));
        return;
    }
    let hd = num(reg.get("health_delta")).unwrap_or(0.0);
    let sd = num(reg.get("score_delta")).unwrap_or(0.0);
    let laws_changed = reg.get("laws_changed").and_then(Value::as_bool).unwrap_or(false);
    out.push(format!("vs snapshot #{} ({}): health {:+} · law score {:+}{}{}", int(reg.get("prev_snapshot_id")), take(&s(reg.get("prev_created_at")), 16), hd.round() as i64, sd.round() as i64, if reg.get("regressed").and_then(Value::as_bool).unwrap_or(false) { " · REGRESSED" } else { "" }, if laws_changed { " · laws changed between the two" } else { "" }));
    for n in reg.get("new").and_then(Value::as_array).cloned().unwrap_or_default() {
        out.push(format!("- new: `{}` ×{}{}", s(n.get("law_key")), int(n.get("count")), n.get("lines").and_then(Value::as_array).filter(|l| !l.is_empty()).map(|l| format!(" (L{})", l.iter().map(|x| int(Some(x))).collect::<Vec<_>>().join(", L"))).unwrap_or_default()));
    }
    for f in reg.get("fixed").and_then(Value::as_array).cloned().unwrap_or_default() {
        out.push(format!("- fixed: `{}` ×{}", s(f.get("law_key")), int(f.get("count"))));
    }
}

/// `constitutionFile` file branch → markdown.
pub fn file_review_md(review: &Value, generated: &str) -> String {
    let path = s(review.get("path"));
    let mut out: Vec<String> = vec![stamp("file", &path, review, generated), String::new()];
    if review.get("found").and_then(Value::as_bool) != Some(true) {
        out.push(format!("# {path}\n\n_No constitution snapshot covers this file yet — antiIDE has not judged it. Ask the `antiloki_file_review` tool after the next judge run, or read the source._"));
        return out.join("\n");
    }
    let violations = review.get("violations").and_then(Value::as_array).cloned().unwrap_or_default();
    let archetype = s(review.get("archetype"));
    out.push(format!("# {path}"));
    out.push(String::new());
    out.push(format!(
        "**Health {}** · law score {} · archetype {}{} · snapshot #{} ({}) · repo {} · folder {}",
        int(review.get("health")), int(review.get("score")), if archetype.is_empty() { "—".into() } else { archetype.clone() },
        if archetype == "glue" { " (re-export/constants/types — scored on laws + defects only)" } else { "" },
        int(review.get("snapshot_id")), take(&s(review.get("created_at")), 16),
        pct(review.get("neighborhood").and_then(|n| n.get("pct_repo"))), pct(review.get("neighborhood").and_then(|n| n.get("pct_folder"))),
    ));
    let sig = review.get("signals").cloned().unwrap_or(json!({}));
    out.push(format!(
        "loc {} · complexity {} · fan-in {} · fan-out {} · churn {} · findings {}",
        int(sig.get("loc")), int(sig.get("complexity")), int(sig.get("fan_in")), int(sig.get("fan_out")), int(sig.get("churn")), int(sig.get("findings")),
    ));
    out.push(String::new());
    out.push("## Axes (weak axes carry a fix)".into());
    axes_table(review.get("axes").unwrap_or(&json!({})), violations.len(), &mut out);
    out.push(String::new());
    out.push(format!("## Violations ({})", violations.len()));
    violations_block(&violations, false, &mut out);
    out.push(String::new());
    let anchors = review.get("anchors").cloned().unwrap_or(json!({}));
    let importers = anchors.get("importers").and_then(Value::as_array).cloned().unwrap_or_default();
    let imports = anchors.get("imports").and_then(Value::as_array).cloned().unwrap_or_default();
    out.push("## Neighborhood (runtime imports only — type-only edges excluded)".into());
    out.push(format!("- imported by ({}): {}", importers.len(), if importers.is_empty() { "nothing — an entry point or dead".into() } else { importers.iter().map(|p| format!("`{}`", s(Some(p)))).collect::<Vec<_>>().join(", ") }));
    out.push(format!("- imports ({}): {}", imports.len(), if imports.is_empty() { "nothing".into() } else { imports.iter().map(|p| format!("`{}`", s(Some(p)))).collect::<Vec<_>>().join(", ") }));
    let findings = anchors.get("findings").and_then(Value::as_array).cloned().unwrap_or_default();
    if !findings.is_empty() {
        out.push(String::new());
        out.push("## Findings (bugfinder)".into());
        for f in findings { out.push(format!("- L{}: {}", int(f.get("line")), s(f.get("text")))); }
    }
    out.push(String::new());
    out.push("## Since the last snapshot".into());
    regression_block(review.get("regression").unwrap_or(&Value::Null), &mut out);
    out.join("\n")
}

/// `constitutionFile` folder branch (`folder_review`) → markdown.
pub fn folder_review_md(review: &Value, generated: &str) -> String {
    let path = s(review.get("path"));
    let mut out: Vec<String> = vec![stamp("folder", &path, review, generated), String::new()];
    if review.get("found").and_then(Value::as_bool) != Some(true) {
        out.push(format!("# {path}/\n\n_No constitution snapshot covers this folder yet — antiIDE has not judged it. Ask the `antiloki_folder_review` tool after the next judge run._"));
        return out.join("\n");
    }
    let roster = review.get("roster").cloned().unwrap_or(json!({}));
    let members = review.get("members").and_then(Value::as_array).cloned().unwrap_or_default();
    let violations = review.get("violations").and_then(Value::as_array).cloned().unwrap_or_default();
    out.push(format!("# {path}/"));
    out.push(String::new());
    out.push(format!(
        "**Health {}** over {} files ({} logic, {} glue, {} test{}) · logic-only mean {} · law score {} · snapshot #{} · tests basis {}",
        int(review.get("health")), int(review.get("files")), int(roster.get("logic")), int(roster.get("glue")), int(roster.get("test")),
        if num(roster.get("unknown")).unwrap_or(0.0) > 0.0 { format!(", {} unknown", int(roster.get("unknown"))) } else { String::new() },
        int(review.get("health_logic")), int(review.get("score")), int(review.get("snapshot_id")), s(review.get("tests_basis")),
    ));
    out.push("glue files (re-exports, constants, types) are scored on laws + defects only — they lift the mean; the logic-only mean is the number to defend.".into());
    out.push(String::new());
    out.push("## Axis means".into());
    let axes = review.get("axes").cloned().unwrap_or(json!({}));
    let mut axis_bits: Vec<String> = Vec::new();
    for (ax, _) in AXIS_ORDER {
        if let Some(a) = nz(axes.get(ax)) { axis_bits.push(format!("{ax} {}", int(a.get("score")))); }
    }
    out.push(axis_bits.join(" · "));
    out.push(String::new());
    out.push("## Members, worst first (health · archetype · loc · the weakest axis)".into());
    let mut sorted = members.clone();
    sorted.sort_by(|a, b| num(a.get("health")).unwrap_or(100.0).partial_cmp(&num(b.get("health")).unwrap_or(100.0)).unwrap_or(std::cmp::Ordering::Equal).then_with(|| s(a.get("path")).cmp(&s(b.get("path")))));
    for m in sorted.iter().take(40) {
        let ax = m.get("axes").cloned().unwrap_or(json!({}));
        let weakest = AXIS_ORDER.iter()
            .filter_map(|(k, _)| nz(ax.get(*k)).and_then(|a| num(a.get("score"))).map(|sc| (k, sc)))
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let weak_txt = weakest.map(|(k, sc)| format!("{k} {}", sc.round() as i64)).unwrap_or_default();
        out.push(format!("- `{}` — {} · {} · loc {}{}", s(m.get("path")), int(m.get("health")), s(m.get("archetype")), int(m.get("loc")), if weak_txt.is_empty() { String::new() } else { format!(" · weakest: {weak_txt}") }));
    }
    if members.len() > 40 { out.push(format!("- … {} more (open a file review for any of them)", members.len() - 40)); }
    if review.get("members_truncated").and_then(Value::as_bool).unwrap_or(false) { out.push("- roster truncated at 400".into()); }
    out.push(String::new());
    out.push(format!("## Violations across the folder ({})", violations.len()));
    violations_block(&violations, true, &mut out);
    out.push(String::new());
    out.push("## Since the last snapshot".into());
    regression_block(review.get("regression").unwrap_or(&Value::Null), &mut out);
    let history = review.get("history").and_then(Value::as_array).cloned().unwrap_or_default();
    if history.len() > 1 {
        out.push(String::new());
        out.push(format!("history: {}", history.iter().map(|h| format!("#{} {}", int(h.get("snapshot_id")), int(h.get("health")))).collect::<Vec<_>>().join(" → ")));
    }
    out.join("\n")
}

/// `composedConstitution` → the ruleset the agent is judged against.
pub fn laws_md(composed: &Value, generated: &str) -> String {
    let laws: Vec<Value> = composed.get("laws").and_then(Value::as_array).cloned().unwrap_or_default().into_iter()
        .filter(|l| l.get("status").and_then(Value::as_str) == Some("ratified")).collect();
    let exceptions = composed.get("exceptions").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut out: Vec<String> = vec![
        format!("<!-- antiloki context · kind=laws · laws={} · generated={generated} -->", laws.len()),
        String::new(),
        format!("# The constitution — {} ratified laws", laws.len()),
        String::new(),
        "Every edit you make is judged against these. A law with a detector scores (`score`); `advise` laws are guidance. Check a file before you finish with the `antiloki_check` tool (or re-read the file review).".into(),
        String::new(),
    ];
    let mut cur_scope = String::new();
    for l in &laws {
        let scope = s(l.get("scope"));
        if scope != cur_scope { out.push(format!("## {scope}")); cur_scope = scope; }
        let applies = l.get("applies_json").map(|a| match a { Value::String(raw) => serde_json::from_str::<Value>(raw).unwrap_or(json!({})), o => o.clone() }).unwrap_or(json!({}));
        let mut applies_bits: Vec<String> = Vec::new();
        for k in ["languages", "frameworks", "paths"] {
            if let Some(arr) = applies.get(k).and_then(Value::as_array).filter(|a| !a.is_empty()) {
                applies_bits.push(format!("{k}: {}", arr.iter().map(|x| s(Some(x))).collect::<Vec<_>>().join(", ")));
            }
        }
        out.push(format!(
            "- **{}** `{}` — weight {} · {} · {}{}",
            s(l.get("title")), s(l.get("key")), int(l.get("weight")), s(l.get("severity")), s(l.get("enforcement")),
            if applies_bits.is_empty() { String::new() } else { format!(" · applies {}", applies_bits.join("; ")) },
        ));
        let rule = s(l.get("rule_md"));
        for line in rule.lines().filter(|l| !l.trim().is_empty()).take(6) { out.push(format!("  {}", line.trim())); }
    }
    if !exceptions.is_empty() {
        out.push(String::new());
        out.push(format!("## Exceptions ({})", exceptions.len()));
        for e in &exceptions {
            out.push(format!("- `{}` — {}{}", s(e.get("law_key")), s(e.get("path_glob")).is_empty().then(|| String::new()).unwrap_or_else(|| format!("`{}` ", s(e.get("path_glob")))), take(&first_line(&s(e.get("reason"))), 160)));
        }
    }
    out.join("\n")
}

/// The runtime import graph around one folder: member file nodes + one hop out, type-only edges
/// excluded. `nodes` = atlas file nodes `{id, path}`, `edges` = `{from_node, to_node, type, kind}`.
pub fn graph_json(folder: &str, nodes: &[Value], edges: &[Value], generated: &str) -> Value {
    let prefix = format!("{}/", folder.trim_end_matches('/'));
    let path_of: Map<String, Value> = nodes.iter().map(|n| (s(n.get("id")), json!(s(n.get("path"))))).collect();
    let is_member = |id: &str| path_of.get(id).map(|p| s(Some(p)).starts_with(&prefix)).unwrap_or(false);
    let mut kept_edges: Vec<Value> = Vec::new();
    let mut touched: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for e in edges {
        if s(e.get("type")) != "imports" || s(e.get("kind")) == "type" { continue; }
        let (f, t) = (s(e.get("from_node")), s(e.get("to_node")));
        if !(is_member(&f) || is_member(&t)) { continue; }
        touched.insert(f.clone());
        touched.insert(t.clone());
        kept_edges.push(json!({ "from": path_of.get(&f).cloned().unwrap_or(json!(f)), "to": path_of.get(&t).cloned().unwrap_or(json!(t)), "kind": if s(e.get("kind")).is_empty() { json!("value") } else { e.get("kind").cloned().unwrap() } }));
    }
    for n in nodes { if is_member(&s(n.get("id"))) { touched.insert(s(n.get("id"))); } }
    let mut internal = 0usize; let mut outbound = 0usize; let mut inbound = 0usize;
    for e in &kept_edges {
        let fm = s(e.get("from")).starts_with(&prefix);
        let tm = s(e.get("to")).starts_with(&prefix);
        match (fm, tm) { (true, true) => internal += 1, (true, false) => outbound += 1, _ => inbound += 1 }
    }
    let files: Vec<Value> = touched.iter().map(|id| {
        let p = s(path_of.get(id));
        json!({ "path": p, "member": p.starts_with(&prefix) })
    }).collect();
    json!({
        "antiloki_context": { "kind": "graph", "path": folder, "generated": generated, "note": "runtime import edges only — type-only imports vanish at compile time and are excluded" },
        "counts": { "members": files.iter().filter(|f| f["member"] == json!(true)).count(), "internal": internal, "outbound": outbound, "inbound": inbound },
        "files": files,
        "edges": kept_edges,
    })
}

/// `atlasTestMap` → which tests reach the folder's files.
pub fn tests_md(test_map: &Value, folder: &str, generated: &str) -> String {
    let prefix = format!("{}/", folder.trim_end_matches('/'));
    let instrument = test_map.get("instrument").cloned().unwrap_or(json!({}));
    let cbs = test_map.get("coverage_by_source").cloned().unwrap_or(json!({}));
    let mut out: Vec<String> = vec![
        format!("<!-- antiloki context · kind=tests · path={folder} · generated={generated} -->"),
        String::new(),
        format!("# Tests reaching {folder}/"),
        String::new(),
    ];
    let test_files = num(instrument.get("test_files")).unwrap_or(0.0) as i64;
    if test_files == 0 {
        out.push("unmeasured — the atlas holds no test files, so \"uncovered\" would be a lie. Add the first test and this becomes a measurement.".into());
        return out.join("\n");
    }
    out.push(format!("instrument: {} test files · {} link to source by import · {} inline · {} e2e specs (drive a URL, import nothing — not attributable){}", test_files, int(instrument.get("static_links")), int(instrument.get("inline_files")), int(instrument.get("e2e_specs")), if instrument.get("lcov").and_then(Value::as_bool).unwrap_or(false) { " · lcov" } else { "" }));
    out.push(String::new());
    let mut covered: Vec<(String, Vec<String>)> = Vec::new();
    if let Some(m) = cbs.as_object() {
        for (p, tests) in m {
            if p.starts_with(&prefix) {
                covered.push((p.clone(), tests.as_array().map(|a| a.iter().map(|t| s(Some(t))).collect()).unwrap_or_default()));
            }
        }
    }
    covered.sort();
    if covered.is_empty() {
        out.push("no test imports any file in this folder (static-import basis).".into());
    } else {
        for (p, tests) in covered { out.push(format!("- `{p}` ← {}", tests.iter().map(|t| format!("`{t}`")).collect::<Vec<_>>().join(", "))); }
    }
    out.join("\n")
}

/// `blastRadius` (+ the path of every node id) → who breaks if this file changes.
pub fn blast_md(blast: &Value, seed_path: &str, path_of: &Map<String, Value>, generated: &str) -> String {
    let affected: Vec<String> = blast.get("affected").and_then(Value::as_array).map(|a| a.iter().map(|id| s(path_of.get(&s(Some(id)))).is_empty().then(|| s(Some(id))).unwrap_or_else(|| s(path_of.get(&s(Some(id)))))).collect()).unwrap_or_default();
    let tests: Vec<String> = blast.get("affected_tests").and_then(Value::as_array).map(|a| a.iter().map(|t| s(Some(t))).collect()).unwrap_or_default();
    let mut out: Vec<String> = vec![
        format!("<!-- antiloki context · kind=blast · path={seed_path} · generated={generated} -->"),
        String::new(),
        format!("# Blast radius of {seed_path}"),
        String::new(),
        format!("{} files depend on it transitively (runtime imports only) · {} tests reach the set", affected.len(), tests.len()),
    ];
    let mut sorted = affected.clone(); sorted.sort();
    for p in sorted.iter().take(60) { out.push(format!("- `{p}`")); }
    if sorted.len() > 60 { out.push(format!("- … {} more", sorted.len() - 60)); }
    if !tests.is_empty() {
        out.push(String::new());
        out.push("run these first:".into());
        for t in &tests { out.push(format!("- `{t}`")); }
    }
    out.join("\n")
}

/// `constitutionCheck` → what the candidate content fixes / introduces.
pub fn check_md(check: &Value) -> String {
    let path = s(check.get("path"));
    let after = check.get("after").cloned().unwrap_or(json!({}));
    let mut out: Vec<String> = vec![format!("# Check: {path}"), String::new()];
    out.push(format!(
        "{} laws re-measured · violations before {} → after {} · law score {} → ≈{}",
        int(check.get("checked_laws")), int(check.get("before").and_then(|b| b.get("violations"))), int(after.get("violations")),
        int(check.get("compliance").and_then(|c| c.get("before"))), int(check.get("compliance").and_then(|c| c.get("after_est"))),
    ));
    let introduced = check.get("introduced").and_then(Value::as_array).cloned().unwrap_or_default();
    let fixed = check.get("fixed").and_then(Value::as_array).cloned().unwrap_or_default();
    let unverifiable = check.get("unverifiable").and_then(Value::as_array).cloned().unwrap_or_default();
    if introduced.is_empty() && fixed.is_empty() && unverifiable.is_empty() { out.push("no change in violations.".into()); }
    for u in &unverifiable { out.push(format!("- CANNOT TELL `{}` — {} hit(s) now; the stored review recorded {} and no total", s(u.get("law_key")), int(u.get("now")), s(u.get("recorded")))); }
    for i in &introduced { out.push(format!("- INTRODUCED `{}` ×{} — {}", s(i.get("law_key")), int(i.get("count")), s(i.get("title")))); }
    for f in &fixed { out.push(format!("- fixed `{}` ×{}{} — {}", s(f.get("law_key")), int(f.get("count")), if f.get("at_least").and_then(Value::as_bool).unwrap_or(false) { "+" } else { "" }, s(f.get("title")))); }
    let list = after.get("list").and_then(Value::as_array).cloned().unwrap_or_default();
    if !list.is_empty() {
        out.push(String::new());
        out.push("remaining:".into());
        for v in list.iter().take(30) {
            let line = match nz(v.get("line")) { Some(l) => format!("L{}", int(Some(l))), None => "file".into() };
            out.push(format!("- `{}` {line}{}", s(v.get("law_key")), take(&first_line(&s(v.get("evidence"))), 120).is_empty().then(String::new).unwrap_or_else(|| format!(": `{}`", take(&first_line(&s(v.get("evidence"))), 120)))));
        }
    }
    if let Some(n) = nz(check.get("note")) { out.push(String::new()); out.push(s(Some(n))); }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_review() -> Value {
        json!({
            "path": "src/ui/Output.tsx", "found": true, "snapshot_id": 7, "created_at": "2026-09-14 10:00:00", "score": 70, "health": 62, "archetype": "logic",
            "engineering_version": "v3",
            "axes": {
                "compliance": { "score": 70, "parts": { "law_score": 70 } },
                "structure": { "score": 15, "parts": { "basis": "cognitive", "worst_block": "tabify", "worst_line": 87, "worst_cognitive": 26, "max_nesting": 4, "nested_loops": 2, "god_file": 0 } },
                "tests": { "score": 40, "parts": { "covered": 0, "basis": "static-import", "pct_fan_in": 0.5 } },
                "coupling": { "score": 90, "parts": { "pct_fan_in": 0.2, "pct_fan_out": 0.3, "cycle": 0 } }
            },
            "signals": { "loc": 210, "complexity": 30, "fan_in": 2, "fan_out": 5, "churn": 3, "findings": 0 },
            "violations": [
                { "law_key": "no-loose-equality", "title": "No loose equality", "rule_md": "Use === and !==.\nmore", "weight": 5, "severity": "warn", "line": 12, "symbol": "tabify", "evidence": "if (a == b) {", "fix": "replace == with ===" },
                { "law_key": "no-loose-equality", "title": "No loose equality", "rule_md": "Use === and !==.", "weight": 5, "severity": "warn", "line": 40, "symbol": null, "evidence": "x != y" }
            ],
            "neighborhood": { "pct_repo": 0.4, "pct_folder": 0.1 },
            "anchors": { "importers": ["src/ui/index.ts"], "imports": ["src/lib/x.ts", "src/lib/y.ts"], "findings": [] },
            "regression": { "prev_snapshot_id": 6, "prev_created_at": "2026-09-01 00:00:00", "new": [{ "law_key": "no-loose-equality", "count": 1, "lines": [40] }], "fixed": [], "health_delta": -3, "score_delta": -5, "regressed": true, "comparable": true, "laws_changed": false }
        })
    }

    #[test]
    fn file_review_names_the_weak_axes_and_the_lines() {
        let md = file_review_md(&file_review(), "2026-09-14T10:00:00Z");
        assert!(md.starts_with("<!-- antiloki context · kind=file · path=src/ui/Output.tsx · engineering_version=v3 · snapshot=7 · generated=2026-09-14T10:00:00Z -->"), "{md}");
        assert!(md.contains("**Health 62** · law score 70 · archetype logic"));
        assert!(md.contains("| structure | 15 | 0.15 | worst block `tabify` L87 (cognitive 26, nesting 4), 2 nested loops | split `tabify` (L87)"), "{md}");
        assert!(md.contains("| tests | 40 | 0.16 | no test reaches it (basis static-import); fan-in 50th pct | write one test"), "{md}");
        assert!(md.contains("| coupling | 90 | 0.12 | fan-in 20th pct · fan-out 30th pct |  |"), "a healthy axis carries no fix");
        assert!(md.contains("- **No loose equality** `no-loose-equality` ×2 (weight 5, warn) — Use === and !==."));
        assert!(md.contains("  - L12 in `tabify`: `if (a == b) {`"));
        assert!(md.contains("  - L40: `x != y`"));
        assert!(md.contains("  - fix: replace == with ==="));
        assert!(md.contains("- imported by (1): `src/ui/index.ts`"));
        assert!(md.contains("vs snapshot #6 (2026-09-01 00:00): health -3 · law score -5 · REGRESSED"), "{md}");
        assert!(md.contains("- new: `no-loose-equality` ×1 (L40)"));
        assert!(md.lines().count() < 60);
    }

    #[test]
    fn unmeasured_tests_and_baseline_reset_are_said_plainly() {
        let mut r = file_review();
        r["axes"]["tests"] = json!({ "score": null, "parts": { "covered": null, "basis": "unmeasured" } });
        r["regression"] = json!({ "prev_snapshot_id": 6, "prev_engineering_version": "v2", "comparable": false });
        let md = file_review_md(&r, "g");
        assert!(md.contains("| tests | — | 0.16 | unmeasured — unmeasured |"), "{md}");
        assert!(md.contains("baseline reset — the previous snapshot #6 was judged by engineering_version v2"));
    }

    #[test]
    fn not_found_is_an_honest_stub() {
        let md = file_review_md(&json!({ "path": "src/new.ts", "found": false }), "g");
        assert!(md.contains("_No constitution snapshot covers this file yet"));
    }

    #[test]
    fn folder_review_lists_members_worst_first() {
        let r = json!({
            "path": "src/ui", "found": true, "is_folder": true, "snapshot_id": 7, "files": 3, "health": 80, "health_logic": 70, "score": 85, "engineering_version": "v3", "tests_basis": "static-import",
            "roster": { "total": 3, "logic": 2, "glue": 1, "test": 0, "unknown": 0, "carried": 0 },
            "axes": { "compliance": { "score": 90 }, "structure": { "score": 60 } },
            "members": [
                { "path": "src/ui/A.tsx", "health": 80, "archetype": "logic", "loc": 100, "axes": { "structure": { "score": 90 }, "tests": { "score": 40 } } },
                { "path": "src/ui/B.tsx", "health": 60, "archetype": "logic", "loc": 300, "axes": { "structure": { "score": 30 } } },
                { "path": "src/ui/index.ts", "health": 100, "archetype": "glue", "loc": 3, "axes": {} }
            ],
            "violations": [ { "path": "src/ui/B.tsx", "law_key": "no-var", "title": "No var", "rule_md": "r", "weight": 5, "severity": "warn", "line": 3, "evidence": "var x" } ],
            "regression": null, "history": [ { "snapshot_id": 6, "health": 82 }, { "snapshot_id": 7, "health": 80 } ]
        });
        let md = folder_review_md(&r, "g");
        assert!(md.contains("**Health 80** over 3 files (2 logic, 1 glue, 0 test) · logic-only mean 70"));
        let b = md.find("`src/ui/B.tsx` — 60 · logic · loc 300 · weakest: structure 30").unwrap();
        let a = md.find("`src/ui/A.tsx` — 80 · logic · loc 100 · weakest: tests 40").unwrap();
        assert!(b < a, "worst first");
        assert!(md.contains("  - `src/ui/B.tsx` L3: `var x`"));
        assert!(md.contains("first snapshot — nothing to compare yet"));
        assert!(md.contains("history: #6 82 → #7 80"));
    }

    #[test]
    fn laws_md_groups_by_scope_and_prints_the_rule() {
        let composed = json!({
            "laws": [
                { "key": "no-var", "title": "No var", "scope": "project", "weight": 5, "severity": "warn", "enforcement": "score", "status": "ratified", "rule_md": "Use let/const.\n\nSecond line.", "applies_json": "{\"languages\":[\"ts\"]}" },
                { "key": "dead", "title": "Dead", "scope": "project", "status": "suspended", "rule_md": "x" }
            ],
            "exceptions": [ { "law_key": "no-var", "path_glob": "legacy/**", "reason": "old code" } ]
        });
        let md = laws_md(&composed, "g");
        assert!(md.contains("# The constitution — 1 ratified laws"));
        assert!(md.contains("## project\n- **No var** `no-var` — weight 5 · warn · score · applies languages: ts\n  Use let/const.\n  Second line."), "{md}");
        assert!(!md.contains("Dead"));
        assert!(md.contains("- `no-var` — `legacy/**` old code"));
    }

    #[test]
    fn graph_keeps_runtime_edges_touching_the_folder() {
        let nodes = vec![json!({"id":"1","path":"src/ui/A.tsx"}), json!({"id":"2","path":"src/ui/B.tsx"}), json!({"id":"3","path":"src/lib/x.ts"}), json!({"id":"4","path":"src/other/z.ts"})];
        let edges = vec![
            json!({"from_node":"1","to_node":"2","type":"imports","kind":"value"}),
            json!({"from_node":"1","to_node":"3","type":"imports","kind":null}),
            json!({"from_node":"2","to_node":"3","type":"imports","kind":"type"}),
            json!({"from_node":"4","to_node":"1","type":"imports","kind":"dynamic"}),
            json!({"from_node":"3","to_node":"4","type":"imports","kind":"value"}),
        ];
        let g = graph_json("src/ui", &nodes, &edges, "g");
        assert_eq!(g["counts"], json!({ "members": 2, "internal": 1, "outbound": 1, "inbound": 1 }));
        assert_eq!(g["edges"].as_array().unwrap().len(), 3, "type edge and the far edge dropped");
        assert_eq!(g["edges"][1], json!({ "from": "src/ui/A.tsx", "to": "src/lib/x.ts", "kind": "value" }));
    }

    #[test]
    fn tests_md_says_unmeasured_when_no_instrument() {
        let md = tests_md(&json!({ "instrument": { "test_files": 0 }, "coverage_by_source": {} }), "src/ui", "g");
        assert!(md.contains("unmeasured — the atlas holds no test files"));
        let md = tests_md(&json!({ "instrument": { "test_files": 3, "static_links": 2, "inline_files": 0, "e2e_specs": 1, "lcov": false }, "coverage_by_source": { "src/ui/A.tsx": ["src/ui/A.test.tsx"], "src/lib/x.ts": ["t"] } }), "src/ui", "g");
        assert!(md.contains("instrument: 3 test files · 2 link to source by import · 0 inline · 1 e2e specs"));
        assert!(md.contains("- `src/ui/A.tsx` ← `src/ui/A.test.tsx`"));
        assert!(!md.contains("src/lib/x.ts"));
    }

    #[test]
    fn blast_and_check_render() {
        let mut path_of = Map::new();
        path_of.insert("9".into(), json!("src/ui/B.tsx"));
        let md = blast_md(&json!({ "affected": ["9"], "affected_tests": ["src/ui/B.test.tsx"], "count": 1 }), "src/ui/A.tsx", &path_of, "g");
        assert!(md.contains("1 files depend on it transitively"));
        assert!(md.contains("- `src/ui/B.tsx`"));
        assert!(md.contains("run these first:\n- `src/ui/B.test.tsx`"));
        let md = check_md(&json!({ "path": "a.ts", "checked_laws": 4, "before": { "violations": 2 }, "after": { "violations": 1, "list": [ { "law_key": "no-var", "line": 3, "evidence": "var x" } ] }, "fixed": [ { "law_key": "no-var", "count": 1, "title": "No var" } ], "introduced": [], "compliance": { "before": 80, "after_est": 90 }, "note": null }));
        assert!(md.contains("4 laws re-measured · violations before 2 → after 1 · law score 80 → ≈90"));
        assert!(md.contains("- fixed `no-var` ×1 — No var"));
        assert!(md.contains("- `no-var` L3: `var x`"));
    }
}
