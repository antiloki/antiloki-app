//! course-press.rs — twin of skills/codebase/course-press.mjs (expeditions v4, press one COURSE
//! for a faculty). NAMES ARE CONTRACT for the JS *named exports*: `validGame` · `gatherTopicDocs` ·
//! `warmupTopicModule` · `gatherDocumentation` · `warmupModule` · `coursePress` (default). The
//! module-private JS helpers `ensurePrompt` / `pick` / `S` / `parseMaybeTruncated` / `parseCourse`
//! are ported too (kept camelCase/snake_case per existing sibling convention) because they are
//! fully deterministic and independently testable.
//!
//! PARITY-DEFERRED — THREE boundaries, same shape as magazine-press.rs's own doc:
//! (1) `magazineObserve` (skills/codebase/magazine-observe.mjs). Per magazine-observe.rs's own
//!     module doc, its entire orchestrator has no Rust body (composes `cachedFetch`, network).
//!     `course-press.mjs` calls it un-guarded (`const obs = await magazineObserve(...)`, no
//!     try/catch) — a failure propagates as a thrown error. Modeled here as
//!     `deferred_magazine_observe`, invoked with `?` so `coursePress` propagates it exactly like
//!     the uncaught JS `await`. This is the reason `coursePress` itself is unreachable past faculty
//!     validation + the two `ensure*` DDL calls in a test — every helper below is ported as its own
//!     pure function and unit-tested directly on constructed inputs.
//! (2) `cachedFetch` (skills/codebase/_helpers/web-cache.mjs), used inside `gatherDocumentation`
//!     and `gatherTopicDocs`. Unlike (1), JS wraps EVERY `cachedFetch` call in `.catch(() => null)`
//!     — a network failure degrades to `null` and execution continues. Modeled here as
//!     `deferred_cached_fetch`, always `Err`, with `.ok()` at each call site mirroring the JS
//!     `.catch(() => null)`. Because production has no working network fetch either way, this
//!     Err-degrades-to-None path IS the real, exercised behavior — both gathering functions are
//!     ported in full and unit-tested with this deferred stub composed in (same pattern the
//!     playbook calls out: "the deferred dep Erring is itself a real, testable branch").
//! (3) the course-design AI call, dispatched through `crate::ai_ai_call::aiCall` (also always
//!     `Err`). JS wraps this one in try/catch and returns a STATUS object
//!     (`{ok:false, reason:'press-failed', error}`) instead of throwing — mirrored as `Ok(status)`.
//!     `coursePress` never reaches this boundary either (blocked by (1) first), so it is documented
//!     here for completeness but not independently reachable from `coursePress`.
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

const SKILL: &str = "course-press";
const MAX_TECHS: usize = 4;
const PROMPT: &str = "research.course.press.v4";

const SYSTEM: &str = "You are a course designer at a small technical faculty. You teach TECHNOLOGIES — frameworks, runtimes, tools — using only the live documentation extracts you are given. Knowledge before training: first a substantial didactic lecture, then games that train exactly what the lecture taught. Never invent facts beyond the extracts; never quiz on the reader's own codebase.";
const TEMPLATE: &str = "Design a technology course for the {{faculty_title}} faculty (\"{{motto}}\"). The course teaches TECHNOLOGY — never the reader's own code. Ground every lecture and every game STRICTLY in the live documentation extracts below (each technology's own site, readme and blog).\n\nCOURSE SCOPE: {{scope}}\n\nTECHNOLOGIES + LIVE DOCUMENTATION EXTRACTS (JSON):\n{{documentation}}\n\nRules:\n- If the scope names ONE topic: write 3 to 4 CHAPTERS about that topic alone (\"tech\" = the topic on each), progressing from mental model → how it works → practical use.\n- If the scope is the whole stack: 2 to 3 modules, one per technology, most-important first.\n- Each module/chapter: \"tech\", then \"lesson_md\" — a REAL LECTURE of 200 to 380 words in 2-4 paragraphs (separated by blank lines): what it is, the core mental model, how it actually works, and one or two practical insights taken from the extracts. Didactic, concrete, no filler. Then \"games\": exactly 2 training games answerable from that lecture alone.\n- Game kinds (use variety): quiz {\"kind\":\"quiz\",\"q\":\"…\",\"options\":[\"…\",…2-5],\"answer\":<correct index>} · trivia {\"kind\":\"trivia\",\"statement\":\"…\",\"truth\":true|false} · order {\"kind\":\"order\",\"prompt\":\"…\",\"items\":[\"…\",…3-6] in the CORRECT order} · match {\"kind\":\"match\",\"prompt\":\"…\",\"pairs\":[{\"a\":\"…\",\"b\":\"…\"},…2-6]}.\n\nPut into course_json ONLY one JSON object: {\"title\":\"…\",\"modules\":[{\"tech\":\"…\",\"lesson_md\":\"…\",\"games\":[{…},{…}]}]}";

// String(v ?? '')
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
} // JS .slice(0,n) (BMP-equivalent, disclosed approximation — codebase-wide convention)

// `const S = (v, cap) => String(v ?? '').slice(0, cap);`
fn s_cap(v: Option<&Value>, cap: usize) -> String {
    take_chars(&str_qq(v), cap)
}

// JS: encodeURIComponent — unreserved set A-Z a-z 0-9 - _ . ! ~ * ' ( )
fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(Value::as_str) == Some(TEMPLATE) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT, "template_md": TEMPLATE, "system_md": SYSTEM,
            "default_tier": "mid", "default_temperature": "warm", "phase_flow": "single_only",
            "max_output_tokens": 6000,
            "output_fields": [{ "field_name": "course_json", "field_type": "string" }],
        }),
        db,
    )?;
    Ok(())
}

