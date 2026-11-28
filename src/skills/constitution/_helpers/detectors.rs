//! detectors.rs — twin of skills/constitution/_helpers/detectors.mjs, the deterministic detector
//! engine behind every scoring law (regex / absent / path / import / metric / graph / ast / stmt).
//! NAMES ARE CONTRACT: `langsOfPath`, `globToRegex`, `pathMatchesAny`, `lawApplies`,
//! `exceptionCovers`, `resolveLayer`, `runDetector` (kept exactly as the JS source names them).
//!
//! ## `globToRegex` — verified NOT a bug
//! The JS source's `DBL` constant looks like an empty string (`const DBL = '';`) when viewed
//! through a tool that renders control characters invisibly — it is actually the single control
//! character `\x01` (SOH), used as a placeholder sentinel so `**` can be swapped for `.*` AFTER
//! the later `*` → `[^/]*` substitution runs, without that substitution corrupting the freshly
//! inserted `.*` (which itself contains a `*`). Confirmed against a live Node run of the real
//! `globToRegex` this pass (`src/*.js` → matches within one segment only; `src/**/*.js` → matches
//! across segments; `?` → exactly one char; `.` escaped literal) — this twin ports the SANE,
//! intended algorithm.
//!
//! ## `globToRegex` is TRANSITIVE (returns a `Regex`, not `serde_json::Value`)
//! Real `pub fn` + native `#[cfg(test)]` coverage, no JSON fixture — same convention as
//! ts-query.rs's `getTree`/`queryMatches`. `pathMatchesAny` (and everything built on it —
//! `lawApplies`, `exceptionCovers`, `resolveLayer`) calls it directly, in-process, exactly like
//! the JS module does; those callers ARE fixture-tested (`sig: "no-db"`), so `globToRegex`'s real
//! behavior is exercised transitively through them.
//!
//! ## `runDetector` is TRANSITIVE (the `ast` tier needs a live native `Tree`/`Language`)
//! Every OTHER detector tier (`regex`/`import`/`absent`/`path`/`metric`/`graph`/`stmt`/`layer`) is
//! plain JSON logic over `ctx`. Only `detector: 'ast'` needs `ctx.tsTree`/`ctx.tsLanguage` — real
//! parsed-tree handles (see constitution-judge.mjs: `ctx.tsTree = parsed.tree; ctx.tsLanguage =
//! parsed.language;`, where `parsed = await getTree(grammar, source)`). A `serde_json::Value`
//! cannot carry a `tree_sitter::Tree`, so this twin's `runDetector` takes two EXTRA parameters
//! (`ts_tree: Option<&Tree>`, `ts_language: Option<&Language>`) beyond the JS `(law, ctx)` pair —
//! the same category of "necessary Rust-only signature difference" ts-query.rs's `queryMatches`
//! documents (there, an extra `source: &str`; here, the tree/language the JSON `ctx` cannot hold).
//! Because it takes non-JSON params it cannot ride the `(law: &Value, ctx: &Value)` no-db-helper
//! fixture lane at all — class `transitive`, no fixture, real logic + `#[cfg(test)]` coverage for
//! every tier including `ast` (built on `crate::constitution__helpers_ts_query::queryMatches`,
//! which is itself real native tree-sitter, not a stub).
//!
//! ## Residual limitation inherited from ts-query.rs: regex-crate has no lookaround/backrefs
//! `regex`/`absent` detector `cfg.pattern` values are DYNAMIC (read from a law row's config_json
//! at runtime, not fixed JS source text) — gotcha #7's usual "translate lookaround by hand" advice
//! doesn't apply to data we don't control. Checked seed-laws.mjs's SEED_LAWS this pass: none of its
//! `detector:'regex'|'absent'` patterns use lookaround/lookbehind/backreferences, so this twin is
//! faithful for all data that exists today. A FUTURE law added via `system.law.add` with such a
//! pattern would compile in JS (`new RegExp` succeeds) but fail to compile here (`Regex::new`
//! returns `Err`) — `runDetector` treats a failed compile as "not judgeable" (returns `Value::Null`,
//! mirroring the JS `catch { return null; }`), which is a safe degrade (no false hit), not a panic.
use regex::Regex;
use serde_json::{json, Value};
use tree_sitter::{Language, Tree};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => js_string(x),
    }
}
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}

pub fn langsOfPath(path: &Value) -> Value {
    let p = js_string(path).to_lowercase();
    // /\.tsx?$/
    if p.ends_with(".ts") || p.ends_with(".tsx") {
        return json!(["ts", "js"]);
    }
    // /\.[cm]?jsx?$/ — 3 optional prefixes (none/c/m) × 2 suffixes (js/jsx) = 6 literal endings
    if p.ends_with(".js") || p.ends_with(".jsx") || p.ends_with(".cjs") || p.ends_with(".cjsx") || p.ends_with(".mjs") || p.ends_with(".mjsx") {
        return json!(["js"]);
    }
    if p.ends_with(".py") {
        return json!(["py"]);
    }
    json!([])
}

const GLOB_SENTINEL: char = '\u{1}';

/// globToRegex(glob) — TRANSITIVE (returns a compiled Regex, not JSON). See module doc for the
/// "not a bug" note on the sentinel-based `**` → `.*` substitution.
pub fn globToRegex(glob: &Value) -> Result<Regex, String> {
    let g = js_string(glob);
    let mut esc = String::with_capacity(g.len());
    for c in g.chars() {
        if matches!(c, '.' | '+' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\') {
            esc.push('\\');
        }
        esc.push(c);
    }
    let esc = esc.replace("**", &GLOB_SENTINEL.to_string());
    let esc = esc.replace('*', "[^/]*");
    let esc = esc.split(GLOB_SENTINEL).collect::<Vec<_>>().join(".*");
    let esc = esc.replace('?', ".");
    let pattern = format!("(^|/){esc}$|^{esc}$");
    Regex::new(&pattern).map_err(|e| e.to_string())
}

