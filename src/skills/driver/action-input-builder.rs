//! action-input-builder.rs — twin of skills/driver/action-input-builder.mjs (db READ lane, mostly
//! pure). NAMES ARE CONTRACT: `actionInputBuilder` (default). Given the action the state machine
//! chose + the gathered facts, produce the right input shape for that action (registry dispatch).
// QUALITY: builder registry — per-action input shaping; two arms load a row (pl.approve, tl.sign).
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
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
// String(v)
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
// `${v}`
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
// JS `??` right operand: nz(a).or(nz(b))
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
// JS Number(v)
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
    }
}

// JS strict `x === 0`: true only when x is the number 0.
fn strict_eq_zero(v: Option<&Value>) -> bool {
    matches!(v, Some(Value::Number(n)) if n.as_f64() == Some(0.0))
}
// `${v ?? def}`
fn coalesce_tmpl(v: Option<&Value>, def: &str) -> String {
    match nz(v) { Some(x) => str_qq(Some(x)), None => def.to_string() }
}
// insert v unless it is JS `undefined` (None): a key set to undefined is dropped by JSON.stringify.
fn ins_opt(m: &mut Map<String, Value>, k: &str, v: Option<&Value>) {
    if let Some(x) = v { m.insert(k.into(), x.clone()); }
}
// merged-arg field read: `{...facts}[k]` — None when facts is absent/null/non-object or key missing.
fn fget<'a>(facts: Option<&'a Value>, k: &str) -> Option<&'a Value> {
    facts.and_then(|f| f.get(k))
}

fn obj_cs(slug: Option<&Value>) -> Value {
    let mut m = Map::new();
    ins_opt(&mut m, "contract_slug", slug);
    Value::Object(m)
}
fn obj_cs_rev(slug: Option<&Value>) -> Value {
    let mut m = Map::new();
    ins_opt(&mut m, "contract_slug", slug);
    m.insert("reviewer".into(), json!("driver:auto"));
    Value::Object(m)
}
fn obj_cs_mode(slug: Option<&Value>, mode: &str) -> Value {
    let mut m = Map::new();
    ins_opt(&mut m, "contract_slug", slug);
    m.insert("mode".into(), json!(mode));
    Value::Object(m)
}
fn obj_card(card_slug: Option<&Value>) -> Value {
    let mut m = Map::new();
    ins_opt(&mut m, "card_slug", card_slug);
    Value::Object(m)
}

const BODY_FIELDS: [&str; 9] = [
    "raw_intent", "motivation", "strategy", "fix", "acceptance",
    "children", "diagnosis", "recommendation", "architecture",
];

fn flatten_body_to_prose(body: &Value) -> String {
    // if (!body || typeof body !== 'object') return '';  (typeof object === object/array/null)
    if !(truthy(Some(body)) && (body.is_object() || body.is_array())) { return String::new(); }
    let mut parts: Vec<String> = Vec::new();
    for k in BODY_FIELDS {
        let v = match body.get(k) { Some(x) if !x.is_null() => x, _ => continue };
        if let Value::String(s) = v {
            let t = s.trim();
            if !t.is_empty() {
                parts.push(format!("{}: {}", k.to_ascii_uppercase(), t));
            }
        } else if let Value::Array(arr) = v {
            if !arr.is_empty() {
                let joined = arr.iter().filter(|x| x.is_string())
                    .map(|x| x.as_str().unwrap().to_string())
                    .collect::<Vec<_>>().join(" \u{00B7} ");
                parts.push(format!("{}: {}", k.to_ascii_uppercase(), joined));
            }
        }
    }
    parts.join("\n\n")
}

