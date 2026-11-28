//! render/action-result-renderers.rs — twin of skills/render/action-result-renderers.mjs.
//! NAMES ARE CONTRACT: `renderActionResult` (named export, no-db helper — action_name, result,
//! opts) renders a dispatched action's result for the Interviewer REPL; `actionResultRenderers`
//! (default export, standard/db lane, ignores db) is the lint-conformant introspector returning
//! the registry shape. The JS renderer wraps every call in try/catch and never throws — this twin
//! mirrors the SUCCESS-PATH branches only (playbook's throws gotcha): every helper here is written
//! to never panic for well-formed inputs, so the catch-all fallback text is not modeled.
//!
//! Sentinel scheme: the JS source wraps its internal block markers (H1/H2/H3/PRE/LI/BR/HR/P-end/
//! bold/dim) in U+0001 (SOH) control bytes on both sides so the tag-strip pass can't confuse a
//! literal "H1"/"BR" appearing in HTML content with a marker. This twin uses the same U+0001
//! wrapping (Rust's `\u{1}` string escape) so the split/compare logic matches byte-for-byte.
// QUALITY: HTML→TUI converter port; `<(strong|b)...>...</\1>` backreferences (regex crate has none,
// gotcha #7) are approximated as two sequential same-tag passes — identical to the JS single pass
// for non-nested/non-mismatched strong/b (and em/i) markup, which is what the fixtures exercise.
use rusqlite::Connection;
use regex::Regex;
use serde_json::{json, Value};

// ---- standard helpers (playbook-verbatim) ----
fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v:&Value)->String{ match v{Value::Null=>"null".into(),Value::String(s)=>s.clone(),Value::Number(n)=>n.to_string(),Value::Bool(b)=>b.to_string(),Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","),Value::Object(_)=>"[object Object]".into()} } // = JS String(v)
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand: nz(a).or(nz(b))
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

// ---- ANSI / paint ----
fn ansi_code(color: &str) -> &'static str {
    match color {
        "reset" => "\x1b[0m", "bold" => "\x1b[1m", "dim" => "\x1b[2m",
        "red" => "\x1b[31m", "green" => "\x1b[32m", "yellow" => "\x1b[33m",
        "blue" => "\x1b[34m", "cyan" => "\x1b[36m", "magenta" => "\x1b[35m",
        _ => "",
    }
}
fn paint(color: &str, text: &str, on: bool) -> String {
    if on { format!("{}{}{}", ansi_code(color), text, ansi_code("reset")) } else { text.to_string() }
}

const MAX_FALLBACK: usize = 1500;

// JS: if (s==null) return ''; s=String(s); return s.length<=n ? s : s.slice(0,n-1)+'…';
fn truncate(v: Option<&Value>, n: i64) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(x) => {
            let s = js_string(x);
            let chars: Vec<char> = s.chars().collect();
            let len = chars.len() as i64;
            if len <= n { return s; }
            let end_idx: usize = if n >= 1 { (n - 1) as usize } else { (len - 1).max(0) as usize };
            let head: String = chars[..end_idx.min(chars.len())].iter().collect();
            format!("{}\u{2026}", head)
        }
    }
}
// JS: s=String(s??''); return s.length>=n ? s : s+' '.repeat(n-s.length); (called only on already-
// resolved strings, so the ??'' coercion is a no-op at every call site)
fn pad_right(s: &str, n: usize) -> String {
    let len = s.chars().count();
    if len >= n { s.to_string() } else { format!("{}{}", s, " ".repeat(n - len)) }
}
fn as_array(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.clone(),
        Value::Null => vec![],
        Value::Object(map) => {
            if let Some(Value::Array(a)) = map.get("rows") { return a.clone(); }
            if let Some(Value::Array(a)) = map.get("items") { return a.clone(); }
            if let Some(Value::Array(a)) = map.get("findings") { return a.clone(); }
            vec![v.clone()]
        }
        _ => vec![v.clone()],
    }
}
// Array.prototype.join semantics: null/undefined elements stringify to '' (not "null").
fn array_join(items: &[Value], sep: &str) -> String {
    items.iter().map(|v| match v { Value::Null => String::new(), other => js_string(other) }).collect::<Vec<_>>().join(sep)
}

// ---- Renderers ----

fn render_card_list(result: &Value, on: bool) -> String {
    let rows = as_array(result);
    if rows.is_empty() { return paint("dim", "  (no cards)", on); }
    let slug_w = rows.iter().map(|r| str_qq(r.get("slug")).chars().count()).max().unwrap_or(0).min(28);
    let status_w = rows.iter().map(|r| str_qq(r.get("status")).chars().count()).max().unwrap_or(0).min(12);
    let mut lines: Vec<String> = vec![
        paint("bold", &format!("  {}  {}  title", pad_right("slug", slug_w), pad_right("status", status_w)), on),
        format!("  {}  {}  {}", "\u{2500}".repeat(slug_w), "\u{2500}".repeat(status_w), "\u{2500}".repeat(40)),
    ];
    for r in &rows {
        lines.push(format!("  {}  {}  {}",
            pad_right(&truncate(r.get("slug"), slug_w as i64), slug_w),
            pad_right(&truncate(r.get("status"), status_w as i64), status_w),
            truncate(r.get("title"), 60)));
    }
    lines.push(paint("dim", &format!("  \u{2500}\u{2500} {} card{}", rows.len(), if rows.len() == 1 { "" } else { "s" }), on));
    lines.join("\n")
}

