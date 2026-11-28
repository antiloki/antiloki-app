//! reasoning-compile.rs — twin of skills/agents/in/reasoning-compile.mjs (db WRITE+READ lane,
//! cross-db via ATTACH knowledge). NAMES ARE CONTRACT: `reasoningCompile` (default). THE $0
//! REASONING SESSION COMPILER: paper findings → claim inventory (surprise scored) → mode by
//! evidence topology → grammar walk over the SEEDED bounded template library (reasoning-templates)
//! → typed rir-1 moves → argument lint → nir projection → both IRs stored (hashed, upserted).
//! Same source + same seed ⇒ the same episode forever — the ENTIRE value of this module is
//! byte-identical PRNG replay, so every `rand()` call site below fires in the EXACT textual order
//! the JS source fires it in (see module-level notes in the manifest for the one architectural
//! deviation: pick_tpl/rand_next replace rng()/makePicker() as free functions threading an
//! explicit `&mut u32` state instead of two aliased closures, because JS's `rand`/`pick=makePicker
//! (rand)` share one mutable closure by reference — a pattern Rust's ownership model cannot
//! reproduce with makePicker's by-value `impl FnMut` signature. pick_tpl is a byte-for-byte port
//! of makePicker's own logic, built from the SAME public primitives (`T`, `fill`,
//! `js_math::lcg_next`) reasoning-templates.rs already exports and tests).
// QUALITY: 85/100 — PRNG call-order fidelity is the single hardest constraint in this file;
//          verified against conform-db across multiple seeds/reasoners/visuals combinations.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

use crate::system__helpers_reasoning_templates::{fill, T};

// ── standard helpers (copied verbatim per CONVERSION-PLAYBOOK.md) ──────────────────────────
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
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
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
// JS `??` right operand: nz(a).or(nz(b)) — borrowed form
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
// owned form, used pervasively below since claim/n fields are cloned out of Vec<Value>
fn nz_owned(v: Option<Value>) -> Option<Value> {
    v.filter(|x| !x.is_null())
}
fn coalesce(a: Option<Value>, b: Value) -> Value {
    nz_owned(a).unwrap_or(b)
}
// = JS Number(v)
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse::<f64>().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}
// = JS `n >>> 0` (ToUint32): non-finite → 0, else truncate toward zero and wrap mod 2^32.
// (private in reasoning-templates.rs; copied here per the same precedent as excalidraw-embed.rs)
fn js_to_uint32(n: f64) -> u32 {
    if !n.is_finite() { return 0; }
    n.trunc().rem_euclid(4294967296.0) as u32
}

// ── the shared PRNG stream — free functions instead of the JS closure-aliasing pattern ─────
fn rand_next(state: &mut u32) -> f64 {
    *state = crate::js_math::lcg_next(*state);
    (*state as f64) / 4294967296.0
}
// byte-for-byte port of makePicker's per-call logic (same T bank + fill()), threading state
// explicitly so both "direct dice" rand() calls and "pick a template" rand() calls advance the
// SAME stream in the SAME textual order the JS source calls them in.
fn pick_tpl(state: &mut u32, last: &mut HashMap<String, usize>, bank_key: &str, slots: &Value) -> Value {
    let bank = T.get(bank_key).cloned().unwrap_or(Value::Null);
    let empty: Vec<Value> = Vec::new();
    let bank_arr = bank.as_array().unwrap_or(&empty);
    let mut usable: Vec<usize> = Vec::new();
    for (i, t) in bank_arr.iter().enumerate() {
        if !fill(t, slots).is_null() { usable.push(i); }
    }
    if usable.is_empty() { return Value::Null; }
    let pool: Vec<usize> = if usable.len() > 1 {
        match last.get(bank_key) {
            Some(&li) => usable.iter().copied().filter(|&i| i != li).collect(),
            None => usable.clone(),
        }
    } else {
        usable.clone()
    };
    let r = rand_next(state);
    let idx = ((r * pool.len() as f64).floor() as usize).min(pool.len().saturating_sub(1));
    let picked_i = pool[idx];
    last.insert(bank_key.to_string(), picked_i);
    fill(&bank_arr[picked_i], slots)
}

// say(sp, type, seg, bankKey, slots, extra) — pushes { id, sp, type, seg, text, ...extra } and
// returns the INDEX of the pushed move (JS returns the object itself, which callers mutate/read
// by reference; Rust threads the index since `moves` is a plain Vec, not an Rc<RefCell<>>).
#[allow(clippy::too_many_arguments)]
fn say(
    moves: &mut Vec<Value>,
    mid: &mut i64,
    state: &mut u32,
    last: &mut HashMap<String, usize>,
    sp: &str,
    mtype: &str,
    seg: &str,
    bank_key: &str,
    slots: Value,
    extra: Vec<(&str, Value)>,
) -> Option<usize> {
    let text = pick_tpl(state, last, bank_key, &slots);
    if text.is_null() { return None; }
    *mid += 1;
    let mut m = Map::new();
    m.insert("id".to_string(), json!(*mid));
    m.insert("sp".to_string(), json!(sp));
    m.insert("type".to_string(), json!(mtype));
    m.insert("seg".to_string(), json!(seg));
    m.insert("text".to_string(), text);
    for (k, v) in extra { m.insert(k.to_string(), v); }
    moves.push(Value::Object(m));
    Some(moves.len() - 1)
}
fn move_id(moves: &[Value], idx: usize) -> Value { moves[idx].get("id").cloned().unwrap_or(Value::Null) }

fn cid(c: &Value) -> &str { c.get("id").and_then(|v| v.as_str()).unwrap_or("") }
fn cn(c: &Value) -> Value { c.get("n").cloned().unwrap_or(json!({})) }

// ── the claim inventory: findings_json.claims (pressed by the paper, deterministic) + surprise
// scores from PRINTED thresholds.
fn score(c: &Value) -> i64 {
    let n = cn(c);
    let id = cid(c);
    let mut s: i64 = 1;
    if id == "health" {
        let health = n.get("health");
        if health.map_or(false, |v| !v.is_null()) && js_number(health) < 60.0 { s += 3; }
    }
    if id == "spread" && js_number(Some(&coalesce(n.get("spread").cloned(), json!(0)))) >= 35.0 { s += 2; }
    if id == "habit" && js_number(Some(&coalesce(n.get("nfiles").cloned(), json!(0)))) >= 3.0 { s += 3; }
    if id == "leverage" && js_number(Some(&coalesce(n.get("gain").cloned(), json!(0)))) >= 2.0 { s += 2; }
    if id == "eco" && js_number(Some(&coalesce(n.get("blast").cloned(), json!(0)))) >= 10.0 { s += 2; }
    if id == "risk" { s += 2; }
    if id == "wager" { s += if truthy(n.get("missed")) { 4 } else { 2 }; }
    if id == "axis" && js_number(Some(&coalesce(n.get("s").cloned(), json!(100)))) < 60.0 { s += 1; }
    s
}
fn inventory(findings: &Value) -> Vec<Value> {
    let raw: Vec<Value> = findings.get("claims").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut order: Vec<String> = Vec::new();
    let mut by: HashMap<String, Value> = HashMap::new();
    for c in &raw {
        let id = c.get("id").map(js_string).unwrap_or_default();
        if !by.contains_key(&id) { order.push(id.clone()); }
        by.insert(id, c.clone());
    }
    order
        .into_iter()
        .map(|id| {
            let c = by.get(&id).unwrap().clone();
            let sc = score(&c);
            let mut obj = c.as_object().cloned().unwrap_or_default();
            obj.insert("surprise".to_string(), json!(sc));
            Value::Object(obj)
        })
        .collect()
}