fn extract_raw_intent(card: Option<&Value>, card_row: Option<&Value>) -> String {
    // title = cardRow?.title && cardRow.title.length >= 5 ? cardRow.title : null
    let title: Option<&str> = match nz(card_row).and_then(|cr| cr.get("title")) {
        Some(Value::String(s)) if s.chars().count() >= 5 => Some(s.as_str()),
        _ => None,
    };
    // body = JSON.parse(cardRow?.body_json ?? '""'); catch → null
    let src = match nz(nz(card_row).and_then(|cr| cr.get("body_json"))) {
        Some(Value::String(s)) => s.clone(),
        Some(other) => js_string(other),
        None => "\"\"".to_string(),
    };
    let body: Value = serde_json::from_str::<Value>(&src).unwrap_or(Value::Null);

    let prose = flatten_body_to_prose(&body);
    if let Some(t) = title {
        if !prose.is_empty() {
            let composed = format!("{}\n\n{}", t, prose);
            return if composed.chars().count() > 3500 {
                let head: String = composed.chars().take(3500).collect();
                format!("{}\n\n\u{2026}(truncated)", head)
            } else {
                composed
            };
        }
    }
    // typeof body === 'object' && body?.raw_intent && body.raw_intent.length >= 5
    if let Some(Value::String(ri)) = body.get("raw_intent") {
        if ri.chars().count() >= 5 {
            return match title {
                Some(t) => format!("{}\n\n{}", t, ri),
                None => ri.clone(),
            };
        }
    }
    if let Some(t) = title { return t.to_string(); }
    // `Drive card ${card.slug} through PL → contract → DEV → QA workflow`
    let cs = tmpl(card.and_then(|c| c.get("slug")));
    format!("Drive card {} through PL \u{2192} contract \u{2192} DEV \u{2192} QA workflow", cs)
}