fn render_memory_list(result: &Value, on: bool) -> String {
    let rows = as_array(result);
    if rows.is_empty() { return paint("dim", "  (no memories)", on); }
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Vec<Value>> = std::collections::HashMap::new();
    for m in &rows {
        let type_val: Value = nz(m.get("type")).cloned().unwrap_or(json!("unknown"));
        let key = tmpl(Some(&type_val));
        if !groups.contains_key(&key) {
            order.push(key.clone());
            groups.insert(key.clone(), Vec::new());
        }
        groups.get_mut(&key).unwrap().push(m.clone());
    }
    let mut lines: Vec<String> = Vec::new();
    for key in &order {
        let group = &groups[key];
        lines.push(paint("bold", &format!("  [{}] ({})", key, group.len()), on));
        for m in group.iter().take(8) {
            lines.push(format!("    #{}  {}", tmpl(m.get("id")), truncate(m.get("title"), 60)));
            if truthy(m.get("guidance")) {
                lines.push(paint("dim", &format!("         \u{2192} {}", truncate(m.get("guidance"), 80)), on));
            }
        }
        if group.len() > 8 {
            lines.push(paint("dim", &format!("    \u{2026} +{} more", group.len() - 8), on));
        }
    }
    lines.push(paint("dim", &format!("  \u{2500}\u{2500} {} memor{}", rows.len(), if rows.len() == 1 { "y" } else { "ies" }), on));
    lines.join("\n")
}

fn render_ranked(result: &Value, on: bool) -> String {
    let rows = as_array(result);
    if rows.is_empty() { return paint("dim", "  (no matches)", on); }
    let mut lines: Vec<String> = Vec::new();
    for (i, r) in rows.iter().take(10).enumerate() {
        let score = nz(r.get("score")).or_else(|| nz(r.get("composite_score"))).or_else(|| nz(r.get("cosine")));
        let score_str = match score {
            Some(s) => format!(" [{:.3}]", js_number(Some(s))),
            None => String::new(),
        };
        let id_val: Value = nz(r.get("memory_id")).or_else(|| nz(r.get("id"))).cloned().unwrap_or(json!(""));
        let title_val: Value = nz(r.get("title")).or_else(|| nz(r.get("guidance"))).cloned().unwrap_or(json!(""));
        lines.push(format!("  {}.{} #{}  {}", i + 1, score_str, tmpl(Some(&id_val)), truncate(Some(&title_val), 60)));
        if let Some(Value::Array(sources)) = r.get("sources") {
            if !sources.is_empty() {
                lines.push(paint("dim", &format!("      via: {}", array_join(sources, ", ")), on));
            }
        }
    }
    if rows.len() > 10 {
        lines.push(paint("dim", &format!("  \u{2026} +{} more", rows.len() - 10), on));
    }
    lines.join("\n")
}

fn render_keeper_review(result: &Value, on: bool) -> String {
    let mut lines: Vec<String> = Vec::new();
    let u = result.get("unvalidated_recent").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let c = result.get("consolidation_candidates").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let o = result.get("orphan_memories").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    lines.push(paint("bold", &format!("  Unvalidated-but-used ({})", u.len()), on));
    for m in u.iter().take(5) {
        let uses = nz(m.get("uses")).cloned().unwrap_or(json!(0));
        lines.push(format!("    #{}  {}  [uses: {}]", tmpl(m.get("id")), truncate(m.get("title"), 60), tmpl(Some(&uses))));
    }
    if u.len() > 5 { lines.push(paint("dim", &format!("    \u{2026} +{}", u.len() - 5), on)); }
    lines.push(paint("bold", &format!("  Consolidation candidates ({})", c.len()), on));
    for cluster in c.iter().take(5) {
        let member_ids = cluster.get("member_ids").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let shared_tags = cluster.get("shared_tags").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        lines.push(format!("    cluster of {}: tags=[{}]", member_ids.len(), array_join(&shared_tags, ", ")));
    }
    lines.push(paint("bold", &format!("  Orphans ({})", o.len()), on));
    for m in o.iter().take(5) {
        lines.push(format!("    #{}  {}", tmpl(m.get("id")), truncate(m.get("title"), 60)));
    }
    if truthy(result.get("summary_md")) {
        lines.push(paint("dim", &format!("  \u{2500}\u{2500} {}", tmpl(result.get("summary_md"))), on));
    }
    lines.join("\n")
}

fn render_driver_tick(result: &Value, on: bool) -> String {
    if !truthy(Some(result)) { return paint("dim", "  (no tick result)", on); }
    let outcome_val: Value = nz(result.get("outcome")).cloned().unwrap_or(json!("unknown"));
    let outcome = outcome_val.as_str().unwrap_or("");
    let color = match outcome { "fired" => "green", "failed" => "red", "idle" => "dim", _ => "yellow" };
    let card_slug: Value = nz(result.get("card_slug")).cloned().unwrap_or(json!("\u{2014}"));
    let decided_action: Value = nz(result.get("decided_action")).cloned().unwrap_or(json!("\u{2014}"));
    let duration: Value = nz(result.get("duration_ms")).cloned().unwrap_or(json!("?"));
    let cost: Value = nz(result.get("cost_usd")).cloned().unwrap_or(json!(0));
    let mut lines: Vec<String> = vec![
        paint(color, &format!("  \u{2295} tick {}", tmpl(Some(&outcome_val))), on),
        paint("dim", &format!("    card={}  action={}", tmpl(Some(&card_slug)), tmpl(Some(&decided_action))), on),
        paint("dim", &format!("    duration={}ms  cost=${:.4}", tmpl(Some(&duration)), js_number(Some(&cost))), on),
    ];
    if truthy(result.get("decided_reason")) {
        lines.push(paint("dim", &format!("    why: {}", truncate(result.get("decided_reason"), 100)), on));
    }
    if truthy(result.get("error_message")) {
        lines.push(paint("red", &format!("    error: {}", truncate(result.get("error_message"), 100)), on));
    }
    lines.join("\n")
}

