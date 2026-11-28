//! THE MASTER PAGE (RFC 0010 §4.16): one self-contained HTML page for a run, made from DATA, never from code. A spec (sections of widgets, a closed list) plus what the run produced
//! (node outputs, documents, costs, events) go in; one page comes out: inline CSS and a few lines of inline script for the section list, no network, no model. All text is escaped; the only
//! links are `http(s)`; the only frame is a sandboxed `embed` of an HTML document the run itself made. Pure: nothing here touches the disk or the run.
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

pub const WIDGETS: [&str; 14] = ["hero", "summary", "markdown", "kpis", "table", "checklist", "timeline", "cards", "cost", "links", "callout", "bar", "embed", "media"];
const MAX_SECTIONS: usize = 12;
const MAX_WIDGETS: usize = 24;
const TEXT_CAP: usize = 400_000;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Section { #[serde(default)] pub title: String, #[serde(default)] pub widgets: Vec<Value> }
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Spec { #[serde(default)] pub title: String, #[serde(default)] pub sections: Vec<Section> }

/// A video, picture or sound the run made. `uri` is its bytes as a data address when it is small enough to carry inside the page (the page is a document of its own and cannot reach the serve), else None.
#[derive(Clone, Debug, Default)] pub struct MediaSrc { pub name: String, pub kind: String, pub bytes: u64, pub uri: Option<String> }
/// A document the run made, with its text when it is text we may read.
#[derive(Clone, Debug, Default)]
pub struct ArtSrc { pub node: Option<String>, pub name: String, pub bytes: u64, pub text: Option<String> }
/// What the run has, to resolve a widget's `from` against.
#[derive(Clone, Debug, Default)]
pub struct Sources {
    pub team: String, pub ask: String, pub status: String, pub cost_usd: f64, pub started_ms: i64, pub ended_ms: i64,
    pub values: Map<String, Value>, pub labels: BTreeMap<String, String>, pub node_cost: BTreeMap<String, f64>,
    pub artifacts: Vec<ArtSrc>, pub events: Vec<(i64, String, String)>, pub media: Vec<MediaSrc>,
}

// ── what a `from` may name ────────────────────────────────────────────────────────────────────────────────────────────────

/// The node ids and artifact names a spec reads, so the validator can check them against the graph. `(node ids, artifact names)`. Pure.
pub fn refs(spec: &Spec) -> (Vec<String>, Vec<String>) {
    let (mut nodes, mut arts) = (vec![], vec![]);
    let mut take = |f: &str| {
        if let Some(a) = f.strip_prefix("artifact:") { arts.push(a.to_string()); }
        else if let Some(n) = f.strip_prefix("node:") { nodes.push(n.split('.').next().unwrap_or("").to_string()); }
        else if let Some(n) = f.strip_prefix("report:") { nodes.push(n.to_string()); }
    };
    for w in spec.sections.iter().flat_map(|s| &s.widgets) {
        if let Some(f) = w["from"].as_str() { take(f); }
        for it in w["items"].as_array().into_iter().flatten() { if let Some(f) = it["from"].as_str() { take(f); } }
    }
    (nodes, arts)
}
fn source_ok(f: &str) -> bool { f == "ask" || f.starts_with("artifact:") || f.starts_with("report:") || (f.starts_with("node:") && f.contains('.')) || ["run:cost", "run:time", "run:status", "run:artifacts", "run:nodes", "run:events"].contains(&f) }

/// What is wrong with a spec, in words the Leader can act on. Empty = fine. Pure.
pub fn check(spec: &Spec) -> Vec<String> {
    let mut out = vec![];
    if spec.sections.is_empty() { out.push("the page has no sections".into()); }
    if spec.sections.len() > MAX_SECTIONS { out.push(format!("at most {MAX_SECTIONS} sections")); }
    for (i, s) in spec.sections.iter().enumerate() {
        let sn = if s.title.trim().is_empty() { format!("section {}", i + 1) } else { s.title.clone() };
        if s.widgets.len() > MAX_WIDGETS { out.push(format!("{sn}: at most {MAX_WIDGETS} widgets")); }
        for (j, w) in s.widgets.iter().enumerate() {
            let t = w["type"].as_str().unwrap_or(""); let at = format!("{sn}, widget {} ({})", j + 1, if t.is_empty() { "no type" } else { t });
            if !WIDGETS.contains(&t) { out.push(format!("{at}: not a widget; they are {}", WIDGETS.join(", "))); continue; }
            if let Some(f) = w["from"].as_str() { if !source_ok(f) { out.push(format!("{at}: `{f}` is not a source (artifact:<name>, node:<id>.<field>, report:<id>, ask, run:cost|time|status|artifacts|nodes|events)")); } }
            let has_text = w["text"].as_str().map_or(false, |x| !x.trim().is_empty()) || w["from"].is_string();
            match t {
                "summary" | "markdown" | "callout" if !has_text => out.push(format!("{at}: say what to show (`from` or `text`)")),
                "embed" if !w["from"].as_str().map_or(false, |f| f.starts_with("artifact:") && f.ends_with(".html")) => out.push(format!("{at}: an embed takes an HTML document the run makes (`from`: `artifact:<name>.html`)")),
                "kpis" | "cards" | "checklist" if !w["items"].is_array() && !w["from"].is_string() => out.push(format!("{at}: needs `items`")),
                "table" | "bar" if !w["from"].is_string() && !w["series"].is_array() => out.push(format!("{at}: needs `from` (or `series`)")),
                _ => {}
            }
            for it in w["items"].as_array().into_iter().flatten() { if let Some(f) = it["from"].as_str() { if !source_ok(f) { out.push(format!("{at}: `{f}` is not a source")); } } }
        }
    }
    out
}

// ── escaping and the small Markdown ───────────────────────────────────────────────────────────────────────────────────────

pub fn esc(s: &str) -> String { s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;") }
fn clip(s: &str) -> String { if s.len() > TEXT_CAP { let mut c: String = s.chars().take(TEXT_CAP).collect(); c.push_str("\n\n[cut]"); c } else { s.to_string() } }
fn http(u: &str) -> bool { let l = u.trim().to_ascii_lowercase(); (l.starts_with("http://") || l.starts_with("https://")) && !u.chars().any(|c| c.is_whitespace() || c == '"' || c == '<' || c == '>') }
/// Inline Markdown on already-escaped text: `code`, **bold**, *italic*, [text](http link).
fn inline(t: &str) -> String {
    let t = esc(t); let mut out = String::new(); let cs: Vec<char> = t.chars().collect(); let mut i = 0;
    let find = |from: usize, pat: &str| -> Option<usize> { let p: Vec<char> = pat.chars().collect(); (from..cs.len().saturating_sub(p.len() - 1)).find(|&k| cs[k..k + p.len()] == p[..]) };
    while i < cs.len() {
        if cs[i] == '`' { if let Some(e) = find(i + 1, "`") { out.push_str(&format!("<code>{}</code>", cs[i + 1..e].iter().collect::<String>())); i = e + 1; continue; } }
        if cs[i] == '*' && cs.get(i + 1) == Some(&'*') { if let Some(e) = find(i + 2, "**") { if e > i + 2 { out.push_str(&format!("<strong>{}</strong>", cs[i + 2..e].iter().collect::<String>())); i = e + 2; continue; } } }
        if cs[i] == '*' && cs.get(i + 1).map_or(false, |c| !c.is_whitespace() && *c != '*') { if let Some(e) = find(i + 1, "*") { if e > i + 1 { out.push_str(&format!("<em>{}</em>", cs[i + 1..e].iter().collect::<String>())); i = e + 1; continue; } } }
        if cs[i] == '[' { if let Some(m) = find(i + 1, "](") { if let Some(e) = find(m + 2, ")") { let url: String = cs[m + 2..e].iter().collect::<String>().replace("&amp;", "&"); if http(&url) { out.push_str(&format!("<a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\">{}</a>", esc(&url), cs[i + 1..m].iter().collect::<String>())); i = e + 1; continue; } } } }
        out.push(cs[i]); i += 1;
    }
    out
}
/// Markdown to HTML: headings, paragraphs, lists, fenced code, quotes, simple tables, rules. Everything is escaped first. Pure.
pub fn md(src: &str) -> String {
    let owned = clip(src); let lines: Vec<&str> = owned.lines().collect(); let mut out = String::new(); let mut i = 0;
    let cells = |l: &str| -> Vec<String> { l.trim().trim_matches('|').split('|').map(|c| c.trim().to_string()).collect() };
    let sep = |l: &str| { let t = l.trim(); t.contains('-') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) };
    while i < lines.len() {
        let l = lines[i]; let t = l.trim();
        if t.is_empty() { i += 1; continue; }
        if t.starts_with("```") { let mut code = vec![]; i += 1; while i < lines.len() && !lines[i].trim().starts_with("```") { code.push(lines[i]); i += 1; } i += 1; out.push_str(&format!("<pre><code>{}</code></pre>", esc(&code.join("\n")))); continue; }
        if let Some(h) = (1..=4).rev().find(|n| t.starts_with(&format!("{} ", "#".repeat(*n)))) { out.push_str(&format!("<h{0}>{1}</h{0}>", h + 1, inline(t[h + 1..].trim()))); i += 1; continue; }
        if t.len() >= 3 && t.chars().all(|c| c == '-' || c == '*' || c == '_') { out.push_str("<hr>"); i += 1; continue; }
        if t.starts_with('>') { let mut q = vec![]; while i < lines.len() && lines[i].trim().starts_with('>') { q.push(lines[i].trim().trim_start_matches('>').trim()); i += 1; } out.push_str(&format!("<blockquote>{}</blockquote>", inline(&q.join(" ")))); continue; }
        if t.starts_with('|') && i + 1 < lines.len() && sep(lines[i + 1]) {
            let head = cells(t); i += 2; let mut rows = vec![]; while i < lines.len() && lines[i].trim().starts_with('|') { rows.push(cells(lines[i])); i += 1; }
            out.push_str("<div class=\"tw\"><table><thead><tr>"); for h in &head { out.push_str(&format!("<th>{}</th>", inline(h))); } out.push_str("</tr></thead><tbody>");
            for r in rows { out.push_str("<tr>"); for c in r { out.push_str(&format!("<td>{}</td>", inline(&c))); } out.push_str("</tr>"); } out.push_str("</tbody></table></div>"); continue;
        }
        let bullet = |x: &str| x.strip_prefix("- ").or_else(|| x.strip_prefix("* ")).or_else(|| x.strip_prefix("+ ")).map(str::to_string);
        let ordered = |x: &str| { let d = x.chars().take_while(|c| c.is_ascii_digit()).count(); (d > 0 && x[d..].starts_with(". ")).then(|| x[d + 2..].to_string()) };
        if bullet(t).is_some() || ordered(t).is_some() {
            let ol = ordered(t).is_some(); out.push_str(if ol { "<ol>" } else { "<ul>" });
            while i < lines.len() { let x = lines[i].trim(); match if ol { ordered(x) } else { bullet(x) } { Some(item) => { out.push_str(&format!("<li>{}</li>", inline(&item))); i += 1; } None => break } }
            out.push_str(if ol { "</ol>" } else { "</ul>" }); continue;
        }
        let mut p = vec![]; while i < lines.len() && !lines[i].trim().is_empty() && !lines[i].trim().starts_with("```") && !lines[i].trim().starts_with('#') && !lines[i].trim().starts_with('>') && !lines[i].trim().starts_with("- ") && !lines[i].trim().starts_with("* ") && !lines[i].trim().starts_with('|') { p.push(lines[i].trim()); i += 1; }
        if p.is_empty() { p.push(t); i += 1; }
        out.push_str(&format!("<p>{}</p>", inline(&p.join(" "))));
    }
    out
}

// ── resolving a source ────────────────────────────────────────────────────────────────────────────────────────────────────

enum Got { Text(String), Json(Value) }
fn usd(x: f64) -> String { format!("${x:.2}") }
fn dur(ms: i64) -> String { let s = (ms.max(0) / 1000) as i64; if s < 60 { format!("{s}s") } else if s < 3600 { format!("{}m {}s", s / 60, s % 60) } else { format!("{}h {}m", s / 3600, (s % 3600) / 60) } }
fn plain(v: &Value) -> String { match v { Value::String(s) => s.clone(), Value::Null => String::new(), Value::Number(n) => n.to_string(), Value::Bool(b) => b.to_string(), o => serde_json::to_string_pretty(o).unwrap_or_default() } }
fn art<'a>(src: &'a Sources, name: &str) -> Option<&'a ArtSrc> {
    if let Some((n, f)) = name.split_once('/') { return src.artifacts.iter().find(|a| a.node.as_deref() == Some(n) && a.name == f); }
    src.artifacts.iter().find(|a| a.node.is_none() && a.name == name).or_else(|| src.artifacts.iter().find(|a| a.name == name))
}
fn get(src: &Sources, f: &str) -> Result<Got, String> {
    if f == "ask" { return Ok(Got::Text(src.ask.clone())); }
    match f {
        "run:cost" => return Ok(Got::Text(usd(src.cost_usd))), "run:status" => return Ok(Got::Text(src.status.clone())),
        "run:time" => return Ok(Got::Text(dur(if src.ended_ms > 0 { src.ended_ms - src.started_ms } else { 0 }))),
        "run:artifacts" => return Ok(Got::Text(src.artifacts.len().to_string())), "run:nodes" => return Ok(Got::Text(src.values.len().to_string())),
        "run:events" => return Ok(Got::Json(json!(src.events.iter().map(|(t, n, x)| json!({ "t_ms": t, "node": n, "text": x })).collect::<Vec<_>>()))),
        _ => {}
    }
    if let Some(n) = f.strip_prefix("report:") { let v = src.values.get(n).ok_or_else(|| format!("`{f}`: {n} has not run"))?; let t = ["report", "said", "text"].iter().map(|k| plain(&v[*k])).find(|t| !t.trim().is_empty()); return t.map(Got::Text).ok_or_else(|| format!("`{f}`: {n} wrote no report")); }
    if let Some(rest) = f.strip_prefix("node:") { let (n, field) = rest.split_once('.').ok_or_else(|| format!("`{f}`: say node:<id>.<field>"))?; let v = src.values.get(n).ok_or_else(|| format!("`{f}`: {n} has not run"))?; let x = field.split('.').fold(Some(v), |c, k| c.and_then(|c| c.get(k))).ok_or_else(|| format!("`{f}`: {n} has no `{field}`"))?; return Ok(if x.is_array() || x.is_object() { Got::Json(x.clone()) } else { Got::Text(plain(x)) }); }
    if let Some(a) = f.strip_prefix("artifact:") { let x = art(src, a).ok_or_else(|| format!("`{f}`: the run made no document called {a}"))?; let t = x.text.clone().ok_or_else(|| format!("`{f}`: {a} cannot be read here"))?; if a.ends_with(".json") { if let Ok(j) = serde_json::from_str::<Value>(&t) { return Ok(Got::Json(j)); } } return Ok(Got::Text(t)); }
    Err(format!("`{f}` is not a source"))
}
fn text_of(src: &Sources, w: &Value) -> Result<String, String> { match w["from"].as_str() { Some(f) => Ok(match get(src, f)? { Got::Text(t) => t, Got::Json(j) => plain(&j) }), None => Ok(w["text"].as_str().unwrap_or("").to_string()) } }
fn rows_of(g: Got) -> Vec<Value> {
    match g {
        Got::Json(Value::Array(a)) => a,
        Got::Json(Value::Object(o)) => o.into_iter().map(|(k, v)| json!({ "key": k, "value": v })).collect(),
        Got::Json(v) => vec![v],
        Got::Text(t) => { // a CSV
            let mut lines = t.lines().filter(|l| !l.trim().is_empty()); let head: Vec<String> = match lines.next() { Some(h) => csv(h), None => return vec![] };
            lines.take(200).map(|l| { let c = csv(l); Value::Object(head.iter().enumerate().map(|(i, h)| (h.clone(), json!(c.get(i).cloned().unwrap_or_default()))).collect()) }).collect()
        }
    }
}
fn csv(l: &str) -> Vec<String> { let (mut out, mut cur, mut q) = (vec![], String::new(), false); let mut it = l.chars().peekable(); while let Some(c) = it.next() { match c { '"' if q && it.peek() == Some(&'"') => { cur.push('"'); it.next(); } '"' => q = !q, ',' if !q => out.push(std::mem::take(&mut cur)), c => cur.push(c) } } out.push(cur); out }