pub fn pathMatchesAny(path: &Value, globs: &Value) -> Value {
    let arr = match globs.as_array() {
        Some(a) if !a.is_empty() => a,
        _ => return json!(false),
    };
    let path_str = js_string(path);
    for g in arr {
        if let Ok(re) = globToRegex(g) {
            if re.is_match(&path_str) {
                return json!(true);
            }
        }
    }
    json!(false)
}

/// lawApplies(law, ctx) — ctx: { path, frameworks }.
pub fn lawApplies(law: &Value, ctx: &Value) -> Value {
    let a: Value = law
        .get("applies_json")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .filter(|v| !v.is_null())
        .unwrap_or(json!({}));
    let path = ctx.get("path").cloned().unwrap_or(Value::Null);
    let frameworks_ctx = ctx.get("frameworks").cloned().unwrap_or(json!([]));

    let langs = langsOfPath(&path);
    let langs_arr = langs.as_array().cloned().unwrap_or_default();
    if let Some(a_langs) = a.get("languages").and_then(|v| v.as_array()).filter(|v| !v.is_empty()) {
        if !a_langs.iter().any(|l| langs_arr.contains(l)) {
            return json!(false);
        }
    }
    if let Some(a_fw) = a.get("frameworks").and_then(|v| v.as_array()).filter(|v| !v.is_empty()) {
        let fw = frameworks_ctx.as_array().cloned().unwrap_or_default();
        if !a_fw.iter().any(|f| fw.contains(f)) {
            return json!(false);
        }
    }
    if let Some(a_paths) = a.get("paths").and_then(|v| v.as_array()).filter(|v| !v.is_empty()) {
        if !truthy(Some(&pathMatchesAny(&path, &json!(a_paths)))) {
            return json!(false);
        }
    }
    if let Some(a_excl) = a.get("paths_exclude").and_then(|v| v.as_array()) {
        if !a_excl.is_empty() && truthy(Some(&pathMatchesAny(&path, &json!(a_excl)))) {
            return json!(false);
        }
    }
    json!(true)
}

pub fn exceptionCovers(exceptions: &Value, law_key: &Value, path: &Value) -> Value {
    let empty: Vec<Value> = Vec::new();
    let arr = exceptions.as_array().unwrap_or(&empty); // exceptions ?? []
    let key_str = js_string(law_key);
    for e in arr {
        let matches_key = e.get("law_key").map(|v| js_string(v) == key_str).unwrap_or(false);
        if matches_key {
            let glob = e.get("path_glob").cloned().unwrap_or(Value::Null);
            if truthy(Some(&pathMatchesAny(path, &json!([glob])))) {
                return json!(true);
            }
        }
    }
    json!(false)
}

pub fn resolveLayer(path: &Value, layers: &Value) -> Value {
    let empty: Vec<Value> = Vec::new();
    let arr = layers.as_array().unwrap_or(&empty); // layers ?? []
    for layer in arr {
        let paths = layer.get("paths").cloned().unwrap_or(Value::Null);
        if truthy(Some(&pathMatchesAny(path, &paths))) {
            return layer.get("name").cloned().unwrap_or(Value::Null);
        }
    }
    Value::Null
}

/// The most hits ONE law reports on ONE file by default - what the judge SCORES and stores as rows. A context carrying `"uncapped": true` lifts it
/// (the check, and the judge's own total count): a cap is a limit on what is listed, never on what is counted.
pub const HITS_CAP: usize = 5;

/// The cap this context asks for: `HITS_CAP`, or no limit when `ctx.uncapped` is true. Pure.
pub fn cap_of(ctx: &Value) -> usize {
    if ctx.get("uncapped").and_then(Value::as_bool).unwrap_or(false) { usize::MAX } else { HITS_CAP }
}

fn line_of_index(source: &str, idx: usize) -> i64 {
    let mut line: i64 = 1;
    for (i, b) in source.bytes().enumerate() {
        if i >= idx {
            break;
        }
        if b == b'\n' {
            line += 1;
        }
    }
    line
}

fn symbol_at(spans: Option<&Value>, line: Option<i64>) -> Value {
    let (spans, line) = match (spans, line) {
        (Some(s), Some(l)) => (s, l),
        _ => return Value::Null,
    };
    let Some(arr) = spans.as_array() else { return Value::Null };
    for s in arr {
        let start = s.get("start").and_then(|v| v.as_i64());
        let end = s.get("end").and_then(|v| v.as_i64());
        if let (Some(start), Some(end)) = (start, end) {
            if line >= start && line <= end {
                return s.get("name").cloned().unwrap_or(Value::Null); // hit?.name ?? null
            }
        }
    }
    Value::Null
}

/// `new RegExp(pattern, flags)` for the subset of flags this codebase's law configs use
/// (i/m/s — 'g' is handled by the caller's iteration strategy, not the compiled pattern).
fn build_regex(pattern: &str, flags: &str) -> Option<Regex> {
    let mut prefix = String::new();
    for f in ['i', 'm', 's'] {
        if flags.contains(f) {
            prefix.push(f);
        }
    }
    let src = if prefix.is_empty() { pattern.to_string() } else { format!("(?{prefix}){pattern}") };
    Regex::new(&src).ok()
}

fn push_hit(out: &mut Vec<Value>, cap: usize, spans: Option<&Value>, line: Option<i64>, evidence: &str) {
    if out.len() < cap {
        let line_v = match line {
            Some(l) => json!(l),
            None => Value::Null,
        };
        out.push(json!({ "line": line_v, "evidence": evidence, "symbol": symbol_at(spans, line) }));
    }
}

fn fact_str(f: &Value, key: &str) -> String {
    f.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}