// `const pick = (res, field) => res?.output_kv?.find((o) => o.field_name === field && o.parent_output_id == null)?.value_text ?? null;`
fn pick(res: &Value, field: &str) -> Option<String> {
    let rows = res.get("output_kv")?.as_array()?;
    let row = rows.iter().find(|o| {
        o.get("field_name").and_then(Value::as_str) == Some(field)
            && o.get("parent_output_id").map_or(true, |v| v.is_null())
    })?;
    row.get("value_text").and_then(Value::as_str).map(|s| s.to_string())
}

// ── validGame (JS named export) — strict per-kind validation, a malformed game is DROPPED. ──────
pub fn validGame(g: &Value) -> Option<Value> {
    if !g.is_object() {
        return None;
    }
    let kind = str_qq(g.get("kind"));
    match kind.as_str() {
        "quiz" => {
            let options: Vec<String> = g.get("options").and_then(Value::as_array).into_iter().flatten()
                .map(|o| s_cap(Some(o), 120)).filter(|s| !s.is_empty()).take(5).collect();
            // `Number.isInteger(answer)` — mirror via a finite, fractionless f64 check.
            let answer_i = g.get("answer").and_then(Value::as_f64).and_then(|n| {
                if n.is_finite() && n.fract() == 0.0 { Some(n as i64) } else { None }
            });
            if !truthy(g.get("q")) || options.len() < 2 {
                return None;
            }
            let Some(a) = answer_i else { return None };
            if a < 0 || a as usize >= options.len() {
                return None;
            }
            Some(json!({ "kind": kind, "q": s_cap(g.get("q"), 200), "options": options, "answer": a }))
        }
        "trivia" => {
            if !truthy(g.get("statement")) || !matches!(g.get("truth"), Some(Value::Bool(_))) {
                return None;
            }
            Some(json!({ "kind": kind, "statement": s_cap(g.get("statement"), 220), "truth": g.get("truth").cloned().unwrap_or(Value::Null) }))
        }
        "order" => {
            let items: Vec<String> = g.get("items").and_then(Value::as_array).into_iter().flatten()
                .map(|o| s_cap(Some(o), 90)).filter(|s| !s.is_empty()).take(6).collect();
            if !truthy(g.get("prompt")) || items.len() < 3 {
                return None;
            }
            Some(json!({ "kind": kind, "prompt": s_cap(g.get("prompt"), 200), "items": items }))
        }
        "match" => {
            let pairs: Vec<Value> = g.get("pairs").and_then(Value::as_array).into_iter().flatten()
                .map(|p| (s_cap(p.get("a"), 90), s_cap(p.get("b"), 90)))
                .filter(|(a, b)| !a.is_empty() && !b.is_empty())
                .take(6)
                .map(|(a, b)| json!({ "a": a, "b": b }))
                .collect();
            if !truthy(g.get("prompt")) || pairs.len() < 2 {
                return None;
            }
            Some(json!({ "kind": kind, "prompt": s_cap(g.get("prompt"), 200), "pairs": pairs }))
        }
        _ => None,
    }
}

// JS truthy() coercion, used for the `!g.q` / `!g.statement` / `!g.prompt` guards above.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// PARITY-DEFERRED boundary #1 (see module doc): magazineObserve has no Rust body.
fn deferred_magazine_observe(_faculty: &str, _input: &Value) -> Result<Value, String> {
    Err("magazine-observe: PARITY-DEFERRED — magazineObserve's department builders depend on cachedFetch (network), no Rust body".to_string())
}

// PARITY-DEFERRED boundary #2 (see module doc): cachedFetch has no Rust body — every JS call site
// wraps it in `.catch(() => null)`, mirrored by callers doing `.ok()` on this Result.
fn deferred_cached_fetch(_db: &Connection, _url: &str) -> Result<String, String> {
    Err("web-cache: cachedFetch is PARITY-DEFERRED — real network fetch, no Rust body".to_string())
}