pub fn actionInputBuilder(input: &Value, db: &Connection) -> Result<Value, String> {
    let action_name = input.get("action_name");
    let facts = input.get("facts");
    let card_row = input.get("cardRow");

    // if (!action_name) return LEGACY_DEFAULT({ card: { slug: facts?.card?.slug ?? null } });
    if !truthy(action_name) {
        let cslug = nz(nz(facts).and_then(|f| f.get("card"))).and_then(|c| c.get("slug"));
        let x = nz(cslug).cloned().unwrap_or(Value::Null);
        let mut m = Map::new();
        m.insert("card_slug".into(), x.clone());
        m.insert("slug".into(), x);
        return Ok(Value::Object(m));
    }

    let card = fget(facts, "card");
    let contract = fget(facts, "contract");
    let pl = fget(facts, "plSession");

    let contract_slug = nz(contract).and_then(|c| c.get("slug")); // contract?.slug
    let card_slug_opt = card.and_then(|c| c.get("slug")); // card.slug (direct)
    let pl_id = nz(pl).and_then(|p| p.get("id")); // plSession?.id

    let an = action_name.and_then(|v| v.as_str()).unwrap_or("");

    let out: Value = match an {
        // ── PL ──────────────────────────────────────────────────────
        "pl.session.start" => {
            let ri = extract_raw_intent(card, card_row);
            let mut m = Map::new();
            m.insert("raw_intent".into(), json!(ri));
            ins_opt(&mut m, "approved_card_slug", card_slug_opt);
            Value::Object(m)
        }
        "pl.session.advance.clarify"
        | "pl.session.advance.research"
        | "pl.session.advance.decompose"
        | "pl.session.finalize" => {
            let mut m = Map::new();
            ins_opt(&mut m, "session_id", pl_id);
            Value::Object(m)
        }
        "pl.session.approve" => {
            let mut proposed_card_id = Value::Null;
            if truthy(pl_id) {
                let idv = pl_id.cloned().unwrap_or(Value::Null);
                if let Ok(rows) = crate::db_json::query_rows(
                    db,
                    "SELECT id FROM pl_proposed_cards WHERE session_id = ? AND status = 'awaiting_approval' ORDER BY id DESC LIMIT 1",
                    &[idv],
                ) {
                    if let Some(row) = rows.as_array().and_then(|a| a.first()) {
                        proposed_card_id = nz(row.get("id")).cloned().unwrap_or(Value::Null);
                    }
                }
            }
            let mut m = Map::new();
            ins_opt(&mut m, "session_id", pl_id);
            m.insert("proposed_card_id".into(), proposed_card_id);
            m.insert("approved_by".into(), json!("driver:auto"));
            Value::Object(m)
        }

        // ── TL ──────────────────────────────────────────────────────
        "contract.draft.auto" => {
            let mut m = Map::new();
            ins_opt(&mut m, "card_slug", card_slug_opt);
            m.insert("pl_session_id".into(), nz(pl_id).cloned().unwrap_or(Value::Null));
            Value::Object(m)
        }
        "contract.gate.tl.sign" => {
            let slug = nz(contract).and_then(|c| c.get("slug"));
            let mut full: Option<Value> = None;
            if truthy(slug) {
                let sv = slug.cloned().unwrap_or(Value::Null);
                if let Ok(rows) = crate::db_json::query_rows(db, "SELECT * FROM contracts WHERE slug = ?", &[sv]) {
                    full = rows.as_array().and_then(|a| a.first()).cloned();
                }
            }
            let c: Value = full.unwrap_or_else(|| nz(contract).cloned().unwrap_or_else(|| json!({})));
            let is_trivial = c.get("tier").and_then(|v| v.as_str()) == Some("trivial");
            let is_xs = c.get("size").and_then(|v| v.as_str()) == Some("XS");
            let requires = truthy(c.get("tier")) && !is_trivial && !is_xs;
            let briefing: Value = if requires {
                json!(format!(
                    "Auto-signed by driver:auto. Contract follows {} template defaults; size={}, risk={}, tier={}. Reviewer should validate that acceptance criteria match the card body and that no must_obey law was skipped.",
                    coalesce_tmpl(c.get("template_kind"), "detected"),
                    coalesce_tmpl(c.get("size"), "?"),
                    coalesce_tmpl(c.get("risk"), "?"),
                    coalesce_tmpl(c.get("tier"), "?"),
                ))
            } else {
                Value::Null
            };
            let mut m = Map::new();
            m.insert("contract".into(), c);
            m.insert("decision".into(), json!("accept"));
            m.insert("signed_by".into(), json!("driver:auto"));
            m.insert("tl_briefing_md".into(), briefing);
            Value::Object(m)
        }
        "card.gate.pl.ratify" => {
            let mut m = Map::new();
            ins_opt(&mut m, "slug", card_slug_opt);
            ins_opt(&mut m, "card_slug", card_slug_opt);
            ins_opt(&mut m, "contract_slug", contract_slug);
            m.insert("decision".into(), json!("accept"));
            m.insert("signed_by".into(), json!("driver:auto"));
            m.insert("ratified_by".into(), json!("driver:auto"));
            Value::Object(m)
        }
        "tl.sandbox.create" => obj_cs(contract_slug),

        // ── DEV ─────────────────────────────────────────────────────
        "git-manager.contract.load" => {
            let mut m = Map::new();
            ins_opt(&mut m, "slug", contract_slug);
            ins_opt(&mut m, "contract_slug", contract_slug);
            Value::Object(m)
        }
        "git-manager.implement.code" => obj_cs(contract_slug),
        "git-manager.build.gate"
        | "git-manager.build.baseline"
        | "git-manager.test.baseline"
        | "git-manager.test.gate" => obj_cs(contract_slug),
        "system.bugfinder.run" => {
            let mut m = Map::new();
            ins_opt(&mut m, "contract_slug", contract_slug);
            m.insert("source".into(), json!("pipeline"));
            Value::Object(m)
        }
        "tl.refine.microtasks" => {
            // bypass_cache: (devAttempts ?? 0) > 0 || (buildGateFresh && lastBuildGate?.passed === 0)
            let a_val = nz(fget(facts, "devAttempts")).cloned().unwrap_or(json!(0));
            let a = js_number(Some(&a_val)) > 0.0;
            let bgf = fget(facts, "buildGateFresh");
            let bypass: Option<Value> = if a {
                Some(json!(true))
            } else if truthy(bgf) {
                Some(json!(strict_eq_zero(nz(fget(facts, "lastBuildGate")).and_then(|o| o.get("passed")))))
            } else {
                bgf.cloned()
            };
            let mut m = Map::new();
            ins_opt(&mut m, "contract_slug", contract_slug);
            if let Some(v) = bypass { m.insert("bypass_cache".into(), v); }
            Value::Object(m)
        }
        "tl.plan.contract" => {
            let left = truthy(fget(facts, "planReviewFresh"))
                && strict_eq_zero(nz(fget(facts, "lastPlanReview")).and_then(|o| o.get("passed")));
            let right = truthy(fget(facts, "planCoverageFresh"))
                && strict_eq_zero(nz(fget(facts, "lastPlanCoverage")).and_then(|o| o.get("passed")));
            let mut m = Map::new();
            ins_opt(&mut m, "contract_slug", contract_slug);
            m.insert("bypass_cache".into(), json!(left || right));
            Value::Object(m)
        }
        "tl.author.snippets" => {
            let a_val = nz(fget(facts, "devAttempts")).cloned().unwrap_or(json!(0));
            let a = js_number(Some(&a_val)) > 0.0;
            let b = truthy(fget(facts, "buildGateFresh"))
                && strict_eq_zero(nz(fget(facts, "lastBuildGate")).and_then(|o| o.get("passed")));
            let c = truthy(fget(facts, "contractQaReviewFresh"))
                && strict_eq_zero(nz(fget(facts, "lastContractQaReview")).and_then(|o| o.get("passed")));
            let mut m = Map::new();
            ins_opt(&mut m, "contract_slug", contract_slug);
            m.insert("bypass_cache".into(), json!(a || b || c));
            Value::Object(m)
        }
        "pl.review.contract" => obj_cs(contract_slug),
        "tl.contract.amend" => obj_cs(contract_slug),
        "tl.contract.redraft" => {
            let mut m = Map::new();
            ins_opt(&mut m, "old_slug", contract_slug);
            Value::Object(m)
        }
        "man.audit.contract.signed" => obj_cs(contract_slug),

        // ── QA ──────────────────────────────────────────────────────
        "qa.contract.review" => {
            let mode = if truthy(fget(facts, "hasPlanDefs")) && !truthy(fget(facts, "allFilesAuthored")) { "plan" } else { "full" };
            let mut m = Map::new();
            ins_opt(&mut m, "contract_slug", contract_slug);
            m.insert("reviewer".into(), json!("driver:auto"));
            m.insert("mode".into(), json!(mode));
            Value::Object(m)
        }
        "qa.contract.plan.coverage" => obj_cs_rev(contract_slug),

        // ── SWARM ───────────────────────────────────────────────────
        "swarm.manifest.design" => obj_cs(contract_slug),
        "swarm.manifest.lint"
        | "swarm.author.blocks"
        | "swarm.review.contract"
        | "qa.swarm.coverage" => obj_cs_rev(contract_slug),

        // ── PIPELINE-V2 ─────────────────────────────────────────────
        "pl.screening" | "tl.contract.assemble" => obj_card(card_slug_opt),
        "pl.ratify.contract" => obj_cs(contract_slug),
        "blueprint.design" => obj_cs(contract_slug),
        "blueprint.judge.design" => obj_cs_mode(contract_slug, "design"),
        "blueprint.judge.snippets" => obj_cs_mode(contract_slug, "snippets"),
        "snippet.author" => obj_cs(contract_slug),
        "blueprint.apply" => {
            // target_root: contract?.worktree_path || contract?.sandbox_path
            let wt = nz(contract).and_then(|c| c.get("worktree_path"));
            let target = if truthy(wt) { wt } else { nz(contract).and_then(|c| c.get("sandbox_path")) };
            let mut m = Map::new();
            ins_opt(&mut m, "contract_slug", contract_slug);
            ins_opt(&mut m, "target_root", target);
            Value::Object(m)
        }
        "blueprint.plan.seal" | "blueprint.build.seal" => obj_cs(contract_slug),

        // ── WORKTREE ────────────────────────────────────────────────
        "git-manager.worktree.create" => obj_cs_mode(contract_slug, "create"),
        "git-manager.worktree.merge" => obj_cs_mode(contract_slug, "merge"),
        "git-manager.pr.open" => obj_cs(contract_slug),

        // ── QA (second definitions win over the earlier duplicates) ─
        "qa.gate.contract.verify" => {
            let mut m = Map::new();
            ins_opt(&mut m, "contract_slug", contract_slug);
            m.insert("verifier".into(), json!("driver:auto"));
            Value::Object(m)
        }
        "qa.acceptance.run" => obj_cs(contract_slug),
        "qa.scope.check" => obj_cs(contract_slug),
        "qa.sandbox.promote" => obj_cs(contract_slug),
        "qa.sandbox.reset" => {
            let mut m = Map::new();
            ins_opt(&mut m, "contract_slug", contract_slug);
            m.insert("reason".into(), json!("driver:auto reset (sandbox cleared by orchestration)"));
            Value::Object(m)
        }

        // ── POST-QA ─────────────────────────────────────────────────
        "man.card.review" | "keeper.card.consolidate" | "keeper.journey.append" => obj_card(card_slug_opt),
        "system.codebase.integrate" => obj_cs(contract_slug),

        "board-keeper.card.status.change" => {
            let mut m = Map::new();
            ins_opt(&mut m, "slug", card_slug_opt);
            m.insert("status".into(), json!("done"));
            m.insert("reason".into(), json!("driver:auto \u{2014} contract done + MAN reviewed + keeper consolidated + journey appended \u{2192} card closes"));
            Value::Object(m)
        }

        // unknown → LEGACY_DEFAULT(facts ?? {}) = { card_slug: card.slug, slug: card.slug }
        _ => {
            let mut m = Map::new();
            ins_opt(&mut m, "card_slug", card_slug_opt);
            ins_opt(&mut m, "slug", card_slug_opt);
            Value::Object(m)
        }
    };
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(i: Value) -> Value {
        actionInputBuilder(&i, &Connection::open_in_memory().unwrap()).unwrap()
    }
    fn facts() -> Value {
        json!({
            "card": { "slug": "CARD-A", "status": "backlog" },
            "contract": { "slug": "CARD-A-C1", "status": "tl_approved", "tier": "standard", "sandbox_path": null },
            "plSession": { "id": 42, "status": "done", "approved_card_slug": "CARD-A" },
            "lastQaRun": null,
            "devAttempts": 0
        })
    }

    #[test]
    fn dispatches() {
        // pl.session.start — title (>= 5) used verbatim when body has no fields
        let r = call(json!({ "action_name": "pl.session.start", "facts": facts(),
            "cardRow": { "title": "Make the thing work", "body_json": "{}" } }));
        assert_eq!(r["raw_intent"], json!("Make the thing work"));
        assert_eq!(r["approved_card_slug"], json!("CARD-A"));

        // pl.session.start — title + flattened body composed
        let r = call(json!({ "action_name": "pl.session.start", "facts": facts(),
            "cardRow": { "title": "Rich card title", "body_json": "{\"motivation\":\"why\",\"children\":[\"a\",\"b\"]}" } }));
        assert_eq!(r["raw_intent"], json!("Rich card title\n\nMOTIVATION: why\n\nCHILDREN: a \u{00B7} b"));

        // pl.session.start — short title, synthesized fallback
        let r = call(json!({ "action_name": "pl.session.start", "facts": facts(),
            "cardRow": { "title": "x", "body_json": "{}" } }));
        assert_eq!(r["raw_intent"], json!("Drive card CARD-A through PL \u{2192} contract \u{2192} DEV \u{2192} QA workflow"));

        // pl.session.advance.* — session_id only
        assert_eq!(call(json!({ "action_name": "pl.session.finalize", "facts": facts() })),
            json!({ "session_id": 42 }));

        // contract.gate.tl.sign — full contract loaded from db + briefing for non-trivial
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE contracts (slug TEXT, tier TEXT, size TEXT, template_kind TEXT, risk INTEGER); \
            INSERT INTO contracts (slug,tier,size,template_kind,risk) VALUES ('C1','standard','M','feature-add',3);").unwrap();
        let r = actionInputBuilder(&json!({ "action_name": "contract.gate.tl.sign",
            "facts": { "contract": { "slug": "C1" } } }), &db).unwrap();
        assert_eq!(r["contract"]["tier"], json!("standard"));
        assert_eq!(r["decision"], json!("accept"));
        assert!(r["tl_briefing_md"].as_str().unwrap().contains("size=M, risk=3, tier=standard"));

        // card.gate.pl.ratify — both slug + contract_slug + decision
        let r = call(json!({ "action_name": "card.gate.pl.ratify", "facts": facts() }));
        assert_eq!(r["slug"], json!("CARD-A"));
        assert_eq!(r["contract_slug"], json!("CARD-A-C1"));
        assert_eq!(r["ratified_by"], json!("driver:auto"));

        // tl.refine.microtasks — red gate bypasses, base facts omit bypass_cache
        let r = call(json!({ "action_name": "tl.refine.microtasks",
            "facts": { "contract": { "slug": "C1" }, "devAttempts": 0, "buildGateFresh": true, "lastBuildGate": { "passed": 0 } } }));
        assert_eq!(r["bypass_cache"], json!(true));
        let r = call(json!({ "action_name": "tl.refine.microtasks", "facts": facts() }));
        assert_eq!(r.as_object().unwrap().contains_key("bypass_cache"), false);

        // qa.gate.contract.verify — second definition wins (has verifier)
        let r = call(json!({ "action_name": "qa.gate.contract.verify", "facts": facts() }));
        assert_eq!(r["verifier"], json!("driver:auto"));

        // blueprint.apply — target_root falls back to sandbox_path (null here)
        let r = call(json!({ "action_name": "blueprint.apply", "facts": facts() }));
        assert_eq!(r["target_root"], Value::Null);
        assert!(r.as_object().unwrap().contains_key("target_root"));

        // unknown action → legacy default
        assert_eq!(call(json!({ "action_name": "totally.unknown", "facts": facts() })),
            json!({ "card_slug": "CARD-A", "slug": "CARD-A" }));

        // no action_name → legacy default (null-coalesced)
        assert_eq!(call(json!({ "facts": facts() })),
            json!({ "card_slug": "CARD-A", "slug": "CARD-A" }));
        assert_eq!(call(Value::Null), json!({ "card_slug": null, "slug": null }));
    }
}
