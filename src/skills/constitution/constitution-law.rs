//! constitution-law.rs — twin of skills/constitution/constitution-law.mjs (db lane, cross-db
//! `laws`+`judgments`). NAMES ARE CONTRACT: `resolveFrameworks` (db-first helper), `composedConstitution`
//! (db-first helper), `constitutionLaw` (default). THE CONSTITUTION's management surface
//! (docs/constitution-review-plan.html D1/D2/N5): one op-dispatched skill carries the whole panel —
//! list / seed / catalog / enable / add / ratify / dismiss / remove / suspend / unsuspend / reweight /
//! except / unexcept.
//!
//! `resolveFrameworks` calls the already-converted `codebase_stack_facts::stackFacts` twin (a real
//! fs read, fixture-controllable via a seeded `workspaces` row — see stack-facts.rs's module doc);
//! any failure (no manifest, missing workspace) collapses to the JS try/catch's fallback of just
//! `['node']`, modeled here via `if let Ok(..)`.
//!
//! `composedConstitution` is the REAL twin of the named export other units (constitution-estimate.rs)
//! had to INLINE a private copy of before this module existed — see that file's module doc for the
//! cross-module-inlining rationale. This is the canonical, `pub` version.
//!
//! `updated_at=${now}` where JS's `now` is the literal string `"datetime('now')"` spliced into the
//! SQL text: frozen per the datetime lane to `datetime('2026-01-01 00:00:00')` everywhere it appears
//! (ratify/suspend/unsuspend/reweight/add's ON CONFLICT UPDATE/seed's heal UPDATE).
//!
//! Not modeled: `context.db` missing (Rust's `db: &Connection` is non-nullable — same convention as
//! every sibling twin) and `add`'s `if (!SCOPES.includes(scope))` guard, which is provably dead code
//! here — `normalizeScope` (schema.rs) always returns one of the 6 canonical SCOPES values by
//! construction, so the JS guard can never actually fire.
// QUALITY: 80/100 — a 13-op CRUD-ish dispatcher; every op is a direct 1:1 SQL port, the only real
// logic is composedConstitution's sort/normalize and resolveFrameworks' fs-backed lookup.

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } } // = JS String(v)
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

// `Math.max(0, Math.min(10, x))` — NaN propagates (Rust's f64::max/min would instead discard NaN).
fn clamp_0_10(x: f64) -> f64 { if x.is_nan() { f64::NAN } else { x.max(0.0).min(10.0) } }

// `try { JSON.parse(s ?? 'null') } catch { null }` — an absent/NULL column and a malformed-JSON
// column both collapse to `null`; only well-formed JSON text survives.
fn safe(s: Option<&Value>) -> Value {
    let text = s.and_then(Value::as_str).unwrap_or("null");
    serde_json::from_str(text).unwrap_or(Value::Null)
}

// `String(input?.key ?? '').trim().toLowerCase().replace(/[^a-z0-9]+/g,'-').replace(/^-|-$/g,'')`
fn slug_key(v: Option<&Value>) -> String {
    let raw = str_qq(v).trim().to_lowercase();
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"[^a-z0-9]+").unwrap());
    re.replace_all(&raw, "-").trim_matches('-').to_string()
}

// same order as constitution/_helpers/schema.mjs's SCOPES (schema.rs's copy is private, so this
// unit carries its own — see constitution-estimate.rs's identical precedent).
const SCOPES: [&str; 6] = ["business", "architecture", "project", "framework", "community", "language"];

// map stack-facts' display framework -> the tokens laws match on (verbatim from JS's FW_TOKENS).
const FW_TOKENS: &[(&str, &[&str])] = &[
    ("React", &["react"]), ("Next.js", &["react", "node"]), ("Remix", &["react", "node"]), ("Gatsby", &["react"]),
    ("NestJS", &["node", "nestjs"]), ("Express", &["node", "express"]), ("Fastify", &["node"]), ("Koa", &["node"]),
    ("Vue", &["vue"]), ("Nuxt", &["vue", "node"]), ("Angular", &["angular"]), ("Svelte", &["svelte"]),
];

/// `export function resolveFrameworks(db)` — always includes 'node' (the safe JS/TS default), plus
/// the tokens FW_TOKENS maps the detected stack-facts framework to. Swallows any stackFacts failure
/// (no manifest, no active workspace) exactly like the JS try/catch, falling back to just `['node']`.
pub fn resolveFrameworks(db: &Connection) -> Value {
    let mut set: Vec<String> = vec!["node".to_string()];
    if let Ok(sf) = crate::codebase_stack_facts::stackFacts(&json!({}), db) {
        let framework = sf.get("facts").and_then(|f| f.get("framework")).and_then(Value::as_str);
        if let Some(fw) = framework {
            for entry in FW_TOKENS {
                if entry.0 != fw { continue; }
                for t in entry.1 {
                    if !set.iter().any(|s| s.as_str() == *t) { set.push((*t).to_string()); }
                }
                break;
            }
        }
    }
    json!(set)
}