// ── gatherTopicDocs (JS named export) — deep grounding for ONE topic. ────────────────────────────
pub fn gatherTopicDocs(db: &Connection, obs: &Value, topic: &str, _ctl: &Value) -> Value {
    let departments = obs.get("departments").cloned().unwrap_or_else(|| json!({}));
    let t = topic.to_lowercase();

    let desk_items: Vec<Value> = departments.get("desk").and_then(|d| d.get("items")).and_then(Value::as_array).cloned().unwrap_or_default();
    let desk: Vec<Value> = desk_items.iter()
        .filter(|d| {
            let label = str_qq(d.get("label"));
            let body = str_qq(d.get("body"));
            format!("{label} {body}").to_lowercase().contains(&t)
        })
        .take(2)
        .map(|d| {
            let label = str_qq(d.get("label"));
            let body = str_qq(d.get("body"));
            json!({ "tech": format!("desk: {label}"), "home": Value::Null, "extract": take_chars(&body, 1500), "recent_posts": [] })
        })
        .collect();

    let ledger_deps: Vec<Value> = departments.get("ledger").and_then(|l| l.get("deps")).and_then(Value::as_array).cloned().unwrap_or_default();
    let dep = ledger_deps.iter().find(|x| {
        let name = str_qq(x.get("name")).to_lowercase();
        name == t || name.rsplit('/').next().unwrap_or("").to_lowercase() == t
    });
    let reg_name = dep.and_then(|d| d.get("name")).map(|v| str_qq(Some(v))).unwrap_or_else(|| t.clone());

    // `cachedFetch(...).catch(() => null)` — deferred; JSON.parse guarded by JS try/catch too.
    let reg_url = format!("https://registry.npmjs.org/{}/latest", encode_uri_component(&reg_name));
    let body = deferred_cached_fetch(db, &reg_url).ok();
    let (home, description, readme) = match body.as_deref().map(serde_json::from_str::<Value>) {
        Some(Ok(doc)) => {
            let home = doc.get("homepage").and_then(Value::as_str).map(|s| s.to_string());
            let description = str_qq(doc.get("description"));
            let readme = doc.get("readme").and_then(Value::as_str).map(|s| take_chars(s, 2500)).unwrap_or_default();
            (home, description, readme)
        }
        _ => (None, String::new(), String::new()),
    };

    let mut extract = String::new();
    if let Some(h) = &home {
        if https_re().is_match(h) {
            if let Some(html) = deferred_cached_fetch(db, h).ok() {
                let art = crate::codebase__helpers_web_research::extractArticle(&json!(html));
                extract = take_chars(art.as_str().unwrap_or(""), 2500);
            }
        }
    }

    let press_items: Vec<Value> = departments.get("press").and_then(|p| p.get("items")).and_then(Value::as_array).cloned().unwrap_or_default();
    let recent_posts: Vec<Value> = press_items.iter()
        .filter(|it| str_qq(it.get("source")) == t)
        .take(4)
        .map(|it| json!({ "title": it.get("title").cloned().unwrap_or(Value::Null), "excerpt": s_cap(it.get("content"), 800) }))
        .collect();

    let extract_joined: String = [description.as_str(), extract.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n");
    let extract_final = take_chars(&extract_joined, 2800);

    let entry = json!({
        "tech": t,
        "home": home.map(Value::from).unwrap_or(Value::Null),
        "extract": extract_final,
        "readme": readme,
        "recent_posts": recent_posts,
    });

    let mut out = desk;
    let has_extract = entry["extract"].as_str().map(|s| !s.is_empty()).unwrap_or(false);
    let has_readme = entry["readme"].as_str().map(|s| !s.is_empty()).unwrap_or(false);
    let has_posts = entry["recent_posts"].as_array().map(|a| !a.is_empty()).unwrap_or(false);
    if has_extract || has_readme || has_posts {
        out.push(entry);
    }
    json!(out)
}

fn https_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)^https://").unwrap())
}

// ── warmupTopicModule (JS named export) — mint the topic warm-up from the observation. ───────────
pub fn warmupTopicModule(obs: &Value, topic: &str) -> Value {
    let t = topic.to_lowercase();
    let press_items: Vec<Value> = obs.get("departments").and_then(|d| d.get("press")).and_then(|p| p.get("items")).and_then(Value::as_array).cloned().unwrap_or_default();
    let wire_items: Vec<Value> = obs.get("departments").and_then(|d| d.get("wire")).and_then(|p| p.get("items")).and_then(Value::as_array).cloned().unwrap_or_default();
    let items: Vec<Value> = press_items.into_iter().chain(wire_items).filter(|it| {
        str_qq(it.get("source")) == t && nz(it.get("date")).is_some()
    }).collect();

    let mut seen = std::collections::HashSet::new();
    let dated: Vec<Value> = items.into_iter().filter(|it| {
        let date = str_qq(it.get("date"));
        if seen.contains(&date) { false } else { seen.insert(date); true }
    }).take(4).collect();

    if dated.len() < 3 {
        return json!([]);
    }
    let mut ordered = dated;
    ordered.sort_by(|a, b| str_qq(b.get("date")).cmp(&str_qq(a.get("date"))));

    let items_out: Vec<Value> = ordered.iter().map(|it| json!(take_chars(&str_qq(it.get("title")), 70))).collect();

    json!([{
        "tech": t,
        "lesson_md": format!("Before the lectures: {t} is a moving target — these are its most recent public releases and posts, straight from its own feeds. Knowing the cadence is part of knowing the technology."),
        "games": [{ "kind": "order", "prompt": format!("Order {t}'s recent news, newest first"), "items": items_out }],
        "minted": "substrate",
    }])
}

