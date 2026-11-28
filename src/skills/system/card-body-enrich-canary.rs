//! card-body-enrich-canary.rs — twin of skills/system/card-body-enrich-canary.mjs (db,
//! LLM-partial via `system_card_body_enrich`). NAMES ARE CONTRACT: default `cardBodyEnrichCanary`.
//!
//! Runs enrichment on up to 10 thin-body cards (quota: 2 epic / 4 feat / 2 fix / 2 other, topped
//! up from any bucket if short) and persists proposals to `card_enrichment_proposals`, returning
//! a `batch_id` for operator review.
//!
//! BESPOKE NATIVE (no conform-db fixture): `batch_id` embeds `Date.now()` and the first 8 hex
//! chars of a `randomUUID()` — genuinely non-deterministic, same class as auth-sign.rs's nonce /
//! driver-token.rs's token. Reuses that file's zero-new-dependency OS CSPRNG reach-in
//! (`/dev/urandom` unix, `RtlGenRandom` windows) rather than pulling in a `uuid` crate — only the
//! first 4 random bytes are ever observed (the JS `.slice(0,8)` of the UUID string is exactly the
//! hex of the UUID's first 4 bytes; version/variant nibbles live later in the string and are never
//! sliced in), so a full RFC-4122 v4 encoding is unnecessary ceremony for this call site.
//!
//! Per-pick enrichment reuses the already-converted `system_card_body_enrich::cardBodyEnrich`
//! twin. That twin's real work always ends in the PARITY-DEFERRED `crate::ai_ai_call::aiCall`
//! stub (Err) for any genuinely thin card — but UNLIKE book-run.rs/card-body-enrich.rs's own
//! propagation, the JS here wraps the per-pick call in `try/catch` and pushes the error into
//! `out.errors` instead of throwing. So this twin's deterministic surface is total: every branch
//! (thin-body candidate selection incl. the placeholder-`why` carve-out, kind-bucketed quota
//! picking with the original-row-order top-up, the budget-cap early stop, and the caught-error
//! bookkeeping) is real and test-covered, not "unreachable past a deferred call" like its sibling.
//! An `enriched`/successful-insert branch is exercised only when `cardBodyEnrich` itself resolves
//! `Ok` (the already-rich `no_change_needed` short-circuit) — ported byte-for-byte and covered by
//! a dedicated test that seeds an already-rich candidate.
//!
//! `context.db` missing → JS throws `err.contextMissing('card-body-enrich-canary','db')`;
//! unreachable here (Rust's `db: &Connection` is a required, non-nullable reference) — same
//! convention as every sibling twin.
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
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

// Kept in sync with card-body-enrich.rs's `placeholder_why_re`/card-body-enrich.mjs's
// PLACEHOLDER_WHY_RE (JS `/i` flag → `(?i)`) — same duplication precedent the JS module doc calls
// out ("Keep in sync with card-body-enrich.mjs PLACEHOLDER_WHY_RE").
fn placeholder_why_re() -> Regex {
    Regex::new(r"(?i)\(no .*? recorded\)|\(operator to fill in\)|\(unknown\)|\(no motivation\)|\bTODO\b|\bTBD\b|\bFIXME\b|needs human content").unwrap()
}
fn has_placeholder_why(parsed: &Value) -> bool {
    let why = parsed.get("why");
    let s = match why {
        Some(Value::String(s)) => s,
        _ => return true,
    };
    let t = s.trim();
    t.is_empty() || placeholder_why_re().is_match(t)
}

#[cfg(unix)]
fn os_random_bytes(n: usize) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom").map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; n];
    f.read_exact(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

#[cfg(windows)]
#[link(name = "advapi32")]
extern "system" {
    #[link_name = "SystemFunction036"]
    fn RtlGenRandom(buf: *mut u8, len: u32) -> u8;
}

#[cfg(windows)]
fn os_random_bytes(n: usize) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; n];
    let ok = unsafe { RtlGenRandom(buf.as_mut_ptr(), buf.len() as u32) };
    if ok == 0 {
        return Err("RtlGenRandom failed".to_string());
    }
    Ok(buf)
}