/// `export function composedConstitution(db)` — self-heals the schema, then the full
/// laws.review_laws ledger (scope normalized) sorted by SCOPES order -> weight desc -> key asc,
/// plus laws.review_law_exceptions ordered by law_key. Returns `{ laws, exceptions }`.
pub fn composedConstitution(db: &Connection) -> Value {
    let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(db);
    let mut laws: Vec<Value> = crate::db_json::query_rows(db, "SELECT * FROM laws.review_laws", &[])
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    for l in laws.iter_mut() {
        if let Value::Object(map) = l {
            let normalized = crate::constitution__helpers_schema::normalizeScope(map.get("scope").unwrap_or(&Value::Null));
            map.insert("scope".to_string(), normalized);
        }
    }
    laws.sort_by(|a, b| {
        let ai = SCOPES.iter().position(|s| Some(*s) == a.get("scope").and_then(Value::as_str)).map(|x| x as i64).unwrap_or(-1);
        let bi = SCOPES.iter().position(|s| Some(*s) == b.get("scope").and_then(Value::as_str)).map(|x| x as i64).unwrap_or(-1);
        ai.cmp(&bi)
            .then_with(|| {
                let aw = a.get("weight").and_then(Value::as_i64).unwrap_or(0);
                let bw = b.get("weight").and_then(Value::as_i64).unwrap_or(0);
                bw.cmp(&aw)
            })
            .then_with(|| {
                let ak = a.get("key").and_then(Value::as_str).unwrap_or("");
                let bk = b.get("key").and_then(Value::as_str).unwrap_or("");
                ak.cmp(bk)
            })
    });
    let exceptions: Vec<Value> = crate::db_json::query_rows(db, "SELECT * FROM laws.review_law_exceptions ORDER BY law_key", &[])
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    json!({ "laws": laws, "exceptions": exceptions })
}

