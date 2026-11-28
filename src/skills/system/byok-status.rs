//! byok-status.rs — twin of skills/system/byok-status.mjs. NAMES ARE CONTRACT:
//! `curateCatalog` (named export, pure/no-db) and `byokStatus` (default export).
//!
//! ## The catalog-sync boundary (the one scoped divergence, disclosed not dropped)
//! JS opens with an optional refresh of OpenRouter's LIVE `/models` catalog:
//! `fetchFn = context.fetch_fn ?? (skipSync ? null : globalThis.fetch)`, and when a fetch
//! function exists it materializes the curated result into `brain.ai_providers`. Two of those
//! three inputs do not exist here: this twin has no `context.fetch_fn` injection point, and the
//! crate's `ureq` is built `default-features = false` (no TLS — see Cargo.toml, the local-qwen
//! endpoint is plain http), so an `https://openrouter.ai` GET is not expressible today.
//!
//! This twin therefore behaves as the **`ANTILOKI_BYOK_SKIP_CATALOG_SYNC=1` lane always**: it
//! reads and reports whatever candidate rows already exist. That is a REAL, supported JS branch,
//! not an invented one — JS takes the identical path under that env var (CI / e2e / offline), and
//! its own doc says "Network failure degrades gracefully: whatever rows already exist are kept and
//! returned". Everything downstream of the fetch — the per-tier active-provider read, the
//! provider-agnostic model listing with its exact ORDER BY, `deriveStrategy`, `is_local`,
//! `failover_rank` defaulting, and the key-presence probe — is fully ported and fixture-tested.
//! Wiring the sync is a one-function change once `ureq`'s `tls` feature is enabled (`materialize`
//! is the only piece still missing, and `curateCatalog` that feeds it is already converted).
//!
//! `openrouter_key_present` mirrors JS's `existsSync(OPENROUTER_KEY_PATH)` — note JS's own
//! `readKey()` also honors `OPENROUTER_API_KEY`, but the RETURNED field is the file probe only,
//! so this twin probes the file only too.
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

const TIERS: [&str; 3] = ["low", "mid", "high"];
const ALL_TIERS: [&str; 4] = ["low", "mid", "high", "image"];

// JS Number(v)
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse::<f64>().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
    }
}
// integer-valued f64 renders as a JSON integer (JS numbers carry no int/float distinction — gotcha #2)
fn num_to_json(f: f64) -> Value {
    if f.fract() == 0.0 && f.abs() < 9e15 { json!(f as i64) } else { json!(f) }
}

// OpenRouter pricing fields are USD-per-token strings. Convert to USD-per-1M, rounded to 4dp.
// Returns Null for missing/unparseable values (mirrors priceToPer1M's `return null`).
fn price_to_per1m(v: Option<&Value>) -> Value {
    let n = js_number(v);
    if !n.is_finite() { return Value::Null; }
    num_to_json((n * 1e6 * 1e4).round() / 1e4)
}

struct Rule { re: &'static str, tier: &'static str }
const CURATION: &[Rule] = &[
    Rule { re: r"deepseek/.*(distill|lite)", tier: "low" },
    Rule { re: r"deepseek/.*(flash|chat|v3)", tier: "mid" },
    Rule { re: r"deepseek/", tier: "high" },
    Rule { re: r"anthropic/.*opus", tier: "high" },
    Rule { re: r"anthropic/.*sonnet", tier: "mid" },
    Rule { re: r"anthropic/.*haiku", tier: "low" },
    Rule { re: r"google/.*(flash-lite|flash-8b)", tier: "low" },
    Rule { re: r"google/.*pro", tier: "high" },
    Rule { re: r"google/.*flash", tier: "mid" },
    Rule { re: r"openai/.*(mini|nano)", tier: "low" },
    Rule { re: r"openai/(gpt-[5-9]|o[0-9])", tier: "high" },
    Rule { re: r"openai/gpt-4", tier: "mid" },
    Rule { re: r"qwen/.*(coder|1\.5b|3b|7b|14b|32b|small)", tier: "low" },
    Rule { re: r"qwen/.*(max|plus|next|235b|72b)", tier: "mid" },
    Rule { re: r"meta-llama/.*(1b|3b|8b)", tier: "low" },
    Rule { re: r"meta-llama/.*(70b|405b)", tier: "mid" },
];
fn curation_regexes() -> &'static Vec<Regex> {
    static R: OnceLock<Vec<Regex>> = OnceLock::new();
    R.get_or_init(|| CURATION.iter().map(|r| Regex::new(r.re).unwrap()).collect())
}
fn low_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(nano|mini|lite|small|tiny|distill|:free|flash-8b|flash-lite|[-/]1b|[-/]3b|[-/]7b|[-/]8b|1\.5b|guard)").unwrap())
}
fn high_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(opus|ultra|-pro\b|pro-|reason|thinking|[-/]o[1-4]\b|gpt-5|grok-[3-9]|[-/](70|72|120|235|405|480|671)b|-max\b|max-)").unwrap())
}