// ── the widgets ───────────────────────────────────────────────────────────────────────────────────────────────────────────

fn card(title: &str, body: String) -> String { format!("<div class=\"card\">{}{}</div>", if title.is_empty() { String::new() } else { format!("<h3>{}</h3>", esc(title)) }, body) }
fn date_of(ms: i64) -> String { if ms <= 0 { return String::new(); } let d = ms / 86_400_000; let z = d + 719_468; let era = z.div_euclid(146_097); let doe = z.rem_euclid(146_097); let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; let y = yoe + era * 400; let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); let mp = (5 * doy + 2) / 153; let dd = doy - (153 * mp + 2) / 5 + 1; let m = if mp < 10 { mp + 3 } else { mp - 9 }; format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, dd) }
fn bars(rows: &[(String, f64)], money: bool) -> String {
    let max = rows.iter().map(|r| r.1).fold(0.0_f64, f64::max).max(f64::MIN_POSITIVE); let mut o = String::from("<div class=\"bars\">");
    for (l, v) in rows { o.push_str(&format!("<div class=\"bar\"><span class=\"bl\">{}</span><span class=\"bt\"><i style=\"width:{:.1}%\"></i></span><span class=\"bv\">{}</span></div>", esc(l), (v / max * 100.0).clamp(0.0, 100.0), if money { usd(*v) } else { format!("{v}") })); }
    o + "</div>"
}
fn widget(src: &Sources, spec: &Spec, w: &Value) -> Result<String, String> {
    let t = w["type"].as_str().unwrap_or("");
    let title = w["title"].as_str().unwrap_or("");
    Ok(match t {
        "hero" => {
            let sub = w["subtitle"].as_str().map(String::from).unwrap_or_else(|| src.ask.chars().take(280).collect());
            let when = date_of(if src.ended_ms > 0 { src.ended_ms } else { src.started_ms });
            format!("<header class=\"hero\"><div class=\"chips\"><span class=\"chip {st}\">{st}</span><span class=\"chip\">{cost}</span>{when}</div><h1>{t}</h1><p class=\"lede\">{sub}</p></header>", st = esc(&src.status), cost = usd(src.cost_usd), when = if when.is_empty() { String::new() } else { format!("<span class=\"chip\">{when}</span>") }, t = esc(if title.is_empty() { if spec.title.is_empty() { &src.team } else { &spec.title } } else { title }), sub = esc(&sub))
        }
        "summary" => card(title, format!("<div class=\"md\">{}</div>", md(&text_of(src, w)?))),
        "markdown" => card(title, format!("<div class=\"md\">{}</div>", md(&text_of(src, w)?))),
        "callout" => format!("<div class=\"callout {}\"><div class=\"md\">{}</div></div>", match w["tone"].as_str() { Some("ok") => "ok", Some("warn") => "warn", _ => "info" }, md(&text_of(src, w)?)),
        "kpis" => {
            let mut o = String::from("<div class=\"kpis\">");
            for it in w["items"].as_array().into_iter().flatten() { let v = match it["from"].as_str() { Some(f) => match get(src, f)? { Got::Text(t) => t, Got::Json(j) => plain(&j) }, None => plain(&it["value"]) }; o.push_str(&format!("<div class=\"kpi\"><b>{}</b><span>{}</span></div>", esc(&v.chars().take(40).collect::<String>()), esc(it["label"].as_str().unwrap_or("")))); }
            o + "</div>"
        }
        "table" => {
            let rows = match w["from"].as_str() { Some(f) => rows_of(get(src, f)?), None => w["series"].as_array().cloned().unwrap_or_default() };
            let cols: Vec<String> = match rows.first() { Some(Value::Object(o)) => o.keys().cloned().collect(), Some(Value::Array(a)) => (0..a.len()).map(|i| format!("{}", i + 1)).collect(), _ => vec!["value".into()] };
            let mut o = String::from("<div class=\"tw\"><table><thead><tr>"); for c in &cols { o.push_str(&format!("<th>{}</th>", esc(c))); } o.push_str("</tr></thead><tbody>");
            for r in rows.iter().take(200) { o.push_str("<tr>"); for (i, c) in cols.iter().enumerate() { let cell = match r { Value::Object(m) => plain(m.get(c).unwrap_or(&Value::Null)), Value::Array(a) => plain(a.get(i).unwrap_or(&Value::Null)), v => plain(v) }; o.push_str(&format!("<td>{}</td>", esc(&cell.chars().take(300).collect::<String>()))); } o.push_str("</tr>"); }
            card(title, o + "</tbody></table></div>")
        }
        "checklist" => {
            let items: Vec<Value> = match w["from"].as_str() { Some(f) => rows_of(get(src, f)?), None => w["items"].as_array().cloned().unwrap_or_default() };
            let mut o = String::from("<ul class=\"check\">");
            for it in items.iter().take(100) { let done = it["done"].as_bool().or_else(|| it["ok"].as_bool()).unwrap_or(false); let text = ["text", "id"].iter().map(|k| plain(&it[*k])).find(|x| !x.is_empty()).unwrap_or_else(|| plain(it)); let ev = plain(&it["evidence"]); o.push_str(&format!("<li class=\"{}\"><i>{}</i><span>{}{}</span></li>", if done { "yes" } else { "no" }, if done { "✓" } else { "✗" }, esc(&text), if ev.is_empty() { String::new() } else { format!("<small>{}</small>", esc(&ev)) })); }
            card(title, o + "</ul>")
        }
        "timeline" => {
            let n = w["limit"].as_u64().unwrap_or(60).min(200) as usize; let mut o = String::from("<ol class=\"tl\">");
            for (ms, node, text) in src.events.iter().rev().take(n).collect::<Vec<_>>().into_iter().rev() { let who = src.labels.get(node).cloned().unwrap_or_else(|| node.clone()); o.push_str(&format!("<li><time>{}</time><b>{}</b><span>{}</span></li>", esc(&dur(*ms - src.started_ms)), esc(&who), esc(&text.chars().take(240).collect::<String>()))); }
            card(if title.is_empty() { "Timeline" } else { title }, o + "</ol>")
        }
        "cards" => {
            let items: Vec<Value> = match w["from"].as_str() { Some(f) => rows_of(get(src, f)?), None => w["items"].as_array().cloned().unwrap_or_default() };
            let mut o = String::from("<div class=\"cards\">");
            for it in items.iter().take(48) { let link = it["link"].as_str().filter(|l| http(l)); let head = esc(it["title"].as_str().unwrap_or("")); o.push_str(&format!("<div class=\"card\">{}<h3>{}</h3><p>{}</p></div>", it["tag"].as_str().map_or(String::new(), |t| format!("<span class=\"chip\">{}</span>", esc(t))), match link { Some(l) => format!("<a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\">{}</a>", esc(l), head), None => head }, esc(&it["text"].as_str().unwrap_or("").chars().take(500).collect::<String>()))); }
            o + "</div>"
        }
        "cost" => { let rows: Vec<(String, f64)> = src.node_cost.iter().filter(|(_, v)| **v > 0.0).map(|(k, v)| (src.labels.get(k).cloned().unwrap_or_else(|| k.clone()), *v)).collect(); card(if title.is_empty() { "Cost by step" } else { title }, if rows.is_empty() { "<p class=\"dim\">nothing was spent</p>".into() } else { bars(&rows, true) }) }
        "media" => {
            if src.media.is_empty() { return Ok(card(if title.is_empty() { "Media" } else { title }, "<p class=\"dim\">the run made no video, picture or sound</p>".into())); }
            let mut o = String::new(); for m in &src.media { let cap = format!("<p class=\"dim\">{} · {:.1} MB</p>", esc(&m.name), m.bytes as f64 / 1_048_576.0);
                match (&m.uri, m.kind.as_str()) {
                    (Some(u), "video") => o.push_str(&format!("<video controls preload=\"metadata\" style=\"width:100%;max-height:420px;background:#000;border-radius:6px\" src=\"{}\"></video>{cap}", esc(u))),
                    (Some(u), "image") => o.push_str(&format!("<img alt=\"{}\" style=\"max-width:100%;border-radius:6px\" src=\"{}\">{cap}", esc(&m.name), esc(u))),
                    (Some(u), "audio") => o.push_str(&format!("<audio controls style=\"width:100%\" src=\"{}\"></audio>{cap}", esc(u))),
                    _ => o.push_str(&format!("<p><code>{}</code> <span class=\"dim\">could not be shown here: play it in the Media player under the result</span></p>", esc(&m.name))) } }
            card(if title.is_empty() { "Media" } else { title }, o)
        }
        "links" => {
            let mut o = String::from("<ul class=\"docs\">"); for a in &src.artifacts { o.push_str(&format!("<li><code>{}</code><span>{}{}</span></li>", esc(&a.name), esc(&a.node.clone().map(|n| format!("draft of {} · ", src.labels.get(&n).cloned().unwrap_or(n))).unwrap_or_default()), if a.bytes > 1024 { format!("{} KB", a.bytes / 1024) } else { format!("{} B", a.bytes) })); }
            card(if title.is_empty() { "Documents" } else { title }, o + "</ul>")
        }
        "bar" => {
            let rows: Vec<(String, f64)> = match w["from"].as_str() { Some(f) => rows_of(get(src, f)?).iter().filter_map(|r| Some((if r["label"].is_string() { plain(&r["label"]) } else { plain(&r["key"]) }, r["value"].as_f64().or_else(|| r["value"].as_str().and_then(|s| s.parse().ok()))?))).collect(), None => w["series"].as_array().into_iter().flatten().filter_map(|r| Some((plain(&r["label"]), r["value"].as_f64()?))).collect() };
            card(title, if rows.is_empty() { "<p class=\"dim\">no numbers</p>".into() } else { bars(&rows, false) })
        }
        "embed" => {
            let f = w["from"].as_str().unwrap_or(""); let html = text_of(src, w)?; if html.len() > TEXT_CAP { return Err(format!("`{f}` is too big to embed")); }
            let h = w["height"].as_u64().unwrap_or(480).clamp(120, 900);
            card(title, format!("<iframe class=\"emb\" sandbox=\"allow-scripts\" referrerpolicy=\"no-referrer\" style=\"height:{h}px\" srcdoc=\"{}\"></iframe>", esc(&html)))
        }
        _ => return Err(format!("`{t}` is not a widget")),
    })
}