/// `export default function constitutionLaw(input, context)`
pub fn constitutionLaw(input: &Value, db: &Connection) -> Result<Value, String> {
    let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(db);
    let op_val = nz(input.get("op")).cloned().unwrap_or_else(|| json!("list"));
    let op = js_string(&op_val);

    if op == "list" {
        let composed = composedConstitution(db);
        let laws = composed.get("laws").and_then(Value::as_array).cloned().unwrap_or_default();
        let exceptions = composed.get("exceptions").cloned().unwrap_or_else(|| json!([]));
        let frameworks = resolveFrameworks(db);

        let mut counts = Map::new();
        for l in &laws {
            let status = l.get("status").and_then(Value::as_str).unwrap_or("").to_string();
            let n = counts.get(&status).and_then(Value::as_i64).unwrap_or(0) + 1;
            counts.insert(status, json!(n));
        }

        let laws_out: Vec<Value> = laws
            .iter()
            .map(|l| {
                let mut m = l.as_object().cloned().unwrap_or_default();
                m.insert("applies".into(), safe(l.get("applies_json")));
                m.insert("config".into(), safe(l.get("config_json")));
                m.insert("evidence".into(), safe(l.get("evidence_json")));
                Value::Object(m)
            })
            .collect();

        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("frameworks".into(), frameworks);
        out.insert("laws".into(), json!(laws_out));
        out.insert("exceptions".into(), exceptions);
        out.insert("counts".into(), Value::Object(counts));
        return Ok(Value::Object(out));
    }

    if op == "seed" {
        let pick: Option<HashSet<String>> = input.get("keys").and_then(Value::as_array).map(|arr| arr.iter().map(js_string).collect());
        let seed_arr = crate::constitution__helpers_seed_laws::SEED_LAWS.as_array().cloned().unwrap_or_default();
        let mut added: i64 = 0;
        // UPGRADE ONLY (2026-10-06): onboarding seeds an EMPTY table, so a corrected seed law (the secrets law learned secret-shaped values) never reached a workspace that
        // already had its laws. Re-seeding everything is not the answer: `remove` hard-deletes, and `INSERT OR IGNORE` would bring back a law the operator deleted. This flag
        // runs only the upgrade below - rows the seed itself wrote, whose definition moved - and inserts nothing.
        let upgrade_only = input.get("upgrade_only").and_then(Value::as_bool).unwrap_or(false);
        for l in seed_arr.iter().filter(|_| !upgrade_only) {
            let key = l.get("key").and_then(Value::as_str).unwrap_or("").to_string();
            if let Some(p) = &pick {
                if !p.contains(&key) { continue; }
            }
            let config_v = nz(l.get("config_json")).cloned().unwrap_or_else(|| json!({}));
            let config_str = serde_json::to_string(&config_v).unwrap();
            let severity_v = nz(l.get("severity")).cloned().unwrap_or_else(|| json!("warn"));
            let confidence_v = match l.get("confidence") {
                None | Some(Value::Null) => json!(0.9),
                Some(v) => v.clone(),
            };
            let enforcement_v = nz(l.get("enforcement")).cloned().unwrap_or_else(|| json!("score"));
            let applies_v = nz(l.get("applies_json")).cloned().unwrap_or_else(|| json!({}));
            let applies_str = serde_json::to_string(&applies_v).unwrap();
            let (changes, _) = crate::db_json::run_write(
                db,
                "INSERT OR IGNORE INTO laws.review_laws
                   (scope, key, title, rule_md, detector, config_json, weight, severity, confidence, enforcement, applies_json, status, created_by)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'ratified', 'seed')",
                &[
                    l.get("scope").cloned().unwrap_or(Value::Null),
                    l.get("key").cloned().unwrap_or(Value::Null),
                    l.get("title").cloned().unwrap_or(Value::Null),
                    l.get("rule_md").cloned().unwrap_or(Value::Null),
                    l.get("detector").cloned().unwrap_or(Value::Null),
                    json!(config_str),
                    l.get("weight").cloned().unwrap_or(Value::Null),
                    severity_v,
                    confidence_v,
                    enforcement_v,
                    json!(applies_str),
                ],
            )?;
            added += changes as i64;
        }
        // UPGRADE a seed law whose definition moved. `INSERT OR IGNORE` above never touches a
        // workspace that already has the row, so a corrected query (2026-09-12: `no-loose-equality`
        // learned `allow-null`) would otherwise reach only workspaces onboarded after the fix.
        // Only rows the seed itself wrote are touched (`created_by='seed'`), and only the parts
        // the seed defines — config, rule text, detector; an operator's `reweight`/`suspend`/
        // exceptions live in other columns and survive. `version` bumps so findings cite key@v2.
        let mut upgraded: i64 = 0;
        for l in &seed_arr {
            let key = l.get("key").and_then(Value::as_str).unwrap_or("").to_string();
            if let Some(p) = &pick {
                if !p.contains(&key) { continue; }
            }
            let config_str = serde_json::to_string(&nz(l.get("config_json")).cloned().unwrap_or_else(|| json!({}))).unwrap();
            let rule_md = js_string(&l.get("rule_md").cloned().unwrap_or(Value::Null));
            let detector = js_string(&l.get("detector").cloned().unwrap_or(Value::Null));
            let (changes, _) = crate::db_json::run_write(
                db,
                &format!("UPDATE laws.review_laws SET config_json=?, rule_md=?, detector=?, version=version+1, updated_at={} \
                          WHERE key=? AND created_by='seed' AND (config_json<>? OR rule_md<>? OR detector<>?)", crate::clock::now_sql()),
                &[json!(config_str), json!(rule_md), json!(detector), json!(key), json!(config_str), json!(rule_md), json!(detector)],
            )?;
            upgraded += changes as i64;
        }
        if upgrade_only { return Ok(json!({ "ok": true, "added": 0, "upgraded": upgraded, "upgrade_only": true })); }
        if let Some(p) = &pick {
            return Ok(json!({ "ok": true, "added": added, "upgraded": upgraded, "total": p.len() as i64 }));
        }
        // HEAL a drifted workspace: re-ratify suspended seed laws iff NOTHING is ratified. Since a
        // fresh insert above always lands with status='ratified', this only fires when every
        // SEED_LAWS key already pre-existed in a non-ratified state — a deep, rare edge; ported
        // faithfully but not fixture-exercised (disclosed-untested, per the git-diff-branch
        // precedent in constitution-estimate.rs). Covered instead by the JS behavior test.
        let mut healed: i64 = 0;
        let ratified: i64 = crate::db_json::query_rows(db, "SELECT COUNT(*) n FROM laws.review_laws WHERE status='ratified'", &[])?
            .as_array()
            .and_then(|a| a.first())
            .and_then(|r| r.get("n"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if ratified == 0 {
            let (changes, _) = crate::db_json::run_write(
                db,
                &format!("UPDATE laws.review_laws SET status='ratified', updated_at={} WHERE created_by='seed' AND status='suspended'", crate::clock::now_sql()),
                &[],
            )?;
            healed = changes as i64;
        }
        return Ok(json!({ "ok": true, "added": added, "upgraded": upgraded, "healed": healed, "total": seed_arr.len() as i64 }));
    }

    if op == "add" {
        let key = slug_key(input.get("key"));
        let scope_raw = nz(input.get("scope")).cloned().unwrap_or_else(|| json!("business"));
        let scope_v = crate::constitution__helpers_schema::normalizeScope(&json!(js_string(&scope_raw)));
        let scope = scope_v.as_str().unwrap_or("project").to_string();
        if key.is_empty() {
            return Err("constitution-law: key is required".to_string());
        }
        let detector = match nz(input.get("detector")) {
            Some(v) => js_string(v),
            None => "none".to_string(),
        };
        let title = match nz(input.get("title")) {
            Some(v) => js_string(v),
            None => key.clone(),
        };
        let rule_md = str_qq(input.get("rule_md"));
        let config_v = nz(input.get("config")).cloned().unwrap_or_else(|| json!({}));
        let config_str = serde_json::to_string(&config_v).unwrap();
        let weight_raw = nz(input.get("weight")).cloned().unwrap_or_else(|| json!(5));
        let weight = clamp_0_10(js_number(Some(&weight_raw)));
        let severity = match nz(input.get("severity")) {
            Some(v) => js_string(v),
            None => "warn".to_string(),
        };
        let confidence: f64 = match input.get("confidence") {
            None | Some(Value::Null) => 1.0,
            Some(v) => js_number(Some(v)),
        };
        let enforcement = if detector == "none" {
            "advise".to_string()
        } else {
            match nz(input.get("enforcement")) {
                Some(v) => js_string(v),
                None => "score".to_string(),
            }
        };
        let applies_v = nz(input.get("applies")).cloned().unwrap_or_else(|| json!({}));
        let applies_str = serde_json::to_string(&applies_v).unwrap();

        crate::db_json::run_write(
            db,
            &format!("INSERT INTO laws.review_laws (scope, key, title, rule_md, detector, config_json, weight, severity, confidence, enforcement, applies_json, status, created_by)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'ratified', 'operator')
             ON CONFLICT(key) DO UPDATE SET title=excluded.title, rule_md=excluded.rule_md, detector=excluded.detector,
               config_json=excluded.config_json, weight=excluded.weight, severity=excluded.severity, confidence=excluded.confidence,
               applies_json=excluded.applies_json, version=version+1, updated_at={}", crate::clock::now_sql()),
            &[
                json!(scope.clone()),
                json!(key.clone()),
                json!(title),
                json!(rule_md),
                json!(detector),
                json!(config_str),
                json!(weight),
                json!(severity),
                json!(confidence),
                json!(enforcement),
                json!(applies_str),
            ],
        )?;
        return Ok(json!({ "ok": true, "key": key, "scope": scope, "authored": true }));
    }

    if op == "catalog" {
        let have: HashSet<String> = crate::db_json::query_rows(db, "SELECT key FROM laws.review_laws", &[])?
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|r| r.get("key").and_then(Value::as_str).map(|s| s.to_string()))
            .collect();
        let catalog_arr = crate::constitution__helpers_law_catalog::LAW_CATALOG.as_array().cloned().unwrap_or_default();
        let source_obj = crate::constitution__helpers_law_catalog::CATALOG_SOURCE.as_object().cloned().unwrap_or_default();
        let catalog: Vec<Value> = catalog_arr
            .iter()
            .map(|c| {
                let key_str = c.get("key").and_then(Value::as_str).unwrap_or("").to_string();
                let mut m = Map::new();
                m.insert("key".into(), c.get("key").cloned().unwrap_or(Value::Null));
                m.insert("scope".into(), c.get("scope").cloned().unwrap_or(Value::Null));
                m.insert("title".into(), c.get("title").cloned().unwrap_or(Value::Null));
                m.insert("rule_md".into(), c.get("rule_md").cloned().unwrap_or(Value::Null));
                m.insert("detector".into(), c.get("detector").cloned().unwrap_or(Value::Null));
                m.insert("weight".into(), c.get("weight").cloned().unwrap_or(Value::Null));
                m.insert("severity".into(), c.get("severity").cloned().unwrap_or(Value::Null));
                m.insert("confidence".into(), c.get("confidence").cloned().unwrap_or(Value::Null));
                m.insert("source".into(), source_obj.get(&key_str).cloned().unwrap_or(Value::Null));
                m.insert("installed".into(), json!(have.contains(&key_str)));
                Value::Object(m)
            })
            .collect();
        return Ok(json!({ "ok": true, "catalog": catalog }));
    }

    if op == "enable" {
        let keys: Vec<String> = if let Some(arr) = input.get("keys").and_then(Value::as_array) {
            arr.iter().map(js_string).collect()
        } else if truthy(input.get("key")) {
            vec![js_string(input.get("key").unwrap())]
        } else {
            Vec::new()
        };
        if keys.is_empty() {
            return Err("constitution-law: key or keys is required".to_string());
        }
        let catalog_arr = crate::constitution__helpers_law_catalog::LAW_CATALOG.as_array().cloned().unwrap_or_default();
        let source_obj = crate::constitution__helpers_law_catalog::CATALOG_SOURCE.as_object().cloned().unwrap_or_default();
        let mut added: i64 = 0;
        for k in &keys {
            let c = match catalog_arr.iter().find(|x| x.get("key").and_then(Value::as_str) == Some(k.as_str())) {
                Some(c) => c,
                None => continue,
            };
            let config_v = nz(c.get("config")).cloned().unwrap_or_else(|| json!({}));
            let config_str = serde_json::to_string(&config_v).unwrap();
            let severity_v = nz(c.get("severity")).cloned().unwrap_or_else(|| json!("warn"));
            let confidence_v = match c.get("confidence") {
                None | Some(Value::Null) => json!(0.9),
                Some(v) => v.clone(),
            };
            let applies_v = nz(c.get("applies")).cloned().unwrap_or_else(|| json!({}));
            let applies_str = serde_json::to_string(&applies_v).unwrap();
            let key_str = c.get("key").and_then(Value::as_str).unwrap_or("").to_string();
            let source_v = source_obj.get(&key_str).cloned().unwrap_or(Value::Null);
            let evidence_str = serde_json::to_string(&json!({ "source": source_v })).unwrap();
            let (changes, _) = crate::db_json::run_write(
                db,
                "INSERT OR IGNORE INTO laws.review_laws (scope, key, title, rule_md, detector, config_json, weight, severity, confidence, enforcement, applies_json, status, evidence_json, created_by)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'score', ?, 'ratified', ?, 'catalog')",
                &[
                    c.get("scope").cloned().unwrap_or(Value::Null),
                    c.get("key").cloned().unwrap_or(Value::Null),
                    c.get("title").cloned().unwrap_or(Value::Null),
                    c.get("rule_md").cloned().unwrap_or(Value::Null),
                    c.get("detector").cloned().unwrap_or(Value::Null),
                    json!(config_str),
                    c.get("weight").cloned().unwrap_or(Value::Null),
                    severity_v,
                    confidence_v,
                    json!(applies_str),
                    json!(evidence_str),
                ],
            )?;
            added += changes as i64;
        }
        return Ok(json!({ "ok": true, "added": added }));
    }

    // ── single-law ops ──────────────────────────────────────────────────────────
    let key_input = input.get("key");
    let key: Option<String> = if truthy(key_input) { Some(js_string(key_input.unwrap())) } else { None };
    let law: Option<Value> = match &key {
        Some(k) => crate::db_json::query_rows(db, "SELECT * FROM laws.review_laws WHERE key = ?", &[json!(k.clone())])?
            .as_array()
            .and_then(|a| a.first())
            .cloned(),
        None => None,
    };

    if op == "ratify" {
        if law.is_none() { return Err("constitution-law: key is required".to_string()); }
        let k = key.clone().unwrap();
        crate::db_json::run_write(db, &format!("UPDATE laws.review_laws SET status='ratified', updated_at={} WHERE key=?", crate::clock::now_sql()), &[json!(k.clone())])?;
        return Ok(json!({ "ok": true, "key": k, "status": "ratified" }));
    }
    if op == "dismiss" {
        if law.is_none() { return Err("constitution-law: key is required".to_string()); }
        let k = key.clone().unwrap();
        crate::db_json::run_write(db, "DELETE FROM laws.review_laws WHERE key=? AND status='candidate'", &[json!(k.clone())])?;
        return Ok(json!({ "ok": true, "key": k, "dismissed": true }));
    }
    // remove: delete a law outright — used by pack-disable for CODE-BACKED laws only.
    if op == "remove" {
        if law.is_none() { return Err("constitution-law: key is required".to_string()); }
        let k = key.clone().unwrap();
        crate::db_json::run_write(db, "DELETE FROM laws.review_laws WHERE key=?", &[json!(k.clone())])?;
        return Ok(json!({ "ok": true, "key": k, "removed": true }));
    }
    if op == "suspend" {
        if law.is_none() { return Err("constitution-law: key is required".to_string()); }
        let k = key.clone().unwrap();
        crate::db_json::run_write(db, &format!("UPDATE laws.review_laws SET status='suspended', updated_at={} WHERE key=?", crate::clock::now_sql()), &[json!(k.clone())])?;
        return Ok(json!({ "ok": true, "key": k, "status": "suspended" }));
    }
    if op == "unsuspend" {
        if law.is_none() { return Err("constitution-law: key is required".to_string()); }
        let k = key.clone().unwrap();
        crate::db_json::run_write(db, &format!("UPDATE laws.review_laws SET status='ratified', updated_at={} WHERE key=?", crate::clock::now_sql()), &[json!(k.clone())])?;
        return Ok(json!({ "ok": true, "key": k, "status": "ratified" }));
    }
    if op == "reweight" {
        if law.is_none() { return Err("constitution-law: key is required".to_string()); }
        let k = key.clone().unwrap();
        let w = clamp_0_10(js_number(input.get("weight")));
        if !w.is_finite() {
            return Err("constitution-law: weight is required".to_string());
        }
        // a weight change bumps the version so findings stay attributable across the shift
        crate::db_json::run_write(db, &format!("UPDATE laws.review_laws SET weight=?, version=version+1, updated_at={} WHERE key=?", crate::clock::now_sql()), &[json!(w), json!(k.clone())])?;
        return Ok(json!({ "ok": true, "key": k, "weight": w }));
    }
    if op == "except" {
        let key_str = match key {
            Some(k) => k,
            None => return Err("constitution-law: key is required".to_string()),
        };
        let glob = str_qq(input.get("path_glob")).trim().to_string();
        let reason = str_qq(input.get("reason")).trim().to_string();
        if glob.is_empty() || reason.is_empty() {
            return Err("constitution-law: path_glob+reason is required".to_string());
        }
        let created_by = js_string(&nz(input.get("created_by")).cloned().unwrap_or_else(|| json!("operator")));
        crate::db_json::run_write(
            db,
            "INSERT INTO laws.review_law_exceptions (law_key, path_glob, reason, created_by) VALUES (?, ?, ?, ?)",
            &[json!(key_str.clone()), json!(glob.clone()), json!(reason), json!(created_by)],
        )?;
        return Ok(json!({ "ok": true, "key": key_str, "path_glob": glob }));
    }
    if op == "unexcept" {
        let id = js_number(input.get("exception_id"));
        if id == 0.0 || id.is_nan() {
            return Err("constitution-law: exception_id is required".to_string());
        }
        crate::db_json::run_write(db, "DELETE FROM laws.review_law_exceptions WHERE id=?", &[json!(id)])?;
        return Ok(json!({ "ok": true, "removed": id }));
    }

    Err(format!(
        "constitution-law: op must be one of: list, seed, catalog, enable, add, ratify, dismiss, remove, suspend, unsuspend, reweight, except, unexcept (got '{op}')"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments").unwrap();
        c
    }

    fn workspaces_conn() -> Connection {
        let c = conn();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')",
        )
        .unwrap();
        c
    }

    #[test]
    fn resolve_frameworks_adds_react_when_stack_facts_finds_it() {
        let c = workspaces_conn();
        c.execute(
            &format!("INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', '{}/stack-facts-found', 1)", env!("ANTILOKI_FIXTURES")),
            [],
        )
        .unwrap();
        assert_eq!(resolveFrameworks(&c), json!(["node", "react"]));
    }

    #[test]
    fn resolve_frameworks_falls_back_to_node_only_without_a_manifest() {
        let c = workspaces_conn();
        c.execute(
            &format!("INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', '{}/does-not-exist', 1)", env!("ANTILOKI_FIXTURES")),
            [],
        )
        .unwrap();
        assert_eq!(resolveFrameworks(&c), json!(["node"]));
    }

    #[test]
    fn composed_constitution_normalizes_sorts_and_lists_exceptions() {
        let c = conn();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, weight, status) VALUES ('code','low-weight','Low','r',3,'ratified')", []).unwrap();
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, weight, status) VALUES ('business','high-weight','High','r',8,'candidate')", []).unwrap();
        c.execute("INSERT INTO laws.review_law_exceptions (law_key, path_glob, reason) VALUES ('high-weight', '**/*.gen.*', 'generated')", []).unwrap();
        let r = composedConstitution(&c);
        let laws = r["laws"].as_array().unwrap();
        assert_eq!(laws[0]["key"], json!("high-weight")); // business (idx 0) before project (idx 2, 'code' normalized)
        assert_eq!(laws[0]["scope"], json!("business"));
        assert_eq!(laws[1]["key"], json!("low-weight"));
        assert_eq!(laws[1]["scope"], json!("project")); // 'code' -> 'project'
        assert_eq!(r["exceptions"][0]["law_key"], json!("high-weight"));
    }

    #[test]
    fn list_reports_frameworks_counts_and_parsed_json_fields() {
        let c = workspaces_conn();
        c.execute(
            &format!("INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', '{}/stack-facts-found', 1)", env!("ANTILOKI_FIXTURES")),
            [],
        )
        .unwrap();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, weight, status, applies_json, config_json, evidence_json) VALUES ('language','a','A','r',3,'ratified','{\"languages\":[\"js\"]}','{}',NULL)", []).unwrap();
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, weight, status, evidence_json) VALUES ('language','b','B','r',5,'candidate','not-json')", []).unwrap();
        let r = constitutionLaw(&json!({ "op": "list" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["frameworks"], json!(["node", "react"]));
        assert_eq!(r["counts"], json!({ "candidate": 1, "ratified": 1 }));
        let laws = r["laws"].as_array().unwrap();
        let by_key = |k: &str| laws.iter().find(|l| l["key"] == json!(k)).unwrap();
        assert_eq!(by_key("a")["applies"], json!({ "languages": ["js"] }));
        assert_eq!(by_key("a")["evidence"], Value::Null); // NULL column -> safe(null) -> null
        assert_eq!(by_key("b")["evidence"], Value::Null); // malformed JSON -> caught -> null
    }

    #[test]
    fn seed_installs_all_and_healed_zero_on_a_fresh_table() {
        let c = conn();
        let r = constitutionLaw(&json!({ "op": "seed" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["added"], json!(17));
        assert_eq!(r["healed"], json!(0));
        assert_eq!(r["total"], json!(17));
    }

    #[test]
    fn seed_with_keys_filter_returns_no_healed_field() {
        let c = conn();
        let r = constitutionLaw(&json!({ "op": "seed", "keys": ["no-var", "no-debugger", "bogus-key"] }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "added": 2, "upgraded": 0, "total": 3 }));
    }

    #[test]
    fn seed_upgrades_a_seed_law_whose_definition_moved_and_is_idempotent() {
        // The workspace was onboarded with the OLD `no-loose-equality` (no allow-null). Seeding
        // again must replace the query and bump the version — but leave the operator's weight and
        // status alone, and do nothing the second time.
        let c = conn();
        crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c.execute_batch("INSERT INTO laws.review_laws (scope, key, title, rule_md, detector, config_json, weight, enforcement, applies_json, status, created_by) \
            VALUES ('language','no-loose-equality','old title','old rule','ast','{\"queries\":{\"tsx\":\"(binary_expression) @hit\"}}', 9, 'advise', '{}', 'suspended', 'seed')").unwrap();
        c.execute_batch("INSERT INTO laws.review_laws (scope, key, title, rule_md, detector, config_json, weight, enforcement, applies_json, status, created_by) \
            VALUES ('language','no-var','mine','mine','regex','{\"pattern\":\"var\"}', 1, 'score', '{}', 'ratified', 'operator')").unwrap();
        let r = constitutionLaw(&json!({ "op": "seed", "keys": ["no-loose-equality", "no-var"] }), &c).unwrap();
        assert_eq!(r["added"], json!(0));
        assert_eq!(r["upgraded"], json!(1), "the seed row moved; the operator's row is not the seed's to touch");
        let row = crate::db_json::query_rows(&c, "SELECT config_json, weight, enforcement, status, version, rule_md FROM laws.review_laws WHERE key='no-loose-equality'", &[]).unwrap();
        assert!(row[0]["config_json"].as_str().unwrap().contains("allow-null"));
        assert_eq!(row[0]["version"], json!(2));
        assert_eq!(row[0]["weight"], json!(9), "reweight survives");
        assert_eq!(row[0]["enforcement"], json!("advise"), "operator enforcement survives");
        assert_eq!(row[0]["status"], json!("suspended"), "a suspension survives");
        let mine = crate::db_json::query_rows(&c, "SELECT config_json FROM laws.review_laws WHERE key='no-var'", &[]).unwrap();
        assert_eq!(mine[0]["config_json"], json!("{\"pattern\":\"var\"}"));
        let again = constitutionLaw(&json!({ "op": "seed", "keys": ["no-loose-equality", "no-var"] }), &c).unwrap();
        assert_eq!(again["upgraded"], json!(0), "idempotent");
    }

    #[test]
    fn upgrade_only_corrects_a_moved_seed_law_and_never_resurrects_a_removed_one() {
        let c = conn();
        constitutionLaw(&json!({ "op": "seed" }), &c).unwrap();
        // an old workspace: the secrets law still has the name-only pattern, and the operator removed no-var
        c.execute_batch("UPDATE laws.review_laws SET config_json='{\"pattern\":\"old\"}' WHERE key='sec-no-hardcoded-secret'").unwrap();
        constitutionLaw(&json!({ "op": "remove", "key": "no-var" }), &c).unwrap();
        let r = constitutionLaw(&json!({ "op": "seed", "upgrade_only": true }), &c).unwrap();
        assert_eq!(r["added"], json!(0), "inserts nothing");
        assert_eq!(r["upgraded"], json!(1), "the moved seed law is corrected");
        let sec = crate::db_json::query_rows(&c, "SELECT config_json, version FROM laws.review_laws WHERE key='sec-no-hardcoded-secret'", &[]).unwrap();
        assert!(sec[0]["config_json"].as_str().unwrap().contains("Bearer"), "the new pattern is in");
        assert_eq!(sec[0]["version"], json!(2));
        let gone = crate::db_json::query_rows(&c, "SELECT COUNT(*) n FROM laws.review_laws WHERE key='no-var'", &[]).unwrap();
        assert_eq!(gone[0]["n"], json!(0), "a law the operator removed stays removed");
        assert_eq!(constitutionLaw(&json!({ "op": "seed", "upgrade_only": true }), &c).unwrap()["upgraded"], json!(0), "idempotent");
    }

    #[test]
    fn add_inserts_new_law_with_slugified_key_and_advise_enforcement() {
        let c = conn();
        let r = constitutionLaw(&json!({ "op": "add", "key": "  My New LAW!! ", "scope": "code", "title": "My New Law", "rule_md": "do it" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "key": "my-new-law", "scope": "project", "authored": true }));
        let row = crate::db_json::query_rows(&c, "SELECT enforcement, weight, confidence, created_by FROM laws.review_laws WHERE key='my-new-law'", &[]).unwrap();
        assert_eq!(row[0]["enforcement"], json!("advise"));
        assert_eq!(row[0]["weight"], json!(5));
        assert_eq!(row[0]["confidence"], json!(1.0));
        assert_eq!(row[0]["created_by"], json!("operator"));
    }

    #[test]
    fn add_upserts_existing_key_bumping_version() {
        let c = conn();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, weight, version) VALUES ('project','existing-law','Old','r',3,1)", []).unwrap();
        let r = constitutionLaw(&json!({ "op": "add", "key": "existing-law", "scope": "framework", "title": "New Title", "weight": 15, "detector": "regex" }), &c).unwrap();
        assert_eq!(r["scope"], json!("framework"));
        let row = crate::db_json::query_rows(&c, "SELECT title, weight, version, enforcement FROM laws.review_laws WHERE key='existing-law'", &[]).unwrap();
        assert_eq!(row[0]["title"], json!("New Title"));
        assert_eq!(row[0]["weight"], json!(10)); // clamped from 15
        assert_eq!(row[0]["version"], json!(2));
        // ON CONFLICT's SET list omits `enforcement` (JS SQL verbatim) — the pre-existing row's
        // column DEFAULT ('score') survives the upsert untouched, regardless of what the computed
        // `enforcement` local would have inserted on a fresh row.
        assert_eq!(row[0]["enforcement"], json!("score"));
    }

    #[test]
    fn catalog_marks_installed_entries() {
        let c = conn();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md) VALUES ('community','no-eval','x','r')", []).unwrap();
        let r = constitutionLaw(&json!({ "op": "catalog" }), &c).unwrap();
        let catalog = r["catalog"].as_array().unwrap();
        let no_eval = catalog.iter().find(|x| x["key"] == json!("no-eval")).unwrap();
        assert_eq!(no_eval["installed"], json!(true));
        assert!(no_eval["source"].as_str().unwrap().contains("CWE-95"));
        let no_alert = catalog.iter().find(|x| x["key"] == json!("no-alert")).unwrap();
        assert_eq!(no_alert["installed"], json!(false));
    }

    #[test]
    fn enable_with_keys_array_skips_unknown_and_installs_known() {
        let c = conn();
        let r = constitutionLaw(&json!({ "op": "enable", "keys": ["no-eval", "totally-bogus"] }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "added": 1 }));
        let row = crate::db_json::query_rows(&c, "SELECT status, created_by, enforcement FROM laws.review_laws WHERE key='no-eval'", &[]).unwrap();
        assert_eq!(row[0]["status"], json!("ratified"));
        assert_eq!(row[0]["created_by"], json!("catalog"));
        assert_eq!(row[0]["enforcement"], json!("score"));
    }

    #[test]
    fn enable_with_singular_key_field() {
        let c = conn();
        let r = constitutionLaw(&json!({ "op": "enable", "key": "no-alert" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "added": 1 }));
    }

    #[test]
    fn single_law_ops_ratify_dismiss_remove_suspend_unsuspend() {
        let c = conn();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, status) VALUES ('project','r1','R1','r','candidate')", []).unwrap();
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, status) VALUES ('project','d1','D1','r','candidate')", []).unwrap();
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, status) VALUES ('project','x1','X1','r','ratified')", []).unwrap();
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, status) VALUES ('project','s1','S1','r','ratified')", []).unwrap();
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, status) VALUES ('project','u1','U1','r','suspended')", []).unwrap();

        assert_eq!(constitutionLaw(&json!({ "op": "ratify", "key": "r1" }), &c).unwrap(), json!({ "ok": true, "key": "r1", "status": "ratified" }));
        let row = crate::db_json::query_rows(&c, "SELECT status FROM laws.review_laws WHERE key='r1'", &[]).unwrap();
        assert_eq!(row[0]["status"], json!("ratified"));

        assert_eq!(constitutionLaw(&json!({ "op": "dismiss", "key": "d1" }), &c).unwrap(), json!({ "ok": true, "key": "d1", "dismissed": true }));
        let row = crate::db_json::query_rows(&c, "SELECT COUNT(*) n FROM laws.review_laws WHERE key='d1'", &[]).unwrap();
        assert_eq!(row[0]["n"], json!(0));

        assert_eq!(constitutionLaw(&json!({ "op": "remove", "key": "x1" }), &c).unwrap(), json!({ "ok": true, "key": "x1", "removed": true }));
        let row = crate::db_json::query_rows(&c, "SELECT COUNT(*) n FROM laws.review_laws WHERE key='x1'", &[]).unwrap();
        assert_eq!(row[0]["n"], json!(0));

        assert_eq!(constitutionLaw(&json!({ "op": "suspend", "key": "s1" }), &c).unwrap(), json!({ "ok": true, "key": "s1", "status": "suspended" }));
        assert_eq!(constitutionLaw(&json!({ "op": "unsuspend", "key": "u1" }), &c).unwrap(), json!({ "ok": true, "key": "u1", "status": "ratified" }));
    }

    #[test]
    fn reweight_clamps_into_0_to_10() {
        let c = conn();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, weight, version) VALUES ('project','w1','W1','r',5,1)", []).unwrap();
        let r = constitutionLaw(&json!({ "op": "reweight", "key": "w1", "weight": 15 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "key": "w1", "weight": 10.0 }));
        let row = crate::db_json::query_rows(&c, "SELECT weight, version FROM laws.review_laws WHERE key='w1'", &[]).unwrap();
        assert_eq!(row[0]["weight"], json!(10));
        assert_eq!(row[0]["version"], json!(2));

        let r2 = constitutionLaw(&json!({ "op": "reweight", "key": "w1", "weight": -3 }), &c).unwrap();
        assert_eq!(r2["weight"], json!(0.0));
    }

    #[test]
    fn except_and_unexcept_round_trip() {
        let c = conn();
        let r = constitutionLaw(&json!({ "op": "except", "key": "no-var", "path_glob": "**/*.test.*", "reason": "test exemption" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "key": "no-var", "path_glob": "**/*.test.*" }));
        let row = crate::db_json::query_rows(&c, "SELECT id, created_by FROM laws.review_law_exceptions WHERE law_key='no-var'", &[]).unwrap();
        assert_eq!(row[0]["created_by"], json!("operator"));
        let id = row[0]["id"].as_i64().unwrap();

        let r2 = constitutionLaw(&json!({ "op": "unexcept", "exception_id": id }), &c).unwrap();
        assert_eq!(r2, json!({ "ok": true, "removed": id as f64 }));
        let row2 = crate::db_json::query_rows(&c, "SELECT COUNT(*) n FROM laws.review_law_exceptions", &[]).unwrap();
        assert_eq!(row2[0]["n"], json!(0));
    }

    #[test]
    fn guard_throws_and_unknown_op_carry_the_expected_messages() {
        let c = conn();
        assert_eq!(constitutionLaw(&json!({ "op": "ratify" }), &c).unwrap_err(), "constitution-law: key is required");
        assert_eq!(constitutionLaw(&json!({ "op": "enable" }), &c).unwrap_err(), "constitution-law: key or keys is required");
        assert_eq!(constitutionLaw(&json!({ "op": "except", "key": "k" }), &c).unwrap_err(), "constitution-law: path_glob+reason is required");
        assert_eq!(constitutionLaw(&json!({ "op": "unexcept" }), &c).unwrap_err(), "constitution-law: exception_id is required");
        assert!(constitutionLaw(&json!({ "op": "bogus" }), &c).unwrap_err().contains("op must be one of"));
    }
}