fn render_consistency_scan(result: &Value, on: bool) -> String {
    let findings = result.get("findings").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let summary = result.get("summary").cloned().unwrap_or(json!({}));
    if findings.is_empty() { return paint("dim", "  (no findings)", on); }
    let mut critical: Vec<Value> = Vec::new();
    let mut warning: Vec<Value> = Vec::new();
    let mut info: Vec<Value> = Vec::new();
    for f in &findings {
        match f.get("severity").and_then(|v| v.as_str()) {
            Some("critical") => critical.push(f.clone()),
            Some("warning") => warning.push(f.clone()),
            _ => info.push(f.clone()),
        }
    }
    let _ = &info; // info bucket exists for parity with JS `buckets` but is never rendered individually
    let crit_n = nz(summary.get("critical")).cloned().unwrap_or(json!(0));
    let warn_n = nz(summary.get("warning")).cloned().unwrap_or(json!(0));
    let info_n = nz(summary.get("info")).cloned().unwrap_or(json!(0));
    let mut lines: Vec<String> = vec![
        paint("bold", &format!("  {} critical \u{b7} {} warning \u{b7} {} info", tmpl(Some(&crit_n)), tmpl(Some(&warn_n)), tmpl(Some(&info_n))), on),
    ];
    let buckets: [(&str, &Vec<Value>, &str); 2] = [("critical", &critical, "red"), ("warning", &warning, "yellow")];
    for (sev, bucket, color) in buckets {
        if bucket.is_empty() { continue; }
        lines.push(paint(color, &format!("  [{}]", sev.to_uppercase()), on));
        for f in bucket.iter().take(6) {
            lines.push(format!("    \u{2022} [{}] {}", tmpl(f.get("dimension")), truncate(f.get("message"), 100)));
        }
        if bucket.len() > 6 {
            lines.push(paint("dim", &format!("    \u{2026} +{}", bucket.len() - 6), on));
        }
    }
    lines.join("\n")
}

fn render_anatomy_latest(result: &Value, on: bool) -> String {
    let payload: &Value = nz(result.get("payload")).unwrap_or(result);
    let has_counts = payload.get("counts").map_or(false, |v| truthy(Some(v)));
    if !has_counts {
        return render_json_fallback(result, on);
    }
    let counts = payload.get("counts").unwrap();
    let id_val: Value = nz(payload.get("id")).cloned().unwrap_or(json!("?"));
    let skills = nz(counts.get("skills")).cloned().unwrap_or(json!("?"));
    let actions = nz(counts.get("actions")).cloned().unwrap_or(json!("?"));
    let agents = nz(counts.get("agents")).cloned().unwrap_or(json!("?"));
    let tables = nz(counts.get("tables")).cloned().unwrap_or(json!("?"));
    let mut lines: Vec<String> = vec![
        paint("bold", &format!("  substrate anatomy snapshot #{}", tmpl(Some(&id_val))), on),
        format!("    skills={}  actions={}  agents={}  tables={}", tmpl(Some(&skills)), tmpl(Some(&actions)), tmpl(Some(&agents)), tmpl(Some(&tables))),
    ];
    if truthy(payload.get("taken_at")) {
        lines.push(paint("dim", &format!("    taken_at: {}", tmpl(payload.get("taken_at"))), on));
    }
    lines.join("\n")
}

fn render_terse_list(result: &Value, on: bool) -> String {
    let rows = as_array(result);
    if rows.is_empty() { return paint("dim", "  (empty)", on); }
    let mut lines: Vec<String> = Vec::new();
    for r in rows.iter().take(15) {
        let id_val: Value = nz(r.get("id")).or_else(|| nz(r.get("slug"))).or_else(|| nz(r.get("name"))).cloned().unwrap_or(json!("?"));
        let label_val: Value = nz(r.get("title")).or_else(|| nz(r.get("description"))).or_else(|| nz(r.get("statement"))).cloned().unwrap_or(json!(""));
        let suffix = if truthy(Some(&label_val)) { format!("  {}", truncate(Some(&label_val), 70)) } else { String::new() };
        lines.push(format!("  - {}{}", tmpl(Some(&id_val)), suffix));
    }
    if rows.len() > 15 {
        lines.push(paint("dim", &format!("  \u{2026} +{} more", rows.len() - 15), on));
    }
    lines.join("\n")
}

fn indent_lines(s: &str) -> String {
    s.split('\n').map(|l| format!("  {}", l)).collect::<Vec<_>>().join("\n")
}

fn render_json_fallback(result: &Value, on: bool) -> String {
    if result.is_null() { return paint("dim", "  (no result)", on); }
    if let Value::String(s) = result {
        let trimmed: String = s.trim_start().chars().take(200).collect::<String>().to_lowercase();
        if trimmed.starts_with("<!doctype") || trimmed.starts_with("<html") {
            return render_html_as_tui(result, on);
        }
        let truncated = truncate(Some(result), 600);
        return indent_lines(&truncated);
    }
    let mut s = serde_json::to_string_pretty(result).unwrap_or_else(|_| js_string(result));
    if s.chars().count() > MAX_FALLBACK {
        let head: String = s.chars().take(MAX_FALLBACK).collect();
        s = format!("{}\n  \u{2026}(truncated)", head);
    }
    indent_lines(&s)
}

// ---- HTML → TUI renderer ----

const MARK_H1_OPEN: &str = "\u{1}H1\u{1}";
const MARK_H1_CLOSE: &str = "\u{1}/H1\u{1}";
const MARK_H2_OPEN: &str = "\u{1}H2\u{1}";
const MARK_H2_CLOSE: &str = "\u{1}/H2\u{1}";
const MARK_H3_OPEN: &str = "\u{1}H3\u{1}";
const MARK_H3_CLOSE: &str = "\u{1}/H3\u{1}";
const MARK_BOLD_OPEN: &str = "\u{1}B\u{1}";
const MARK_BOLD_CLOSE: &str = "\u{1}/B\u{1}";
const MARK_DIM_OPEN: &str = "\u{1}D\u{1}";
const MARK_DIM_CLOSE: &str = "\u{1}/D\u{1}";
const MARK_LI: &str = "\u{1}LI\u{1}";
const MARK_BREAK: &str = "\u{1}BR\u{1}";
const MARK_HR: &str = "\u{1}HR\u{1}";
const MARK_PRE_OPEN: &str = "\u{1}PRE\u{1}";
const MARK_PRE_CLOSE: &str = "\u{1}/PRE\u{1}";
const MARK_P_END: &str = "\u{1}/P\u{1}";