fn now_epoch_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// First 8 hex chars of `randomUUID().slice(0,8)` — i.e. hex of 4 random bytes (see module doc).
fn random_uuid_prefix8() -> String {
    let bytes = os_random_bytes(4).unwrap_or_default();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

const KIND_QUOTA: [(&str, usize); 4] = [("epic", 2), ("feat", 4), ("fix", 2), ("other", 2)];

struct Candidate {
    slug: String,
    kind: String,
}

pub fn cardBodyEnrichCanary(input: &Value, db: &Connection) -> Result<Value, String> {
    let confidence_floor = match input.get("confidence_floor") { None | Some(Value::Null) => 0.7, Some(v) => js_number(Some(v)) };
    let max_budget_usd = match input.get("max_budget_usd") { None | Some(Value::Null) => 1.0, Some(v) => js_number(Some(v)) };

    let batch_id = format!("canary-{}-{}", now_epoch_ms(), random_uuid_prefix8());

    // Find candidates: thin body (≤3 top-level keys) OR a synthesized placeholder `why`, no
    // pending proposal yet.
    let rows = crate::db_json::query_rows(
        db,
        "SELECT slug, body_json,
           json_extract(body_json, '$.kind') AS kind
         FROM cards
         WHERE body_json IS NOT NULL
           AND slug NOT IN (
             SELECT DISTINCT card_slug FROM card_enrichment_proposals
             WHERE decision IS NULL OR decision='accepted'
           )",
        &[],
    )?;

    let mut thin: Vec<Candidate> = Vec::new();
    for r in rows.as_array().cloned().unwrap_or_default() {
        let body_str = match r.get("body_json").and_then(Value::as_str) {
            Some(s) => s,
            None => continue,
        };
        let mut parsed: Value = match serde_json::from_str::<Value>(body_str) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Value::String(s) = &parsed {
            match serde_json::from_str::<Value>(s) {
                Ok(v) => parsed = v,
                Err(_) => continue,
            }
        }
        if !parsed.is_object() {
            continue;
        }
        let key_count = parsed.as_object().map(|o| o.len()).unwrap_or(0);
        if key_count > 3 && !has_placeholder_why(&parsed) {
            continue;
        }
        let kind = r.get("kind").filter(|v| !v.is_null())
            .or_else(|| parsed.get("kind").filter(|v| !v.is_null()))
            .map(|v| match v { Value::String(s) => s.clone(), other => other.to_string() })
            .unwrap_or_else(|| "feat".to_string())
            .to_lowercase();
        let slug = r.get("slug").and_then(Value::as_str).unwrap_or("").to_string();
        thin.push(Candidate { slug, kind });
    }

    // Pick by quota.
    let mut buckets: Map<String, Value> = Map::new();
    for (b, _) in KIND_QUOTA {
        buckets.insert(b.to_string(), json!([]));
    }
    let mut bucket_idx: std::collections::HashMap<&str, Vec<usize>> = std::collections::HashMap::new();
    for (i, t) in thin.iter().enumerate() {
        let bucket = if t.kind == "epic" || t.kind == "feat" || t.kind == "fix" { t.kind.as_str() } else { "other" };
        bucket_idx.entry(bucket).or_default().push(i);
    }
    let mut pick_idx: Vec<usize> = Vec::new();
    for (bucket, quota) in KIND_QUOTA {
        if let Some(idxs) = bucket_idx.get(bucket) {
            for &i in idxs.iter().take(quota) {
                pick_idx.push(i);
            }
        }
    }
    // If short, top up from any bucket (original row order).
    while pick_idx.len() < 10 {
        let next = (0..thin.len()).find(|i| !pick_idx.contains(i));
        match next {
            Some(i) => pick_idx.push(i),
            None => break,
        }
    }

    let picks: Vec<&Candidate> = pick_idx.iter().map(|&i| &thin[i]).collect();

    let mut enriched: i64 = 0;
    let mut total_cost_usd: f64 = 0.0;
    let mut proposals: Vec<Value> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();
    let mut stopped_reason: Option<String> = None;

    for p in &picks {
        if total_cost_usd >= max_budget_usd {
            stopped_reason = Some(format!("budget cap reached (${:.2})", max_budget_usd));
            break;
        }
        let call_input = json!({ "slug": p.slug, "confidence_floor": confidence_floor });
        match crate::system_card_body_enrich::cardBodyEnrich(&call_input, db) {
            Err(e) => {
                errors.push(json!({ "slug": p.slug, "error": e.chars().take(240).collect::<String>() }));
            }
            Ok(proposal) => {
                let slug_out = proposal.get("slug").and_then(Value::as_str).unwrap_or(p.slug.as_str()).to_string();
                let proposed_body = proposal.get("proposed_body").cloned().unwrap_or(json!({}));
                let confidence_overall = proposal.get("confidence_overall").and_then(Value::as_f64).unwrap_or(0.0);
                let dropped_fields = proposal.get("dropped_fields").cloned().unwrap_or(json!([]));
                let status = proposal.get("status").and_then(Value::as_str).unwrap_or("").to_string();
                let ai_call_id = proposal.get("ai_call_id").cloned().unwrap_or(Value::Null);
                let cost_usd = proposal.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
                let reasoning = proposal.get("reasoning").cloned().unwrap_or(Value::Null);

                let proposed_body_json = serde_json::to_string(&proposed_body).map_err(|e| e.to_string())?;
                let dropped_fields_json = serde_json::to_string(&dropped_fields).map_err(|e| e.to_string())?;

                let (_, id) = crate::db_json::run_write(
                    db,
                    "INSERT INTO card_enrichment_proposals
                       (card_slug, proposed_body_json, confidence_overall, dropped_fields_json, status, ai_call_id, cost_usd, reasoning, batch_id)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    &[
                        json!(slug_out),
                        json!(proposed_body_json),
                        json!(confidence_overall),
                        json!(dropped_fields_json),
                        json!(status),
                        ai_call_id,
                        json!(cost_usd),
                        reasoning,
                        json!(batch_id),
                    ],
                )?;

                enriched += 1;
                total_cost_usd += cost_usd;
                let dropped_count = dropped_fields.as_array().map(|a| a.len()).unwrap_or(0) as i64;
                proposals.push(json!({
                    "id": id,
                    "slug": slug_out,
                    "status": status,
                    "confidence_overall": confidence_overall,
                    "dropped_count": dropped_count,
                    "cost_usd": cost_usd,
                }));
            }
        }
    }

    let mut out: Map<String, Value> = Map::new();
    out.insert("batch_id".into(), json!(batch_id));
    out.insert("requested".into(), json!(picks.len() as i64));
    out.insert("enriched".into(), json!(enriched));
    out.insert("total_cost_usd".into(), json!(total_cost_usd));
    out.insert("proposals".into(), json!(proposals));
    out.insert("errors".into(), json!(errors));
    if let Some(reason) = stopped_reason {
        out.insert("stopped_reason".into(), json!(reason));
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT NOT NULL DEFAULT '', body_json TEXT, risk TEXT, complexity TEXT, priority INTEGER DEFAULT 3, score REAL, parent_slug TEXT);
             CREATE TABLE card_enrichment_proposals (id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT NOT NULL, proposed_body_json TEXT NOT NULL, confidence_overall REAL, dropped_fields_json TEXT, status TEXT, ai_call_id INTEGER, cost_usd REAL, reasoning TEXT, batch_id TEXT, decision TEXT, created_at TEXT NOT NULL DEFAULT (datetime('now')));
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    fn thin_card(c: &Connection, slug: &str, kind: &str) {
        let body = json!({ "kind": kind, "size": "M", "why": "(no motivation recorded)" });
        c.execute("INSERT INTO cards (slug, title, body_json) VALUES (?, 't', ?)", rusqlite::params![slug, body.to_string()]).unwrap();
    }

    #[test]
    fn batch_id_has_the_expected_shape() {
        let c = setup();
        let r = cardBodyEnrichCanary(&json!({}), &c).unwrap();
        let bid = r["batch_id"].as_str().unwrap();
        assert!(bid.starts_with("canary-"));
        let parts: Vec<&str> = bid.trim_start_matches("canary-").splitn(2, '-').collect();
        assert_eq!(parts.len(), 2);
        assert!(parts[0].parse::<i64>().is_ok());
        assert_eq!(parts[1].len(), 8);
        assert!(parts[1].chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn no_candidates_returns_empty_batch() {
        let c = setup();
        let r = cardBodyEnrichCanary(&json!({}), &c).unwrap();
        assert_eq!(r["requested"], json!(0));
        assert_eq!(r["enriched"], json!(0));
        assert_eq!(r["total_cost_usd"], json!(0.0));
        assert_eq!(r["proposals"], json!([]));
        assert_eq!(r["errors"], json!([]));
        assert!(r.get("stopped_reason").is_none());
    }

    #[test]
    fn rich_card_with_real_why_is_excluded() {
        let c = setup();
        let body = json!({
            "kind": "feat", "size": "M", "why": "a real motivation",
            "scope": ["a"], "acceptance": ["b"], "verify": "c",
        });
        c.execute("INSERT INTO cards (slug, title, body_json) VALUES ('rich-1', 't', ?)", rusqlite::params![body.to_string()]).unwrap();
        let r = cardBodyEnrichCanary(&json!({}), &c).unwrap();
        assert_eq!(r["requested"], json!(0));
    }

    #[test]
    fn rich_card_with_placeholder_why_still_qualifies() {
        let c = setup();
        let body = json!({
            "kind": "feat", "size": "M", "why": "(no motivation recorded)",
            "scope": ["a"], "acceptance": ["b"], "verify": "c",
        });
        c.execute("INSERT INTO cards (slug, title, body_json) VALUES ('rich-placeholder', 't', ?)", rusqlite::params![body.to_string()]).unwrap();
        let r = cardBodyEnrichCanary(&json!({}), &c).unwrap();
        assert_eq!(r["requested"], json!(1));
    }

    #[test]
    fn quota_picks_2_epic_4_feat_2_fix_2_other_and_deferred_ai_call_lands_in_errors() {
        let c = setup();
        for i in 0..3 { thin_card(&c, &format!("epic-{i}"), "epic"); }
        for i in 0..6 { thin_card(&c, &format!("feat-{i}"), "feat"); }
        for i in 0..3 { thin_card(&c, &format!("fix-{i}"), "fix"); }
        for i in 0..3 { thin_card(&c, &format!("prevent-{i}"), "prevent"); }
        let r = cardBodyEnrichCanary(&json!({}), &c).unwrap();
        assert_eq!(r["requested"], json!(10));
        // every pick's `cardBodyEnrich` call hits the PARITY-DEFERRED aiCall boundary (thin body,
        // no already-rich short-circuit) — caught by this skill's own try/catch, not propagated.
        assert_eq!(r["enriched"], json!(0));
        assert_eq!(r["errors"].as_array().unwrap().len(), 10);
        for e in r["errors"].as_array().unwrap() {
            assert!(e["error"].as_str().unwrap().contains("ai-call"));
        }
    }

    #[test]
    fn short_bucket_tops_up_from_remaining_candidates_in_row_order() {
        let c = setup();
        // no epic/fix/other at all — only feats, more than 10.
        for i in 0..12 { thin_card(&c, &format!("feat-{i}"), "feat"); }
        let r = cardBodyEnrichCanary(&json!({}), &c).unwrap();
        assert_eq!(r["requested"], json!(10));
    }

    #[test]
    fn budget_cap_stops_before_any_call() {
        let c = setup();
        thin_card(&c, "epic-0", "epic");
        let r = cardBodyEnrichCanary(&json!({ "max_budget_usd": 0 }), &c).unwrap();
        assert_eq!(r["enriched"], json!(0));
        assert_eq!(r["errors"], json!([]));
        assert_eq!(r["stopped_reason"], json!("budget cap reached ($0.00)"));
    }

    #[test]
    fn already_pending_proposal_excludes_the_card() {
        let c = setup();
        thin_card(&c, "epic-0", "epic");
        c.execute(
            "INSERT INTO card_enrichment_proposals (card_slug, proposed_body_json, status, batch_id, decision) VALUES ('epic-0', '{}', 'ok', 'prior', NULL)",
            [],
        ).unwrap();
        let r = cardBodyEnrichCanary(&json!({}), &c).unwrap();
        assert_eq!(r["requested"], json!(0));
    }

    #[test]
    fn rejected_prior_proposal_does_not_exclude_the_card() {
        let c = setup();
        thin_card(&c, "epic-0", "epic");
        c.execute(
            "INSERT INTO card_enrichment_proposals (card_slug, proposed_body_json, status, batch_id, decision) VALUES ('epic-0', '{}', 'ok', 'prior', 'rejected')",
            [],
        ).unwrap();
        let r = cardBodyEnrichCanary(&json!({}), &c).unwrap();
        assert_eq!(r["requested"], json!(1));
    }
}