const CSS: &str = ":root{--bg:#fcfcfd;--panel:#fff;--fg:#1b1c1f;--dim:#6e7079;--line:#e8e9ec;--accent:#5e6ad2;--ok:#2f8f5b;--warn:#b27a1c;--bad:#c5483f;--code:#f3f4f6;color-scheme:light}@media(prefers-color-scheme:dark){:root{--bg:#0f1011;--panel:#151618;--fg:#e6e7e9;--dim:#8a8d96;--line:#25272b;--accent:#8b93f0;--ok:#4cc38a;--warn:#e0a94a;--bad:#e5766c;--code:#1b1c1f;color-scheme:dark}}*{box-sizing:border-box}html{scroll-behavior:smooth}body{margin:0;background:var(--bg);color:var(--fg);font:13.5px/1.55 ui-sans-serif,system-ui,-apple-system,'Segoe UI',sans-serif;-webkit-font-smoothing:antialiased}.wrap{display:grid;grid-template-columns:200px minmax(0,1fr);gap:0;max-width:1060px;margin:0 auto}nav{position:sticky;top:0;align-self:start;height:100vh;padding:24px 12px;border-right:1px solid var(--line);overflow:auto}nav b{display:block;font-size:11px;letter-spacing:.06em;text-transform:uppercase;color:var(--dim);margin:0 8px 10px;font-weight:500}nav a{display:block;padding:4px 8px;border-radius:6px;color:var(--dim);text-decoration:none;font-size:13px}nav a:hover{color:var(--fg)}nav a.on{background:var(--code);color:var(--fg)}main{padding:26px 32px 80px;max-width:780px;min-width:0}section{margin:0 0 30px;scroll-margin-top:16px}section>h2{font-size:11px;letter-spacing:.06em;text-transform:uppercase;color:var(--dim);margin:0 0 10px;font-weight:500}.hero h1{font-size:22px;line-height:1.25;letter-spacing:-.01em;margin:8px 0 6px;font-weight:600}.lede{color:var(--dim);margin:0;max-width:64ch}.chips{display:flex;gap:6px;flex-wrap:wrap}.chip{display:inline-block;font-size:11.5px;padding:0 7px;line-height:20px;border:1px solid var(--line);border-radius:6px;color:var(--dim)}.chip.done{color:var(--ok)}.chip.failed,.chip.blocked{color:var(--bad)}.card{background:var(--panel);border:1px solid var(--line);border-radius:8px;padding:12px 14px;margin:0 0 10px}.card h3{margin:0 0 6px;font-size:13.5px;font-weight:600}.md>:first-child{margin-top:0}.md>:last-child{margin-bottom:0}.md h2,.md h3,.md h4,.md h5{margin:16px 0 6px;line-height:1.3;font-weight:600}.md h2{font-size:16px}.md h3{font-size:14px}.md p{margin:7px 0}.md ul,.md ol{margin:7px 0;padding-left:20px}.md code{background:var(--code);padding:1px 5px;border-radius:4px;font:12px ui-monospace,Menlo,monospace}.md pre{background:var(--code);padding:9px 11px;border-radius:6px;overflow:auto}.md pre code{padding:0;background:none}.md blockquote{margin:8px 0;padding:0 12px;border-left:2px solid var(--line);color:var(--dim)}.md a,.card a{color:var(--accent);text-decoration:none}.md a:hover,.card a:hover{text-decoration:underline}.md hr{border:0;border-top:1px solid var(--line)}.tw{overflow:auto}table{border-collapse:collapse;width:100%;font-size:13px}th,td{text-align:left;padding:6px 10px;border-bottom:1px solid var(--line);vertical-align:top}th{color:var(--dim);font-weight:500;font-size:11.5px}.kpis{display:grid;grid-template-columns:repeat(auto-fit,minmax(120px,1fr));gap:8px;margin:0 0 10px}.kpi{background:var(--panel);border:1px solid var(--line);border-radius:8px;padding:10px 12px}.kpi b{display:block;font-size:18px;letter-spacing:-.01em;font-weight:600}.kpi span{color:var(--dim);font-size:11.5px}.callout{border:1px solid var(--line);border-left:2px solid var(--accent);border-radius:6px;padding:8px 12px;margin:0 0 10px;background:var(--panel)}.callout.ok{border-left-color:var(--ok)}.callout.warn{border-left-color:var(--warn)}.check{list-style:none;margin:0;padding:0}.check li{display:flex;gap:10px;padding:5px 0;border-bottom:1px solid var(--line)}.check li:last-child{border:0}.check i{font-style:normal;font-weight:600}.check .yes i{color:var(--ok)}.check .no i{color:var(--bad)}.check small{display:block;color:var(--dim)}.tl{list-style:none;margin:0;padding:0}.tl li{display:grid;grid-template-columns:58px 120px minmax(0,1fr);gap:10px;padding:5px 0;border-bottom:1px solid var(--line);font-size:12.5px}.tl time{color:var(--dim);font-variant-numeric:tabular-nums}.cards{display:grid;grid-template-columns:repeat(auto-fill,minmax(220px,1fr));gap:10px;margin:0 0 10px}.cards .card{margin:0}.cards p{margin:5px 0 0;color:var(--dim)}.bars{display:grid;gap:6px}.bar{display:grid;grid-template-columns:120px minmax(0,1fr) 64px;gap:10px;align-items:center;font-size:12.5px}.bt{height:4px;background:var(--code);border-radius:2px;overflow:hidden}.bt i{display:block;height:100%;background:var(--accent)}.bv{text-align:right;color:var(--dim);font-variant-numeric:tabular-nums}.docs{list-style:none;margin:0;padding:0}.docs li{display:flex;justify-content:space-between;gap:10px;padding:5px 0;border-bottom:1px solid var(--line)}.docs span,.dim{color:var(--dim)}.emb{width:100%;border:1px solid var(--line);border-radius:6px;background:#fff}@media(max-width:760px){.wrap{display:block}nav{position:static;height:auto;border:0;border-bottom:1px solid var(--line);display:flex;gap:4px;overflow:auto;padding:10px 12px}nav b{display:none}nav a{white-space:nowrap}main{padding:18px 16px 60px}.tl li{grid-template-columns:50px minmax(0,1fr)}.tl li b{display:none}.bar{grid-template-columns:90px minmax(0,1fr) 52px}}";
const JS: &str = "(function(){var l=[].slice.call(document.querySelectorAll('nav a')),s=l.map(function(a){return document.getElementById(a.getAttribute('href').slice(1))});if(!('IntersectionObserver' in window))return;var o=new IntersectionObserver(function(e){e.forEach(function(x){if(x.isIntersecting){l.forEach(function(a){a.classList.toggle('on',a.getAttribute('href')==='#'+x.target.id)})}})},{rootMargin:'-10% 0px -80% 0px'});s.forEach(function(x){x&&o.observe(x)})})();";