fn tier_by_heuristic(lid: &str, pricing: Option<&Value>) -> &'static str {
    if low_re().is_match(lid) { return "low"; }
    if high_re().is_match(lid) { return "high"; }
    let out = js_number(pricing.and_then(|p| p.get("completion"))) * 1e6;
    if out.is_finite() {
        if out <= 0.5 { return "low"; }
        if out >= 8.0 { return "high"; }
    }
    "mid"
}

// Pure: raw OpenRouter /models data[] → { low:[], mid:[], high:[], image:[] }. NAMES ARE
// CONTRACT with skills/system/byok-status.mjs's curateCatalog.
pub fn curateCatalog(raw_models: &Value) -> Value {
    let mut low = Vec::new();
    let mut mid = Vec::new();
    let mut high = Vec::new();
    let mut image = Vec::new();
    let arr = match raw_models.as_array() { Some(a) => a, None => {
        return json!({ "low": [], "mid": [], "high": [], "image": [] });
    } };
    let regexes = curation_regexes();
    for m in arr {
        let id = match m.get("id").and_then(|v| v.as_str()) { Some(s) => s, None => continue };
        let lid = id.to_lowercase();
        let out_mods = m.get("architecture").and_then(|a| a.get("output_modalities")).and_then(|v| v.as_array());
        let is_image = out_mods.map_or(false, |a| a.iter().any(|x| x.as_str() == Some("image")));
        let tier = if is_image {
            "image"
        } else {
            CURATION.iter().zip(regexes.iter())
                .find(|(_, re)| re.is_match(&lid))
                .map(|(r, _)| r.tier)
                .unwrap_or_else(|| tier_by_heuristic(&lid, m.get("pricing")))
        };
        let pricing = m.get("pricing");
        let mut entry = Map::new();
        entry.insert("model_id".into(), json!(id));
        entry.insert("cost_in".into(), price_to_per1m(pricing.and_then(|p| p.get("prompt"))));
        entry.insert("cost_out".into(), price_to_per1m(pricing.and_then(|p| p.get("completion"))));
        let ctx = js_number(m.get("context_length"));
        entry.insert("max_context".into(), if ctx.is_finite() { num_to_json(ctx) } else { Value::Null });
        let row = Value::Object(entry);
        match tier {
            "low" => low.push(row),
            "mid" => mid.push(row),
            "high" => high.push(row),
            _ => image.push(row),
        }
    }
    json!({ "low": low, "mid": mid, "high": high, "image": image })
}

/// JS `deriveStrategy(active)` — 'byok' only when EVERY non-null tier provider is openrouter.
/// An empty set is 'mixed' (JS returns early on `!providers.length`). Note it iterates TIERS
/// (3), not ALL_TIERS — 'image' never influences the strategy label.
fn derive_strategy(active: &Map<String, Value>) -> &'static str {
    let providers: Vec<&str> = TIERS
        .iter()
        .filter_map(|t| active.get(*t).and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .collect();
    if providers.is_empty() {
        return "mixed";
    }
    if providers.iter().all(|p| *p == "openrouter") {
        "byok"
    } else {
        "mixed"
    }
}

