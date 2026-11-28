//! render/autonomy-score-html.rs — twin of skills/render/autonomy-score-html.mjs (db lane).
//! NAMES ARE CONTRACT: `autonomyScoreHtml` (default). Renders skills/score/autonomy-score.mjs's
//! JSON (called with include_details:true, details_limit:100 forced) as a self-contained
//! dark-mode HTML dashboard. CSS + markup are transcribed byte-for-byte from the JS template
//! literals, including the pre-existing `<body>quot;` typo in the nav style attribute — this is
//! a verbatim transcription, not a fix (byte-identical output is the contract).
use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand

// escHtml(s) = String(s ?? '').replace(&).replace(<).replace(>).replace(")
fn esc_str(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
fn esc_html(v: Option<&Value>) -> String { esc_str(&str_qq(v)) }
// `x ?? '-'` then escHtml(that) — nullish falls to the literal dash (escaping it is a no-op).
fn esc_or_dash(v: Option<&Value>) -> String { match nz(v) { Some(x) => esc_html(Some(x)), None => "-".to_string() } }
// `x ?? '-'` interpolated directly (no escHtml wrapper) — nullish falls to the literal dash.
fn num_or_dash(v: Option<&Value>) -> String { match nz(v) { Some(x) => tmpl(Some(x)), None => "-".to_string() } }

const CSS: &str = r#"
:root { --bg:#0e1116; --panel:#161b22; --border:#30363d; --fg:#e6edf3; --muted:#8b949e; --accent:#58a6ff; --good:#3fb950; --warn:#d29922; --bad:#f85149; }
body { font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif; background:var(--bg); color:var(--fg); margin:0; padding:0; line-height:1.55; }
.wrap { max-width:1080px; margin:0 auto; padding:2em 1.5em 4em; }
h1 { font-size:1.7em; margin:0 0 0.2em; }
h2 { font-size:1.25em; margin-top:2em; padding-bottom:0.3em; border-bottom:1px solid var(--border); }
.kicker { color:var(--accent); font-family:ui-monospace,monospace; font-size:0.78em; text-transform:uppercase; letter-spacing:0.08em; margin-bottom:0.2em; }
.meta { color:var(--muted); font-size:0.85em; margin:0.3em 0 1em; font-family:ui-monospace,monospace; }
.hero { background:var(--panel); border:1px solid var(--accent); border-radius:10px; padding:2em; margin:1em 0; text-align:center; }
.hero .label { color:var(--muted); font-size:0.85em; font-family:ui-monospace,monospace; }
.hero .num { font-size:4em; font-weight:700; color:var(--accent); line-height:1; margin:0.1em 0; }
.hero .sub { color:var(--muted); font-size:0.9em; }
.grid { display:grid; grid-template-columns:repeat(auto-fit,minmax(180px,1fr)); gap:0.8em; margin:1em 0; }
.tile { background:var(--panel); border:1px solid var(--border); padding:1em; border-radius:6px; }
.tile .label { color:var(--muted); font-size:0.78em; }
.tile .num { font-size:1.8em; font-weight:600; color:var(--accent); line-height:1.1; }
.tile .sub { color:var(--muted); font-size:0.78em; }
table { width:100%; border-collapse:collapse; margin:1em 0; font-size:0.92em; }
th { background:var(--panel); padding:0.55em 0.8em; text-align:left; color:var(--muted); border-bottom:1px solid var(--border); font-size:0.85em; }
td { padding:0.5em 0.8em; border-bottom:1px solid var(--border); }
code { font-family:ui-monospace,monospace; font-size:0.88em; background:rgba(0,0,0,0.3); padding:1px 5px; border-radius:3px; }
.tag { padding:2px 8px; border-radius:10px; font-size:0.78em; font-family:ui-monospace,monospace; background:var(--panel); border:1px solid var(--border); }
.tag.fp-1 { background:rgba(63,185,80,0.15); color:var(--good); border-color:rgba(63,185,80,0.4); }
.tag.fp-0 { background:rgba(248,81,73,0.10); color:var(--bad);  border-color:rgba(248,81,73,0.4); }
.empty { background:var(--panel); border:1px solid var(--border); border-radius:8px; padding:2em; text-align:center; color:var(--muted); }
"#;

// Object.entries(score.by_tier ?? {}).map(...).join('') — iteration order = serde_json Map
// insertion order (preserve_order), which mirrors the upstream twin's first-seen tier order.
fn render_tiers(by_tier: Option<&Value>) -> String {
    let mut out = String::new();
    if let Some(Value::Object(map)) = by_tier {
        for (t, b) in map {
            let points = tmpl(b.get("points"));
            let first_pass_s = tmpl(b.get("first_pass"));
            let closed_s = tmpl(b.get("closed"));
            let ratio = if truthy(b.get("closed")) {
                let fp = b.get("first_pass").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let cl = b.get("closed").and_then(|v| v.as_f64()).unwrap_or(0.0);
                format!("{:.2}", fp / cl)
            } else {
                "n/a".to_string()
            };
            out.push_str(&format!(
                "<div class=\"tile\"><div class=\"label\">{}</div><div class=\"num\">{}</div><div class=\"sub\">{}/{} first-pass \u{b7} ratio {}</div></div>",
                esc_str(t), points, first_pass_s, closed_s, ratio
            ));
        }
    }
    out
}

fn render_details_row(d: &Value) -> String {
    let slug = esc_html(d.get("slug"));
    let tier = esc_or_dash(d.get("tier"));
    let size = esc_or_dash(d.get("size"));
    let complexity = num_or_dash(d.get("complexity"));
    let risk = num_or_dash(d.get("risk"));
    let contract_score = tmpl(d.get("contract_score"));
    let fp = d.get("first_pass");
    let fp_class = tmpl(fp);
    let fp_text = if truthy(fp) { "yes" } else { "no" };
    let points = tmpl(d.get("points"));
    let updated_raw = str_qq(d.get("updated_at"));
    let updated_sliced: String = updated_raw.chars().take(10).collect();
    let updated = esc_str(&updated_sliced);

    format!(
        "\n          <tr>\n            <td><code>{}</code></td>\n            <td>{}</td>\n            <td>{}</td>\n            <td>{}</td>\n            <td>{}</td>\n            <td>{}</td>\n            <td><span class=\"tag fp-{}\">{}</span></td>\n            <td><strong>{}</strong></td>\n            <td class=\"meta\">{}</td>\n          </tr>",
        slug, tier, size, complexity, risk, contract_score, fp_class, fp_text, points, updated
    )
}

fn render_details(details: &[Value]) -> String {
    if details.is_empty() {
        "<div class=\"empty\">No closed contracts yet in this window. Once contracts ship via the gate chain, the score will populate.</div>".to_string()
    } else {
        let rows: String = details.iter().map(render_details_row).collect::<Vec<_>>().join("");
        format!(
            "<table>\n        <tr><th>slug</th><th>tier</th><th>size</th><th>cmpl</th><th>risk</th><th>\u{d7}</th><th>first-pass</th><th>points</th><th>closed</th></tr>\n        {}\n      </table>",
            rows
        )
    }
}

pub fn autonomyScoreHtml(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('autonomy-score-html','db') when no db — the db lane always
    // injects one, so the guard-throw path is not exercised (fixtures cover success only).
    let mut merged = input.as_object().cloned().unwrap_or_default();
    merged.insert("include_details".into(), json!(true));
    merged.insert("details_limit".into(), json!(100));
    let score = crate::score_autonomy_score::autonomyScore(&Value::Object(merged), db)?;

    let tiers = render_tiers(score.get("by_tier"));
    let details_arr = score.get("details").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let details_html = render_details(&details_arr);

    let since = esc_html(score.get("window").and_then(|w| w.get("since")));
    let until = esc_html(score.get("window").and_then(|w| w.get("until")));
    let period_score = tmpl(score.get("period_score"));
    let first_pass_count = tmpl(score.get("first_pass_count"));
    let contracts_closed = tmpl(score.get("contracts_closed"));
    let ratio = tmpl(score.get("ratio"));
    let streak = tmpl(score.get("streak"));
    let tiers_or_empty = if tiers.is_empty() { "<div class=\"empty\">No tiered breakdown yet.</div>".to_string() } else { tiers };
    let formula = esc_html(score.get("formula"));
    let size_weight_str = score.get("size_weight_table").and_then(|v| v.as_object())
        .map(|m| m.iter().map(|(k, v)| format!("{}={}", k, tmpl(Some(v)))).collect::<Vec<_>>().join(" \u{b7} "))
        .unwrap_or_default();

    let lines: Vec<String> = vec![
        "<!DOCTYPE html>".to_string(),
        format!("<html><head><meta charset=\"UTF-8\"><title>antiloki \u{b7} autonomy score</title><style>{}</style></head>", CSS),
        "<body><nav style=\"padding:0.55em 1.5em;background:#161b22;border-bottom:1px solid #30363d;font-family:-apple-system,BlinkMacSystemFont,<body>quot;Segoe UI<body>quot;,sans-serif;font-size:0.85em;\"><a href=\"./index.html\" style=\"color:#58a6ff;text-decoration:none;\">\u{2190} antiloki docs</a></nav><div class=\"wrap\">".to_string(),
        "<div class=\"kicker\">antiloki-v3 \u{b7} autonomy score</div>".to_string(),
        "<h1>Autonomy score \u{2014} weighted first-pass shipping</h1>".to_string(),
        format!("<p class=\"meta\">Window: <code>{}</code> \u{2192} <code>{}</code>. Auto-generated \u{b7} regen <code>node bin/antiloki.mjs system.score.autonomy.html '{{}}' &gt; docs/autonomy-score.html</code></p>", since, until),
        String::new(),
        "<div class=\"hero\">".to_string(),
        "  <div class=\"label\">period score</div>".to_string(),
        format!("  <div class=\"num\">{}</div>", period_score),
        format!("  <div class=\"sub\">{} / {} contracts first-pass \u{b7} ratio {}</div>", first_pass_count, contracts_closed, ratio),
        "</div>".to_string(),
        String::new(),
        "<div class=\"grid\">".to_string(),
        format!("  <div class=\"tile\"><div class=\"label\">contracts closed</div><div class=\"num\">{}</div></div>", contracts_closed),
        format!("  <div class=\"tile\"><div class=\"label\">first-pass count</div><div class=\"num\">{}</div></div>", first_pass_count),
        format!("  <div class=\"tile\"><div class=\"label\">ratio</div><div class=\"num\">{}</div></div>", ratio),
        format!("  <div class=\"tile\"><div class=\"label\">streak</div><div class=\"num\">{}</div><div class=\"sub\">consecutive most-recent first_pass=1</div></div>", streak),
        "</div>".to_string(),
        String::new(),
        "<h2>By tier</h2>".to_string(),
        format!("<div class=\"grid\">{}</div>", tiers_or_empty),
        String::new(),
        "<h2>Per-contract detail</h2>".to_string(),
        details_html,
        String::new(),
        "<h2>Formula</h2>".to_string(),
        format!("<p class=\"meta\">{}</p>", formula),
        format!("<p class=\"meta\">size_weight: {}</p>", size_weight_str),
        "<p class=\"meta\">first_pass_flag = 1 iff (current_attempt=1 AND superseded_by IS NULL AND no escaped/blocked law_violations linked to this contract)</p>".to_string(),
        "</div></body></html>".to_string(),
    ];

    Ok(json!(lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws;
            CREATE TABLE contracts (slug TEXT NOT NULL UNIQUE, card_slug TEXT, title TEXT, status TEXT NOT NULL DEFAULT 'proposed', size TEXT, complexity INTEGER, risk INTEGER, tier TEXT, current_attempt INTEGER NOT NULL DEFAULT 1, superseded_by TEXT, signed_by TEXT, updated_at TEXT, created_at TEXT);
            CREATE TABLE laws.law_violations (id INTEGER PRIMARY KEY, context_json TEXT, outcome TEXT);
            INSERT INTO contracts (slug,card_slug,title,status,size,complexity,risk,tier,current_attempt,superseded_by,signed_by,updated_at,created_at) VALUES
              ('SLUG-1','CARD-1','t1','done','L',4,3,'high-risk',1,NULL,NULL,'2026-01-10','2026-01-01'),
              ('SLUG-2','CARD-2','t2','done','M',2,2,'high-risk',1,NULL,NULL,'2026-01-09','2026-01-01'),
              ('SLUG-3','CARD-3','t3','done','S',3,1,'standard',2,NULL,NULL,'2026-01-08','2026-01-01'),
              ('SLUG-4','CARD-4','t4','done','XS',1,1,'standard',1,'SLUG-4B',NULL,'2026-01-07','2026-01-01'),
              ('SLUG-5','CARD-5','t5','done','L',5,5,NULL,1,NULL,NULL,'2026-01-06','2026-01-01'),
              ('SLUG-6','CARD-6','t6','done',NULL,NULL,NULL,'trivial',1,NULL,NULL,'2026-01-05','2026-01-01');
            INSERT INTO laws.law_violations (id,context_json,outcome) VALUES
              (1,'{\"contract_slug\":\"SLUG-2\",\"note\":\"threshold\"}','escaped');").unwrap();
        c
    }

    #[test]
    fn renders_full_dashboard_with_tiers_and_details_table() {
        let html = autonomyScoreHtml(&json!({}), &seeded()).unwrap();
        let s = html.as_str().unwrap();
        assert!(s.starts_with("<!DOCTYPE html>\n<html><head>"));
        assert!(s.ends_with("</div></body></html>"));
        assert!(s.contains("<title>antiloki \u{b7} autonomy score</title>"));
        assert!(s.contains(":root { --bg:#0e1116;"));
        // 6 done contracts, SLUG-1/5/6 first-pass=1 (SLUG-2 has a violation, SLUG-3 is attempt 2, SLUG-4 superseded)
        assert!(s.contains("<div class=\"label\">period score</div>\n  <div class=\"num\">185</div>"));
        assert!(s.contains("<div class=\"label\">first-pass count</div><div class=\"num\">3</div>"));
        assert!(s.contains("<div class=\"sub\">3 / 6 contracts first-pass \u{b7} ratio 0.5</div>"));
        // by_tier renders a tile per tier, including the null-tier bucket "unknown"
        assert!(s.contains("<div class=\"label\">unknown</div>"));
        assert!(s.contains("<div class=\"label\">high-risk</div>"));
        assert!(s.contains("<div class=\"label\">standard</div>"));
        assert!(s.contains("<div class=\"label\">trivial</div>"));
        // per-contract table: escaped slug, dash fallbacks for SLUG-6's null size/complexity/risk
        assert!(s.contains("<td><code>SLUG-6</code></td>"));
        assert!(s.contains("<td>trivial</td>\n            <td>-</td>\n            <td>-</td>\n            <td>-</td>"));
        // fp tags render both branches
        assert!(s.contains("class=\"tag fp-1\">yes</span>"));
        assert!(s.contains("class=\"tag fp-0\">no</span>"));
        assert!(s.contains("<p class=\"meta\">size_weight: XS=1 \u{b7} S=2 \u{b7} M=3 \u{b7} L=5</p>"));
        assert!(!s.contains("No tiered breakdown yet."));
        assert!(!s.contains("No closed contracts yet"));
    }

    #[test]
    fn renders_empty_state_when_no_closed_contracts() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws;
            CREATE TABLE contracts (slug TEXT NOT NULL UNIQUE, card_slug TEXT, title TEXT, status TEXT NOT NULL DEFAULT 'proposed', size TEXT, complexity INTEGER, risk INTEGER, tier TEXT, current_attempt INTEGER NOT NULL DEFAULT 1, superseded_by TEXT, signed_by TEXT, updated_at TEXT, created_at TEXT);
            CREATE TABLE laws.law_violations (id INTEGER PRIMARY KEY, context_json TEXT, outcome TEXT);
            INSERT INTO contracts (slug,card_slug,title,status,updated_at,created_at) VALUES ('S','C','t','doing','2026-01-01','2026-01-01');").unwrap();
        let html = autonomyScoreHtml(&json!({}), &c).unwrap();
        let s = html.as_str().unwrap();
        assert!(s.contains("<div class=\"grid\"><div class=\"empty\">No tiered breakdown yet.</div></div>"));
        assert!(s.contains("<div class=\"empty\">No closed contracts yet in this window. Once contracts ship via the gate chain, the score will populate.</div>"));
        assert!(s.contains("Window: <code>all</code> \u{2192} <code>now</code>"));
        assert!(s.contains("<div class=\"num\">0</div>")); // period score
    }

    #[test]
    fn since_until_window_filters_and_echoes_dates() {
        let html = autonomyScoreHtml(&json!({ "since": "2026-01-06", "until": "2026-01-10" }), &seeded()).unwrap();
        let s = html.as_str().unwrap();
        assert!(s.contains("Window: <code>2026-01-06</code> \u{2192} <code>2026-01-10</code>"));
        // SLUG-1..SLUG-5 in window; first_pass_count=2 (SLUG-1, SLUG-5), contracts_closed=5, ratio 0.4
        assert!(s.contains("<div class=\"sub\">2 / 5 contracts first-pass \u{b7} ratio 0.4</div>"));
        assert!(!s.contains("SLUG-6"));
    }

    #[test]
    fn esc_str_escapes_amp_lt_gt_quot_in_order() {
        assert_eq!(esc_str("<a href=\"x\">A & B</a>"), "&lt;a href=&quot;x&quot;&gt;A &amp; B&lt;/a&gt;");
    }
}