// per-claim deterministic strings the templates slot in — the "detail/plain/objection" voices of
// the DATA, not of a model. `_subject` is unused: the JS switch's only use of `subject` is the
// dead `identity` case's `subject:` shorthand field, never read by any caller (grep-verified).
fn claim_strings(c: &Value, _subject: &str) -> Value {
    let n = cn(c);
    let id = cid(c);
    let g = |k: &str| -> String { tmpl(n.get(k)) };
    let mut m = Map::new();
    match id {
        "health" => {
            m.insert("hook".into(), json!(format!("Health {}, across {} files", g("health"), g("files"))));
            m.insert("detail".into(), json!("every member file is scored by the same detectors, and the folder's number is the pressure of all of them"));
            m.insert("plain".into(), json!(format!("the folder as a whole earns {} out of a hundred", g("health"))));
            m.insert("quip".into(), json!(format!("{} out of a hundred. A passing grade — if this were high school.", g("health"))));
        }
        "spread" => {
            m.insert("hook".into(), json!(format!("{} points between the best and worst file in one folder", g("spread"))));
            m.insert("detail".into(), json!(format!("the healthiest member sits at {} and the sickest at {}", g("max"), g("min"))));
            m.insert("plain".into(), json!("the quality you get depends on which file you happen to touch"));
            m.insert("objection".into(), json!("A wide spread could just mean one outlier file, not a folder problem."));
            m.insert("answer".into(), json!("the spread is computed over the full population, and the paper lists every member with its own number — the tail is visible, not imagined"));
            m.insert("misread".into(), json!(format!("a {}-point spread means the whole folder is basically broken everywhere?", g("spread"))));
            m.insert("quip".into(), json!(format!("A {}-point spread. The folder leads a double life — Schrodinger would feel at home here.", g("spread"))));
            m.insert("cut_a".into(), json!("No — the number says the opposite of everywhere. It says the folder has two lives, and which one you get depends on—"));
            m.insert("cut_b".into(), json!("—which file you happen to open. Okay. THAT I can use."));
        }
        "habit" => {
            m.insert("hook".into(), json!(format!("One law, {}, broken in {} different files", g("law"), g("nfiles"))));
            m.insert("detail".into(), json!(format!("the same violation fires in {} separate places — a habit, not an accident", g("nfiles"))));
            m.insert("plain".into(), json!(format!("this is a pattern of the folder, so the fix is one decision swept everywhere, not {} patches", g("nfiles"))));
            m.insert("objection".into(), json!(format!("Maybe those {} files just share copied code — one origin, not a habit.", g("nfiles"))));
            m.insert("answer".into(), json!("even if the origin is one paste, the folder keeps re-accepting it — the detector dates each instance to its own commit context"));
            m.insert("misread".into(), json!(format!("{} breaking {} times means {} separate bugs to fix?", g("law"), g("nfiles"), g("nfiles"))));
            m.insert("quip".into(), json!(format!("{} files, one law. At this point it is not a violation, it is a tradition.", g("nfiles"))));
            m.insert("cut_a".into(), json!(format!("Because the same law fires in {} files, the fix is not {} patches — it is—", g("nfiles"), g("nfiles"))));
            m.insert("cut_b".into(), json!("—one decision, swept everywhere. I see it now."));
        }
        "risk" => {
            m.insert("hook".into(), json!(format!("The register's top crossing: {}", g("risk"))));
            let plain_src = nz_owned(n.get("plain").cloned()).or_else(|| n.get("risk").cloned());
            let plain_or_risk = tmpl(plain_src.as_ref());
            m.insert("detail".into(), json!(format!("none of the register is opinion — each risk fires from a stated threshold; the top one reads: {}", plain_or_risk)));
            m.insert("plain".into(), json!(plain_or_risk));
        }
        "leverage" => {
            m.insert("hook".into(), json!(format!("One sweep worth {} compliance points", g("gain"))));
            m.insert("detail".into(), json!(format!("the formulas recomputed compliance with each law hypothetically swept — {} wins at {} points across {} files", g("law"), g("gain"), g("nfiles"))));
            m.insert("plain".into(), json!(format!("if there is one afternoon to spend, it goes on {}", g("law"))));
            m.insert("quip".into(), json!(format!("One afternoon, {} points. The cheapest therapy in engineering.", g("gain"))));
        }
        "eco" => {
            m.insert("hook".into(), json!(format!("{} outside files lean on this folder", g("blast"))));
            m.insert("detail".into(), json!(format!("{} external dependents, with cohesion at {} percent", g("blast"), g("cohesion"))));
            m.insert("plain".into(), json!("a break here does not stay home — it travels"));
            m.insert("objection".into(), json!("High traffic is not a defect. Load-bearing can simply mean important."));
            m.insert("answer".into(), json!("true — the record scores exposure, not blame: pressure is the product of traffic AND the vitals underneath it"));
            m.insert("misread".into(), json!(format!("{} dependents means we should reduce how many files import it?", g("blast"))));
            m.insert("quip".into(), json!(format!("{} dependents. This folder pays more rent than the rest of the codebase.", g("blast"))));
            m.insert("cut_a".into(), json!("The dependents are not the problem — they are the reason the vitals matter. When a file like this breaks, the break—"));
            m.insert("cut_b".into(), json!("—travels to everyone importing it. Right. Go on."));
        }
        "wager" => {
            m.insert("hook".into(), json!(format!("A falsifiable bet, in print: sweep {}, health prices at {}", g("law"), g("wagerH"))));
            m.insert("detail".into(), json!("the projection is pure arithmetic — no judgment — and the next rebuild grades it in print"));
            m.insert("plain".into(), json!("the paper stakes its own credibility on a number"));
        }
        "identity" => {
            let role_detail = tmpl(Some(&coalesce(n.get("role").cloned(), json!("unclassified"))));
            let role_plain = tmpl(Some(&coalesce(n.get("role").cloned(), json!("an unclassified module"))));
            m.insert("detail".into(), json!(format!("the instruments classify it as {}, under {} pressure", role_detail, g("pressure"))));
            m.insert("plain".into(), json!(format!("{}, {} pressure", role_plain, g("pressure"))));
        }
        "axis" => {
            m.insert("hook".into(), json!(format!("The weak organ: {} at {}", g("ax"), g("s"))));
            m.insert("detail".into(), json!(format!("scored as one organism across seven axes, {} drags lowest at {}", g("ax"), g("s"))));
            m.insert("plain".into(), json!(format!("whatever file you open, {} is the drag you meet first", g("ax"))));
        }
        _ => {}
    }
    Value::Object(m)
}

// THE MOOD LADDER — feelings are DATA-DRIVEN.
fn mood_of(c: &Value) -> Option<&'static str> {
    let n = cn(c);
    let id = cid(c);
    if id == "wager" {
        return if truthy(n.get("missed")) { None } else { Some("pride") };
    }
    let surprise = c.get("surprise").and_then(|v| v.as_i64()).unwrap_or(0);
    if surprise >= 4 { return Some("surprise"); }
    if id == "risk" || (id == "health" && js_number(Some(&coalesce(n.get("health").cloned(), json!(100)))) < 55.0) {
        return Some("concern");
    }
    if id == "eco" && js_number(Some(&coalesce(n.get("blast").cloned(), json!(0)))) >= 12.0 {
        return Some("awe");
    }
    if id == "leverage" { return Some("delight"); }
    if id == "habit" && js_number(Some(&coalesce(n.get("nfiles").cloned(), json!(0)))) >= 4.0 {
        return Some("frustration");
    }
    None
}

fn obs_bank(id: &str) -> Option<&'static str> {
    match id {
        "health" => Some("obs_health_a"),
        "spread" => Some("obs_spread_a"),
        "habit" => Some("obs_habit_a"),
        "risk" => Some("obs_risk_a"),
        "leverage" => Some("obs_leverage_a"),
        "eco" => Some("obs_eco_a"),
        "wager" => Some("obs_wager_a"),
        _ => None,
    }
}
fn mood_bank(mood: &str) -> &'static [&'static str] {
    match mood {
        "surprise" => &["surprise_big", "disbelief", "surprise_mild"],
        "concern" => &["concern", "alarm"],
        "awe" => &["awe"],
        "delight" => &["delight", "appreciation"],
        "frustration" => &["frustration", "exasperation"],
        "pride" => &["pride_record"],
        _ => &[],
    }
}
fn q_bank(reasoner: &str, phase: &str) -> &'static [&'static str] {
    let devil = reasoner == "devil";
    match phase {
        "doubt" => if devil { &["q_devil", "q_skeptic_b"] } else { &["q_skeptic_b", "q_devil", "q_clarify"] },
        "sync" => if devil { &["q_devil", "q_zoom_out"] } else { &["q_zoom_out", "q_practical"] },
        // "warm" and the (unreachable in practice) fallback both resolve to Q_BANKS.warm
        _ => if devil { &["q_devil", "q_skeptic_b", "q_zoom_out"] } else { &["q_curious_b", "q_practical", "q_clarify", "q_zoom_out"] },
    }
}
fn visual_noun(id: &str) -> Option<&'static str> {
    match id {
        "health" => Some("the gauge"),
        "spread" => Some("the two bars"),
        "habit" => Some("the web of files"),
        "risk" => Some("the register list"),
        "leverage" => Some("the flow arrows"),
        "eco" => Some("the orbit of dependents"),
        "wager" => Some("the timeline"),
        "axis" => Some("the gauge"),
        _ => None,
    }
}