fn named_entity(full: &str) -> String {
    match full {
        "&amp;" => "&".to_string(),
        "&lt;" => "<".to_string(),
        "&gt;" => ">".to_string(),
        "&quot;" => "\"".to_string(),
        "&#39;" => "'".to_string(),
        "&apos;" => "'".to_string(),
        "&nbsp;" => " ".to_string(),
        other => other.to_string(),
    }
}

fn decode_entities_str(s: &str) -> String {
    let named_re = Regex::new(r"&(amp|lt|gt|quot|#39|apos|nbsp);").unwrap();
    let s1 = named_re.replace_all(s, |caps: &regex::Captures| named_entity(&caps[0]));
    let dec_re = Regex::new(r"&#(\d+);").unwrap();
    let s2 = dec_re.replace_all(&s1, |caps: &regex::Captures| {
        caps[1].parse::<u32>().ok().and_then(char::from_u32).map(|c| c.to_string()).unwrap_or_default()
    });
    let hex_re = Regex::new(r"(?i)&#x([0-9a-f]+);").unwrap();
    let s3 = hex_re.replace_all(&s2, |caps: &regex::Captures| {
        u32::from_str_radix(&caps[1], 16).ok().and_then(char::from_u32).map(|c| c.to_string()).unwrap_or_default()
    });
    s3.into_owned()
}

fn extract_title(html: &str) -> Option<String> {
    let title_re = Regex::new(r"(?is)<title[^>]*>(.*?)</title>").unwrap();
    let h1_re = Regex::new(r"(?is)<h1[^>]*>(.*?)</h1>").unwrap();
    let cap = title_re.captures(html).or_else(|| h1_re.captures(html));
    cap.map(|c| {
        let inner = c.get(1).map(|m| m.as_str()).unwrap_or("");
        let decoded = decode_entities_str(inner);
        let ws_re = Regex::new(r"\s+").unwrap();
        ws_re.replace_all(&decoded, " ").trim().to_string()
    })
}

// width=78, indent="  " — hardcoded, the only values ever passed at any call site.
fn word_wrap(text: &str) -> String {
    let nl_re = Regex::new(r"\n+").unwrap();
    let indent = "  ";
    let indent_len = indent.chars().count();
    let mut out: Vec<String> = Vec::new();
    for para in nl_re.split(text) {
        let words: Vec<&str> = para.split_whitespace().collect();
        if words.is_empty() { continue; }
        let mut line = indent.to_string();
        for w in words {
            let sep_len = if line.chars().count() == indent_len { 0 } else { 1 };
            let candidate_len = line.chars().count() + sep_len + w.chars().count();
            if candidate_len > 78 {
                out.push(line.clone());
                line = format!("{}{}", indent, w);
            } else {
                if sep_len == 1 { line.push(' '); }
                line.push_str(w);
            }
        }
        if !line.trim().is_empty() { out.push(line); }
    }
    out.join("\n")
}

fn apply_inline_markers(text: &str, on: bool) -> String {
    if on {
        text.replace(MARK_BOLD_OPEN, ansi_code("bold"))
            .replace(MARK_BOLD_CLOSE, ansi_code("reset"))
            .replace(MARK_DIM_OPEN, ansi_code("dim"))
            .replace(MARK_DIM_CLOSE, ansi_code("reset"))
    } else {
        text.replace(MARK_BOLD_OPEN, "")
            .replace(MARK_BOLD_CLOSE, "")
            .replace(MARK_DIM_OPEN, "")
            .replace(MARK_DIM_CLOSE, "")
    }
}

fn strip_markers(text: &str) -> String {
    let re = Regex::new("\u{1}[^\u{1}]*\u{1}").unwrap();
    re.replace_all(text, "").into_owned()
}

fn split_with_markers(body: &str) -> Vec<String> {
    let re = Regex::new("\u{1}(?:H1|/H1|H2|/H2|H3|/H3|PRE|/PRE|LI|BR|HR|/P)\u{1}").unwrap();
    let mut out = Vec::new();
    let mut last = 0usize;
    for m in re.find_iter(body) {
        out.push(body[last..m.start()].to_string());
        out.push(m.as_str().to_string());
        last = m.end();
    }
    out.push(body[last..].to_string());
    out
}

fn flush_para(out: &mut Vec<String>, para_buffer: &mut String, on: bool) {
    let t = para_buffer.trim().to_string();
    para_buffer.clear();
    if t.is_empty() { return; }
    let styled = apply_inline_markers(&t, on);
    let stripped = strip_markers(&styled);
    out.push(word_wrap(&stripped));
    out.push(String::new());
}