// ── gatherDocumentation (JS named export) — the whole-stack grounding. ──────────────────────────
pub fn gatherDocumentation(db: &Connection, obs: &Value, _ctl: &Value) -> Value {
    let departments = obs.get("departments").cloned().unwrap_or_else(|| json!({}));
    let desk_items: Vec<Value> = departments.get("desk").and_then(|d| d.get("items")).and_then(Value::as_array).cloned().unwrap_or_default();
    let desk_docs: Vec<Value> = desk_items.iter()
        .filter(|d| str_qq(d.get("body")).len() > 120)
        .take(2)
        .map(|d| {
            let label = str_qq(d.get("label"));
            let body = str_qq(d.get("body"));
            json!({ "tech": format!("desk: {label}"), "home": Value::Null, "extract": take_chars(&body, 1200), "recent_posts": [] })
        })
        .collect();

    if obs.get("faculty").and_then(Value::as_str) == Some("product") {
        let rivals: Vec<Value> = departments.get("rivals").and_then(|r| r.get("rivals")).and_then(Value::as_array).cloned().unwrap_or_default();
        let mut out = desk_docs;
        out.extend(
            rivals.iter()
                .filter(|r| r.get("ok").and_then(Value::as_bool).unwrap_or(false))
                .take(MAX_TECHS)
                .map(|r| {
                    let mut parts: Vec<String> = vec![str_qq(r.get("title")), str_qq(r.get("description"))];
                    for c in r.get("claims").and_then(Value::as_array).into_iter().flatten() {
                        parts.push(str_qq(Some(c)));
                    }
                    let extract = take_chars(&parts.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · "), 900);
                    json!({ "tech": r.get("name").cloned().unwrap_or(Value::Null), "home": r.get("url").cloned().unwrap_or(Value::Null), "extract": extract, "recent_posts": [] })
                }),
        );
        return json!(out);
    }

    let press = departments.get("press").cloned().unwrap_or_else(|| json!({}));
    let blogs: Vec<Value> = press.get("blogs").and_then(Value::as_array).cloned().unwrap_or_default();
    let deps: Vec<Value> = departments.get("ledger").and_then(|l| l.get("deps")).and_then(Value::as_array).cloned().unwrap_or_default()
        .into_iter().filter(|d| !truthy(d.get("dev"))).collect();

    let mut techs: Vec<(String, Value)> = Vec::new();
    let mut push = |name: Option<&Value>, home: Value| {
        let n = str_qq(name).to_lowercase();
        if !n.is_empty() && !techs.iter().any(|(t, _)| t == &n) {
            techs.push((n, home));
        }
    };
    for b in &blogs {
        push(b.get("tech"), Value::Null);
    }
    let horizon_products: Vec<Value> = departments.get("horizon").and_then(|h| h.get("products")).and_then(Value::as_array).cloned().unwrap_or_default();
    for p in &horizon_products {
        push(p.get("product"), Value::Null);
    }
    for d in &deps {
        let name = str_qq(d.get("name"));
        let last_seg = name.rsplit('/').next().unwrap_or("").to_string();
        push(Some(&json!(last_seg)), Value::Null);
    }

    let mut out: Vec<Value> = Vec::new();
    for (name, home0) in techs.into_iter().take(MAX_TECHS) {
        let mut home: Option<String> = home0.as_str().map(|s| s.to_string());
        if home.is_none() {
            let reg_url = format!("https://registry.npmjs.org/{}/latest", encode_uri_component(&name));
            let body = deferred_cached_fetch(db, &reg_url).ok();
            home = match body.as_deref().map(serde_json::from_str::<Value>) {
                Some(Ok(doc)) => doc.get("homepage").and_then(Value::as_str).map(|s| s.to_string()),
                _ => None,
            };
        }
        let mut extract = String::new();
        if let Some(h) = &home {
            if https_re().is_match(h) {
                let html = deferred_cached_fetch(db, h).ok();
                if let Some(html) = html {
                    let art = crate::codebase__helpers_web_research::extractArticle(&json!(html));
                    extract = take_chars(art.as_str().unwrap_or(""), 1200);
                }
            }
        }
        let press_items: Vec<Value> = press.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
        let recent_posts: Vec<Value> = press_items.iter()
            .filter(|it| str_qq(it.get("source")) == name)
            .take(2)
            .map(|it| json!({ "title": it.get("title").cloned().unwrap_or(Value::Null), "excerpt": s_cap(it.get("content"), 500) }))
            .collect();
        if !extract.is_empty() || !recent_posts.is_empty() {
            out.push(json!({ "tech": name, "home": home.map(Value::from).unwrap_or(Value::Null), "extract": extract, "recent_posts": recent_posts }));
        }
    }

    let mut result = desk_docs;
    result.extend(out);
    json!(result)
}

// ── warmupModule (JS named export) — module 0, minted deterministically, zero AI. ────────────────
pub fn warmupModule(docs: &Value) -> Value {
    let docs_arr: Vec<Value> = docs.as_array().cloned().unwrap_or_default();
    let with_home: Vec<&Value> = docs_arr.iter().filter(|d| truthy(d.get("home"))).take(5).collect();
    let pairs: Vec<Value> = with_home.iter().map(|d| {
        let home = str_qq(d.get("home"));
        let host = url_hostname(&home).unwrap_or_else(|| home.clone());
        json!({ "a": d.get("tech").cloned().unwrap_or(Value::Null), "b": host })
    }).collect();
    if pairs.len() < 2 {
        return json!([]);
    }
    let tech_list: Vec<String> = docs_arr.iter().map(|d| str_qq(d.get("tech"))).collect();
    json!([{
        "tech": "the stack",
        "lesson_md": format!("This course covers {} — the technologies this project is built from. Every lesson ahead is grounded in documentation scraped live from each technology's own site. First: prove you know where each one lives.", tech_list.join(", ")),
        "games": [{ "kind": "match", "prompt": "Match each technology to its home on the web", "pairs": pairs }],
        "minted": "substrate",
    }])
}

// Minimal `new URL(s).hostname.replace(/^www\./,'')` stand-in — mirrors web-cache.rs's own parser
// (no IDNA/percent-decoding); on a malformed URL JS's `try{}catch{keep raw}` keeps the raw string,
// mirrored by returning None so the caller falls back to the raw home string.
fn url_hostname(s: &str) -> Option<String> {
    let idx = s.find("://")?;
    let rest = &s[idx + 3..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let host_port = match authority.rfind('@') {
        Some(i) => &authority[i + 1..],
        None => authority,
    };
    let host = match host_port.rfind(':') {
        Some(i) => &host_port[..i],
        None => host_port,
    };
    if host.is_empty() {
        return None;
    }
    let host = host.to_lowercase();
    Some(host.strip_prefix("www.").map(|s| s.to_string()).unwrap_or(host))
}

// ── parseMaybeTruncated — walk back to the last closed object and re-seal the document. ──────────
fn parse_maybe_truncated(raw: &str) -> Option<Value> {
    if let Some(start) = raw.find('{') {
        if let Some(end) = raw.rfind('}') {
            if end >= start {
                if let Ok(v) = serde_json::from_str::<Value>(&raw[start..=end]) {
                    return Some(v);
                }
            }
        }
    }
    let start = raw.find('{')?;
    let body = &raw[start..];
    let mut cut = body.len();
    while cut > 40 {
        let idx = body[..cut].rfind('}')?;
        let base = &body[..=idx];
        for close in ["", "]}", "}]}", "]}]}"] {
            let candidate = format!("{base}{close}");
            if let Ok(v) = serde_json::from_str::<Value>(&candidate) {
                return Some(v);
            }
        }
        cut = idx;
    }
    None
}

fn fence_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"```[a-z]*\n?|```").unwrap())
}