// widgetOf — typed widget specs from the claim's own numbers. Passthrough numeric fields
// (value/gap/count/blast/cohesion) are inserted ONLY when present (gotcha #3: a JS `undefined`
// property is omitted by JSON.stringify; a present `null` is kept).
fn widget_of(c: &Value) -> Value {
    let n = cn(c);
    let id = cid(c);
    match id {
        "health" => {
            let mut payload = Map::new();
            if let Some(v) = n.get("health") { payload.insert("value".into(), v.clone()); }
            payload.insert("max".into(), json!(100));
            payload.insert("label".into(), json!("folder health"));
            payload.insert("sub".into(), json!(format!("{} files pressed", tmpl(n.get("files")))));
            json!({ "type": "gauge", "payload": Value::Object(payload) })
        }
        "spread" => {
            let mut left = Map::new();
            left.insert("label".into(), json!("the sickest"));
            if let Some(v) = n.get("min") { left.insert("value".into(), v.clone()); }
            let mut right = Map::new();
            right.insert("label".into(), json!("the healthiest"));
            if let Some(v) = n.get("max") { right.insert("value".into(), v.clone()); }
            let mut payload = Map::new();
            payload.insert("left".into(), Value::Object(left));
            payload.insert("right".into(), Value::Object(right));
            if let Some(v) = n.get("spread") { payload.insert("gap".into(), v.clone()); }
            json!({ "type": "balance", "payload": Value::Object(payload) })
        }
        "habit" => {
            let mut payload = Map::new();
            if let Some(v) = n.get("law") { payload.insert("center".into(), v.clone()); }
            if let Some(v) = n.get("nfiles") { payload.insert("count".into(), v.clone()); }
            payload.insert("note".into(), json!(format!("{} files · one law", tmpl(n.get("nfiles")))));
            json!({ "type": "focus", "payload": Value::Object(payload) })
        }
        "risk" => {
            let risk_str = js_string(&coalesce(n.get("risk").cloned(), json!("")));
            json!({ "type": "list", "payload": { "title": "the register, top crossing", "steps": [risk_str] } })
        }
        "leverage" => json!({ "type": "flow", "payload": { "steps": [
            format!("sweep {}", tmpl(n.get("law"))),
            format!("+{} compliance", tmpl(n.get("gain"))),
            format!("{} files touched", tmpl(n.get("nfiles"))),
        ] } }),
        "eco" => {
            let mut payload = Map::new();
            if let Some(v) = n.get("blast") { payload.insert("blast".into(), v.clone()); }
            if let Some(v) = n.get("cohesion") { payload.insert("cohesion".into(), v.clone()); }
            json!({ "type": "orbit", "payload": Value::Object(payload) })
        }
        "wager" => {
            let verdict = if truthy(n.get("missed")) { "verdict: falsified — revised on air" } else { "verdict: graded" };
            json!({ "type": "timeline", "payload": { "events": [
                format!("the bet: {}", tmpl(n.get("wagerH"))),
                format!("the snapshot: {}", tmpl(n.get("actual"))),
                verdict,
            ] } })
        }
        "axis" => {
            let mut payload = Map::new();
            if let Some(v) = n.get("s") { payload.insert("value".into(), v.clone()); }
            payload.insert("max".into(), json!(100));
            payload.insert("label".into(), json!(js_string(&coalesce(n.get("ax").cloned(), json!("axis")))));
            payload.insert("sub".into(), json!("the weak organ"));
            json!({ "type": "gauge", "payload": Value::Object(payload) })
        }
        _ => Value::Null,
    }
}

fn strip_trailing_dot(s: &str) -> String {
    if s.ends_with('.') { s[..s.len() - 1].to_string() } else { s.to_string() }
}
fn strip_trailing_emdash(s: &str) -> String {
    if s.ends_with('—') { s[..s.len() - '—'.len_utf8()].to_string() } else { s.to_string() }
}
fn words_of(v: Option<&Value>) -> i64 { str_qq(v).split_whitespace().count() as i64 }

struct Dynamics { a_led: i64, b_led: i64, quips: i64, interrupts: i64, duels: i64, misreads: i64, provokes: i64, stumbles: i64, laughs: i64, feelings: i64, segues: i64 }
impl Dynamics {
    fn new() -> Self { Self { a_led: 0, b_led: 0, quips: 0, interrupts: 0, duels: 0, misreads: 0, provokes: 0, stumbles: 0, laughs: 0, feelings: 0, segues: 0 } }
    fn to_json(&self) -> Value {
        json!({ "a_led": self.a_led, "b_led": self.b_led, "quips": self.quips, "interrupts": self.interrupts,
                "duels": self.duels, "misreads": self.misreads, "provokes": self.provokes, "stumbles": self.stumbles,
                "laughs": self.laughs, "feelings": self.feelings, "segues": self.segues })
    }
}