fn fact_i64(f: &Value, key: &str) -> i64 {
    f.get(key).and_then(|v| v.as_i64()).unwrap_or(0)
}

fn pair_check(out: &mut Vec<Value>, cap: usize, spans: Option<&Value>, pairs: Option<&Value>, add_k: &str, rem_k: &str, what: &str) {
    let a = pairs.and_then(|p| p.get(add_k)).and_then(|e| e.get("n")).and_then(|v| v.as_i64()).unwrap_or(0);
    let r = pairs.and_then(|p| p.get(rem_k)).and_then(|e| e.get("n")).and_then(|v| v.as_i64()).unwrap_or(0);
    if a > r {
        let line = pairs.and_then(|p| p.get(add_k)).and_then(|e| e.get("line")).and_then(|v| v.as_i64());
        let evidence = format!("{a}× {what} vs {r}× cleanup — the difference leaks");
        push_hit(out, cap, spans, line, &evidence);
    }
}

/// runDetector(law, ctx, ts_tree, ts_language) — see module doc for why this takes two extra
/// native params beyond the JS `(law, ctx)` pair, and why the whole unit is class `transitive`.
pub fn runDetector(law: &Value, ctx: &Value, ts_tree: Option<&Tree>, ts_language: Option<&Language>) -> Value {
    let cfg_str = match law.get("config_json") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => "{}".to_string(),
        Some(other) => js_string(other),
    };
    let cfg: Value = serde_json::from_str(&cfg_str).ok().filter(|v: &Value| !v.is_null()).unwrap_or(json!({}));
    let detector = law.get("detector").and_then(|v| v.as_str()).unwrap_or("");
    let spans = ctx.get("spans");
    let cap = cap_of(ctx);
    let mut out: Vec<Value> = Vec::new();

    if detector == "regex" || detector == "import" {
        let source = match ctx.get("source").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return Value::Null,
        };
        let pattern = cfg.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
        let flags = cfg.get("flags").and_then(|v| v.as_str()).unwrap_or("g");
        let re = match build_regex(pattern, flags) {
            Some(r) => r,
            None => return Value::Null,
        };
        let import_line_re = Regex::new(r"^\s*(import|export|const .*=\s*require)\b").unwrap();
        let lines: Vec<&str> = source.split('\n').collect();
        for m in re.find_iter(source) {
            if out.len() >= cap {
                break;
            }
            let line = line_of_index(source, m.start());
            if detector == "import" {
                let line_text = lines.get((line - 1) as usize).copied().unwrap_or("");
                if !import_line_re.is_match(line_text) {
                    continue;
                }
            }
            let evidence: String = m.as_str().chars().take(160).collect::<String>().replace('\n', "⏎");
            out.push(json!({ "line": line, "evidence": evidence, "symbol": symbol_at(spans, Some(line)) }));
        }
        return json!(out);
    }

    if detector == "absent" {
        let source = match ctx.get("source").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return Value::Null,
        };
        let pattern = cfg.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
        let flags = cfg.get("flags").and_then(|v| v.as_str()).unwrap_or("m");
        let re = match build_regex(pattern, flags) {
            Some(r) => r,
            None => return Value::Null,
        };
        if !re.is_match(source) {
            let pattern_disp: String = js_string(cfg.get("pattern").unwrap_or(&Value::Null)).chars().take(120).collect();
            out.push(json!({ "line": Value::Null, "evidence": format!("required pattern missing: {pattern_disp}"), "symbol": Value::Null }));
        }
        return json!(out);
    }

    if detector == "path" {
        let pattern = cfg.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
        let re = match build_regex(pattern, "") {
            Some(r) => r,
            None => return Value::Null,
        };
        let path_str = tmpl(ctx.get("path"));
        if re.is_match(&path_str) {
            out.push(json!({ "line": Value::Null, "evidence": format!("path violates convention: {path_str}"), "symbol": Value::Null }));
        }
        return json!(out);
    }

    if detector == "metric" {
        let metric_key = tmpl(cfg.get("metric"));
        let v = match ctx.get("metrics").and_then(|m| m.get(&metric_key)) {
            Some(x) if !x.is_null() => x,
            _ => return Value::Null,
        };
        if let Some(Value::Number(_)) = cfg.get("max") {
            let max = js_number(cfg.get("max"));
            if js_number(Some(v)) > max {
                let evidence = format!("{}={} > max {}", metric_key, tmpl(Some(v)), tmpl(cfg.get("max")));
                out.push(json!({ "line": Value::Null, "evidence": evidence, "symbol": Value::Null }));
            }
        }
        return json!(out);
    }

    if detector == "graph" {
        let smells = match ctx.get("smells").and_then(|v| v.as_array()) {
            Some(a) => a,
            None => return Value::Null,
        };
        let smell = cfg.get("smell").cloned().unwrap_or(Value::Null);
        if smells.iter().any(|s| *s == smell) {
            out.push(json!({ "line": Value::Null, "evidence": format!("atlas smell: {}", tmpl(cfg.get("smell"))), "symbol": Value::Null }));
        }
        return json!(out);
    }

    if detector == "ast" {
        let (tree, language) = match (ts_tree, ts_language) {
            (Some(t), Some(l)) => (t, l),
            _ => return Value::Null,
        };
        let grammar = match ctx.get("grammar").and_then(|v| v.as_str()) {
            Some(g) if !g.is_empty() => g,
            _ => return Value::Null,
        };
        let entry = match cfg.get("queries").and_then(|q| q.get(grammar)) {
            Some(e) if !e.is_null() => e,
            _ => return Value::Null,
        };
        let (query_str, mut filter): (&str, Option<&str>) = if entry.is_string() {
            (entry.as_str().unwrap_or(""), cfg.get("filter").and_then(|v| v.as_str()))
        } else {
            (entry.get("q").and_then(|v| v.as_str()).unwrap_or(""), entry.get("filter").and_then(|v| v.as_str()))
        };
        // THE PROJECT'S OWN RULE WINS. A law that names an ESLint rule (`config_json.project_rule
        // .eslint`) defers to what the codebase configured for itself (ctx.project_rules, read
        // from its lint config): `off` retires the law here, `always` drops the null carve-out,
        // `smart`/`allow-null` keep it. Absent → the seed's own filter.
        if let Some(rule) = cfg.get("project_rule").and_then(|p| p.get("eslint")).and_then(Value::as_str) {
            if let Some(setting) = ctx.get("project_rules").and_then(|r| r.get(rule)).filter(|v| !v.is_null()) {
                match setting.get("level").and_then(Value::as_str) {
                    Some("off") => return Value::Null,
                    _ => match setting.get("option").and_then(Value::as_str) {
                        Some("always") | Some("never-null") => filter = None,
                        Some("smart") | Some("allow-null") => filter = Some("allow-null"),
                        _ => {}
                    },
                }
            }
        }
        let source = ctx.get("source").and_then(|v| v.as_str()).unwrap_or("");
        let hits = match crate::constitution__helpers_ts_query::queryMatchesUpTo(grammar, language, tree, source, query_str, filter, cap) {
            Some(h) => h,
            None => return Value::Null,
        };
        for h in hits.into_iter().take(cap) {
            let line = h.get("line").and_then(|v| v.as_i64());
            let evidence: String = js_string(h.get("text").unwrap_or(&Value::Null)).chars().take(160).collect();
            out.push(json!({ "line": line, "evidence": evidence, "symbol": symbol_at(spans, line) }));
        }
        return json!(out);
    }

    if detector == "stmt" {
        if !truthy(ctx.get("stmt")) {
            return Value::Null;
        }
        let st = ctx.get("stmt").unwrap();
        let empty: Vec<Value> = Vec::new();
        let facts = st.get("facts").and_then(|v| v.as_array()).unwrap_or(&empty);
        let pairs = st.get("pairs");
        let check = cfg.get("check").and_then(|v| v.as_str()).unwrap_or("");

        if check == "await-in-loop" {
            for f in facts {
                if fact_str(f, "kind") == "await" && fact_i64(f, "loopDepth") >= 1 {
                    push_hit(&mut out, cap, spans, Some(fact_i64(f, "line")), "await inside a loop — serialized I/O");
                }
            }
            return json!(out);
        }
        if check == "nested-loop" {
            for f in facts {
                if fact_str(f, "kind") == "loop" && fact_i64(f, "loopDepth") >= 1 {
                    let evidence = format!("a loop inside a loop (depth {}) — O(n²) shape", fact_i64(f, "loopDepth") + 1);
                    push_hit(&mut out, cap, spans, Some(fact_i64(f, "line")), &evidence);
                }
            }
            return json!(out);
        }
        if check == "alloc-in-loop" {
            for f in facts {
                if fact_str(f, "kind") == "alloc" && fact_i64(f, "loopDepth") >= 1 {
                    push_hit(&mut out, cap, spans, Some(fact_i64(f, "line")), "an object/array/closure allocated every iteration");
                }
            }
            return json!(out);
        }
        if check == "concat-in-loop" {
            for f in facts {
                if fact_str(f, "kind") == "concat" && fact_i64(f, "loopDepth") >= 1 {
                    push_hit(&mut out, cap, spans, Some(fact_i64(f, "line")), "+= inside a loop — quadratic string building");
                }
            }
            return json!(out);
        }
        if check == "max-nesting" {
            let max_v = cfg.get("max").filter(|v| v.is_number()).cloned().unwrap_or(json!(4));
            let max = js_number(Some(&max_v));
            let max_nesting = st.get("maxNesting");
            let depth = max_nesting.and_then(|m| m.get("depth")).and_then(|v| v.as_f64()).unwrap_or(0.0);
            if depth > max {
                let line = max_nesting.and_then(|m| m.get("line")).and_then(|v| v.as_i64());
                let depth_disp = tmpl(max_nesting.and_then(|m| m.get("depth")));
                let evidence = format!("nesting {depth_disp} deep (max {})", tmpl(Some(&max_v)));
                push_hit(&mut out, cap, spans, line, &evidence);
            }
            return json!(out);
        }
        if check == "bool-chain" {
            let max_v = cfg.get("max").filter(|v| v.is_number()).cloned().unwrap_or(json!(3));
            let max = js_number(Some(&max_v));
            let mut per: std::collections::BTreeMap<i64, i64> = std::collections::BTreeMap::new();
            for f in facts {
                if fact_str(f, "kind") == "bool" {
                    *per.entry(fact_i64(f, "line")).or_insert(0) += 1;
                }
            }
            for (line, n) in per {
                if (n as f64) >= max {
                    let evidence = format!("{n}× &&/|| chained in one expression — name the predicate");
                    push_hit(&mut out, cap, spans, Some(line), &evidence);
                }
            }
            return json!(out);
        }
        if check == "test-assertions" {
            let tb = pairs.and_then(|p| p.get("t_block")).and_then(|e| e.get("n")).and_then(|v| v.as_i64()).unwrap_or(0);
            let ta = pairs.and_then(|p| p.get("t_assert")).and_then(|e| e.get("n")).and_then(|v| v.as_i64()).unwrap_or(0);
            if tb > 0 && ta < tb {
                let line = pairs.and_then(|p| p.get("t_block")).and_then(|e| e.get("line")).and_then(|v| v.as_i64());
                let evidence = format!("{tb}× it/test vs {ta}× expect/assert — some tests can never fail");
                push_hit(&mut out, cap, spans, line, &evidence);
            }
            return json!(out);
        }
        if check == "conditional-assert" {
            for f in facts {
                if fact_str(f, "kind") == "assert" && (fact_i64(f, "depth") - fact_i64(f, "loopDepth")) >= 1 {
                    push_hit(&mut out, cap, spans, Some(fact_i64(f, "line")), "an assertion inside a branch — when the guard is false, the test silently proves nothing");
                }
            }
            return json!(out);
        }
        if check == "listener-cleanup" {
            pair_check(&mut out, cap, spans, pairs, "listener_add", "listener_remove", "addEventListener");
            return json!(out);
        }
        if check == "interval-clear" {
            pair_check(&mut out, cap, spans, pairs, "interval_set", "interval_clear", "setInterval");
            return json!(out);
        }
        if check == "subscription-cleanup" {
            pair_check(&mut out, cap, spans, pairs, "sub", "unsub", "subscribe");
            return json!(out);
        }
        return Value::Null;
    }

    // 'layer' is decided over the import GRAPH, not one file's source — never fires per-file.
    if detector == "layer" {
        return Value::Null;
    }

    Value::Null
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── langsOfPath ──────────────────────────────────────────────────────────────────────────
    #[test]
    fn langs_of_path_maps_extensions() {
        assert_eq!(langsOfPath(&json!("a/b.ts")), json!(["ts", "js"]));
        assert_eq!(langsOfPath(&json!("a/B.TSX")), json!(["ts", "js"]));
        assert_eq!(langsOfPath(&json!("a/b.mjs")), json!(["js"]));
        assert_eq!(langsOfPath(&json!("a/b.py")), json!(["py"]));
        assert_eq!(langsOfPath(&json!("a/b.rs")), json!([]));
    }

    // ── globToRegex / pathMatchesAny ────────────────────────────────────────────────────────
    #[test]
    fn glob_to_regex_literal_matches_exactly() {
        let re = globToRegex(&json!("foo")).unwrap();
        assert!(re.is_match("foo"));
        assert!(!re.is_match("f00"));
        assert!(!re.is_match("faoo"));
    }

    #[test]
    fn glob_to_regex_single_star_stays_within_one_segment() {
        let re = globToRegex(&json!("src/*.js")).unwrap();
        assert!(re.is_match("src/a.js"));
        assert!(!re.is_match("src/a/b.js"));
    }

    #[test]
    fn glob_to_regex_double_star_crosses_segments() {
        let re = globToRegex(&json!("src/**/*.js")).unwrap();
        assert!(re.is_match("src/a/b/c.js"));
        assert!(!re.is_match("src/c.js"));
        let re2 = globToRegex(&json!("test/**")).unwrap();
        assert!(re2.is_match("test/a/b"));
    }

    #[test]
    fn glob_to_regex_question_mark_and_dot_escape() {
        let re = globToRegex(&json!("a?c")).unwrap();
        assert!(re.is_match("abc"));
        assert!(!re.is_match("axyzc"));
        let re2 = globToRegex(&json!("a.b.c")).unwrap();
        assert!(!re2.is_match("aXbXc"));
    }

    #[test]
    fn path_matches_any_basics() {
        assert_eq!(pathMatchesAny(&json!("a/b.js"), &json!([])), json!(false));
        assert_eq!(pathMatchesAny(&json!("a/b.js"), &Value::Null), json!(false));
        assert_eq!(pathMatchesAny(&json!("a/foo"), &json!(["foo"])), json!(true));
        assert_eq!(pathMatchesAny(&json!("src/a.js"), &json!(["src/*.js"])), json!(true));
        assert_eq!(pathMatchesAny(&json!("src/a.ts"), &json!(["src/*.js"])), json!(false));
    }

    // ── lawApplies ───────────────────────────────────────────────────────────────────────────
    #[test]
    fn law_applies_language_framework_path_gating() {
        let law = |applies: Value| json!({ "applies_json": applies.to_string() });
        let ctx = |path: &str, frameworks: Value| json!({ "path": path, "frameworks": frameworks });
        assert_eq!(lawApplies(&law(json!({})), &ctx("a/b.ts", json!([]))), json!(true));
        assert_eq!(lawApplies(&law(json!({"languages":["py"]})), &ctx("a/b.ts", json!([]))), json!(false));
        assert_eq!(lawApplies(&law(json!({"languages":["ts"]})), &ctx("a/b.ts", json!([]))), json!(true));
        assert_eq!(lawApplies(&law(json!({"frameworks":["react"]})), &ctx("a/b.ts", json!(["vue"]))), json!(false));
        assert_eq!(lawApplies(&law(json!({"frameworks":["react"]})), &ctx("a/b.ts", json!(["react"]))), json!(true));
        assert_eq!(lawApplies(&law(json!({"paths":["foo"]})), &ctx("a/foo", json!([]))), json!(true));
        assert_eq!(lawApplies(&law(json!({"paths":["zzz"]})), &ctx("a/foo", json!([]))), json!(false));
        assert_eq!(lawApplies(&law(json!({"paths_exclude":["foo"]})), &ctx("a/foo", json!([]))), json!(false));
        assert_eq!(lawApplies(&json!({"applies_json":"not json"}), &ctx("a/b.ts", json!([]))), json!(true));
        assert_eq!(lawApplies(&json!({}), &ctx("a/b.ts", json!([]))), json!(true));
    }

    // ── exceptionCovers ──────────────────────────────────────────────────────────────────────
    #[test]
    fn exception_covers_matches_law_key_and_glob() {
        let exceptions = json!([{ "law_key": "L1", "path_glob": "foo" }]);
        assert_eq!(exceptionCovers(&exceptions, &json!("L1"), &json!("a/foo")), json!(true));
        assert_eq!(exceptionCovers(&exceptions, &json!("L2"), &json!("a/foo")), json!(false));
        assert_eq!(exceptionCovers(&Value::Null, &json!("L1"), &json!("a/foo")), json!(false));
    }

    // ── resolveLayer ─────────────────────────────────────────────────────────────────────────
    #[test]
    fn resolve_layer_first_match_wins() {
        let layers = json!([{ "name": "test", "paths": ["spec"] }, { "name": "core", "paths": ["foo"] }]);
        assert_eq!(resolveLayer(&json!("a/foo"), &layers), json!("core"));
        assert_eq!(resolveLayer(&json!("a/spec"), &layers), json!("test"));
        assert_eq!(resolveLayer(&json!("a/zzz"), &layers), Value::Null);
        assert_eq!(resolveLayer(&json!("a/zzz"), &Value::Null), Value::Null);
    }

    // ── runDetector: regex / import / absent / path / metric / graph / layer / none ────────────
    #[test]
    fn run_detector_regex_hits_capped_and_line_mapped() {
        let law = json!({ "detector": "regex", "config_json": json!({"pattern":"console\\.log\\s*\\("}).to_string() });
        let source = "a\nconsole.log(1)\nb\nconsole.log(2)\n";
        let out = runDetector(&law, &json!({ "source": source, "spans": [] }), None, None);
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["line"], json!(2));
        assert_eq!(arr[1]["line"], json!(4));
        assert_eq!(arr[0]["symbol"], Value::Null);
    }

    #[test]
    fn a_cap_limits_what_is_listed_and_an_uncapped_context_counts_everything() {
        let law = json!({ "detector": "regex", "config_json": json!({"pattern":"console\\.log\\s*\\("}).to_string() });
        let source: String = (1..=9).map(|i| format!("console.log({i})\n")).collect();
        let capped = runDetector(&law, &json!({ "source": source, "spans": [] }), None, None);
        assert_eq!(capped.as_array().unwrap().len(), HITS_CAP, "the default still lists five");
        let all = runDetector(&law, &json!({ "source": source, "spans": [], "uncapped": true }), None, None);
        assert_eq!(all.as_array().unwrap().len(), 9, "uncapped counts every hit");
        assert_eq!(all[0], capped[0], "the first five are the same hits in the same order");
        assert_eq!(all[4], capped[4]);
        assert_eq!(cap_of(&json!({ "uncapped": false })), HITS_CAP);
        assert_eq!(cap_of(&json!({})), HITS_CAP);
    }

    #[test]
    fn an_ast_law_is_counted_past_the_cap_too() {
        // `no-loose-equality` is a tree-sitter query, which had a cap of its own (6) under the detector's 5: the A/B's copied legacy pattern lived past both
        let seed = crate::constitution__helpers_seed_laws::SEED_LAWS.as_array().unwrap().iter().find(|l| l["key"] == "no-loose-equality").cloned().expect("the seed law");
        let law = json!({ "detector": seed["detector"], "config_json": seed["config_json"].to_string() });
        let source: String = (1..=9).map(|i| format!("if (a{i} == \"x\") {{ f(); }}\n")).collect();
        let (tree, language) = crate::constitution__helpers_ts_query::getTree("typescript", &source).expect("a typescript tree");
        let ctx = json!({ "source": source, "spans": [], "grammar": "typescript" });
        let capped = runDetector(&law, &ctx, Some(&tree), Some(&language));
        assert_eq!(capped.as_array().unwrap().len(), HITS_CAP, "listed: five");
        let uncapped_ctx = json!({ "source": source, "spans": [], "grammar": "typescript", "uncapped": true });
        let all = runDetector(&law, &uncapped_ctx, Some(&tree), Some(&language));
        assert_eq!(all.as_array().unwrap().len(), 9, "counted: nine");
        assert_eq!(all[0], capped[0]);
    }

    #[test]
    fn run_detector_regex_null_on_missing_source_or_bad_pattern() {
        let law1 = json!({ "detector": "regex", "config_json": json!({"pattern":"x"}).to_string() });
        assert_eq!(runDetector(&law1, &json!({}), None, None), Value::Null);
        let law2 = json!({ "detector": "regex", "config_json": json!({"pattern":"("}).to_string() });
        assert_eq!(runDetector(&law2, &json!({ "source": "x" }), None, None), Value::Null);
    }

    #[test]
    fn run_detector_import_filters_non_import_lines() {
        let law = json!({ "detector": "import", "config_json": json!({"pattern":"lodash"}).to_string() });
        let source = "import x from 'lodash';\nconst y = lodash.map([]);\nexport const z = require('lodash');\n";
        let out = runDetector(&law, &json!({ "source": source, "spans": [] }), None, None);
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["line"], json!(1));
        assert_eq!(arr[1]["line"], json!(3));
    }

    #[test]
    fn run_detector_absent_fires_only_when_missing() {
        let law = json!({ "detector": "absent", "config_json": json!({"pattern":"use strict"}).to_string() });
        assert_eq!(runDetector(&law, &json!({ "source": "use strict;\nx();" }), None, None), json!([]));
        let out = runDetector(&law, &json!({ "source": "x();" }), None, None);
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["line"], Value::Null);
        assert!(arr[0]["evidence"].as_str().unwrap().contains("required pattern missing"));
    }

    #[test]
    fn run_detector_path_convention() {
        let law = json!({ "detector": "path", "config_json": json!({"pattern":"^src/legacy/"}).to_string() });
        assert_eq!(runDetector(&law, &json!({ "path": "src/legacy/foo.js" }), None, None).as_array().unwrap().len(), 1);
        assert_eq!(runDetector(&law, &json!({ "path": "src/modern/foo.js" }), None, None).as_array().unwrap().len(), 0);
    }

    #[test]
    fn run_detector_metric_null_when_unmeasured_hit_when_over_max() {
        let law = json!({ "detector": "metric", "config_json": json!({"metric":"loc","max":200}).to_string() });
        assert_eq!(runDetector(&law, &json!({ "metrics": {} }), None, None), Value::Null);
        assert_eq!(runDetector(&law, &json!({ "metrics": {"loc": 150} }), None, None), json!([]));
        let out = runDetector(&law, &json!({ "metrics": {"loc": 250} }), None, None);
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["evidence"], json!("loc=250 > max 200"));
    }

    #[test]
    fn run_detector_graph_membership() {
        let law = json!({ "detector": "graph", "config_json": json!({"smell":"god-file"}).to_string() });
        assert_eq!(runDetector(&law, &json!({}), None, None), Value::Null);
        assert_eq!(runDetector(&law, &json!({ "smells": ["cycle"] }), None, None), json!([]));
        assert_eq!(runDetector(&law, &json!({ "smells": ["god-file"] }), None, None).as_array().unwrap().len(), 1);
    }

    #[test]
    fn run_detector_layer_always_null() {
        assert_eq!(runDetector(&json!({ "detector": "layer" }), &json!({ "smells": [] }), None, None), Value::Null);
    }

    #[test]
    fn run_detector_unknown_detector_is_null() {
        assert_eq!(runDetector(&json!({ "detector": "none" }), &json!({}), None, None), Value::Null);
        assert_eq!(runDetector(&json!({ "detector": "bogus" }), &json!({}), None, None), Value::Null);
    }

    // ── runDetector: ast (real native tree-sitter, no fixture) ──────────────────────────────────
    #[test]
    fn run_detector_ast_null_without_real_tree_or_grammar() {
        let law = json!({ "detector": "ast", "config_json": json!({"queries":{"tsx":"(debugger_statement) @hit"}}).to_string() });
        assert_eq!(runDetector(&law, &json!({ "grammar": "tsx" }), None, None), Value::Null);
    }

    #[test]
    fn run_detector_ast_empty_catch_via_real_tsx_parse() {
        let law = json!({ "detector": "ast", "config_json": json!({"queries":{"tsx":{"q":"(catch_clause body: (statement_block) @hit)","filter":"empty"}}}).to_string() });
        let src = "function f() { try { g(); } catch (e) {} }";
        let (tree, lang) = crate::constitution__helpers_ts_query::getTree("tsx", src).expect("tsx loads");
        let out = runDetector(&law, &json!({ "grammar": "tsx", "source": src, "spans": [] }), Some(&tree), Some(&lang));
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["evidence"], json!("{}"));
    }

    #[test]
    fn run_detector_ast_defers_to_the_projects_eqeqeq() {
        // The seeded no-loose-equality: allow-null by default; the project's own eqeqeq decides.
        let law = json!({ "detector": "ast", "config_json": crate::constitution__helpers_seed_laws::SEED_LAWS.as_array().unwrap().iter().find(|l| l["key"] == json!("no-loose-equality")).unwrap()["config_json"].to_string() });
        let src = "if (a == null) {} if (c == d) {}";
        let (tree, lang) = crate::constitution__helpers_ts_query::getTree("tsx", src).unwrap();
        let run = |rules: Value| runDetector(&law, &json!({ "grammar": "tsx", "source": src, "spans": [], "project_rules": rules }), Some(&tree), Some(&lang));
        // no project rule → the seed's allow-null: `c == d` only
        assert_eq!(run(json!({})).as_array().unwrap().len(), 1);
        // React's config
        assert_eq!(run(json!({ "eqeqeq": { "level": "error", "option": "allow-null" } })).as_array().unwrap().len(), 1);
        // a project that wants the strict form everywhere
        assert_eq!(run(json!({ "eqeqeq": { "level": "error", "option": "always" } })).as_array().unwrap().len(), 2);
        // a project that turned the rule off retires the law
        assert_eq!(run(json!({ "eqeqeq": { "level": "off", "option": null } })), Value::Null);
    }

    #[test]
    fn run_detector_ast_bare_query_string_uses_cfg_filter() {
        // 'entry' is a bare query STRING here (not {q,filter}) — filter comes from cfg.filter.
        let law = json!({ "detector": "ast", "config_json": json!({"queries":{"tsx":"(catch_clause body: (statement_block) @hit)"},"filter":"empty"}).to_string() });
        let src = "function f() { try { g(); } catch (e) { log(e); } }";
        let (tree, lang) = crate::constitution__helpers_ts_query::getTree("tsx", src).unwrap();
        let out = runDetector(&law, &json!({ "grammar": "tsx", "source": src, "spans": [] }), Some(&tree), Some(&lang));
        assert_eq!(out.as_array().unwrap().len(), 0); // non-empty catch excluded by the filter
    }

    #[test]
    fn run_detector_ast_no_query_for_grammar_is_null() {
        let law = json!({ "detector": "ast", "config_json": json!({"queries":{"python":"(pass_statement) @hit"}}).to_string() });
        let src = "let x = 1;";
        let (tree, lang) = crate::constitution__helpers_ts_query::getTree("tsx", src).unwrap();
        assert_eq!(runDetector(&law, &json!({ "grammar": "tsx", "source": src, "spans": [] }), Some(&tree), Some(&lang)), Value::Null);
    }

    // ── runDetector: stmt tiers ──────────────────────────────────────────────────────────────
    #[test]
    fn run_detector_stmt_await_in_loop() {
        let law = json!({ "detector": "stmt", "config_json": json!({"check":"await-in-loop"}).to_string() });
        let stmt = json!({ "facts": [
            { "kind": "await", "line": 3, "depth": 1, "loopDepth": 0 },
            { "kind": "await", "line": 5, "depth": 2, "loopDepth": 1 },
        ], "pairs": {}, "maxNesting": { "depth": 0, "line": null } });
        let out = runDetector(&law, &json!({ "stmt": stmt, "spans": [] }), None, None);
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["line"], json!(5));
    }

    #[test]
    fn run_detector_stmt_nested_loop_evidence() {
        let law = json!({ "detector": "stmt", "config_json": json!({"check":"nested-loop"}).to_string() });
        let stmt = json!({ "facts": [{ "kind": "loop", "line": 4, "depth": 1, "loopDepth": 1 }], "pairs": {}, "maxNesting": { "depth": 0, "line": null } });
        let out = runDetector(&law, &json!({ "stmt": stmt, "spans": [] }), None, None);
        assert_eq!(out[0]["evidence"], json!("a loop inside a loop (depth 2) — O(n²) shape"));
    }

    #[test]
    fn run_detector_stmt_max_nesting_default_and_override() {
        let law = json!({ "detector": "stmt", "config_json": json!({"check":"max-nesting"}).to_string() });
        let stmt = json!({ "facts": [], "pairs": {}, "maxNesting": { "depth": 5, "line": 9 } });
        let out = runDetector(&law, &json!({ "stmt": stmt.clone(), "spans": [] }), None, None);
        assert_eq!(out[0]["evidence"], json!("nesting 5 deep (max 4)"));
        let law2 = json!({ "detector": "stmt", "config_json": json!({"check":"max-nesting","max":6}).to_string() });
        assert_eq!(runDetector(&law2, &json!({ "stmt": stmt, "spans": [] }), None, None), json!([]));
    }

    #[test]
    fn run_detector_stmt_bool_chain_sorted_by_line() {
        let law = json!({ "detector": "stmt", "config_json": json!({"check":"bool-chain"}).to_string() });
        let stmt = json!({ "facts": [
            { "kind": "bool", "line": 10 }, { "kind": "bool", "line": 10 }, { "kind": "bool", "line": 10 },
            { "kind": "bool", "line": 3 }, { "kind": "bool", "line": 3 },
        ], "pairs": {}, "maxNesting": { "depth": 0, "line": null } });
        let out = runDetector(&law, &json!({ "stmt": stmt, "spans": [] }), None, None);
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["line"], json!(10));
    }

    #[test]
    fn run_detector_stmt_test_assertions() {
        let law = json!({ "detector": "stmt", "config_json": json!({"check":"test-assertions"}).to_string() });
        let stmt = json!({ "facts": [], "pairs": { "t_block": {"n":3,"line":1}, "t_assert": {"n":1,"line":2} }, "maxNesting": {"depth":0,"line":null} });
        let out = runDetector(&law, &json!({ "stmt": stmt, "spans": [] }), None, None);
        assert_eq!(out[0]["evidence"], json!("3× it/test vs 1× expect/assert — some tests can never fail"));
        let stmt2 = json!({ "facts": [], "pairs": { "t_block": {"n":2,"line":1}, "t_assert": {"n":2,"line":2} }, "maxNesting": {"depth":0,"line":null} });
        assert_eq!(runDetector(&law, &json!({ "stmt": stmt2, "spans": [] }), None, None), json!([]));
    }

    #[test]
    fn run_detector_stmt_conditional_assert() {
        let law = json!({ "detector": "stmt", "config_json": json!({"check":"conditional-assert"}).to_string() });
        let stmt = json!({ "facts": [
            { "kind": "assert", "line": 3, "depth": 2, "loopDepth": 2 },
            { "kind": "assert", "line": 6, "depth": 2, "loopDepth": 1 },
        ], "pairs": {}, "maxNesting": {"depth":0,"line":null} });
        let out = runDetector(&law, &json!({ "stmt": stmt, "spans": [] }), None, None);
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["line"], json!(6));
    }

    #[test]
    fn run_detector_stmt_leak_family() {
        let mk = |check: &str| json!({ "detector": "stmt", "config_json": json!({"check": check}).to_string() });
        let stmt = json!({ "facts": [], "pairs": {
            "listener_add": {"n":2,"line":1}, "listener_remove": {"n":1,"line":2},
            "interval_set": {"n":1,"line":3}, "interval_clear": {"n":1,"line":4},
            "sub": {"n":3,"line":5}, "unsub": {"n":0,"line":6},
        }, "maxNesting": {"depth":0,"line":null} });
        let listener_out = runDetector(&mk("listener-cleanup"), &json!({ "stmt": stmt.clone(), "spans": [] }), None, None);
        assert_eq!(listener_out[0]["evidence"], json!("2× addEventListener vs 1× cleanup — the difference leaks"));
        assert_eq!(runDetector(&mk("interval-clear"), &json!({ "stmt": stmt.clone(), "spans": [] }), None, None), json!([]));
        let sub_out = runDetector(&mk("subscription-cleanup"), &json!({ "stmt": stmt, "spans": [] }), None, None);
        assert_eq!(sub_out[0]["evidence"], json!("3× subscribe vs 0× cleanup — the difference leaks"));
    }

    #[test]
    fn run_detector_stmt_unrecognized_check_and_falsy_stmt() {
        let law = json!({ "detector": "stmt", "config_json": json!({"check":"not-a-real-check"}).to_string() });
        let stmt = json!({ "facts": [], "pairs": {}, "maxNesting": {"depth":0,"line":null} });
        assert_eq!(runDetector(&law, &json!({ "stmt": stmt, "spans": [] }), None, None), Value::Null);
        let law2 = json!({ "detector": "stmt", "config_json": json!({"check":"await-in-loop"}).to_string() });
        assert_eq!(runDetector(&law2, &json!({}), None, None), Value::Null);
    }

    #[test]
    fn run_detector_symbol_lookup_via_spans() {
        let law = json!({ "detector": "regex", "config_json": json!({"pattern":"TODO"}).to_string() });
        let source = "a\nb\n// TODO fix\n";
        let spans = json!([{ "start": 2, "end": 5, "name": "myFn" }]);
        let out = runDetector(&law, &json!({ "source": source, "spans": spans }), None, None);
        assert_eq!(out[0]["symbol"], json!("myFn"));
    }
}