/// `export default async function byokStatus(input, context)`.
pub fn byokStatus(_input: &Value, db: &Connection) -> Result<Value, String> {
    // Catalog sync is the ANTILOKI_BYOK_SKIP_CATALOG_SYNC=1 lane here — see the module doc.

    // active provider per tier: first ACTIVE row by id.
    let mut active = Map::new();
    for t in ALL_TIERS {
        let rows = crate::db_json::query_rows(
            db,
            // the same order ai-call dispatches in (2026-09-19): rank, then cli · ollama · openrouter
            "SELECT provider FROM brain.ai_providers WHERE tier = ? AND active = 1 AND provider IN ('cli','ollama','openrouter') \
             ORDER BY failover_rank ASC, CASE provider WHEN 'cli' THEN 0 WHEN 'ollama' THEN 1 ELSE 2 END, id LIMIT 1",
            &[json!(t)],
        )
        .map_err(|e| format!("byok-status: {e}"))?;
        let v = rows
            .as_array()
            .and_then(|a| a.first())
            .and_then(|r| r.get("provider").cloned())
            .unwrap_or(Value::Null);
        active.insert(t.to_string(), v);
    }

    // Every registered model per tier. Non-openrouter (hand-registered local) rows sort FIRST so
    // they aren't buried under the live catalog — the `(provider = 'openrouter') ASC` key.
    let mut models = Map::new();
    for t in ALL_TIERS {
        let rows = crate::db_json::query_rows(
            db,
            "SELECT provider, model_id, cost_per_1m_input_tokens_usd AS cost_in, \
                    cost_per_1m_output_tokens_usd AS cost_out, active, failover_rank \
             FROM brain.ai_providers \
             WHERE tier = ? \
             ORDER BY CASE provider WHEN 'cli' THEN 0 WHEN 'ollama' THEN 1 WHEN 'openrouter' THEN 2 ELSE 3 END, cost_per_1m_output_tokens_usd DESC, model_id ASC",
            &[json!(t)],
        )
        .map_err(|e| format!("byok-status: {e}"))?;
        let list: Vec<Value> = rows
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|r| {
                let provider = r.get("provider").cloned().unwrap_or(Value::Null);
                let is_local = provider.as_str().map(|p| p != "openrouter").unwrap_or(true);
                // KIND (2026-09-19): subscription (a coding CLI you already pay for) · local (Ollama, $0) ·
                // byok (OpenRouter, metered) · legacy (a provider this runtime no longer dispatches)
                let kind = match provider.as_str().unwrap_or("") { "cli" => "subscription", "ollama" | "local" => "local", "openrouter" => "byok", _ => "legacy" };
                json!({
                    "kind": kind,
                    "provider": provider,
                    "model_id": r.get("model_id").cloned().unwrap_or(Value::Null),
                    "cost_in": r.get("cost_in").cloned().unwrap_or(Value::Null),
                    "cost_out": r.get("cost_out").cloned().unwrap_or(Value::Null),
                    // JS: `r.active === 1` — a strict compare, so anything else is false.
                    "active": r.get("active").and_then(Value::as_i64) == Some(1),
                    // JS: `r.failover_rank ?? 0` — nullish, so 0 survives.
                    "failover_rank": match r.get("failover_rank") {
                        Some(Value::Null) | None => json!(0),
                        Some(v) => v.clone(),
                    },
                    "is_local": is_local,
                })
            })
            .collect();
        models.insert(t.to_string(), Value::Array(list));
    }

    let key_path = crate::engine_roots::auth_dir().join("openrouter.key");
    Ok(json!({
        "openrouter_key_present": key_path.exists(),
        "active": Value::Object(active.clone()),
        "strategy": derive_strategy(&active),
        "models": Value::Object(models),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_array_returns_empty_tiers() {
        assert_eq!(curateCatalog(&json!(null)), json!({ "low": [], "mid": [], "high": [], "image": [] }));
    }

    #[test]
    fn curates_and_tiers_models() {
        let raw = json!([
            { "id": "anthropic/claude-opus", "pricing": { "prompt": "0.000015", "completion": "0.000075" }, "context_length": 200000 },
            { "id": "anthropic/claude-sonnet", "pricing": { "prompt": "0.000003", "completion": "0.000015" }, "context_length": 200000 },
            { "id": "openai/gpt-4o-mini", "pricing": { "prompt": "0.00000015", "completion": "0.0000006" }, "context_length": 128000 },
            { "id": "some/unknown-flagship-pro", "pricing": { "prompt": "0.00001", "completion": "0.00002" } },
            { "id": "vendor/image-gen", "architecture": { "output_modalities": ["image"] }, "pricing": {} },
            { "id": 42 },
            { "no_id": true },
        ]);
        let out = curateCatalog(&raw);
        assert_eq!(out["high"].as_array().unwrap().len(), 2); // opus (rule) + unknown-flagship-pro (heuristic: -pro\b + $20/1M out)
        assert_eq!(out["mid"].as_array().unwrap().len(), 1); // sonnet
        assert_eq!(out["low"].as_array().unwrap().len(), 1); // gpt-4o-mini
        assert_eq!(out["image"].as_array().unwrap().len(), 1);
        assert_eq!(out["high"][0], json!({ "model_id": "anthropic/claude-opus", "cost_in": 15, "cost_out": 75, "max_context": 200000 }));
        assert_eq!(out["low"][0]["cost_in"], json!(0.15));
    }

    #[test]
    fn price_to_per1m_handles_missing_and_zero() {
        assert_eq!(price_to_per1m(None), Value::Null);
        assert_eq!(price_to_per1m(Some(&json!("not-a-number"))), Value::Null);
        assert_eq!(price_to_per1m(Some(&json!("0"))), json!(0));
    }

    // ── byokStatus (default export) ───────────────────────────────────────────────────────────

    fn db_with(rows: &str) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.ai_providers (
               id INTEGER PRIMARY KEY AUTOINCREMENT, tier TEXT, provider TEXT, model_id TEXT,
               cost_per_1m_input_tokens_usd REAL, cost_per_1m_output_tokens_usd REAL,
               active INTEGER, failover_rank INTEGER);",
        )
        .unwrap();
        if !rows.is_empty() {
            c.execute_batch(rows).unwrap();
        }
        c
    }

    #[test]
    fn empty_providers_yields_null_actives_and_mixed_strategy() {
        let c = db_with("");
        let r = byokStatus(&json!({}), &c).unwrap();
        for t in ALL_TIERS {
            assert_eq!(r["active"][t], Value::Null, "tier {t}");
            assert_eq!(r["models"][t].as_array().unwrap().len(), 0);
        }
        // no providers at all -> JS returns 'mixed' on the !providers.length early return
        assert_eq!(r["strategy"], json!("mixed"));
    }

    #[test]
    fn all_openrouter_actives_derive_byok() {
        let c = db_with(
            "INSERT INTO brain.ai_providers (tier,provider,model_id,cost_per_1m_input_tokens_usd,cost_per_1m_output_tokens_usd,active,failover_rank) VALUES
               ('low','openrouter','a/x',1,2,1,0),
               ('mid','openrouter','a/y',3,4,1,0),
               ('high','openrouter','a/z',5,6,1,0);",
        );
        let r = byokStatus(&json!({}), &c).unwrap();
        assert_eq!(r["strategy"], json!("byok"));
        assert_eq!(r["active"]["low"], json!("openrouter"));
        // 'image' has no row -> null, and is excluded from the strategy decision
        assert_eq!(r["active"]["image"], Value::Null);
    }

    #[test]
    fn one_non_openrouter_active_makes_it_mixed() {
        let c = db_with(
            "INSERT INTO brain.ai_providers (tier,provider,model_id,active,failover_rank) VALUES
               ('low','ollama','qwen3.5:9b',1,0),
               ('mid','openrouter','a/y',1,0);",
        );
        let r = byokStatus(&json!({}), &c).unwrap();
        assert_eq!(r["strategy"], json!("mixed"));
    }

    #[test]
    fn local_rows_sort_before_openrouter_then_by_cost_out_desc_then_model_id() {
        let c = db_with(
            "INSERT INTO brain.ai_providers (tier,provider,model_id,cost_per_1m_input_tokens_usd,cost_per_1m_output_tokens_usd,active,failover_rank) VALUES
               ('low','openrouter','or/cheap',1,1,1,0),
               ('low','openrouter','or/pricey',1,9,0,0),
               ('low','ollama','local/b',0,0,0,0),
               ('low','ollama','local/a',0,0,0,0);",
        );
        let r = byokStatus(&json!({}), &c).unwrap();
        let ids: Vec<&str> = r["models"]["low"].as_array().unwrap().iter().map(|m| m["model_id"].as_str().unwrap()).collect();
        // both locals first (cost_out equal -> model_id ASC), then openrouter by cost_out DESC
        assert_eq!(ids, vec!["local/a", "local/b", "or/pricey", "or/cheap"]);
        assert_eq!(r["models"]["low"][0]["is_local"], json!(true));
        assert_eq!(r["models"]["low"][3]["is_local"], json!(false));
    }

    #[test]
    fn active_is_a_strict_one_compare_and_failover_rank_defaults_to_zero() {
        let c = db_with(
            "INSERT INTO brain.ai_providers (tier,provider,model_id,active,failover_rank) VALUES
               ('mid','ollama','m/a',1,NULL),
               ('mid','ollama','m/b',0,2),
               ('mid','ollama','m/c',NULL,NULL);",
        );
        let r = byokStatus(&json!({}), &c).unwrap();
        let by = |id: &str| {
            r["models"]["mid"].as_array().unwrap().iter().find(|m| m["model_id"] == json!(id)).cloned().unwrap()
        };
        assert_eq!(by("m/a")["active"], json!(true));
        assert_eq!(by("m/a")["failover_rank"], json!(0)); // NULL -> 0 via ??
        assert_eq!(by("m/b")["active"], json!(false));
        assert_eq!(by("m/b")["failover_rank"], json!(2)); // a real 2 survives
        assert_eq!(by("m/c")["active"], json!(false)); // NULL is not === 1
    }

    #[test]
    fn shape_has_exactly_the_four_documented_keys() {
        let c = db_with("");
        let r = byokStatus(&json!({}), &c).unwrap();
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["openrouter_key_present", "active", "strategy", "models"]);
        assert!(r["openrouter_key_present"].is_boolean());
    }
}