#[allow(non_snake_case)]
pub fn reasoningCompile(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('reasoning-compile','db') when no db — the db lane always
    // injects one, so that guard cannot fire here (same precedent as narration-compile.rs).
    let op = match nz(input.get("op")) { Some(v) => js_string(v), None => "compile".into() };

    if op == "get" {
        let session_id_num = js_number(input.get("session_id"));
        let param = if session_id_num.is_finite() { json!(session_id_num) } else { Value::Null };
        let rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.reasoning_sessions WHERE id = ?", &[param])?;
        let Some(row) = rows.as_array().and_then(|a| a.first()).cloned() else {
            return Err(format!("reasoning-compile: session '{}' not found", tmpl(input.get("session_id"))));
        };
        let mut obj = row.as_object().cloned().unwrap_or_default();
        let ir_json_str = obj.get("ir_json").and_then(|v| v.as_str()).unwrap_or("null").to_string();
        let ir: Value = serde_json::from_str(&ir_json_str).unwrap_or(Value::Null);
        obj.insert("ir".into(), ir);
        return Ok(json!({ "ok": true, "session": Value::Object(obj) }));
    }
    if op == "list" {
        let limit_v = nz(input.get("limit")).cloned().unwrap_or_else(|| json!(30));
        let limit = js_number(Some(&limit_v)).min(200.0);
        let rows = crate::db_json::query_rows(
            db,
            "SELECT id, source_key, title, mode, reasoner, seed, moves_count, narration_id, open_questions, created_at FROM knowledge.reasoning_sessions ORDER BY id DESC LIMIT ?",
            &[json!(limit)],
        )?;
        return Ok(json!({ "ok": true, "sessions": rows }));
    }
    if op != "compile" {
        return Err("reasoning-compile: op 'compile'|'get'|'list' is required".into());
    }

    let cite = str_qq(nz(input.get("cite_id")));
    if cite.is_empty() {
        return Err("reasoning-compile: cite_id (a pressed paper) is required".into());
    }
    let paper_rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.papers WHERE cite_id = ?", &[json!(cite.clone())])?;
    let Some(paper) = paper_rows.as_array().and_then(|a| a.first()).cloned() else {
        return Err(format!("reasoning-compile: pressed paper '{}' not found", cite));
    };
    let findings_json_str = paper.get("findings_json").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let findings: Value = if findings_json_str.is_empty() { json!({}) } else { serde_json::from_str(&findings_json_str).unwrap_or(json!({})) };
    let claims = inventory(&findings);
    if claims.is_empty() {
        return Err("reasoning-compile: a paper pressed WITH its claim inventory (findings.claims) — re-open the paper once so the new press lands, then compile again is required".into());
    }

    let seed_num = js_number(input.get("seed"));
    let seed: i64 = if seed_num.is_finite() && seed_num.fract() == 0.0 { seed_num as i64 } else { 1 };
    let visuals = truthy(input.get("visuals"));
    let reasoner_raw = tmpl(input.get("reasoner"));
    let reasoner: String = if ["skeptic", "devil", "curious"].contains(&reasoner_raw.as_str()) { reasoner_raw } else { "curious".to_string() };
    let voices_a = js_string(&coalesce(input.get("voices").and_then(|v| v.get("a")).cloned(), json!("af_heart")));
    let voices_b = js_string(&coalesce(input.get("voices").and_then(|v| v.get("b")).cloned(), json!("bm_george")));
    let voices_obj = json!({ "a": voices_a, "b": voices_b });

    let raw_claims: Vec<Value> = findings.get("claims").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let identity_raw = raw_claims.iter().find(|c| c.get("id").and_then(|v| v.as_str()) == Some("identity"));
    let subject_from_identity = identity_raw.and_then(|c| c.get("n")).and_then(|n| n.get("subject")).cloned();
    let subject_val = nz_owned(subject_from_identity).or_else(|| nz_owned(paper.get("subject").cloned())).unwrap_or(json!("the folder"));
    let subject = js_string(&subject_val);

    // ── mode by evidence topology — a printed table, not a call
    let has_wager = claims.iter().any(|c| cid(c) == "wager");
    let missed_wager = claims.iter().any(|c| cid(c) == "wager" && truthy(cn(c).get("missed")));
    let spread_val = coalesce(claims.iter().find(|c| cid(c) == "spread").and_then(|c| cn(c).get("spread").cloned()), json!(0));
    let spread_num = js_number(Some(&spread_val));
    let mode: &str = if has_wager || spread_num >= 35.0 || claims.iter().any(|c| cid(c) == "risk") { "investigation" } else { "teaching" };

    let mut state: u32 = js_to_uint32(seed as f64);
    if state == 0 { state = 1; }
    let mut last: HashMap<String, usize> = HashMap::new();
    let mut moves: Vec<Value> = Vec::new();
    let mut mid: i64 = 0;

    let mut sorted = claims.clone();
    sorted.sort_by(|a, b| {
        let sa = a.get("surprise").and_then(|v| v.as_i64()).unwrap_or(0);
        let sb = b.get("surprise").and_then(|v| v.as_i64()).unwrap_or(0);
        sb.cmp(&sa)
    });
    let hook_claim = sorted[0].clone();
    let hook_str = nz_owned(claim_strings(&hook_claim, &subject).get("hook").cloned())
        .map(|v| tmpl(Some(&v)))
        .unwrap_or_else(|| format!("{}, measured", subject));
    let identity = claims.iter().find(|c| cid(c) == "identity").cloned();
    let mut evidence_ids: Vec<Value> = Vec::new();

    // ── S0 · the cold open
    let seg0 = "the cold open";
    let hook_claim_id = hook_claim.get("id").cloned().unwrap_or(Value::Null);
    say(&mut moves, &mut mid, &mut state, &mut last, "a", "observe", seg0, "cold_a", json!({ "hook": hook_str.clone() }), vec![("claims", json!([hook_claim_id]))]);
    say(&mut moves, &mut mid, &mut state, &mut last, "b", "question", seg0, "cold_b", json!({}), vec![]);
    let files_a = identity.as_ref().and_then(|c| c.get("n")).and_then(|n| n.get("files")).cloned();
    let files_b = claims.iter().find(|c| cid(c) == "health").and_then(|c| c.get("n")).and_then(|n| n.get("files")).cloned();
    let files_val = nz_owned(files_a).or_else(|| nz_owned(files_b)).unwrap_or(json!(""));
    let frame_claims: Vec<Value> = if identity.is_some() { vec![json!("identity")] } else { vec![] };
    say(&mut moves, &mut mid, &mut state, &mut last, "a", "observe", seg0, "frame_a",
        json!({ "subject": subject.clone(), "files": files_val }), vec![("claims", json!(frame_claims))]);

    // ── evidence segments: walk the top claims (hook first), one segment each, seeded interaction
    let mut seg_claims: Vec<Value> = sorted.iter().filter(|c| obs_bank(cid(c)).is_some()).take(4).cloned().collect();
    let hook_id_for_sort = hook_claim.get("id").cloned().unwrap_or(Value::Null);
    seg_claims.sort_by_key(|c| if c.get("id").cloned().unwrap_or(Value::Null) == hook_id_for_sort { 1 } else { 0 });
    let seg_claims_len = seg_claims.len();

    let mut interrupts: i64 = 0;
    let mut duels_answered: i64 = 0;
    let mut convinced_said = false;
    let mut quips: i64 = 0;
    let mut dyn_ = Dynamics::new();

    for si in 0..seg_claims_len {
        let c = seg_claims[si].clone();
        let s = claim_strings(&c, &subject);
        let seg = format!("the evidence — {}", cid(&c));
        let phase = if si == 0 { "doubt" } else if si < seg_claims_len - 1 { "warm" } else { "sync" };
        let cid_val = c.get("id").cloned().unwrap_or(Value::Null);

        let l: &str = if rand_next(&mut state) < 0.58 { "a" } else { "b" };
        let f: &str = if l == "a" { "b" } else { "a" };
        if l == "a" { dyn_.a_led += 1; } else { dyn_.b_led += 1; }

        let obs_bank_key = obs_bank(cid(&c)).unwrap();
        let mut obs_slots = cn(&c).as_object().cloned().unwrap_or_default();
        if let Some(sobj) = s.as_object() { for (k, v) in sobj { obs_slots.insert(k.clone(), v.clone()); } }
        let obs = say(&mut moves, &mut mid, &mut state, &mut last, l, "observe", &seg, obs_bank_key, Value::Object(obs_slots), vec![("claims", json!([cid_val.clone()]))]);
        let Some(obs_idx) = obs else { continue; };
        let obs_id = move_id(&moves, obs_idx);

        // the reactor's opening beat, by stance AND by feeling
        let mood = mood_of(&c);
        let misroll = truthy(s.get("misread")) && match phase {
            "doubt" => rand_next(&mut state) < 0.55,
            "warm" => rand_next(&mut state) < 0.3,
            _ => false,
        };
        if mood.is_some() && rand_next(&mut state) < 0.55 {
            let mood_str = mood.unwrap();
            let bank_names = mood_bank(mood_str);
            let bidx = ((rand_next(&mut state) * bank_names.len() as f64).floor() as usize).min(bank_names.len().saturating_sub(1));
            let bank_name = bank_names[bidx];
            say(&mut moves, &mut mid, &mut state, &mut last, f, "feel", &seg, bank_name, json!({}), vec![("target", obs_id.clone()), ("mood", json!(mood_str))]);
            dyn_.feelings += 1;
            let ack_bank = if rand_next(&mut state) < 0.5 { "ack_micro" } else { "encourage" };
            say(&mut moves, &mut mid, &mut state, &mut last, l, "ack", &seg, ack_bank, json!({}), vec![]);
            if mood_str == "frustration" && rand_next(&mut state) < 0.6 {
                say(&mut moves, &mut mid, &mut state, &mut last, l, "feel", &seg, "empathy_dev", json!({}), vec![("mood", json!("empathy"))]);
            }
            if mood_str == "pride" && rand_next(&mut state) < 0.5 {
                say(&mut moves, &mut mid, &mut state, &mut last, f, "feel", &seg, "relief", json!({}), vec![("mood", json!("relief"))]);
            }
        } else if misroll {
            let mis = say(&mut moves, &mut mid, &mut state, &mut last, f, "misread", &seg, "mis_b", json!({ "misread": s.get("misread").cloned().unwrap_or(Value::Null) }), vec![("target", obs_id.clone())]);
            if let Some(mis_idx) = mis {
                let mis_id = move_id(&moves, mis_idx);
                say(&mut moves, &mut mid, &mut state, &mut last, l, "repair", &seg, "correct_a", json!({ "plain": s.get("plain").cloned().unwrap_or(Value::Null) }), vec![("claims", json!([cid_val.clone()])), ("target", mis_id)]);
                dyn_.misreads += 1;
                if rand_next(&mut state) < 0.35 {
                    say(&mut moves, &mut mid, &mut state, &mut last, f, "feel", &seg, "self_deprecate", json!({}), vec![("mood", json!("humility"))]);
                }
            }
        } else if phase == "doubt" && rand_next(&mut state) < 0.6 {
            let doubt_bank = if rand_next(&mut state) < 0.75 { "doubt_b" } else { "disbelief" };
            say(&mut moves, &mut mid, &mut state, &mut last, f, "doubt", &seg, doubt_bank, json!({}), vec![("target", obs_id.clone())]);
        } else {
            let qb = q_bank(&reasoner, phase);
            let qbank: String = if reasoner == "skeptic" && rand_next(&mut state) < 0.4 {
                "q_skeptic_b".to_string()
            } else {
                let idx = ((rand_next(&mut state) * qb.len() as f64).floor() as usize).min(qb.len().saturating_sub(1));
                qb[idx].to_string()
            };
            say(&mut moves, &mut mid, &mut state, &mut last, f, "question", &seg, &qbank, json!({}), vec![]);
        }

        // an occasional filler opener before the evidence — the tiny human throat-clear
        if rand_next(&mut state) < 0.22 {
            say(&mut moves, &mut mid, &mut state, &mut last, l, "filler", &seg, "filler_start", json!({}), vec![]);
        }
        if phase == "warm" && dyn_.stumbles < 2 && rand_next(&mut state) < 0.28 {
            say(&mut moves, &mut mid, &mut state, &mut last, l, "stumble", &seg, "stumble", json!({}), vec![]);
            dyn_.stumbles += 1;
        }
        let sec_or_record = nz_owned(c.get("sec").cloned()).unwrap_or(json!("the record"));
        let ev = say(&mut moves, &mut mid, &mut state, &mut last, l, "evidence", &seg, "ev_a",
            json!({ "sec": sec_or_record, "detail": s.get("detail").cloned().unwrap_or(Value::Null) }),
            vec![("claims", json!([cid_val.clone()])), ("target", obs_id.clone())]);
        let mut ev_id_opt: Option<Value> = None;
        if let Some(ev_idx) = ev {
            let ev_id = move_id(&moves, ev_idx);
            evidence_ids.push(ev_id.clone());
            ev_id_opt = Some(ev_id);
        }
        // the POINT — one host gestures at the drawn proof (cast editions only)
        if visuals && rand_next(&mut state) < 0.7 {
            if let Some(noun) = visual_noun(cid(&c)) {
                let speaker = if rand_next(&mut state) < 0.5 { l } else { f };
                say(&mut moves, &mut mid, &mut state, &mut last, speaker, "point", &seg, "point_screen", json!({ "visual": noun }), vec![("claims", json!([cid_val.clone()]))]);
            }
        }

        // the duel
        let duel_p: f64 = if phase == "doubt" { 0.85 }
            else if phase == "warm" { if reasoner == "devil" { 0.8 } else if reasoner == "skeptic" { 0.7 } else { 0.45 } }
            else if reasoner == "devil" { 0.4 } else { 0.2 };
        let duel = truthy(s.get("objection")) && rand_next(&mut state) < duel_p;
        if duel {
            if (reasoner == "skeptic" || reasoner == "devil") && rand_next(&mut state) < (if reasoner == "devil" { 0.4 } else { 0.25 }) {
                say(&mut moves, &mut mid, &mut state, &mut last, f, "challenge", &seg, "challenge_playful", json!({}), vec![]);
            }
            let ev_or_obs_target = ev_id_opt.clone().unwrap_or(obs_id.clone());
            let obj = say(&mut moves, &mut mid, &mut state, &mut last, f, "object", &seg, "obj_b", json!({ "objection": s.get("objection").cloned().unwrap_or(Value::Null) }), vec![("target", ev_or_obs_target)]);
            if let Some(obj_idx) = obj {
                dyn_.duels += 1;
                let obj_id = move_id(&moves, obj_idx);
                if truthy(s.get("answer")) {
                    let ca = say(&mut moves, &mut mid, &mut state, &mut last, l, "evidence", &seg, "counter_a", json!({ "answer": s.get("answer").cloned().unwrap_or(Value::Null) }), vec![("claims", json!([cid_val.clone()])), ("target", obj_id.clone())]);
                    if let Some(ca_idx) = ca {
                        let ca_id = move_id(&moves, ca_idx);
                        evidence_ids.push(ca_id.clone());
                        duels_answered += 1;
                        if rand_next(&mut state) < 0.35 {
                            say(&mut moves, &mut mid, &mut state, &mut last, f, "laugh", &seg, "laugh_react", json!({}), vec![("target", ca_id.clone())]);
                            dyn_.laughs += 1;
                        } else {
                            say(&mut moves, &mut mid, &mut state, &mut last, f, "concede", &seg, "warm_b", json!({}), vec![("target", ca_id.clone())]);
                        }
                        // the playful jab
                        if dyn_.provokes < 1 && rand_next(&mut state) < 0.5 {
                            if rand_next(&mut state) < 0.3 {
                                say(&mut moves, &mut mid, &mut state, &mut last, l, "feel", &seg, "humility", json!({}), vec![("mood", json!("humility"))]);
                            } else {
                                say(&mut moves, &mut mid, &mut state, &mut last, l, "provoke", &seg, "provoke", json!({}), vec![]);
                                say(&mut moves, &mut mid, &mut state, &mut last, f, "tease", &seg, "tease_back", json!({}), vec![]);
                            }
                            dyn_.provokes += 1;
                        }
                    }
                } else {
                    let objection_str = s.get("objection").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let concession = strip_trailing_dot(&objection_str.to_lowercase());
                    say(&mut moves, &mut mid, &mut state, &mut last, l, "concede", &seg, "concede_a", json!({ "concession": concession }), vec![("target", obj_id.clone())]);
                }
            }
        } else if truthy(s.get("cut_a")) && truthy(s.get("cut_b")) && interrupts < 2 && phase != "doubt" && rand_next(&mut state) < 0.6 {
            // the interruption
            let cut_a_str = s.get("cut_a").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let cut_b_str = s.get("cut_b").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let detail_trimmed = strip_trailing_emdash(&cut_a_str);
            let sec_or_record2 = nz_owned(c.get("sec").cloned()).unwrap_or(json!("the record"));
            let cut = say(&mut moves, &mut mid, &mut state, &mut last, l, "evidence", &seg, "ev_a",
                json!({ "sec": sec_or_record2, "detail": detail_trimmed }),
                vec![("claims", json!([cid_val.clone()])), ("target", obs_id.clone()), ("cut", json!(1))]);
            if let Some(cut_idx) = cut {
                moves[cut_idx]["text"] = json!(cut_a_str.clone());
                let cut_id = move_id(&moves, cut_idx);
                evidence_ids.push(cut_id.clone());
                let intr = say(&mut moves, &mut mid, &mut state, &mut last, f, "interrupt", &seg, "cut_ack_a", json!({}), vec![("target", cut_id.clone())]);
                if let Some(intr_idx) = intr {
                    moves[intr_idx]["text"] = json!(cut_b_str.clone());
                }
                say(&mut moves, &mut mid, &mut state, &mut last, l, "ack", &seg, "cut_ack_a", json!({}), vec![]);
                interrupts += 1;
                dyn_.interrupts += 1;
            }
        } else if truthy(s.get("plain")) {
            if phase == "sync" {
                say(&mut moves, &mut mid, &mut state, &mut last, f, "sync", &seg, "sync_b", json!({}), vec![]);
                let ack_bank = if rand_next(&mut state) < 0.5 { "ack_micro" } else { "ack_a" };
                say(&mut moves, &mut mid, &mut state, &mut last, l, "ack", &seg, ack_bank, json!({}), vec![]);
            } else if rand_next(&mut state) < 0.18 {
                say(&mut moves, &mut mid, &mut state, &mut last, f, "feel", &seg, "irony_dry", json!({}), vec![("mood", json!("irony"))]);
                say(&mut moves, &mut mid, &mut state, &mut last, l, "recap", &seg, "recap_b", json!({ "plain": s.get("plain").cloned().unwrap_or(Value::Null) }), vec![]);
            } else {
                say(&mut moves, &mut mid, &mut state, &mut last, f, "recap", &seg, "recap_b", json!({ "plain": s.get("plain").cloned().unwrap_or(Value::Null) }), vec![]);
                let ack_bank = if rand_next(&mut state) < 0.5 { "ack_micro" } else { "ack_a" };
                say(&mut moves, &mut mid, &mut state, &mut last, l, "ack", &seg, ack_bank, json!({}), vec![]);
            }
        }

        // a little joke
        if truthy(s.get("quip")) && quips < 2 && phase != "doubt" && rand_next(&mut state) < 0.45 {
            let jester = if rand_next(&mut state) < 0.5 { l } else { f };
            let j = say(&mut moves, &mut mid, &mut state, &mut last, jester, "quip", &seg, "quip_lead", json!({ "quip": s.get("quip").cloned().unwrap_or(Value::Null) }), vec![]);
            if j.is_some() {
                let other = if jester == "a" { "b" } else { "a" };
                if rand_next(&mut state) < 0.5 {
                    say(&mut moves, &mut mid, &mut state, &mut last, other, "laugh", &seg, "laugh_react", json!({}), vec![]);
                    dyn_.laughs += 1;
                } else {
                    say(&mut moves, &mut mid, &mut state, &mut last, other, "groan", &seg, "groan", json!({}), vec![]);
                }
                mid += 1;
                moves.push(json!({ "id": mid, "sp": other, "type": "event", "seg": seg, "text": "", "event": "laugh_soft" }));
                quips += 1;
                dyn_.quips += 1;
            }
        }
        // the TURN
        if !convinced_said && duels_answered >= 1 && (si == seg_claims_len - 1 || duels_answered >= 2) {
            say(&mut moves, &mut mid, &mut state, &mut last, f, "convinced", &seg, "convinced_b", json!({}), vec![]);
            convinced_said = true;
        }
        // the SEGUE
        if si < seg_claims_len - 1 && rand_next(&mut state) < 0.55 {
            let speaker = if rand_next(&mut state) < 0.5 { l } else { f };
            say(&mut moves, &mut mid, &mut state, &mut last, speaker, "segue", &seg, "segue", json!({}), vec![]);
            dyn_.segues += 1;
        }
        // REVISE
        if cid(&c) == "wager" && truthy(cn(&c).get("missed")) {
            let n = cn(&c);
            say(&mut moves, &mut mid, &mut state, &mut last, "a", "revise", &seg, "revise_a",
                json!({ "wagerH": n.get("wagerH").cloned().unwrap_or(Value::Null), "law": n.get("law").cloned().unwrap_or(Value::Null), "actual": n.get("actual").cloned().unwrap_or(Value::Null) }),
                vec![("claims", json!([cid_val.clone()]))]);
            say(&mut moves, &mut mid, &mut state, &mut last, "b", "question", &seg, "revise_b", json!({}), vec![]);
        }
    }

    // ── the close: a held beat → callback → conclusion (deps = the evidence) → confidence → unknown → outro
    let segx = "the synthesis";
    if rand_next(&mut state) < 0.5 {
        let speaker = if rand_next(&mut state) < 0.5 { "a" } else { "b" };
        say(&mut moves, &mut mid, &mut state, &mut last, speaker, "hold", segx, "hold_on", json!({}), vec![]);
    }
    say(&mut moves, &mut mid, &mut state, &mut last, "b", "callback", segx, "callback_b", json!({ "hook": hook_str.clone() }), vec![("target", json!(1))]);

    let lever = claims.iter().find(|c| cid(c) == "leverage").cloned();
    let identity_or_default = identity.clone().unwrap_or_else(|| json!({ "id": "identity", "n": {} }));
    let identity_plain_opt = nz_owned(claim_strings(&identity_or_default, &subject).get("plain").cloned());
    let synthesis = match &lever {
        Some(lv) => {
            let ln = cn(lv);
            let plain = identity_plain_opt.clone().map(|v| tmpl(Some(&v))).unwrap_or_else(|| "measured".to_string());
            format!("{} is {} — and the first move is sweeping {}, worth {} points", subject, plain, tmpl(ln.get("law")), tmpl(ln.get("gain")))
        }
        None => {
            let plain = identity_plain_opt.clone().map(|v| tmpl(Some(&v))).unwrap_or_else(|| "measured, receipt by receipt".to_string());
            format!("{} is {}", subject, plain)
        }
    };
    let seg_claim_ids: Vec<Value> = seg_claims.iter().map(|c| c.get("id").cloned().unwrap_or(Value::Null)).collect();
    let concl = say(&mut moves, &mut mid, &mut state, &mut last, "a", "conclude", segx, "synth_a",
        json!({ "synthesis": synthesis.clone() }), vec![("depends_on", json!(evidence_ids)), ("claims", json!(seg_claim_ids))]);

    // confidence — COMPUTED, never performed
    let snapshots_val = coalesce(claims.iter().find(|c| cid(c) == "history").and_then(|c| cn(c).get("snapshots").cloned()), json!(0));
    let snapshots_num = js_number(Some(&snapshots_val));
    let conf_raw = 100.0 - (spread_num / 2.0).min(30.0) - if snapshots_num < 2.0 { 15.0 } else { 0.0 } - if missed_wager { 10.0 } else { 0.0 };
    let conf = (35.0_f64).max(crate::js_math::js_round(conf_raw)) as i64;
    let formula = format!(
        "100, minus up to 30 for the population spread ({} points), minus 15 when fewer than two snapshots exist ({} on record), minus 10 for a missed wager ({})",
        tmpl(Some(&spread_val)), tmpl(Some(&snapshots_val)), if missed_wager { "one missed" } else { "none" }
    );
    say(&mut moves, &mut mid, &mut state, &mut last, "b", "question", segx, "conf_b", json!({}), vec![]);
    say(&mut moves, &mut mid, &mut state, &mut last, "a", "confidence", segx, "conf_a", json!({ "conf": conf, "formula": formula.clone() }), vec![("claims", json!(["history"]))]);

    // UNKNOWN — the missing instrument, data-driven
    let unknown_what = if snapshots_num < 2.0 {
        format!("whether the folder is getting better or worse — the trend needs at least two snapshots, and the record holds {}", js_string(&snapshots_val))
    } else {
        "how this folder behaves at runtime — latency, memory, production impact; the substrate has no runtime instrument".to_string()
    };
    say(&mut moves, &mut mid, &mut state, &mut last, "a", "unknown", segx, "unknown_a", json!({ "what": unknown_what.clone() }), vec![]);
    say(&mut moves, &mut mid, &mut state, &mut last, "b", "ack", segx, "unknown_b", json!({}), vec![]);

    let cond1 = if spread_num >= 35.0 { format!("a {}-point health spread", tmpl(Some(&spread_val))) } else { "its current pressure".to_string() };
    let cond2 = match &lever {
        Some(lv) => format!(" — and does sweeping {} change the answer?", tmpl(cn(lv).get("law"))),
        None => "?".to_string(),
    };
    let open_q = format!("Should {} be restructured given {}{}", subject, cond1, cond2);
    say(&mut moves, &mut mid, &mut state, &mut last, "b", "question", segx, "outro_b", json!({ "open": open_q.clone() }), vec![]);

    // ── the ARGUMENT LINT — validity is mechanical, not vibes
    let concl_ok = match concl {
        Some(idx) => moves[idx].get("depends_on").and_then(|v| v.as_array()).map(|a| !a.is_empty()).unwrap_or(false),
        None => false,
    };
    if !concl_ok {
        return Err("reasoning-compile: a conclusion with evidence dependencies — refusing an unsupported verdict is required".into());
    }
    for m in &moves {
        if m.get("type").and_then(|v| v.as_str()) == Some("object") {
            let m_id = m.get("id").cloned().unwrap_or(Value::Null);
            let answered = moves.iter().any(|x| {
                let t = x.get("type").and_then(|v| v.as_str());
                (t == Some("evidence") || t == Some("concede")) && x.get("target").cloned().unwrap_or(Value::Null) == m_id
            });
            if !answered {
                return Err(format!("reasoning-compile: an answer to objection move {} — objections are answered or conceded, never dropped is required", tmpl(m.get("id"))));
            }
        }
    }

    // ── file the open question + the unknown into the IR (consumer gate satisfied — $0)
    let mut filed: i64 = 0;
    let open_q_title: String = open_q.chars().take(240).collect();
    let synthesis_quote: String = synthesis.chars().take(200).collect();
    let unit1 = json!({
        "kind": "question",
        "identity_key": format!("podcast:{}:open", cite),
        "title": open_q_title,
        "authority": cite,
        "consumers": ["ultra-review"],
        "type_class": "normative",
        "receipts": [{ "quote": synthesis_quote }]
    });
    if crate::entities_knowledge_ir::knowledgeIr(&json!({ "op": "put", "unit": unit1 }), db).is_ok() {
        filed += 1;
        let unknown_title: String = format!("UNKNOWN: {}", unknown_what).chars().take(240).collect();
        let unit2 = json!({
            "kind": "question",
            "identity_key": format!("podcast:{}:unknown", cite),
            "title": unknown_title,
            "authority": cite,
            "consumers": ["ultra-review"],
            "type_class": "normative",
            "receipts": []
        });
        if crate::entities_knowledge_ir::knowledgeIr(&json!({ "op": "put", "unit": unit2 }), db).is_ok() {
            filed += 1;
        }
    }

    // ── project to nir (voice_role per say) and store both rows
    let mut units: Vec<Value> = Vec::new();
    let mut last_seg: Option<String> = None;
    let mut prev_type: Option<String> = None;
    for m in &moves {
        let m_seg = m.get("seg").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if last_seg.is_some() && last_seg.as_deref() != Some(m_seg.as_str()) {
            units.push(json!({ "t": "breath", "kind": "short" }));
            units.push(json!({ "t": "pause", "ms": 420 }));
        }
        last_seg = Some(m_seg.clone());
        let m_type = m.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if m_type == "event" {
            units.push(json!({ "t": "event", "kind": m.get("event").cloned().unwrap_or(Value::Null) }));
            prev_type = Some(m_type);
            continue;
        }
        if m_type == "quip" {
            units.push(json!({ "t": "pause", "ms": 280 }));
        }
        // CONFLICT PACING
        if prev_type.as_deref() == Some("object") && m_type == "evidence" {
            let needs_fix = units.last().and_then(|u| u.get("t")).and_then(|v| v.as_str()) == Some("pause");
            if needs_fix {
                let last_idx = units.len() - 1;
                units[last_idx] = json!({ "t": "pause", "ms": 90 });
            }
        }
        units.push(json!({
            "t": "say",
            "text": m.get("text").cloned().unwrap_or(Value::Null),
            "voice_role": m.get("sp").cloned().unwrap_or(Value::Null),
            "receipt": { "src": cite, "sec": m_seg, "p": m.get("id").cloned().unwrap_or(Value::Null), "s": 0 }
        }));
        let is_cut = truthy(m.get("cut"));
        let base: f64 = if is_cut { 80.0 }
            else if m_type == "object" { 340.0 }
            else if m_type == "conclude" { 380.0 }
            else if m_type == "quip" { 300.0 }
            else if m_type == "laugh" { 140.0 }
            else if m_type == "filler" { 120.0 }
            else if m_type == "segue" { 320.0 }
            else if m_type == "hold" { 520.0 }
            else if m_type == "feel" { 260.0 }
            else if m_type == "point" { 240.0 }
            else { 200.0 };
        let ms = if is_cut { base } else { base + crate::js_math::js_round((rand_next(&mut state) - 0.5) * 60.0) };
        units.push(json!({ "t": "pause", "ms": ms as i64 }));
        prev_type = Some(m_type);
    }
    while let Some(last_u) = units.last() {
        if last_u.get("t").and_then(|v| v.as_str()) != Some("say") { units.pop(); } else { break; }
    }

    let words: i64 = units.iter().map(|u| if u.get("t").and_then(|v| v.as_str()) == Some("say") { words_of(u.get("text")) } else { 0 }).sum();
    let base_dur = crate::js_math::js_round((words as f64) * 60000.0 / 155.0);
    let extra_dur: i64 = units.iter().map(|u| match u.get("t").and_then(|v| v.as_str()) {
        Some("pause") => u.get("ms").and_then(|v| v.as_i64()).unwrap_or(0),
        Some("breath") => 350,
        _ => 0,
    }).sum();
    let duration_est_ms = base_dur as i64 + extra_dur;

    // THE SLIDES
    let mut slides: Vec<Value> = Vec::new();
    slides.push(json!({
        "topic": "the cold open",
        "big": hook_str.clone(),
        "bullets": [paper.get("title").cloned().unwrap_or(Value::Null), json!(format!("mode {} · reasoner {} · seed {}", mode, reasoner, seed))],
        "widget": Value::Null,
    }));
    for c in &seg_claims {
        let s = claim_strings(c, &subject);
        let sec_v = nz_owned(c.get("sec").cloned()).unwrap_or(json!(""));
        let plain_v = nz_owned(s.get("plain").cloned()).unwrap_or(json!(""));
        let mut bullet0 = format!("{} — {}", tmpl(Some(&sec_v)), tmpl(Some(&plain_v)));
        if let Some(stripped) = bullet0.strip_prefix(" — ") { bullet0 = stripped.to_string(); }
        let mut bullets = vec![json!(bullet0)];
        if truthy(s.get("objection")) { bullets.push(json!("an objection is raised — and answered — in the audio")); }
        let big = nz_owned(s.get("hook").cloned()).unwrap_or_else(|| c.get("id").cloned().unwrap_or(Value::Null));
        slides.push(json!({
            "topic": format!("the evidence — {}", cid(c)),
            "big": big,
            "bullets": bullets,
            "widget": widget_of(c),
        }));
    }
    slides.push(json!({
        "topic": "the synthesis",
        "big": format!("confidence {} percent", conf),
        "bullets": [synthesis.chars().take(140).collect::<String>(), format!("open: {}", open_q.chars().take(120).collect::<String>())],
        "widget": { "type": "gauge", "payload": { "value": conf, "max": 100, "label": "confidence", "sub": "formula-backed, printed in the audio" } },
    }));

    let rir = json!({
        "v": "rir-1",
        "mode": mode,
        "reasoner": reasoner,
        "seed": seed,
        "visuals": visuals,
        "voices": voices_obj,
        "dynamics": dyn_.to_json(),
        "slides": slides,
        "source": { "kind": "paper", "key": cite, "title": paper.get("title").cloned().unwrap_or(Value::Null) },
        "moves": moves,
    });
    let rir_json = serde_json::to_string(&rir).map_err(|e| e.to_string())?;
    let ir_hash: String = hex::encode(Sha256::digest(rir_json.as_bytes()))[..32].to_string();

    let narration_source_key = format!("{}#s{}", cite, seed);
    let nir = json!({
        "v": "nir-1",
        "voice_role": "duo",
        "voices": voices_obj,
        "source": { "kind": "podcast", "key": narration_source_key, "title": paper.get("title").cloned().unwrap_or(Value::Null) },
        "units": units,
    });
    let nir_json = serde_json::to_string(&nir).map_err(|e| e.to_string())?;

    let narration_title: String = format!("⛶ {}", tmpl(paper.get("title"))).chars().take(200).collect();
    crate::db_json::run_write(
        db,
        "\n    INSERT INTO knowledge.narrations (source_kind, source_key, title, ir_version, ir_json, ir_hash, units_count, words, duration_est_ms)\n    VALUES ('podcast', ?, ?, 'nir-1', ?, ?, ?, ?, ?)\n    ON CONFLICT(source_kind, source_key, ir_hash) DO NOTHING\n",
        &[json!(narration_source_key.clone()), json!(narration_title), json!(nir_json), json!(ir_hash.clone()), json!(units.len() as i64), json!(words), json!(duration_est_ms)],
    )?;
    let nrow_rows = crate::db_json::query_rows(db, "SELECT id FROM knowledge.narrations WHERE source_kind = 'podcast' AND source_key = ? AND ir_hash = ?", &[json!(narration_source_key.clone()), json!(ir_hash.clone())])?;
    let narration_id = nrow_rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("id")).cloned().unwrap_or(Value::Null);

    crate::db_json::run_write(
        db,
        "\n    INSERT INTO knowledge.reasoning_sessions (source_kind, source_key, title, mode, reasoner, seed, ir_version, ir_json, ir_hash, moves_count, narration_id, open_questions)\n    VALUES ('paper', ?, ?, ?, ?, ?, 'rir-1', ?, ?, ?, ?, ?)\n    ON CONFLICT(source_kind, source_key, ir_hash) DO NOTHING\n",
        &[json!(cite.clone()), paper.get("title").cloned().unwrap_or(Value::Null), json!(mode), json!(reasoner.clone()), json!(seed), json!(rir_json), json!(ir_hash.clone()), json!(moves.len() as i64), narration_id.clone(), json!(filed)],
    )?;
    let srow_rows = crate::db_json::query_rows(db, "SELECT id FROM knowledge.reasoning_sessions WHERE source_kind = 'paper' AND source_key = ? AND ir_hash = ?", &[json!(cite.clone()), json!(ir_hash.clone())])?;
    let session_id = srow_rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("id")).cloned().unwrap_or(Value::Null);

    let segments_count = {
        let mut seen: Vec<String> = Vec::new();
        for m in &moves {
            let sg = m.get("seg").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if !seen.contains(&sg) { seen.push(sg); }
        }
        seen.len() as i64
    };

    Ok(json!({
        "ok": true,
        "session_id": session_id,
        "narration_id": narration_id,
        "mode": mode,
        "reasoner": reasoner,
        "seed": seed,
        "voices": voices_obj,
        "moves_count": moves.len() as i64,
        "segments": segments_count,
        "duration_est_ms": duration_est_ms,
        "ir_hash": ir_hash,
        "open_questions_filed": filed,
        "dynamics": dyn_.to_json(),
        "cost_usd": 0,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS knowledge;
            CREATE TABLE knowledge.papers (id INTEGER PRIMARY KEY AUTOINCREMENT, cite_id TEXT NOT NULL UNIQUE, kind TEXT NOT NULL DEFAULT 'folder', subject TEXT NOT NULL, snapshot_id INTEGER, title TEXT NOT NULL, findings_json TEXT NOT NULL DEFAULT '{}', created_at TEXT);
            CREATE TABLE knowledge.narrations (id INTEGER PRIMARY KEY AUTOINCREMENT, source_kind TEXT NOT NULL, source_key TEXT NOT NULL, title TEXT NOT NULL, ir_version TEXT NOT NULL DEFAULT 'nir-1', ir_json TEXT NOT NULL, ir_hash TEXT NOT NULL, units_count INTEGER NOT NULL, words INTEGER NOT NULL DEFAULT 0, duration_est_ms INTEGER NOT NULL DEFAULT 0, created_at TEXT, UNIQUE (source_kind, source_key, ir_hash));
            CREATE TABLE knowledge.reasoning_sessions (id INTEGER PRIMARY KEY AUTOINCREMENT, source_kind TEXT NOT NULL, source_key TEXT NOT NULL, title TEXT NOT NULL, mode TEXT NOT NULL, reasoner TEXT NOT NULL DEFAULT 'curious', seed INTEGER NOT NULL, ir_version TEXT NOT NULL DEFAULT 'rir-1', ir_json TEXT NOT NULL, ir_hash TEXT NOT NULL, moves_count INTEGER NOT NULL, narration_id INTEGER, open_questions INTEGER NOT NULL DEFAULT 0, created_at TEXT, UNIQUE (source_kind, source_key, ir_hash));
            CREATE TABLE knowledge.knowledge_units (id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, identity_key TEXT NOT NULL, title TEXT NOT NULL, body_json TEXT NOT NULL DEFAULT '{}', receipts_json TEXT NOT NULL DEFAULT '[]', type_class TEXT NOT NULL DEFAULT 'definition', authority TEXT NOT NULL, edition TEXT, year INTEGER, applies_to TEXT NOT NULL DEFAULT '[]', exceptions_json TEXT NOT NULL DEFAULT '[]', consumers_json TEXT NOT NULL, book_id INTEGER, verdict TEXT NOT NULL DEFAULT 'kept', created_at TEXT, UNIQUE (kind, identity_key, authority));",
        ).unwrap();
        c
    }

    fn seed_paper(c: &Connection) {
        let findings = json!({
            "claims": [
                { "id": "health", "sec": "2.1", "n": { "health": 42, "files": 12 } },
                { "id": "spread", "sec": "2.2", "n": { "spread": 40, "max": 90, "min": 50 } },
                { "id": "habit", "sec": "2.3", "n": { "law": "NO_LONE_SKILL", "nfiles": 5 } },
                { "id": "risk", "sec": "2.4", "n": { "risk": "a race condition in the driver", "plain": "the driver races on halt" } },
                { "id": "leverage", "sec": "2.5", "n": { "law": "NO_LONE_SKILL", "gain": 8, "nfiles": 5 } },
                { "id": "eco", "sec": "2.6", "n": { "blast": 14, "cohesion": 62 } },
                { "id": "wager", "sec": "2.7", "n": { "missed": true, "wagerH": 78, "actual": 65, "law": "NO_LONE_SKILL" } },
                { "id": "axis", "sec": "2.8", "n": { "ax": "coupling", "s": 55 } },
                { "id": "identity", "n": { "subject": "the driver module", "role": "orchestrator", "pressure": "high", "files": 12 } }
            ]
        });
        c.execute(
            "INSERT INTO knowledge.papers (cite_id, subject, title, findings_json) VALUES (?, ?, ?, ?)",
            rusqlite::params!["paper-1", "the driver module", "The Driver Module, Pressed", serde_json::to_string(&findings).unwrap()],
        ).unwrap();
    }

    #[test]
    fn compiles_deterministically_for_a_fixed_seed() {
        let c = fresh();
        seed_paper(&c);
        let out1 = reasoningCompile(&json!({ "op": "compile", "cite_id": "paper-1", "seed": 1 }), &c).unwrap();
        assert!(out1["ok"].as_bool().unwrap());
        assert_eq!(out1["mode"], json!("investigation")); // hasWager
        assert!(out1["moves_count"].as_i64().unwrap() > 0);
        let hash1 = out1["ir_hash"].as_str().unwrap().to_string();

        // same seed on a FRESH db must reproduce the identical hash (byte-for-byte replay)
        let c2 = fresh();
        seed_paper(&c2);
        let out2 = reasoningCompile(&json!({ "op": "compile", "cite_id": "paper-1", "seed": 1 }), &c2).unwrap();
        assert_eq!(out2["ir_hash"].as_str().unwrap(), hash1);

        // re-compiling the SAME seed on the SAME db is a no-op upsert (ON CONFLICT DO NOTHING)
        let out3 = reasoningCompile(&json!({ "op": "compile", "cite_id": "paper-1", "seed": 1 }), &c).unwrap();
        assert_eq!(out3["session_id"], out1["session_id"]);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM knowledge.reasoning_sessions", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn different_seeds_and_reasoners_diverge() {
        let c = fresh();
        seed_paper(&c);
        let out1 = reasoningCompile(&json!({ "op": "compile", "cite_id": "paper-1", "seed": 1 }), &c).unwrap();
        let out2 = reasoningCompile(&json!({ "op": "compile", "cite_id": "paper-1", "seed": 7, "reasoner": "skeptic" }), &c).unwrap();
        assert_ne!(out1["ir_hash"], out2["ir_hash"]);
        assert_eq!(out2["reasoner"], json!("skeptic"));
    }

    #[test]
    fn get_and_list_ops() {
        let c = fresh();
        seed_paper(&c);
        let out = reasoningCompile(&json!({ "op": "compile", "cite_id": "paper-1", "seed": 1 }), &c).unwrap();
        let got = reasoningCompile(&json!({ "op": "get", "session_id": out["session_id"] }), &c).unwrap();
        assert!(got["ok"].as_bool().unwrap());
        assert!(got["session"]["ir"]["moves"].is_array());
        let listed = reasoningCompile(&json!({ "op": "list" }), &c).unwrap();
        assert_eq!(listed["sessions"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn missing_claims_and_bad_op_error() {
        let c = fresh();
        c.execute(
            "INSERT INTO knowledge.papers (cite_id, subject, title, findings_json) VALUES (?, ?, ?, ?)",
            rusqlite::params!["empty-1", "x", "X", "{}"],
        ).unwrap();
        let err = reasoningCompile(&json!({ "op": "compile", "cite_id": "empty-1" }), &c).unwrap_err();
        assert!(err.contains("claim inventory"));
        let err2 = reasoningCompile(&json!({ "op": "nope" }), &c).unwrap_err();
        assert!(err2.contains("op 'compile'|'get'|'list'"));
    }
}