/// The page: a section list on the left (a bar on a phone), one calm column. A widget that cannot be built is an error naming it, never a hole. Pure.
pub fn render(spec: &Spec, src: &Sources) -> Result<String, String> {
    // A SPEC WITH NO WIDGET AT ALL IS A PAGE WITH NOTHING ON IT (the operator 2026-11-15: "the report showed empty"): the Leader named a section and no widget in it. The page the run would get with no spec at all is better than a blank one.
    if spec.sections.iter().all(|s| s.widgets.is_empty()) { let mut a = auto_spec(src); if !spec.title.is_empty() { a.title = spec.title.clone(); } return render(&a, src); }
    let bad = check(spec); if !bad.is_empty() { return Err(bad.join("; ")); }
    let title = if spec.title.trim().is_empty() { src.team.clone() } else { spec.title.clone() };
    let (mut nav, mut body) = (String::new(), String::new());
    for (i, s) in spec.sections.iter().enumerate() {
        let id = format!("s{}", i + 1); let hero_only = s.widgets.len() == 1 && s.widgets[0]["type"] == "hero";
        if !s.title.trim().is_empty() && !hero_only { nav.push_str(&format!("<a href=\"#{id}\">{}</a>", esc(&s.title))); }
        body.push_str(&format!("<section id=\"{id}\">{}", if s.title.trim().is_empty() || hero_only { String::new() } else { format!("<h2>{}</h2>", esc(&s.title)) }));
        for (j, w) in s.widgets.iter().enumerate() { body.push_str(&widget(src, spec, w).map_err(|e| format!("{}, widget {}: {e}", if s.title.is_empty() { format!("section {}", i + 1) } else { s.title.clone() }, j + 1))?); }
        body.push_str("</section>");
    }
    Ok(format!("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{t}</title><style>{CSS}</style></head><body><div class=\"wrap\"><nav><b>{t}</b>{nav}</nav><main>{body}</main></div><script>{JS}</script></body></html>", t = esc(&title)))
}

