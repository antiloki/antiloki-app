//! narration-compile.rs — twin of skills/agents/in/narration-compile.mjs (db WRITE+READ lane,
//! cross-db via ATTACH knowledge). NAMES ARE CONTRACT: `narrationCompile` (default) +
//! `splitSentences` + `compileMarkdown` (named, no-db helpers). THE NARRATION COMPILER:
//! deterministic $0 markdown → nir-1 units (say/pause/breath/event), printed prosody rules
//! (heading pause 700/300, paragraph breath+pause 250, list item pause 150, code/table
//! omitted-with-receipt). `compile` upserts by (source_kind, source_key, ir_hash) — identical
//! source recompiles to the identical hash and the INSERT is a no-op (ON CONFLICT DO NOTHING).
// QUALITY: hand-rolled lookahead/lookbehind (regex crate has neither) for sentence splitting —
// mask-then-split-then-restore, same 3-pass abbreviation/initial/decimal protection as the JS.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;
use regex::Regex;

// = JS String(v ?? '')
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// = JS String(v)
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
// = JS `${v}`
fn tmpl(v: Option<&Value>) -> String { match v { None => "undefined".into(), Some(Value::Null) => "null".into(), Some(x) => str_qq(Some(x)) } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
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

// splitSentences: mask dots that never end a sentence (decimals, initials, abbreviations) with a
// control placeholder, split on [.!?]-then-whitespace-then-[quote?]Capital/digit, restore, trim.
const MASK: char = '\u{1}';
fn is_word_char(c: char) -> bool { c.is_ascii_alphanumeric() || c == '_' }

// (\d)\.(\d) → $1<MASK>$2 — no lookaround needed, portable via regex crate.
fn mask_digits(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(\d)\.(\d)").unwrap());
    let repl = format!("${{1}}{MASK}${{2}}");
    re.replace_all(s, repl.as_str()).into_owned()
}

// \b([A-Z])\.(?=\s+[A-Z]) — hand-rolled: regex crate has no lookahead. A capital letter at a word
// boundary followed by a dot, itself followed by whitespace then another capital, masks its dot
// (protects runs of initials like "J. R. R. Tolkien").
fn mask_initials(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < n {
        let c = chars[i];
        if c.is_ascii_uppercase() && (i == 0 || !is_word_char(chars[i - 1])) && i + 1 < n && chars[i + 1] == '.' {
            let ws_start = i + 2;
            let mut j = ws_start;
            while j < n && chars[j].is_whitespace() { j += 1; }
            if j > ws_start && j < n && chars[j].is_ascii_uppercase() {
                out.push(c);
                out.push(MASK);
                i += 2;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

// \b(e\.g|i\.e|etc|vs|cf|Fig|Dr|Mr|Mrs|Ms|St|No|Vol|Ch|Sec|approx)\. — no lookaround here (\b is a
// plain anchor), directly portable. Every literal '.' in the matched text (e.g. both dots of
// "e.g.") is masked, case-insensitive, alternation order preserved (regex crate is leftmost-first).
fn mask_abbrev(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?i)\b(e\.g|i\.e|etc|vs|cf|Fig|Dr|Mr|Mrs|Ms|St|No|Vol|Ch|Sec|approx)\.").unwrap());
    re.replace_all(s, |caps: &regex::Captures| caps[0].chars().map(|c| if c == '.' { MASK } else { c }).collect::<String>()).into_owned()
}

// /(?<=[.!?])\s+(?=["\u{201C}(]?[A-Z0-9])/ — hand-rolled: split at each maximal whitespace run
// whose immediately preceding char is [.!?] and whose immediately following (post optional open
// quote/paren) char is a capital letter or digit. Whitespace chars can't satisfy the lookahead, so
// the greedy \s+ never needs to backtrack short — the maximal run boundary is always the candidate.
fn split_on_boundary(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < n {
        if chars[i].is_whitespace() {
            let ws_start = i;
            let mut j = i;
            while j < n && chars[j].is_whitespace() { j += 1; }
            let prev_ok = ws_start > 0 && matches!(chars[ws_start - 1], '.' | '!' | '?');
            let mut k = j;
            if k < n && (chars[k] == '"' || chars[k] == '\u{201C}' || chars[k] == '(') { k += 1; }
            let next_ok = k < n && (chars[k].is_ascii_uppercase() || chars[k].is_ascii_digit());
            if prev_ok && next_ok {
                parts.push(chars[start..ws_start].iter().collect::<String>());
                start = j;
            }
            i = j;
            continue;
        }
        i += 1;
    }
    parts.push(chars[start..].iter().collect::<String>());
    parts
}

fn split_sentences_core(text: &str) -> Vec<String> {
    let masked = mask_abbrev(&mask_initials(&mask_digits(text)));
    split_on_boundary(&masked)
        .into_iter()
        .map(|x| x.chars().map(|c| if c == MASK { '.' } else { c }).collect::<String>())
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

pub fn splitSentences(text: &Value) -> Value {
    json!(split_sentences_core(&str_qq(Some(text))))
}

// stripInline: markdown link/emphasis/blockquote/whitespace normalization — no lookaround, all 4
// regexes port directly.
fn strip_inline(s: &str) -> String {
    static LINK: OnceLock<Regex> = OnceLock::new();
    let link = LINK.get_or_init(|| Regex::new(r"\[([^\]]*)\]\([^)]*\)").unwrap());
    let s1 = link.replace_all(s, "$1");
    static EMPH: OnceLock<Regex> = OnceLock::new();
    let emph = EMPH.get_or_init(|| Regex::new(r"[*_`~]{1,3}([^*_`~]+)[*_`~]{1,3}").unwrap());
    let s2 = emph.replace_all(&s1, "$1");
    static QUOTE: OnceLock<Regex> = OnceLock::new();
    let quote = QUOTE.get_or_init(|| Regex::new(r"(?m)^>\s?").unwrap());
    let s3 = quote.replace_all(&s2, "");
    static WS: OnceLock<Regex> = OnceLock::new();
    let ws = WS.get_or_init(|| Regex::new(r"\s+").unwrap());
    let s4 = ws.replace_all(&s3, " ");
    s4.trim().to_string()
}

fn words_count(s: &str) -> i64 { s.split_whitespace().count() as i64 }

struct Block { kind: &'static str, text: String }

fn flush_block(cur: &mut Vec<String>, blocks: &mut Vec<Block>) {
    let text = cur.join("\n").trim().to_string();
    if !text.is_empty() {
        static LIST_TEST: OnceLock<Regex> = OnceLock::new();
        let re = LIST_TEST.get_or_init(|| Regex::new(r"^\s*(?:[-*•]|\d+[.)])\s").unwrap());
        let first_line = cur.first().map(|s| s.as_str()).unwrap_or("");
        blocks.push(Block { kind: if re.is_match(first_line) { "list" } else { "para" }, text });
    }
    cur.clear();
}

fn compile_markdown_core(md: &str, src: &Value) -> Value {
    let normalized = md.replace("\r\n", "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();

    static FENCE: OnceLock<Regex> = OnceLock::new();
    let fence_re = FENCE.get_or_init(|| Regex::new(r"^\s*```").unwrap());
    static TABLE: OnceLock<Regex> = OnceLock::new();
    let table_re = TABLE.get_or_init(|| Regex::new(r"^\s*\|").unwrap());
    static HEADING_TEST: OnceLock<Regex> = OnceLock::new();
    let heading_test_re = HEADING_TEST.get_or_init(|| Regex::new(r"^#{1,6}\s").unwrap());
    static HEADING_STRIP: OnceLock<Regex> = OnceLock::new();
    let heading_strip_re = HEADING_STRIP.get_or_init(|| Regex::new(r"^#+\s*").unwrap());

    // pass 1: fence/table removal with event receipts, block assembly
    let mut blocks: Vec<Block> = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    let mut in_fence = false;
    for ln in &lines {
        if fence_re.is_match(ln) {
            if !in_fence {
                flush_block(&mut cur, &mut blocks);
                in_fence = true;
            } else {
                in_fence = false;
                blocks.push(Block { kind: "code", text: String::new() });
            }
            continue;
        }
        if in_fence { continue; }
        if table_re.is_match(ln) {
            flush_block(&mut cur, &mut blocks);
            if blocks.last().map(|b| b.kind) != Some("table") {
                blocks.push(Block { kind: "table", text: String::new() });
            }
            continue;
        }
        if heading_test_re.is_match(ln) {
            flush_block(&mut cur, &mut blocks);
            let text = heading_strip_re.replace(ln, "").trim().to_string();
            blocks.push(Block { kind: "heading", text });
            continue;
        }
        if ln.trim().is_empty() { flush_block(&mut cur, &mut blocks); continue; }
        cur.push((*ln).to_string());
    }
    flush_block(&mut cur, &mut blocks);

    // pass 2: blocks → units under the printed rules
    let mut units: Vec<Value> = Vec::new();
    let mut sec = String::new();
    let mut p: i64 = 0;
    static LIST_ITEM_STRIP: OnceLock<Regex> = OnceLock::new();
    let list_item_strip_re = LIST_ITEM_STRIP.get_or_init(|| Regex::new(r"^\s*(?:[-*•]|\d+[.)])\s*").unwrap());

    for b in &blocks {
        match b.kind {
            "code" => { units.push(json!({ "t": "event", "kind": "code_omitted" })); continue; }
            "table" => { units.push(json!({ "t": "event", "kind": "table_omitted" })); continue; }
            "heading" => {
                sec = b.text.clone();
                p = 0;
                if !units.is_empty() { units.push(json!({ "t": "pause", "ms": 700 })); }
                let text = strip_inline(&b.text);
                if !text.is_empty() {
                    units.push(json!({ "t": "say", "text": text, "receipt": { "src": src, "sec": sec, "p": 0, "s": 0 } }));
                    units.push(json!({ "t": "pause", "ms": 300 }));
                }
                continue;
            }
            _ => {}
        }
        p += 1;
        if b.kind == "list" {
            let items: Vec<String> = b.text.split('\n')
                .map(|l| strip_inline(&list_item_strip_re.replace(l, "")))
                .filter(|s| !s.is_empty())
                .collect();
            for (s, item) in items.iter().enumerate() {
                units.push(json!({ "t": "say", "text": item, "receipt": { "src": src, "sec": sec, "p": p, "s": s as i64 } }));
                units.push(json!({ "t": "pause", "ms": 150 }));
            }
            continue;
        }
        let sentences = split_sentences_core(&strip_inline(&b.text));
        for (s, text) in sentences.iter().enumerate() {
            units.push(json!({ "t": "say", "text": text, "receipt": { "src": src, "sec": sec, "p": p, "s": s as i64 } }));
        }
        units.push(json!({ "t": "breath", "kind": "short" }));
        units.push(json!({ "t": "pause", "ms": 250 }));
    }
    // trim a trailing paragraph-break pair
    while units.last().map_or(false, |u| u.get("t").and_then(|v| v.as_str()) != Some("say")) {
        units.pop();
    }

    let words: i64 = units.iter().map(|u| {
        if u.get("t").and_then(|v| v.as_str()) == Some("say") { words_count(u.get("text").and_then(|v| v.as_str()).unwrap_or("")) } else { 0 }
    }).sum();
    let base = ((words as f64) * 60000.0 / 155.0).round() as i64;
    let extra: i64 = units.iter().map(|u| match u.get("t").and_then(|v| v.as_str()) {
        Some("pause") => u.get("ms").and_then(|v| v.as_i64()).unwrap_or(0),
        Some("breath") => if u.get("kind").and_then(|v| v.as_str()) == Some("long") { 600 } else { 300 },
        _ => 0,
    }).sum();
    json!({ "units": units, "words": words, "duration_est_ms": base + extra })
}

pub fn compileMarkdown(md: &Value, src: &Value) -> Value {
    compile_markdown_core(&str_qq(Some(md)), src)
}

pub fn narrationCompile(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS destructures { db } from context and throws err.contextMissing without it — the harness
    // context always carries db, so that guard cannot fire here.
    let op = match nz(input.get("op")) { Some(v) => js_string(v), None => "compile".into() };

    if op == "get" {
        let narration_id_num = js_number(input.get("narration_id"));
        let param = if narration_id_num.is_finite() { json!(narration_id_num) } else { Value::Null };
        let rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.narrations WHERE id = ?", &[param])?;
        let Some(row) = rows.as_array().and_then(|a| a.first()).cloned() else {
            return Err(format!("narration-compile: narration '{}' not found", tmpl(input.get("narration_id"))));
        };
        let mut obj = row.as_object().cloned().unwrap_or_default();
        let ir_json_str = obj.get("ir_json").and_then(|v| v.as_str()).unwrap_or("null").to_string();
        let ir: Value = serde_json::from_str(&ir_json_str).unwrap_or(Value::Null);
        obj.insert("ir".into(), ir);
        return Ok(json!({ "ok": true, "narration": Value::Object(obj) }));
    }

    if op == "list" {
        let limit_v = nz(input.get("limit")).cloned().unwrap_or_else(|| json!(30));
        let limit = js_number(Some(&limit_v)).min(200.0);
        let rows = crate::db_json::query_rows(
            db,
            "\n      SELECT n.id, n.source_kind, n.source_key, n.title, n.ir_version, n.ir_hash, n.units_count,\n             n.words, n.duration_est_ms, n.created_at,\n             (SELECT COUNT(*) FROM knowledge.narration_renders r WHERE r.narration_id = n.id) AS renders\n      FROM knowledge.narrations n ORDER BY n.id DESC LIMIT ?\n    ",
            &[json!(limit)],
        )?;
        return Ok(json!({ "ok": true, "narrations": rows }));
    }

    if op == "compile" {
        let source_kind = match nz(input.get("source_kind")) { Some(v) => js_string(v), None => "text".into() };
        let mut text: Option<String> = match input.get("text") { None | Some(Value::Null) => None, Some(v) => Some(js_string(v)) };
        let mut source_key: Option<String> = match input.get("source_key") { None | Some(Value::Null) => None, Some(v) => Some(js_string(v)) };
        let mut title: Option<String> = match input.get("title") { None | Some(Value::Null) => None, Some(v) => Some(js_string(v)) };

        if source_kind == "book_module" {
            let book_id_num = js_number(input.get("book_id"));
            let ord_num = js_number(input.get("ord"));
            let book_id_ok = book_id_num.is_finite() && book_id_num.fract() == 0.0;
            let ord_ok = ord_num.is_finite() && ord_num.fract() == 0.0;
            if !book_id_ok || !ord_ok {
                return Err("narration-compile: book_id + ord for book_module is required".into());
            }
            let book_id = book_id_num as i64;
            let ord = ord_num as i64;
            let rows = crate::db_json::query_rows(db, "SELECT ord, title, text FROM knowledge.book_blocks WHERE book_id = ? AND ord = ?", &[json!(book_id), json!(ord)])?;
            let Some(b) = rows.as_array().and_then(|a| a.first()).cloned() else {
                return Err(format!("narration-compile: book module '{ord}' not found"));
            };
            text = b.get("text").map(js_string);
            source_key = Some(format!("book-{book_id}-m{ord}"));
            title = title.or_else(|| b.get("title").map(js_string));
        }

        let text_ok = text.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false);
        if !text_ok {
            return Err("narration-compile: text (papers send their composed article; book_module fetches its block) is required".into());
        }
        if source_key.as_deref().map_or(true, |s| s.is_empty()) {
            return Err("narration-compile: source_key is required".into());
        }
        let text_final = text.unwrap();
        let source_key_final = source_key.unwrap();
        let title_final: String = title.unwrap_or_else(|| source_key_final.clone()).chars().take(200).collect();

        let text_trunc: String = text_final.chars().take(240000).collect();
        let compiled = compile_markdown_core(&text_trunc, &json!(source_key_final));
        let units = compiled.get("units").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        if units.is_empty() {
            return Err("narration-compile: a source with at least one narratable sentence is required".into());
        }
        let words_v = compiled.get("words").cloned().unwrap_or(json!(0));
        let duration_v = compiled.get("duration_est_ms").cloned().unwrap_or(json!(0));

        let ir = json!({
            "v": "nir-1",
            "voice_role": "narrator",
            "source": { "kind": source_kind, "key": source_key_final, "title": title_final },
            "units": units,
        });
        let ir_json = serde_json::to_string(&ir).map_err(|e| e.to_string())?;
        let hex_full = hex::encode(Sha256::digest(ir_json.as_bytes()));
        let ir_hash = hex_full[..32].to_string();

        crate::db_json::run_write(
            db,
            "\n      INSERT INTO knowledge.narrations (source_kind, source_key, title, ir_version, ir_json, ir_hash, units_count, words, duration_est_ms)\n      VALUES (?, ?, ?, 'nir-1', ?, ?, ?, ?, ?)\n      ON CONFLICT(source_kind, source_key, ir_hash) DO NOTHING\n    ",
            &[json!(source_kind), json!(source_key_final), json!(title_final), json!(ir_json), json!(ir_hash), json!(units.len() as i64), json!(words_v), json!(duration_v)],
        )?;
        let sel = crate::db_json::query_rows(db, "SELECT id FROM knowledge.narrations WHERE source_kind = ? AND source_key = ? AND ir_hash = ?", &[json!(source_kind), json!(source_key_final), json!(ir_hash)])?;
        let narration_id = sel.as_array().and_then(|a| a.first()).and_then(|r| r.get("id")).cloned().unwrap_or(Value::Null);

        let mut counts = Map::new();
        for u in &units {
            let t = u.get("t").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let entry = counts.entry(t).or_insert(json!(0));
            let n = entry.as_i64().unwrap_or(0) + 1;
            *entry = json!(n);
        }
        let sample: Vec<Value> = units.iter().take(8).cloned().collect();

        return Ok(json!({
            "ok": true,
            "narration_id": narration_id,
            "ir_version": "nir-1",
            "ir_hash": ir_hash,
            "units_count": units.len() as i64,
            "counts": Value::Object(counts),
            "words": words_v,
            "duration_est_ms": duration_v,
            "sample": sample,
        }));
    }

    Err("narration-compile: op 'compile'|'get'|'list' is required".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS knowledge;
            CREATE TABLE knowledge.narrations (id INTEGER PRIMARY KEY, source_kind TEXT NOT NULL, source_key TEXT NOT NULL, title TEXT NOT NULL, ir_version TEXT NOT NULL DEFAULT 'nir-1', ir_json TEXT NOT NULL, ir_hash TEXT NOT NULL, units_count INTEGER NOT NULL, words INTEGER NOT NULL DEFAULT 0, duration_est_ms INTEGER NOT NULL DEFAULT 0, created_at TEXT, UNIQUE (source_kind, source_key, ir_hash));
            CREATE TABLE knowledge.narration_renders (id INTEGER PRIMARY KEY, narration_id INTEGER NOT NULL, section_ord INTEGER, section_title TEXT, backend TEXT NOT NULL, backend_version TEXT, voice TEXT, waveform_path TEXT, waveform_hash TEXT, duration_ms INTEGER, features_requested_json TEXT NOT NULL DEFAULT '[]', features_honored_json TEXT NOT NULL DEFAULT '[]', degraded_json TEXT NOT NULL DEFAULT '[]', timing_json TEXT, cost_usd REAL NOT NULL DEFAULT 0, created_at TEXT);
            CREATE TABLE knowledge.book_blocks (id INTEGER PRIMARY KEY, book_id INTEGER NOT NULL, ord INTEGER NOT NULL, title TEXT NOT NULL, text TEXT NOT NULL, tokens_est INTEGER NOT NULL DEFAULT 0);",
        ).unwrap();
        c
    }

    #[test]
    fn split_sentences_protects_abbreviations_decimals_initials() {
        let out = splitSentences(&json!("Version 3.14 shipped, e.g. the atlas. Dr. Smith reviewed it. Done!"));
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert!(arr[0].as_str().unwrap().contains("3.14") && arr[0].as_str().unwrap().contains("e.g. the atlas."));
        assert!(arr[1].as_str().unwrap().starts_with("Dr. Smith"));
        let out2 = splitSentences(&json!("J. R. R. Tolkien wrote it. It was long."));
        assert_eq!(out2.as_array().unwrap().len(), 2);
        assert_eq!(splitSentences(&Value::Null), json!([]));
    }

    #[test]
    fn compile_markdown_headings_paragraphs_code_tables() {
        let md = "# The Gate\n\nUnits name consumers. Hoarding is refused.\n\n```js\nrun();\n```\n\n| a | b |\n| - | - |\n\nSecond paragraph here.";
        let r = compileMarkdown(&json!(md), &json!("src-1"));
        let units = r["units"].as_array().unwrap();
        assert!(units.iter().any(|u| u["t"] == json!("say") && u["text"] == json!("The Gate")));
        assert!(units.iter().any(|u| u["t"] == json!("pause") && u["ms"] == json!(300)));
        assert!(units.iter().any(|u| u["t"] == json!("event") && u["kind"] == json!("code_omitted")));
        assert!(units.iter().any(|u| u["t"] == json!("event") && u["kind"] == json!("table_omitted")));
        assert!(units.iter().any(|u| u["t"] == json!("breath")));
        for u in units { if u["t"] == json!("say") { assert_eq!(u["receipt"]["src"], json!("src-1")); } }
    }

    #[test]
    fn compile_markdown_deterministic_and_inline_stripping() {
        let a = compileMarkdown(&json!("## Rule\n\nEvery claim carries its receipt. The plan prices before the swarm spends."), &json!("s"));
        let b = compileMarkdown(&json!("## Rule\n\nEvery claim carries its receipt. The plan prices before the swarm spends."), &json!("s"));
        assert_eq!(a, b);
        assert!(a["duration_est_ms"].as_i64().unwrap() > 0);
        assert_eq!(a["words"], json!(13));

        let stripped = compileMarkdown(&json!("See [the laws](https://x) and **hard rules** in `laws.sqlite`."), &json!("s"));
        let say = stripped["units"].as_array().unwrap().iter().find(|u| u["t"] == json!("say")).unwrap();
        assert_eq!(say["text"], json!("See the laws and hard rules in laws.sqlite."));
    }

    #[test]
    fn narration_compile_text_and_dedup() {
        let c = fresh();
        let r = narrationCompile(&json!({ "op": "compile", "text": "Alpha. Beta.", "source_key": "dup-1" }), &c).unwrap();
        assert_eq!(r["ir_hash"], json!("7225682f82c47e43fafa800649fe6eb9"));
        assert_eq!(r["units_count"], json!(2));
        assert_eq!(r["counts"], json!({ "say": 2 }));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM knowledge.narrations", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
        let first_id = r["narration_id"].as_i64().unwrap();

        // seed a pre-existing row with the SAME hash the next identical compile will produce:
        // ON CONFLICT DO NOTHING must leave it untouched and the SELECT must find that row.
        let r2 = narrationCompile(&json!({ "op": "compile", "text": "Alpha. Beta.", "source_key": "dup-1" }), &c).unwrap();
        assert_eq!(r2["narration_id"].as_i64().unwrap(), first_id);
        let n2: i64 = c.query_row("SELECT COUNT(*) FROM knowledge.narrations", [], |row| row.get(0)).unwrap();
        assert_eq!(n2, 1);
    }

    #[test]
    fn narration_compile_book_module_title_fallback() {
        let c = fresh();
        c.execute_batch("INSERT INTO knowledge.book_blocks (id, book_id, ord, title, text) VALUES (1, 1, 2, 'Chapter Two', '## Heading\n\nSome text here.');").unwrap();
        let r = narrationCompile(&json!({ "op": "compile", "source_kind": "book_module", "book_id": 1, "ord": 2 }), &c).unwrap();
        assert!(r["ok"].as_bool().unwrap());
        let get = narrationCompile(&json!({ "op": "get", "narration_id": r["narration_id"] }), &c).unwrap();
        assert_eq!(get["narration"]["source_key"], json!("book-1-m2"));
        assert_eq!(get["narration"]["title"], json!("Chapter Two"));
        assert!(get["narration"]["ir"]["units"].is_array());
    }

    #[test]
    fn narration_compile_list_op_limit_and_renders() {
        let c = fresh();
        c.execute_batch(
            "INSERT INTO knowledge.narrations (id, source_kind, source_key, title, ir_version, ir_json, ir_hash, units_count, words, duration_est_ms, created_at) VALUES
              (1, 'text', 'a', 'A', 'nir-1', '{}', 'h1', 1, 1, 100, '2025-12-01 10:00:00'),
              (2, 'text', 'b', 'B', 'nir-1', '{}', 'h2', 1, 1, 100, '2025-12-02 10:00:00'),
              (3, 'text', 'c', 'C', 'nir-1', '{}', 'h3', 1, 1, 100, '2025-12-03 10:00:00');
            INSERT INTO knowledge.narration_renders (id, narration_id, backend) VALUES (1, 2, 'kokoro'), (2, 2, 'espeak');",
        ).unwrap();
        let all = narrationCompile(&json!({ "op": "list" }), &c).unwrap();
        let rows = all["narrations"].as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["id"], json!(3)); // newest first
        let b_row = rows.iter().find(|r| r["id"] == json!(2)).unwrap();
        assert_eq!(b_row["renders"], json!(2));
        let a_row = rows.iter().find(|r| r["id"] == json!(1)).unwrap();
        assert_eq!(a_row["renders"], json!(0));

        let capped = narrationCompile(&json!({ "op": "list", "limit": 1 }), &c).unwrap();
        assert_eq!(capped["narrations"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn narration_compile_get_not_found_and_bad_op() {
        let c = fresh();
        assert!(narrationCompile(&json!({ "op": "get", "narration_id": 999 }), &c).unwrap_err().contains("not found"));
        assert!(narrationCompile(&json!({ "op": "nope" }), &c).unwrap_err().contains("op 'compile'|'get'|'list'"));
    }
}