fn parse_course(text: Option<&str>) -> Option<Value> {
    let raw = fence_re().replace_all(text.unwrap_or(""), "").to_string();
    let doc = parse_maybe_truncated(&raw)?;
    let modules_raw = doc.get("modules")?.as_array()?;
    let modules: Vec<Value> = modules_raw.iter().filter_map(|mo| {
        let games: Vec<Value> = mo.get("games").and_then(Value::as_array).into_iter().flatten()
            .filter_map(validGame).take(3).collect();
        let lesson = s_cap(mo.get("lesson_md"), 3000);
        if !truthy(mo.get("tech")) || lesson.chars().count() < 80 || games.is_empty() {
            return None;
        }
        Some(json!({ "tech": s_cap(mo.get("tech"), 60), "lesson_md": lesson, "games": games }))
    }).take(5).collect();
    if modules.is_empty() {
        return None;
    }
    let title_raw = s_cap(doc.get("title"), 140);
    let title = if title_raw.is_empty() { "A technology course".to_string() } else { title_raw };
    Some(json!({ "title": title, "modules": modules }))
}

pub fn coursePress(input: &Value, db: &Connection) -> Result<Value, String> {
    let faculty = str_qq(input.get("faculty")).trim().to_string();
    if !crate::codebase_magazine_observe::FACULTIES.contains(&faculty.as_str()) {
        return Err(format!(
            "{SKILL}: 'faculty' must be one of {:?}, got '{faculty}'",
            crate::codebase_magazine_observe::FACULTIES
        ));
    }
    crate::codebase__helpers_magazine_db::ensureFacultyCourses(db);
    crate::codebase__helpers_web_cache::ensureWebCache(db);

    let topic_raw = str_qq(nz(input.get("topic"))).trim().to_lowercase();
    let topic: Value = if topic_raw.is_empty() { Value::Null } else { json!(topic_raw) };

    // PARITY-DEFERRED boundary #1 (see module doc): un-guarded `await magazineObserve(...)` — the
    // JS has no try/catch here, so a failure propagates via `?`. Everything below is ported in full
    // (mirroring magazine-press.rs's own convention) but is unreachable from a test through
    // `coursePress` itself — each downstream piece is exercised standalone above instead.
    let obs = deferred_magazine_observe(&faculty, input)?;

    let prev = crate::codebase__helpers_magazine_db::latestCourse(db, &json!(faculty.clone()), &topic);
    let force = input.get("force").and_then(Value::as_bool).unwrap_or(false);
    if !prev.is_null() {
        let prev_hash = prev.get("source_hash").cloned().unwrap_or(Value::Null);
        let obs_hash = obs.get("hash").cloned().unwrap_or(Value::Null);
        if prev_hash == obs_hash && !force {
            let course_obj = nz(prev.get("course")).cloned().unwrap_or_else(|| json!({}));
            let mut ordered = Map::new();
            ordered.insert("course_no".into(), prev.get("course_no").cloned().unwrap_or(Value::Null));
            ordered.insert("topic".into(), prev.get("topic").cloned().unwrap_or(Value::Null));
            ordered.insert("pressed_at".into(), prev.get("pressed_at").cloned().unwrap_or(Value::Null));
            ordered.insert("source_hash".into(), prev.get("source_hash").cloned().unwrap_or(Value::Null));
            if let Some(o) = course_obj.as_object() {
                for (k, v) in o.iter() {
                    ordered.insert(k.clone(), v.clone());
                }
            }
            return Ok(json!({ "ok": false, "reason": "nothing-new", "course": Value::Object(ordered) }));
        }
    }

    let ctl = json!({ "refresh": false, "stats": { "cached_hits": 0, "fetched": 0 } });
    let docs = if !topic_raw.is_empty() {
        gatherTopicDocs(db, &obs, &topic_raw, &ctl)
    } else {
        gatherDocumentation(db, &obs, &ctl)
    };
    let docs_empty = docs.as_array().map(|a| a.is_empty()).unwrap_or(true);
    if docs_empty {
        let error = if !topic_raw.is_empty() {
            format!("no live documentation reachable for \"{topic_raw}\"")
        } else {
            "no live documentation reachable for this stack — is the ledger empty?".to_string()
        };
        return Ok(json!({ "ok": false, "reason": "press-failed", "error": error }));
    }

    ensure_prompt(db)?;
    let mag = crate::codebase_magazine_press::MAGS().get(faculty.as_str()).cloned().unwrap_or(Value::Null);
    let scope = if !topic_raw.is_empty() {
        format!("ONE topic — \"{topic_raw}\". Write chapters on it alone.")
    } else {
        "the whole stack — one module per technology.".to_string()
    };
    let documentation_json = take_chars(&serde_json::to_string(&docs).unwrap_or_else(|_| "[]".into()), 11000);

    let ai_input = json!({
        "prompt_name": PROMPT,
        "input_kv": [
            { "key": "faculty_title", "value": mag.get("title").cloned().unwrap_or(Value::Null) },
            { "key": "motto", "value": mag.get("motto").cloned().unwrap_or(Value::Null) },
            { "key": "scope", "value": scope },
            { "key": "documentation", "value": documentation_json },
        ],
        "tier": "mid", "temperature": "warm", "phase_flow": "single_only", "provider_fn": Value::Null,
    });

    // ── boundary #3: the course-design AI call — JS catches this one and returns a status object.
    let res = match crate::ai_ai_call::aiCall(&ai_input, db) {
        Ok(r) => r,
        Err(e) => {
            let msg: String = e.chars().take(300).collect();
            return Ok(json!({ "ok": false, "reason": "press-failed", "error": msg }));
        }
    };

    let designed = match parse_course(pick(&res, "course_json").as_deref()) {
        Some(d) => d,
        None => return Ok(json!({ "ok": false, "reason": "press-failed", "error": "the designer returned no valid modules" })),
    };
    let warm = if !topic_raw.is_empty() { warmupTopicModule(&obs, &topic_raw) } else { warmupModule(&docs) };
    let mut modules_combined: Vec<Value> = warm.as_array().cloned().unwrap_or_default();
    modules_combined.extend(designed.get("modules").and_then(Value::as_array).cloned().unwrap_or_default());
    modules_combined.truncate(6);
    let course = json!({ "title": designed.get("title").cloned().unwrap_or(Value::Null), "topic": topic.clone(), "modules": modules_combined });

    let row = crate::codebase__helpers_magazine_db::appendCourse(
        db,
        &json!({
            "faculty": &faculty,
            "topic": topic.clone(),
            "source_hash": obs.get("hash").cloned().unwrap_or(Value::Null),
            "course": &course,
            "model_id": nz(res.get("model_id")).cloned().unwrap_or(Value::Null),
        }),
    );

    let mut out_course = Map::new();
    out_course.insert("course_no".into(), row.get("course_no").cloned().unwrap_or(Value::Null));
    out_course.insert("topic".into(), topic.clone());
    out_course.insert("pressed_at".into(), row.get("pressed_at").cloned().unwrap_or(Value::Null));
    out_course.insert("source_hash".into(), row.get("source_hash").cloned().unwrap_or(Value::Null));
    if let Some(o) = course.as_object() {
        for (k, v) in o.iter() {
            out_course.insert(k.clone(), v.clone());
        }
    }

    Ok(json!({
        "ok": true,
        "course": Value::Object(out_course),
        "ai": crate::codebase__helpers_magazine_db::aiMeta(db, &res),
        "model_id": nz(res.get("model_id")).cloned().unwrap_or(Value::Null),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS knowledge; ATTACH ':memory:' AS brain;").unwrap();
        c
    }

    // ── validGame ────────────────────────────────────────────────────────────
    #[test]
    fn valid_game_quiz_accepts_and_rejects() {
        let ok = json!({ "kind": "quiz", "q": "What?", "options": ["a", "b", "c"], "answer": 1 });
        assert_eq!(validGame(&ok), Some(json!({ "kind": "quiz", "q": "What?", "options": ["a", "b", "c"], "answer": 1 })));
        assert_eq!(validGame(&json!({ "kind": "quiz", "q": "", "options": ["a", "b"], "answer": 0 })), None); // empty q
        assert_eq!(validGame(&json!({ "kind": "quiz", "q": "Q", "options": ["a"], "answer": 0 })), None); // <2 options
        assert_eq!(validGame(&json!({ "kind": "quiz", "q": "Q", "options": ["a", "b"], "answer": 5 })), None); // out of range
        assert_eq!(validGame(&json!({ "kind": "quiz", "q": "Q", "options": ["a", "b"], "answer": 1.5 })), None); // non-integer
    }

    #[test]
    fn valid_game_trivia_order_match() {
        assert_eq!(
            validGame(&json!({ "kind": "trivia", "statement": "X is Y", "truth": true })),
            Some(json!({ "kind": "trivia", "statement": "X is Y", "truth": true }))
        );
        assert_eq!(validGame(&json!({ "kind": "trivia", "statement": "X", "truth": "yes" })), None); // not a bool

        assert_eq!(
            validGame(&json!({ "kind": "order", "prompt": "Order it", "items": ["a", "b", "c"] })),
            Some(json!({ "kind": "order", "prompt": "Order it", "items": ["a", "b", "c"] }))
        );
        assert_eq!(validGame(&json!({ "kind": "order", "prompt": "P", "items": ["a", "b"] })), None); // <3 items

        assert_eq!(
            validGame(&json!({ "kind": "match", "prompt": "Match", "pairs": [{ "a": "x", "b": "y" }, { "a": "p", "b": "q" }] })),
            Some(json!({ "kind": "match", "prompt": "Match", "pairs": [{ "a": "x", "b": "y" }, { "a": "p", "b": "q" }] }))
        );
        assert_eq!(validGame(&json!({ "kind": "match", "prompt": "M", "pairs": [{ "a": "x", "b": "y" }] })), None); // <2 pairs
    }

    #[test]
    fn valid_game_unknown_kind_or_non_object_is_none() {
        assert_eq!(validGame(&json!("not an object")), None);
        assert_eq!(validGame(&json!({ "kind": "essay" })), None);
        assert_eq!(validGame(&json!(null)), None);
    }

    // ── warmupModule ─────────────────────────────────────────────────────────
    #[test]
    fn warmup_module_needs_at_least_two_homed_docs() {
        let docs = json!([{ "tech": "react", "home": "https://react.dev/x" }]);
        assert_eq!(warmupModule(&docs), json!([]));
    }

    #[test]
    fn warmup_module_builds_match_game_with_hostnames() {
        let docs = json!([
            { "tech": "react", "home": "https://react.dev/docs" },
            { "tech": "vite", "home": "https://www.vitejs.dev/guide" },
            { "tech": "no-home-tech" },
        ]);
        let m = warmupModule(&docs);
        let modules = m.as_array().unwrap();
        assert_eq!(modules.len(), 1);
        let games = modules[0]["games"].as_array().unwrap();
        assert_eq!(games[0]["kind"], json!("match"));
        assert_eq!(games[0]["pairs"], json!([
            { "a": "react", "b": "react.dev" },
            { "a": "vite", "b": "vitejs.dev" }, // www. stripped
        ]));
        assert!(modules[0]["lesson_md"].as_str().unwrap().contains("react, vite, no-home-tech"));
        assert_eq!(modules[0]["minted"], json!("substrate"));
    }

    // ── warmupTopicModule ────────────────────────────────────────────────────
    #[test]
    fn warmup_topic_module_needs_three_distinct_dated_items() {
        let obs = json!({ "departments": { "press": { "items": [
            { "source": "vite", "date": "2026-01-01", "title": "a" },
            { "source": "vite", "date": "2026-01-02", "title": "b" },
        ] } } });
        assert_eq!(warmupTopicModule(&obs, "vite"), json!([]));
    }

    #[test]
    fn warmup_topic_module_orders_newest_first_and_dedupes_by_date() {
        let obs = json!({ "departments": {
            "press": { "items": [
                { "source": "vite", "date": "2026-01-01", "title": "release one" },
                { "source": "vite", "date": "2026-01-03", "title": "release three" },
                { "source": "vite", "date": "2026-01-03", "title": "dup date" }, // same date, deduped
                { "source": "other", "date": "2026-01-04", "title": "wrong source" },
            ] },
            "wire": { "items": [
                { "source": "vite", "date": "2026-01-02", "title": "wire two" },
            ] },
        } });
        let m = warmupTopicModule(&obs, "VITE");
        let modules = m.as_array().unwrap();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0]["tech"], json!("vite"));
        let items = modules[0]["games"][0]["items"].as_array().unwrap();
        assert_eq!(items, &vec![json!("release three"), json!("wire two"), json!("release one")]);
    }

    // ── gatherDocumentation ──────────────────────────────────────────────────
    #[test]
    fn gather_documentation_product_faculty_uses_rivals_only() {
        let c = fresh();
        let obs = json!({
            "faculty": "product",
            "departments": {
                "desk": { "items": [{ "label": "Notes", "body": "x".repeat(150) }] },
                "rivals": { "rivals": [
                    { "name": "Acme", "url": "https://acme.example", "ok": true, "title": "T", "description": "D", "claims": ["fast"] },
                    { "name": "Bad", "url": "https://bad.example", "ok": false },
                ] },
            },
        });
        let docs = gatherDocumentation(&c, &obs, &json!({}));
        let arr = docs.as_array().unwrap();
        assert_eq!(arr.len(), 2); // desk doc + one ok rival
        assert_eq!(arr[0]["tech"], json!("desk: Notes"));
        assert_eq!(arr[1]["tech"], json!("Acme"));
        assert_eq!(arr[1]["extract"], json!("T · D · fast"));
    }

    #[test]
    fn gather_documentation_desk_filters_short_bodies() {
        let c = fresh();
        let obs = json!({
            "faculty": "arch",
            "departments": { "desk": { "items": [
                { "label": "Short", "body": "too short" },
                { "label": "Long", "body": "x".repeat(200) },
            ] } },
        });
        let docs = gatherDocumentation(&c, &obs, &json!({}));
        let arr = docs.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["tech"], json!("desk: Long"));
    }

    #[test]
    fn gather_documentation_arch_deferred_fetch_degrades_to_no_home_recent_posts_still_included() {
        let c = fresh();
        let obs = json!({
            "faculty": "arch",
            "departments": {
                "press": { "blogs": [{ "tech": "react" }], "items": [
                    { "source": "react", "title": "React 19", "content": "release notes" },
                ] },
                "ledger": { "deps": [{ "name": "lodash", "dev": false }, { "name": "jest", "dev": true }] },
            },
        });
        let docs = gatherDocumentation(&c, &obs, &json!({}));
        let arr = docs.as_array().unwrap();
        // react (blog, has recent_posts -> included even though home/extract empty via deferred fetch)
        // lodash (dep, no recent_posts, no extract -> excluded); jest is dev -> never considered
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["tech"], json!("react"));
        assert_eq!(arr[0]["home"], Value::Null);
        assert_eq!(arr[0]["extract"], json!(""));
        assert_eq!(arr[0]["recent_posts"], json!([{ "title": "React 19", "excerpt": "release notes" }]));
    }

    #[test]
    fn gather_documentation_caps_at_max_techs() {
        let c = fresh();
        let deps: Vec<Value> = (0..10).map(|i| json!({ "name": format!("pkg{i}"), "dev": false })).collect();
        let press_items: Vec<Value> = (0..10).map(|i| json!({ "source": format!("pkg{i}"), "title": "t", "content": "c" })).collect();
        let obs = json!({ "faculty": "arch", "departments": { "ledger": { "deps": deps }, "press": { "items": press_items } } });
        let docs = gatherDocumentation(&c, &obs, &json!({}));
        assert_eq!(docs.as_array().unwrap().len(), MAX_TECHS);
    }

    // ── gatherTopicDocs ──────────────────────────────────────────────────────
    #[test]
    fn gather_topic_docs_desk_match_plus_recent_posts_survive_deferred_fetch() {
        let c = fresh();
        let obs = json!({ "departments": {
            "desk": { "items": [
                { "label": "Vite Notes", "body": "all about vite build tooling" },
                { "label": "Unrelated", "body": "nothing here" },
            ] },
            "ledger": { "deps": [{ "name": "vite" }] },
            "press": { "items": [
                { "source": "vite", "title": "Vite 6", "content": "notes" },
                { "source": "other", "title": "x", "content": "y" },
            ] },
        } });
        let docs = gatherTopicDocs(&c, &obs, "vite", &json!({}));
        let arr = docs.as_array().unwrap();
        // desk match (label+body contains 'vite') + the entry (recent_posts non-empty)
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["tech"], json!("desk: Vite Notes"));
        assert_eq!(arr[1]["tech"], json!("vite"));
        assert_eq!(arr[1]["home"], Value::Null); // deferred fetch -> no registry doc
        assert_eq!(arr[1]["extract"], json!(""));
        assert_eq!(arr[1]["readme"], json!(""));
        assert_eq!(arr[1]["recent_posts"], json!([{ "title": "Vite 6", "excerpt": "notes" }]));
    }

    #[test]
    fn gather_topic_docs_no_desk_match_no_posts_no_readme_yields_empty() {
        let c = fresh();
        let obs = json!({ "departments": {} });
        let docs = gatherTopicDocs(&c, &obs, "nothing-tracked", &json!({}));
        assert_eq!(docs, json!([]));
    }

    // ── parse_course (private, exercised through coursePress-adjacent parsing behavior) ──────────
    #[test]
    fn parse_course_drops_malformed_modules_keeps_valid_ones() {
        let text = r#"```json
        {"title":"A Course","modules":[
          {"tech":"vite","lesson_md":"XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX","games":[{"kind":"trivia","statement":"S","truth":true}]},
          {"tech":"","lesson_md":"too short module dropped","games":[{"kind":"trivia","statement":"S","truth":true}]},
          {"tech":"no-games","lesson_md":"YYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYYY","games":[]}
        ]}
        ```"#;
        let course = parse_course(Some(text)).unwrap();
        assert_eq!(course["title"], json!("A Course"));
        let modules = course["modules"].as_array().unwrap();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0]["tech"], json!("vite"));
    }

    #[test]
    fn parse_course_no_modules_returns_none() {
        assert_eq!(parse_course(Some("not json at all")), None);
        assert_eq!(parse_course(Some(r#"{"title":"T","modules":[]}"#)), None);
        assert_eq!(parse_course(None), None);
    }

    #[test]
    fn parse_course_missing_title_defaults() {
        let text = r#"{"modules":[{"tech":"x","lesson_md":"ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ","games":[{"kind":"trivia","statement":"S","truth":true}]}]}"#;
        let course = parse_course(Some(text)).unwrap();
        assert_eq!(course["title"], json!("A technology course"));
    }

    #[test]
    fn parse_course_recovers_truncated_json() {
        // truncated mid-way through a second module — should walk back and re-seal
        let text = format!(
            r#"{{"title":"T","modules":[{{"tech":"vite","lesson_md":"{}","games":[{{"kind":"trivia","statement":"S","truth":true}}]}},{{"tech":"trunc"#,
            "A".repeat(90)
        );
        let course = parse_course(Some(&text)).unwrap();
        let modules = course["modules"].as_array().unwrap();
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0]["tech"], json!("vite"));
    }

    // ── ensure_prompt ────────────────────────────────────────────────────────
    #[test]
    fn ensure_prompt_is_idempotent_on_unchanged_template() {
        let c = fresh();
        c.execute_batch(
            "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        ensure_prompt(&c).unwrap();
        let n1: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = ?", [PROMPT], |r| r.get(0)).unwrap();
        assert_eq!(n1, 1);
        ensure_prompt(&c).unwrap();
        let n2: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = ?", [PROMPT], |r| r.get(0)).unwrap();
        assert_eq!(n2, 1);
    }

    // ── coursePress top level ────────────────────────────────────────────────
    #[test]
    fn invalid_faculty_is_rejected_before_any_db_or_deferred_call() {
        let c = fresh();
        let err = coursePress(&json!({ "faculty": "nope" }), &c).unwrap_err();
        assert!(err.contains("faculty"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM knowledge.sqlite_master WHERE name='faculty_courses'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn valid_faculty_ensures_tables_then_propagates_the_deferred_observe_error() {
        let c = fresh();
        let err = coursePress(&json!({ "faculty": "arch" }), &c).unwrap_err();
        // the boundary here is cachedFetch's deferred stub, NOT aiCall — coursePress never
        // reaches an AI call, so this assertion must stay on the PARITY-DEFERRED wording.
        assert!(err.contains("PARITY-DEFERRED"), "unexpected error: {err}");
        assert!(err.contains("cachedFetch"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM knowledge.sqlite_master WHERE name='faculty_courses'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn pick_finds_top_level_field_ignoring_nested() {
        let res = json!({ "output_kv": [
            { "field_name": "other", "value_text": "x", "parent_output_id": null },
            { "field_name": "course_json", "value_text": "  J  ", "parent_output_id": null },
            { "field_name": "course_json", "value_text": "nested", "parent_output_id": 5 },
        ] });
        assert_eq!(pick(&res, "course_json"), Some("  J  ".to_string()));
        assert_eq!(pick(&res, "missing"), None);
    }
}