/// The page a run gets when nobody designed one: the hero, the first report as the summary, every Markdown document as a section, each HTML document embedded, the numbers, the cost, the timeline. Pure.
pub fn auto_spec(src: &Sources) -> Spec {
    let mut sections = vec![Section { title: String::new(), widgets: vec![json!({ "type": "hero" })] }];
    let report = src.values.iter().find_map(|(k, v)| ["report", "said", "text"].iter().map(|f| plain(&v[*f])).find(|t| t.trim().len() > 20).map(|_| k.clone()));
    if let Some(n) = report { sections.push(Section { title: "Summary".into(), widgets: vec![json!({ "type": "summary", "from": format!("report:{n}") })] }); }
    if !src.media.is_empty() { sections.push(Section { title: "Media".into(), widgets: vec![json!({ "type": "media" })] }); }
    sections.push(Section { title: "At a glance".into(), widgets: vec![json!({ "type": "kpis", "items": [{ "label": "status", "from": "run:status" }, { "label": "spent", "from": "run:cost" }, { "label": "time", "from": "run:time" }, { "label": "documents", "from": "run:artifacts" }] })] });
    for a in src.artifacts.iter().filter(|a| a.text.is_some()) {
        let from = match &a.node { Some(n) => format!("artifact:{n}/{}", a.name), None => format!("artifact:{}", a.name) };
        let title = a.name.rsplit_once('.').map_or(a.name.clone(), |x| x.0.replace(['-', '_'], " "));
        if a.name.ends_with(".md") && a.node.is_none() { sections.push(Section { title, widgets: vec![json!({ "type": "markdown", "from": from })] }); }
        else if a.name.ends_with(".html") && a.node.is_none() && a.name != "result.html" && a.bytes < 300_000 { sections.push(Section { title, widgets: vec![json!({ "type": "embed", "from": from })] }); }
    }
    let drafts = src.artifacts.iter().any(|a| a.node.is_some());
    if drafts || src.artifacts.iter().any(|a| a.node.is_none()) { sections.push(Section { title: "Documents".into(), widgets: vec![json!({ "type": "links" })] }); }
    sections.push(Section { title: "Cost".into(), widgets: vec![json!({ "type": "cost" })] });
    sections.push(Section { title: "Timeline".into(), widgets: vec![json!({ "type": "timeline" })] });
    Spec { title: src.team.clone(), sections }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_spec_with_no_widget_still_gives_a_page_and_a_video_the_run_made_is_in_it() {
        let mut s = src(); s.media = vec![MediaSrc { name: "test.mp4".into(), kind: "video".into(), bytes: 2_000_000, uri: Some("data:video/mp4;base64,AAAA".into()) }, MediaSrc { name: "big.mp4".into(), kind: "video".into(), bytes: 90_000_000, uri: None }];
        let blank = Spec { title: "3s test video".into(), sections: vec![Section { title: "3s test video".into(), widgets: vec![] }] }; let page = render(&blank, &s).unwrap();
        assert!(page.contains("At a glance") && page.contains("Media"), "an empty spec gets the page the run would get with no spec"); assert!(page.contains("<video controls") && page.contains("data:video/mp4;base64,AAAA"), "the video is in the page"); assert!(page.contains("could not be shown here"), "a file with no address says where to play it");
        let named = render(&Spec { title: "T".into(), sections: vec![Section { title: "m".into(), widgets: vec![json!({ "type": "media" })] }] }, &s).unwrap(); assert!(named.contains("<video controls")); let none = render(&Spec { title: "T".into(), sections: vec![Section { title: "m".into(), widgets: vec![json!({ "type": "media" })] }] }, &Sources { team: "x".into(), ..Default::default() }).unwrap(); assert!(none.contains("made no video"));
    }
    use super::*;
    fn src() -> Sources {
        let mut s = Sources { team: "Launch".into(), ask: "Plan the launch".into(), status: "done".into(), cost_usd: 0.08, started_ms: 1_700_000_000_000, ended_ms: 1_700_000_090_000, ..Default::default() };
        s.values.insert("writer".into(), json!({ "report": "## Plan\n- **one** thing\n- two <script>alert(1)</script>\n\n[site](https://example.com) [bad](javascript:alert(1))", "rows": [{ "a": 1, "b": "x" }], "results": [{ "id": "c1", "ok": true }, { "id": "c2", "ok": false, "evidence": "no" }] }));
        s.labels.insert("writer".into(), "the Writer".into()); s.node_cost.insert("writer".into(), 0.08);
        s.artifacts.push(ArtSrc { node: None, name: "brief.md".into(), bytes: 20, text: Some("# Brief\n\n| a | b |\n|---|---|\n| 1 | 2 |\n".into()) });
        s.artifacts.push(ArtSrc { node: None, name: "dash.html".into(), bytes: 30, text: Some("<h1 onclick=\"x\">hi</h1>".into()) });
        s.artifacts.push(ArtSrc { node: None, name: "data.csv".into(), bytes: 14, text: Some("name,n\n\"a, b\",2\nc,3\n".into()) });
        s.events.push((1_700_000_001_000, "writer".into(), "the Writer started".into())); s
    }
    fn page(ws: Vec<Value>) -> Result<String, String> { render(&Spec { title: "Launch".into(), sections: vec![Section { title: "One".into(), widgets: ws }] }, &src()) }

    #[test]
    fn text_is_escaped_and_only_http_links_become_links() {
        let h = page(vec![json!({ "type": "summary", "from": "report:writer" })]).unwrap();
        assert!(!h.contains("<script>alert"), "{h}"); assert!(h.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(h.contains("<a href=\"https://example.com\""), "an http link"); assert!(!h.contains("href=\"javascript"), "no javascript link"); assert!(h.contains("<strong>one</strong>"));
    }
    #[test]
    fn every_widget_builds_from_the_run() {
        let ws = vec![json!({ "type": "hero" }), json!({ "type": "summary", "from": "report:writer" }), json!({ "type": "markdown", "from": "artifact:brief.md" }), json!({ "type": "kpis", "items": [{ "label": "spent", "from": "run:cost" }, { "label": "time", "from": "run:time" }] }),
            json!({ "type": "table", "from": "artifact:data.csv" }), json!({ "type": "table", "from": "node:writer.rows" }), json!({ "type": "checklist", "from": "node:writer.results" }), json!({ "type": "timeline" }),
            json!({ "type": "cards", "items": [{ "title": "A", "text": "t", "tag": "x", "link": "https://a.example" }, { "title": "B", "link": "javascript:1" }] }), json!({ "type": "cost" }), json!({ "type": "links" }), json!({ "type": "callout", "tone": "warn", "text": "careful" }),
            json!({ "type": "bar", "series": [{ "label": "a", "value": 3 }, { "label": "b", "value": 6 }] }), json!({ "type": "embed", "from": "artifact:dash.html" })];
        let h = page(ws).unwrap();
        for needle in ["class=\"hero\"", "$0.08", "1m 30s", "<table>", "<td>a, b</td>", "class=\"check\"", "✗", "class=\"tl\"", "class=\"bars\"", "class=\"docs\"", "callout warn", "sandbox=\"allow-scripts\"", "srcdoc=\"&lt;h1 onclick"] { assert!(h.contains(needle), "missing {needle}"); }
        assert!(!h.contains("href=\"javascript"), "a card link is http only"); assert!(h.starts_with("<!doctype html>") && h.contains("<nav>") && !h.contains("http-equiv"));
    }
    #[test]
    fn a_bad_spec_is_said_in_words_before_it_is_built_and_a_missing_source_names_itself() {
        let r = |w: Value| page(vec![w]).unwrap_err();
        assert!(r(json!({ "type": "chart3d" })).contains("not a widget")); assert!(r(json!({ "type": "summary" })).contains("say what to show"));
        assert!(r(json!({ "type": "markdown", "from": "http://evil" })).contains("not a source")); assert!(r(json!({ "type": "embed", "from": "artifact:brief.md" })).contains("HTML document"));
        assert!(r(json!({ "type": "markdown", "from": "artifact:nope.md" })).contains("made no document called nope.md")); assert!(r(json!({ "type": "summary", "from": "report:ghost" })).contains("ghost has not run"));
        // no sections at all is still refused by the checker, and a spec whose sections hold no widget gets the page the run would get with no spec (the operator 2026-11-15: "the report showed empty")
        assert!(check(&Spec::default()).iter().any(|e| e.contains("no sections"))); assert!(render(&Spec::default(), &src()).unwrap().contains("At a glance"));
    }
    #[test]
    fn what_a_spec_reads_is_listed_for_the_validator() {
        let spec = Spec { title: "x".into(), sections: vec![Section { title: "a".into(), widgets: vec![json!({ "type": "summary", "from": "report:writer" }), json!({ "type": "kpis", "items": [{ "label": "l", "from": "node:qa.pass" }] }), json!({ "type": "markdown", "from": "artifact:brief.md" })] }] };
        let (n, a) = refs(&spec); assert_eq!(n, vec!["writer", "qa"]); assert_eq!(a, vec!["brief.md"]);
    }
    #[test]
    fn a_run_nobody_designed_for_still_gets_a_page_from_what_it_has() {
        let s = src(); let spec = auto_spec(&s); assert!(check(&spec).is_empty(), "{:?}", check(&spec));
        let h = render(&spec, &s).unwrap(); assert!(h.contains("Summary") && h.contains("brief") && h.contains("<iframe") && h.contains("Timeline") && h.contains("Cost"));
        let bare = Sources { team: "Empty".into(), status: "done".into(), ..Default::default() }; assert!(render(&auto_spec(&bare), &bare).is_ok(), "a run with nothing still renders");
    }
    #[test]
    fn the_markdown_is_small_and_safe() {
        let h = md("# T\n\ntext with `code` and *em*\n\n1. a\n2. b\n\n```\n<b>x</b>\n```\n\n> quote\n\n---");
        for n in ["<h2>T</h2>", "<code>code</code>", "<em>em</em>", "<ol><li>a</li><li>b</li></ol>", "&lt;b&gt;x&lt;/b&gt;", "<blockquote>quote</blockquote>", "<hr>"] { assert!(h.contains(n), "{n} in {h}"); }
    }
}