fn render_html_as_tui(html: &Value, on: bool) -> String {
    let html_str: &str = match html { Value::String(s) => s.as_str(), _ => "" };
    if html_str.is_empty() {
        return paint("dim", "  (empty html)", on);
    }

    // 1. Strip <head>, <style>, <script>, comments
    let comment_re = Regex::new(r"(?s)<!--.*?-->").unwrap();
    let head_re = Regex::new(r"(?is)<head\b[^>]*>.*?</head>").unwrap();
    let style_re = Regex::new(r"(?is)<style\b[^>]*>.*?</style>").unwrap();
    let script_re = Regex::new(r"(?is)<script\b[^>]*>.*?</script>").unwrap();
    let mut body = comment_re.replace_all(html_str, "").into_owned();
    body = head_re.replace_all(&body, "").into_owned();
    body = style_re.replace_all(&body, "").into_owned();
    body = script_re.replace_all(&body, "").into_owned();

    // 2. title from the ORIGINAL html (mirrors JS: extractTitle(html), not body)
    let title = extract_title(html_str);

    // 3. Block transforms, wrapped in \u{1}-sentinel markers
    let h1_re = Regex::new(r"(?is)<h1\b[^>]*>(.*?)</h1>").unwrap();
    body = h1_re.replace_all(&body, |c: &regex::Captures| format!("\n\n{}{}{}\n\n", MARK_H1_OPEN, &c[1], MARK_H1_CLOSE)).into_owned();
    let h2_re = Regex::new(r"(?is)<h2\b[^>]*>(.*?)</h2>").unwrap();
    body = h2_re.replace_all(&body, |c: &regex::Captures| format!("\n\n{}{}{}\n\n", MARK_H2_OPEN, &c[1], MARK_H2_CLOSE)).into_owned();
    let h3_re = Regex::new(r"(?is)<h3\b[^>]*>(.*?)</h3>").unwrap();
    body = h3_re.replace_all(&body, |c: &regex::Captures| format!("\n{}{}{}\n", MARK_H3_OPEN, &c[1], MARK_H3_CLOSE)).into_owned();
    let h456_re = Regex::new(r"(?is)<h[4-6]\b[^>]*>(.*?)</h[4-6]>").unwrap();
    body = h456_re.replace_all(&body, |c: &regex::Captures| format!("\n{}{}{}\n", MARK_H3_OPEN, &c[1], MARK_H3_CLOSE)).into_owned();

    // Pre/code blocks BEFORE inline code so they don't collide
    let pre_re = Regex::new(r"(?is)<pre\b[^>]*>(.*?)</pre>").unwrap();
    body = pre_re.replace_all(&body, |c: &regex::Captures| {
        let code_tag_re = Regex::new(r"(?i)</?code\b[^>]*>").unwrap();
        let code_text = code_tag_re.replace_all(&c[1], "");
        format!("\n{}{}{}\n", MARK_PRE_OPEN, code_text, MARK_PRE_CLOSE)
    }).into_owned();

    // List items
    let li_re = Regex::new(r"(?is)<li\b[^>]*>(.*?)</li>").unwrap();
    body = li_re.replace_all(&body, |c: &regex::Captures| format!("{}{}\n", MARK_LI, &c[1])).into_owned();

    // Links: keep label + href in dim.
    let a_re = Regex::new(r#"(?is)<a\b[^>]*href=["']([^"']+)["'][^>]*>(.*?)</a>"#).unwrap();
    body = a_re.replace_all(&body, |c: &regex::Captures| format!("{} {}({}){}", c[2].trim(), MARK_DIM_OPEN, &c[1], MARK_DIM_CLOSE)).into_owned();

    // Inline code, strong, em. The original JS combines strong|b (and em|i) into one regex with a
    // \1 backreference on the closing tag name — the regex crate has no backreferences (gotcha #7),
    // so this twin runs two sequential same-tag passes instead. Identical result for non-nested,
    // non-mismatched markup (what the fixtures use); a `<b>...<strong>...</strong></b>` nesting
    // would diverge and is deliberately out of scope.
    let code_re = Regex::new(r"(?is)<code\b[^>]*>(.*?)</code>").unwrap();
    body = code_re.replace_all(&body, |c: &regex::Captures| format!("{}{}{}", MARK_DIM_OPEN, &c[1], MARK_DIM_CLOSE)).into_owned();
    let strong_re = Regex::new(r"(?is)<strong\b[^>]*>(.*?)</strong>").unwrap();
    body = strong_re.replace_all(&body, |c: &regex::Captures| format!("{}{}{}", MARK_BOLD_OPEN, &c[1], MARK_BOLD_CLOSE)).into_owned();
    let b_re = Regex::new(r"(?is)<b\b[^>]*>(.*?)</b>").unwrap();
    body = b_re.replace_all(&body, |c: &regex::Captures| format!("{}{}{}", MARK_BOLD_OPEN, &c[1], MARK_BOLD_CLOSE)).into_owned();
    let em_re = Regex::new(r"(?is)<em\b[^>]*>(.*?)</em>").unwrap();
    body = em_re.replace_all(&body, |c: &regex::Captures| format!("{}{}{}", MARK_BOLD_OPEN, &c[1], MARK_BOLD_CLOSE)).into_owned();
    let i_re = Regex::new(r"(?is)<i\b[^>]*>(.*?)</i>").unwrap();
    body = i_re.replace_all(&body, |c: &regex::Captures| format!("{}{}{}", MARK_BOLD_OPEN, &c[1], MARK_BOLD_CLOSE)).into_owned();

    // Breaks + hr + paragraph ends
    let br_re = Regex::new(r"(?i)<br\s*/?\s*>").unwrap();
    body = br_re.replace_all(&body, MARK_BREAK).into_owned();
    let hr_re = Regex::new(r"(?i)<hr\s*/?\s*>").unwrap();
    body = hr_re.replace_all(&body, MARK_HR).into_owned();
    let p_end_re = Regex::new(r"(?i)</p\s*>").unwrap();
    body = p_end_re.replace_all(&body, MARK_P_END).into_owned();

    // Strip remaining tags
    let tag_re = Regex::new(r"<[^>]+>").unwrap();
    body = tag_re.replace_all(&body, "").into_owned();

    // Decode entities
    body = decode_entities_str(&body);

    // Collapse runs of whitespace WITHIN lines (preserve newlines)
    let ws_inline_re = Regex::new(r"[ \t]+").unwrap();
    body = body.split('\n').map(|l| ws_inline_re.replace_all(l, " ").trim_end().to_string()).collect::<Vec<_>>().join("\n");

    // 4. Materialize markers into ANSI-styled output, line by line
    let mut out: Vec<String> = Vec::new();
    if let Some(t) = &title {
        out.push(paint("bold", &format!("  \u{2550}\u{2550}\u{2550} {} \u{2550}\u{2550}\u{2550}", t), on));
        out.push(String::new());
    }

    let ws_re = Regex::new(r"\s+").unwrap();
    let trailing_nl_re = Regex::new(r"\n+$").unwrap();
    let segments = split_with_markers(&body);
    let mut i = 0usize;
    let mut in_pre = false;
    let mut pre_buffer: Vec<String> = Vec::new();
    let mut para_buffer = String::new();

    while i < segments.len() {
        let seg = segments[i].clone();
        i += 1;
        if seg == MARK_PRE_OPEN { in_pre = true; pre_buffer.clear(); continue; }
        if seg == MARK_PRE_CLOSE {
            in_pre = false;
            let joined = pre_buffer.join("");
            let block = trailing_nl_re.replace(&joined, "");
            for line in block.split('\n') {
                out.push(paint("dim", &format!("  \u{2502} {}", line), on));
            }
            out.push(String::new());
            continue;
        }
        if in_pre { pre_buffer.push(seg); continue; }

        if seg == MARK_H1_OPEN || seg == MARK_H1_CLOSE {
            flush_para(&mut out, &mut para_buffer, on);
            let next = segments.get(i).cloned().unwrap_or_default();
            i += 1;
            let decoded = decode_entities_str(&next);
            let collapsed = ws_re.replace_all(&decoded, " ");
            out.push(paint("bold", &format!("  \u{2550}\u{2550}\u{2550} {} \u{2550}\u{2550}\u{2550}", collapsed.trim()), on));
            out.push(String::new());
            while i < segments.len() && segments[i] != MARK_H1_CLOSE { i += 1; }
            i += 1;
            continue;
        }
        if seg == MARK_H2_OPEN {
            flush_para(&mut out, &mut para_buffer, on);
            let next = segments.get(i).cloned().unwrap_or_default();
            i += 1;
            let decoded = decode_entities_str(&next);
            let collapsed = ws_re.replace_all(&decoded, " ");
            out.push(paint("bold", &format!("  {}", collapsed.trim()), on));
            out.push(String::new());
            while i < segments.len() && segments[i] != MARK_H2_CLOSE { i += 1; }
            i += 1;
            continue;
        }
        if seg == MARK_H3_OPEN {
            flush_para(&mut out, &mut para_buffer, on);
            let next = segments.get(i).cloned().unwrap_or_default();
            i += 1;
            let decoded = decode_entities_str(&next);
            let collapsed = ws_re.replace_all(&decoded, " ");
            out.push(paint("bold", &format!("  {}", collapsed.trim()), on));
            while i < segments.len() && segments[i] != MARK_H3_CLOSE { i += 1; }
            i += 1;
            continue;
        }
        if seg == MARK_LI {
            flush_para(&mut out, &mut para_buffer, on);
            let next = segments.get(i).cloned().unwrap_or_default();
            i += 1;
            let styled = apply_inline_markers(&next, on);
            let stripped = strip_markers(&styled);
            let collapsed = ws_re.replace_all(&stripped, " ");
            out.push(format!("  \u{2022} {}", collapsed.trim()));
            continue;
        }
        if seg == MARK_HR {
            flush_para(&mut out, &mut para_buffer, on);
            out.push(paint("dim", &format!("  {}", "\u{2500}".repeat(30)), on));
            out.push(String::new());
            continue;
        }
        if seg == MARK_BREAK { para_buffer.push('\n'); continue; }
        if seg == MARK_P_END { flush_para(&mut out, &mut para_buffer, on); continue; }

        para_buffer.push_str(&seg);
    }
    flush_para(&mut out, &mut para_buffer, on);

    while out.last().map_or(false, |s| s.is_empty()) { out.pop(); }
    out.join("\n")
}

// ---- Registry dispatch ----

fn registry_render(action_name: &str, result: &Value, on: bool) -> String {
    match action_name {
        "board-keeper.card.list" => render_card_list(result, on),
        "system.memory.list" => render_memory_list(result, on),
        "system.memory.similar" | "system.memory.rag.context" => render_ranked(result, on),
        "keeper.review" => render_keeper_review(result, on),
        "system.driver.tick" => render_driver_tick(result, on),
        "system.consistency.scan" => render_consistency_scan(result, on),
        "system.consistency.report" => render_json_fallback(result, on),
        "system.anatomy.latest" => render_anatomy_latest(result, on),
        "system.law.list" | "system.action.find" | "system.agent.find" | "system.skill.find" | "contract.list" => render_terse_list(result, on),
        "system.docs.getting_started" | "system.docs.index" | "system.docs.driver" | "system.docs.rag"
        | "system.docs.sandbox" | "system.docs.defense" | "system.docs.agent_stages" | "system.docs.concurrency"
        | "card.render.html" | "card.render.index.html" | "contract.render.html" | "contract.render.index.html"
        | "system.anatomy.html" | "system.memory.html" | "system.memory.graph.html" | "man.render.compliance.html"
        | "board-keeper.card.html" | "system.ast.html" | "system.log.tail.html" | "system.score.autonomy.html" => render_html_as_tui(result, on),
        _ => render_json_fallback(result, on),
    }
}

pub fn renderActionResult(action_name: &Value, result: &Value, opts: &Value) -> Value {
    let on = truthy(opts.get("color"));
    let name = action_name.as_str().unwrap_or("");
    json!(registry_render(name, result, on))
}

pub fn actionResultRenderers(_input: &Value, _db: &Connection) -> Result<Value, String> {
    Ok(json!({
        "registered": 34,
        "actions": [
            "board-keeper.card.list", "system.memory.list", "system.memory.similar", "system.memory.rag.context",
            "keeper.review", "system.driver.tick", "system.consistency.scan", "system.consistency.report",
            "system.anatomy.latest", "system.law.list", "system.action.find", "system.agent.find",
            "system.skill.find", "contract.list",
            "system.docs.getting_started", "system.docs.index", "system.docs.driver", "system.docs.rag",
            "system.docs.sandbox", "system.docs.defense", "system.docs.agent_stages", "system.docs.concurrency",
            "card.render.html", "card.render.index.html", "contract.render.html", "contract.render.index.html",
            "system.anatomy.html", "system.memory.html", "system.memory.graph.html", "man.render.compliance.html",
            "board-keeper.card.html", "system.ast.html", "system.log.tail.html", "system.score.autonomy.html"
        ]
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(action: &str, result: Value, opts: Value) -> String {
        renderActionResult(&json!(action), &result, &opts).as_str().unwrap().to_string()
    }

    #[test]
    fn card_list_empty_and_populated() {
        assert_eq!(call("board-keeper.card.list", json!([]), json!({})), "  (no cards)");
        let rows = json!([
            { "slug": "A-1", "status": "backlog", "title": "first card" },
            { "slug": "B-22", "status": "doing", "title": "second card" },
        ]);
        let out = call("board-keeper.card.list", rows, json!({}));
        assert_eq!(out, "  slug  status   title\n  ────  ───────  ────────────────────────────────────────\n  A-1   backlog  first card\n  B-22  doing    second card\n  ── 2 cards");
    }

    #[test]
    fn card_list_color_and_malformed_input_is_safe() {
        let out = call("board-keeper.card.list", json!([{ "slug": "X", "status": "doing", "title": "t" }]), json!({ "color": true }));
        assert!(out.starts_with("\u{1b}[1m"));
        assert!(out.contains("\u{1b}[2m  ── 1 card\u{1b}[0m"));
        // asArray fallback-wraps a non-array object instead of throwing.
        let out2 = call("board-keeper.card.list", json!({ "notAnArray": true }), json!({}));
        assert!(out2.contains("── 1 card"));
    }

    #[test]
    fn memory_list_groups_by_type_and_overflows() {
        let rows = json!([
            { "id": 1, "type": "lesson", "title": "L1", "guidance": "g1" },
            { "id": 2, "type": "lesson", "title": "L2" },
            { "id": 3, "type": "preference", "title": "P1", "guidance": "g3" },
        ]);
        assert_eq!(call("system.memory.list", rows, json!({})),
            "  [lesson] (2)\n    #1  L1\n         \u{2192} g1\n    #2  L2\n  [preference] (1)\n    #3  P1\n         \u{2192} g3\n  ── 3 memories");
        let mut many = Vec::new();
        for i in 1..=9 { many.push(json!({ "id": i, "type": "lesson", "title": format!("L{i}") })); }
        let out = call("system.memory.list", json!(many), json!({}));
        assert!(out.contains("\u{2026} +1 more"));
        assert!(out.ends_with("── 9 memories"));
    }

    #[test]
    fn ranked_scores_sources_and_overflow() {
        let rows = json!([
            { "memory_id": 1, "title": "top match", "score": 0.95, "sources": ["similarity", "graph"] },
            { "memory_id": 2, "title": "second", "score": 0.8 },
        ]);
        assert_eq!(call("system.memory.similar", rows, json!({})),
            "  1. [0.950] #1  top match\n      via: similarity, graph\n  2. [0.800] #2  second");
        assert_eq!(call("system.memory.rag.context", json!([]), json!({})), "  (no matches)");
        let mut many = Vec::new();
        for i in 1..=11 { many.push(json!({ "id": i, "title": format!("T{i}"), "cosine": 0.9 })); }
        let out = call("system.memory.similar", json!(many), json!({}));
        assert!(out.contains("\u{2026} +1 more"));
    }

    #[test]
    fn keeper_review_full_and_empty() {
        let result = json!({
            "unvalidated_recent": [{ "id": 1, "title": "unv", "uses": 2 }],
            "consolidation_candidates": [{ "member_ids": [1, 2, 3], "shared_tags": ["x", "y"] }],
            "orphan_memories": [{ "id": 9, "title": "orphan" }],
            "summary_md": "keeper.review: 1 unvalidated-used, 1 clusters, 1 orphans",
        });
        assert_eq!(call("keeper.review", result, json!({})),
            "  Unvalidated-but-used (1)\n    #1  unv  [uses: 2]\n  Consolidation candidates (1)\n    cluster of 3: tags=[x, y]\n  Orphans (1)\n    #9  orphan\n  ── keeper.review: 1 unvalidated-used, 1 clusters, 1 orphans");
        assert_eq!(call("keeper.review", json!({}), json!({})),
            "  Unvalidated-but-used (0)\n  Consolidation candidates (0)\n  Orphans (0)");
    }

    #[test]
    fn driver_tick_variants() {
        let fired = json!({ "outcome": "fired", "card_slug": "FOO-1", "decided_action": "git-manager.implement.code", "duration_ms": 1234, "cost_usd": 0.0042, "decided_reason": "rule-X matched" });
        assert_eq!(call("system.driver.tick", fired, json!({})),
            "  \u{2295} tick fired\n    card=FOO-1  action=git-manager.implement.code\n    duration=1234ms  cost=$0.0042\n    why: rule-X matched");
        let failed = json!({ "outcome": "failed", "error_message": "boom" });
        assert_eq!(call("system.driver.tick", failed, json!({})),
            "  \u{2295} tick failed\n    card=\u{2014}  action=\u{2014}\n    duration=?ms  cost=$0.0000\n    error: boom");
        assert_eq!(call("system.driver.tick", Value::Null, json!({})), "  (no tick result)");
    }

    #[test]
    fn consistency_scan_buckets() {
        let result = json!({
            "summary": { "critical": 1, "warning": 2, "info": 5 },
            "findings": [
                { "severity": "critical", "dimension": "X", "message": "something broke" },
                { "severity": "warning", "dimension": "Y", "message": "minor drift" },
                { "severity": "warning", "dimension": "Z", "message": "another minor" },
                { "severity": "info", "dimension": "W", "message": "just info" },
            ],
        });
        assert_eq!(call("system.consistency.scan", result, json!({})),
            "  1 critical \u{b7} 2 warning \u{b7} 5 info\n  [CRITICAL]\n    \u{2022} [X] something broke\n  [WARNING]\n    \u{2022} [Y] minor drift\n    \u{2022} [Z] another minor");
        assert_eq!(call("system.consistency.scan", json!({ "findings": [] }), json!({})), "  (no findings)");
    }

    #[test]
    fn anatomy_latest_with_and_without_counts() {
        let result = json!({ "payload": { "id": 7, "counts": { "skills": 200, "actions": 156, "agents": 10, "tables": 50 }, "taken_at": "2026-05-20" } });
        assert_eq!(call("system.anatomy.latest", result, json!({})),
            "  substrate anatomy snapshot #7\n    skills=200  actions=156  agents=10  tables=50\n    taken_at: 2026-05-20");
        // no counts -> falls through to JSON fallback
        let out = call("system.anatomy.latest", json!({ "payload": { "id": 7 } }), json!({}));
        assert!(out.contains("\"id\": 7"));
    }

    #[test]
    fn terse_list_and_overflow() {
        let rows = json!([{ "slug": "LAW-1", "title": "first law" }, { "slug": "LAW-2", "title": "second law" }]);
        assert_eq!(call("system.law.list", rows, json!({})), "  - LAW-1  first law\n  - LAW-2  second law");
        assert_eq!(call("system.law.list", json!([{ "slug": "LAW-3" }]), json!({})), "  - LAW-3");
        let mut many = Vec::new();
        for i in 1..=16 { many.push(json!({ "slug": format!("S{i}") })); }
        let out = call("system.law.list", json!(many), json!({}));
        assert!(out.contains("\u{2026} +1 more"));
    }

    #[test]
    fn json_fallback_branches() {
        assert_eq!(call("unknown", json!({ "foo": "bar", "n": 42 }), json!({})), "  {\n    \"foo\": \"bar\",\n    \"n\": 42\n  }");
        assert_eq!(call("unknown", json!("a string result"), json!({})), "  a string result");
        assert_eq!(call("unknown", Value::Null, json!({})), "  (no result)");
        // big object exceeds MAX_FALLBACK -> truncated
        let mut big = serde_json::Map::new();
        for i in 0..200 { big.insert(format!("key{i}"), json!(format!("value number {i} with some padding text to grow the payload"))); }
        let out = call("unknown", Value::Object(big), json!({}));
        assert!(out.ends_with("\n    \u{2026}(truncated)"));
        // big string exceeds 600 chars -> truncated with ellipsis
        let out2 = call("unknown", json!("x".repeat(700)), json!({}));
        assert_eq!(out2.chars().count(), 602);
        assert!(out2.ends_with('\u{2026}'));
    }

    #[test]
    fn html_auto_detect_and_tui_render() {
        let html = "<!DOCTYPE html><html><body><h1>Detected</h1><p>via fallback</p></body></html>";
        let out = call("unknown.action.not.in.registry", json!(html), json!({}));
        assert!(out.contains("\u{2550}\u{2550}\u{2550} Detected \u{2550}\u{2550}\u{2550}"));
        assert!(out.contains("via fallback"));
        assert!(!out.contains('<'));

        // <title> takes priority over <h1>; style/script are stripped.
        let html2 = "<!DOCTYPE html><html><head><title>antiloki getting started</title><style>body{}</style></head><body><p>body text</p></body></html>";
        assert_eq!(call("system.docs.getting_started", json!(html2), json!({})),
            "  \u{2550}\u{2550}\u{2550} antiloki getting started \u{2550}\u{2550}\u{2550}\n\n  body text");

        let html3 = "<html><body><ul><li>first</li><li>second</li><li>third</li></ul></body></html>";
        assert_eq!(call("system.docs.rag", json!(html3), json!({})), "  \u{2022} first\n  \u{2022} second\n  \u{2022} third");

        let html4 = "<html><body><pre><code>line one\nline two</code></pre></body></html>";
        assert_eq!(call("system.docs.driver", json!(html4), json!({})), "  \u{2502} line one\n  \u{2502} line two");

        let html5 = "<html><body><p>See <a href=\"https://example.com/docs\">the docs</a> for more.</p></body></html>";
        assert_eq!(call("system.docs.index", json!(html5), json!({})), "  See the docs (https://example.com/docs) for more.");

        let html6 = "<html><body><p>Tom &amp; Jerry &lt;rule&gt; &quot;OK&quot; &#39;s fine&#x2014;done.</p></body></html>";
        assert_eq!(call("system.docs.index", json!(html6), json!({})), "  Tom & Jerry <rule> \"OK\" 's fine\u{2014}done.");

        // literal "H1"/"BR" words in text must not be mistaken for sentinel markers.
        let html7 = "<html><body><p>The H1 tag and the BR tag are HTML elements. So is PRE.</p></body></html>";
        assert_eq!(call("system.docs.index", json!(html7), json!({})), "  The H1 tag and the BR tag are HTML elements. So is PRE.");

        assert_eq!(call("system.docs.index", json!(""), json!({})), "  (empty html)");
    }

    #[test]
    fn html_headings_breaks_and_color() {
        let html = "<html><body><h2>Section</h2><h3>Subsection</h3><p>p</p></body></html>";
        assert_eq!(call("system.docs.driver", json!(html), json!({})), "  Section\n\n  Subsection\n  p");

        let html2 = "<html><body><p>line1<br>line2</p><hr><p>after hr</p></body></html>";
        assert_eq!(call("system.docs.index", json!(html2), json!({})),
            "  line1\n  line2\n\n  \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\n\n  after hr");

        let html3 = "<html><body><pre><code>line one\nline two</code></pre></body></html>";
        let out = call("system.docs.driver", json!(html3), json!({ "color": true }));
        assert_eq!(out, "\u{1b}[2m  \u{2502} line one\u{1b}[0m\n\u{1b}[2m  \u{2502} line two\u{1b}[0m");
    }

    #[test]
    fn default_export_returns_registry_shape() {
        let db = Connection::open_in_memory().unwrap();
        let r = actionResultRenderers(&json!({}), &db).unwrap();
        assert_eq!(r["registered"], json!(34));
        assert_eq!(r["actions"].as_array().unwrap().len(), 34);
        assert_eq!(r["actions"][0], json!("board-keeper.card.list"));
    }
}
