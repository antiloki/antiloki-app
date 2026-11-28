//! card-process-events.rs — twin of skills/system/card-process-events.mjs (db READ lane,
//! cross-db `timeline.sacred_timeline` + `brain.ai_calls`/`brain.driver_ticks`/enrichment
//! tables). NAMES ARE CONTRACT: `cardProcessEvents` (default). Chronological merged event
//! stream for ONE card's pipeline execution — audit rows (card + its contracts), attributed
//! AI calls (session_id / contract_slug, with reason↔format pairing + attempt/report/output
//! enrichment), and driver ticks — sorted ts ASC, capped (trims the OLDEST first).
// QUALITY: 80/100 — 3-source merge; before/after diff summary (added/modified/removed +
// truncated field_changes); reason/format phase pairing with backfill; outputs/attempts enrichment
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `${v}` interpolation.
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// JS String(v) (no ??) — used by truncVal for non-null/non-object values.
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
// JS Number(v).
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
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn g(row: &Value, key: &str) -> Value { row.get(key).cloned().unwrap_or(Value::Null) }
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() } // JS .slice(0,n) (BMP-equivalent)
// Canonical string key for a row id (int or string alike) — used for HashMap grouping,
// mirroring JS Map keys where node:sqlite ids are plain numbers.
fn qkey(v: &Value) -> String { tmpl(Some(v)) }

// `Number(x ?? 0)` where x rides a REAL/INTEGER SQL column: node:sqlite's JS numbers never
// print a trailing ".0" for whole values (one Number type), but serde_json keeps float-vs-int
// provenance — a REAL column's whole value would otherwise render "5.0" instead of JS's "5".
// Renormalize any whole-valued number to its integer JSON form so both lanes agree.
fn number_or_zero(v: Option<&Value>) -> Value {
    match nz(v) {
        None => json!(0),
        Some(Value::Number(n)) => match n.as_f64() {
            Some(f) if f.is_finite() && f.fract() == 0.0 => json!(f as i64),
            _ => Value::Number(n.clone()),
        },
        Some(other) => other.clone(),
    }
}

// truncVal(v): before/after field value → short glanceable string for field_changes.
fn trunc_val(v: &Value) -> String {
    match v {
        Value::Null => "\u{2205}".to_string(), // ∅
        Value::Array(a) => format!("[{}]", a.len()),
        Value::Object(_) => "{\u{2026}}".to_string(), // {…}
        other => {
            let s = js_string(other);
            if s.chars().count() > 40 { format!("{}\u{2026}", take_chars(&s, 40)) } else { s }
        }
    }
}

// JS `typeof v === 'object'` (true for both plain objects AND arrays, false for null).
fn is_js_object(v: &Value) -> bool { !v.is_null() && (v.is_object() || v.is_array()) }
// JS `bv !== av` for JSON-sourced values: objects/arrays independently JSON.parsed are NEVER
// reference-equal (always "not equal" in JS, regardless of deep structural equality) — unlike
// serde_json::Value's PartialEq, which compares Maps order-independently by structure.
fn js_strict_eq(a: &Value, b: &Value) -> bool {
    if is_js_object(a) || is_js_object(b) { return false; }
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::String(x), Value::String(y)) => x == y,
        _ => false,
    }
}
// `s ? JSON.parse(s) : {}` — falsy (absent/empty) input defaults to {} WITHOUT parsing; a
// present non-empty string that fails to parse signals abort (JS's try/catch swallows the
// WHOLE diff computation, not just this side) — represented as None.
fn parse_obj_or_empty(s: Option<&str>) -> Option<Map<String, Value>> {
    match s {
        None => Some(Map::new()),
        Some(txt) if txt.is_empty() => Some(Map::new()),
        Some(txt) => match serde_json::from_str::<Value>(txt) {
            Ok(Value::Object(m)) => Some(m),
            Ok(_) => Some(Map::new()), // Object.keys(non-object) ~ []
            Err(_) => None,
        },
    }
}

// Reconstruct a readable "prompt sent" from ai_calls.envelope_json when no ai_call_rendered
// row exists — the envelope's resolved `inputs` are the closest record of what the AI saw.
fn prompt_from_envelope(envelope_json: &str) -> Option<String> {
    let env: Value = serde_json::from_str(envelope_json).ok()?;
    let inputs = env.get("inputs")?;
    let Value::Object(map) = inputs else { return None };
    let mut parts: Vec<String> = Vec::new();
    if truthy(env.get("prompt_name")) {
        parts.push(format!("# {}", tmpl(env.get("prompt_name"))));
    }
    for (k, v) in map {
        if v.is_null() { continue; }
        if let Value::String(s) = v { if s.is_empty() { continue; } }
        let s = match v {
            Value::String(s) => s.clone(),
            other => serde_json::to_string_pretty(other).unwrap_or_default(),
        };
        if s.trim().is_empty() { continue; }
        let body = if s.chars().count() > 4000 {
            format!("{}\n\u{2026}(truncated)", take_chars(&s, 4000))
        } else {
            s
        };
        parts.push(format!("## {k}\n{body}"));
    }
    if parts.is_empty() { None } else { Some(parts.join("\n\n")) }
}

fn probe_column(db: &Connection, schema: &str, table: &str, col: &str) -> bool {
    let sql = format!("SELECT 1 FROM {schema}.pragma_table_info('{table}') WHERE name='{col}'");
    match crate::db_json::query_rows(db, &sql, &[]) {
        Ok(Value::Array(a)) => !a.is_empty(),
        _ => false,
    }
}
fn env_num(name: &str, default: i64) -> i64 {
    match std::env::var(name) {
        Ok(s) => { let t = s.trim(); if t.is_empty() { 0 } else { t.parse::<f64>().unwrap_or(0.0) as i64 } }
        Err(_) => default,
    }
}

pub fn cardProcessEvents(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('card-process-events','db') when no db — the db lane
    // always injects one, so that guard is unreachable here.
    if !truthy(input.get("slug")) {
        return Err("card-process-events: slug is required".to_string());
    }
    let slug = input.get("slug").cloned().unwrap_or(Value::Null);
    let limit_n = js_number(input.get("limit"));
    let base = if limit_n == 0.0 || limit_n.is_nan() { 300.0 } else { limit_n };
    let cap = base.max(1.0).min(1000.0) as i64;

    // ── 1) entity ids: this card + every contract it has produced ──────────
    let contract_slugs = crate::db_json::query_rows(db, "SELECT slug FROM contracts WHERE card_slug = ?", &[slug.clone()])?;
    let mut entity_ids: Vec<Value> = vec![slug.clone()];
    for r in contract_slugs.as_array().cloned().unwrap_or_default() {
        entity_ids.push(g(&r, "slug"));
    }
    let entity_placeholders = vec!["?"; entity_ids.len()].join(",");

    let id_sql = format!(
        "SELECT id FROM sacred_timeline \
         WHERE id >= (SELECT COALESCE(MAX(id),0) - 20000 FROM sacred_timeline) \
           AND entity_type IN ('card', 'contract') AND entity_id IN ({entity_placeholders}) \
         ORDER BY id DESC LIMIT ?"
    );
    let mut id_params = entity_ids.clone();
    id_params.push(json!(cap));
    let id_rows = crate::db_json::query_rows(db, &id_sql, &id_params)?.as_array().cloned().unwrap_or_default();

    let diff_per_row_max = env_num("ANTILOKI_DIFF_PER_ROW_MAX_BYTES", 512 * 1024);
    let diff_total_budget = env_num("ANTILOKI_DIFF_TOTAL_BUDGET_BYTES", 2 * 1024 * 1024);
    let has_exec_env = probe_column(db, "timeline", "sacred_timeline", "exec_env");

    let mut audit_light: Vec<Value> = Vec::new();
    let mut blob_by_id: HashMap<String, Value> = HashMap::new();
    if !id_rows.is_empty() {
        let id_placeholders = vec!["?"; id_rows.len()].join(",");
        let id_args: Vec<Value> = id_rows.iter().map(|r| g(r, "id")).collect();
        let exec_env_col = if has_exec_env { "exec_env," } else { "" };
        let audit_light_sql = format!(
            "SELECT id, ts, actor, operation, entity_type, entity_id, {exec_env_col} \
             COALESCE(octet_length(before_json),0) + COALESCE(octet_length(after_json),0) AS diff_bytes \
             FROM sacred_timeline WHERE id IN ({id_placeholders})"
        );
        audit_light = crate::db_json::query_rows(db, &audit_light_sql, &id_args)?.as_array().cloned().unwrap_or_default();
        audit_light.sort_by(|a, b| js_number(Some(&g(b, "id"))).partial_cmp(&js_number(Some(&g(a, "id")))).unwrap());

        let mut budget = diff_total_budget;
        let mut want_diff: Vec<Value> = Vec::new();
        for r in &audit_light {
            let diff_bytes = js_number(Some(&g(r, "diff_bytes"))) as i64;
            if diff_bytes <= diff_per_row_max && diff_bytes <= budget {
                budget -= diff_bytes;
                want_diff.push(g(r, "id"));
            }
        }
        if !want_diff.is_empty() {
            let diff_placeholders = vec!["?"; want_diff.len()].join(",");
            let diff_sql = format!(
                "SELECT id, before_json, after_json, digital_print FROM sacred_timeline WHERE id IN ({diff_placeholders})"
            );
            for b in crate::db_json::query_rows(db, &diff_sql, &want_diff)?.as_array().cloned().unwrap_or_default() {
                blob_by_id.insert(qkey(&g(&b, "id")), b);
            }
        }
    }
    // Intermediate `audit` rows (id/ts/actor/... + resolved blob fields) — mirrors the JS
    // `audit` array, fed into the merge loop below where diff_summary is computed.
    let mut audit_rows: Vec<Value> = Vec::new();
    for r in &audit_light {
        let id_v = g(r, "id");
        let blob = blob_by_id.get(&qkey(&id_v));
        let mut o = Map::new();
        o.insert("id".into(), id_v);
        o.insert("ts".into(), g(r, "ts"));
        o.insert("actor".into(), g(r, "actor"));
        o.insert("operation".into(), g(r, "operation"));
        o.insert("entity_type".into(), g(r, "entity_type"));
        o.insert("entity_id".into(), g(r, "entity_id"));
        o.insert("exec_env".into(), g(r, "exec_env"));
        o.insert("digital_print".into(), blob.map(|b| g(b, "digital_print")).unwrap_or(Value::Null));
        o.insert("before_json".into(), blob.map(|b| g(b, "before_json")).unwrap_or(Value::Null));
        o.insert("after_json".into(), blob.map(|b| g(b, "after_json")).unwrap_or(Value::Null));
        audit_rows.push(Value::Object(o));
    }

    // ── 2) AI calls attributed by run session_id / contract_slug ───────────
    let has_reason_col = probe_column(db, "brain", "ai_calls", "reason_call_id");
    let has_subagent_col = probe_column(db, "brain", "ai_calls", "subagent");
    let has_reasoning_col = probe_column(db, "brain", "ai_calls", "reasoning_md");
    let has_contract_slug_col = probe_column(db, "brain", "ai_calls", "contract_slug");
    let reason_col_sql = if has_reason_col { "a.reason_call_id," } else { "NULL AS reason_call_id," };
    let subagent_col_sql = if has_subagent_col { "a.subagent," } else { "NULL AS subagent," };
    let reasoning_col_sql = if has_reasoning_col { "a.reasoning_md," } else { "NULL AS reasoning_md," };
    let contract_attr_sql = if has_contract_slug_col {
        "OR a.contract_slug IN (SELECT slug FROM contracts WHERE card_slug = ?)"
    } else {
        ""
    };

    let ai_sql = format!(
        "SELECT a.id, a.ts, a.tier, a.temperature, a.provider, a.model_id, \
                a.cost_usd_actual, a.tokens_in, a.tokens_out, a.duration_ms, \
                a.cached, a.outcome, a.envelope_json, {reason_col_sql} {subagent_col_sql} {reasoning_col_sql} \
                pt.name AS prompt_name, pt.version AS prompt_version \
         FROM brain.ai_calls a \
         LEFT JOIN brain.prompt_templates pt ON pt.id = a.prompt_template_id \
         WHERE a.id >= (SELECT COALESCE(MAX(id),0) - 5000 FROM brain.ai_calls) \
           AND (a.session_id = (SELECT openrouter_session_id FROM cards WHERE slug = ?) {contract_attr_sql}) \
         ORDER BY a.id DESC LIMIT ?"
    );
    let mut ai_params: Vec<Value> = vec![slug.clone()];
    if has_contract_slug_col { ai_params.push(slug.clone()); }
    ai_params.push(json!(cap));
    let mut ai_calls: Vec<Value> = crate::db_json::query_rows(db, &ai_sql, &ai_params)?.as_array().cloned().unwrap_or_default();

    // Linked-reason backfill: a reason leg whose inputs never mention the slug is pulled in
    // via the ids the matched calls reference (best-effort — swallowed on failure).
    if has_reason_col {
        let have: std::collections::HashSet<String> = ai_calls.iter().map(|a| qkey(&g(a, "id"))).collect();
        let mut missing: Vec<Value> = Vec::new();
        let mut seen_missing: std::collections::HashSet<String> = std::collections::HashSet::new();
        for a in &ai_calls {
            if let Some(rid) = nz(a.get("reason_call_id")) {
                let k = qkey(rid);
                if !have.contains(&k) && seen_missing.insert(k) {
                    missing.push(rid.clone());
                }
            }
        }
        if !missing.is_empty() {
            let ph = vec!["?"; missing.len()].join(",");
            let backfill_sql = format!(
                "SELECT a.id, a.ts, a.tier, a.temperature, a.provider, a.model_id, \
                        a.cost_usd_actual, a.tokens_in, a.tokens_out, a.duration_ms, \
                        a.cached, a.outcome, a.envelope_json, a.reason_call_id, {subagent_col_sql} \
                        pt.name AS prompt_name, pt.version AS prompt_version \
                 FROM brain.ai_calls a \
                 LEFT JOIN brain.prompt_templates pt ON pt.id = a.prompt_template_id \
                 WHERE a.id IN ({ph})"
            );
            if let Ok(rows) = crate::db_json::query_rows(db, &backfill_sql, &missing) {
                if let Value::Array(mut rows) = rows { ai_calls.append(&mut rows); }
            }
        }
    }

    // ── Batched enrichment: attempts / reports / rendered / outputs ────────
    let mut attempts_by_id: HashMap<String, Vec<Value>> = HashMap::new();
    let mut report_by_id: HashMap<String, Value> = HashMap::new();
    let mut rendered_by_id: HashMap<String, Value> = HashMap::new();
    let mut outputs_by_id: HashMap<String, Vec<Value>> = HashMap::new();
    if !ai_calls.is_empty() {
        let ids: Vec<Value> = ai_calls.iter().map(|a| g(a, "id")).collect();
        let ph = vec!["?"; ids.len()].join(",");

        if let Ok(rows) = crate::db_json::query_rows(
            db,
            &format!("SELECT ai_call_id, attempt_no, outcome, validation_errors_json FROM brain.ai_call_attempts WHERE ai_call_id IN ({ph}) ORDER BY ai_call_id, attempt_no"),
            &ids,
        ) {
            for r in rows.as_array().cloned().unwrap_or_default() {
                let mut errors = Value::Null;
                if let Some(vej) = r.get("validation_errors_json").and_then(|v| v.as_str()) {
                    if !vej.is_empty() {
                        if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(vej) {
                            let mapped: Vec<Value> = arr.iter().take(3).map(|e| json!(take_chars(&js_string(e), 200))).collect();
                            errors = json!(mapped);
                        }
                    }
                }
                let key = qkey(&g(&r, "ai_call_id"));
                let entry = json!({ "attempt_no": g(&r, "attempt_no"), "outcome": g(&r, "outcome"), "errors": errors });
                attempts_by_id.entry(key).or_default().push(entry);
            }
        }
        if let Ok(rows) = crate::db_json::query_rows(
            db,
            &format!("SELECT ai_call_id, failure_class, severity, diagnosis_md, narrative_md, recommendation_md FROM brain.ai_call_reports WHERE ai_call_id IN ({ph}) ORDER BY id"),
            &ids,
        ) {
            for r in rows.as_array().cloned().unwrap_or_default() {
                let key = qkey(&g(&r, "ai_call_id"));
                report_by_id.insert(key, json!({
                    "failure_class": g(&r, "failure_class"),
                    "severity": g(&r, "severity"),
                    "diagnosis_md": g(&r, "diagnosis_md"),
                    "narrative_md": nz(r.get("narrative_md")).cloned().unwrap_or(Value::Null),
                    "recommendation_md": nz(r.get("recommendation_md")).cloned().unwrap_or(Value::Null),
                }));
            }
        }
        if let Ok(rows) = crate::db_json::query_rows(
            db,
            &format!("SELECT ai_call_id, rendered_prompt_md FROM brain.ai_call_rendered WHERE ai_call_id IN ({ph})"),
            &ids,
        ) {
            for r in rows.as_array().cloned().unwrap_or_default() {
                let key = qkey(&g(&r, "ai_call_id"));
                rendered_by_id.entry(key).or_insert_with(|| g(&r, "rendered_prompt_md"));
            }
        }
        if let Ok(rows) = crate::db_json::query_rows(
            db,
            &format!("SELECT ai_call_id, field_name, value_text, value_number, value_boolean FROM brain.ai_call_outputs WHERE ai_call_id IN ({ph}) AND parent_output_id IS NULL ORDER BY ai_call_id, order_index"),
            &ids,
        ) {
            for r in rows.as_array().cloned().unwrap_or_default() {
                let key = qkey(&g(&r, "ai_call_id"));
                let list = outputs_by_id.entry(key).or_default();
                if list.len() < 10 {
                    let value = nz(r.get("value_text")).or_else(|| nz(r.get("value_number"))).or_else(|| nz(r.get("value_boolean"))).cloned().unwrap_or(Value::Null);
                    list.push(json!({ "field_name": g(&r, "field_name"), "value": value }));
                }
            }
        }
    }

    let mut ai_calls_enriched: Vec<Value> = Vec::new();
    for a in &ai_calls {
        let key = qkey(&g(a, "id"));
        let mut rendered_prompt_md = rendered_by_id.get(&key).cloned().unwrap_or(Value::Null);
        if rendered_prompt_md.is_null() {
            if let Some(env_json) = a.get("envelope_json").and_then(|v| v.as_str()) {
                if let Some(p) = prompt_from_envelope(env_json) { rendered_prompt_md = json!(p); }
            }
        }
        let outputs = outputs_by_id.get(&key).cloned().unwrap_or_default();
        let mut o = a.as_object().cloned().unwrap_or_default();
        o.insert("rendered_prompt_md".into(), rendered_prompt_md);
        o.insert("outputs".into(), json!(outputs));
        ai_calls_enriched.push(Value::Object(o));
    }

    // ── 3) Driver ticks for this card ───────────────────────────────────
    let ticks = crate::db_json::query_rows(
        db,
        "SELECT id, started_at, ended_at, decided_agent, decided_action, decided_reason, outcome, error_message, cost_usd, duration_ms \
         FROM brain.driver_ticks WHERE card_slug = ? ORDER BY id DESC LIMIT ?",
        &[slug.clone(), json!(cap)],
    )?.as_array().cloned().unwrap_or_default();

    // ── Merge + sort by ts ascending ────────────────────────────────────
    let mut merged: Vec<Value> = Vec::new();

    for e in &audit_rows {
        let before_json_v = g(e, "before_json");
        let after_json_v = g(e, "after_json");
        let has_either = matches!(&before_json_v, Value::String(s) if !s.is_empty())
            || matches!(&after_json_v, Value::String(s) if !s.is_empty());

        let mut diff_summary = Value::Null;
        let mut field_changes = Value::Null;
        let mut created = false;
        let mut field_count: i64 = 0;
        if has_either {
            let before_text = match &before_json_v { Value::String(s) if !s.is_empty() => Some(s.as_str()), _ => None };
            let after_text = match &after_json_v { Value::String(s) if !s.is_empty() => Some(s.as_str()), _ => None };
            if let (Some(before), Some(after)) = (parse_obj_or_empty(before_text), parse_obj_or_empty(after_text)) {
                let mut all_keys: Vec<String> = Vec::new();
                let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                for k in before.keys() { if seen.insert(k.clone()) { all_keys.push(k.clone()); } }
                for k in after.keys() { if seen.insert(k.clone()) { all_keys.push(k.clone()); } }

                let mut added: Vec<String> = Vec::new();
                let mut modified: Vec<String> = Vec::new();
                let mut removed: Vec<String> = Vec::new();
                for k in &all_keys {
                    let in_before = before.contains_key(k);
                    let in_after = after.contains_key(k);
                    if !in_before {
                        added.push(k.clone());
                    } else if !in_after {
                        removed.push(k.clone());
                    } else {
                        let bv = &before[k];
                        let av = &after[k];
                        if !js_strict_eq(bv, av) {
                            let both_obj = is_js_object(bv) && is_js_object(av);
                            let stringify_eq = both_obj
                                && serde_json::to_string(bv).unwrap_or_default() == serde_json::to_string(av).unwrap_or_default();
                            if !both_obj || !stringify_eq { modified.push(k.clone()); }
                        }
                    }
                }
                created = before.is_empty() && !added.is_empty();
                field_count = if created { added.len() as i64 } else { (modified.len() + added.len() + removed.len()) as i64 };
                field_changes = json!(modified.iter().take(8).map(|k| json!({
                    "key": k,
                    "from": trunc_val(before.get(k).unwrap_or(&Value::Null)),
                    "to": trunc_val(after.get(k).unwrap_or(&Value::Null)),
                })).collect::<Vec<_>>());
                diff_summary = json!({ "added": added, "modified": modified, "removed": removed });
            }
        }

        let digital_print_v = g(e, "digital_print");
        let digital_print_short = match &digital_print_v {
            Value::String(s) if !s.is_empty() => json!(take_chars(s, 12)),
            _ => Value::Null,
        };

        merged.push(json!({
            "source": "audit",
            "id": format!("audit-{}", tmpl(Some(&g(e, "id")))),
            "ts": g(e, "ts"),
            "operation": g(e, "operation"),
            "actor": g(e, "actor"),
            "exec_env": g(e, "exec_env"),
            "entity_type": g(e, "entity_type"),
            "entity_id": g(e, "entity_id"),
            "digital_print_short": digital_print_short,
            "diff_summary": diff_summary,
            "field_changes": field_changes,
            "created": created,
            "field_count": field_count,
        }));
    }

    // phase + pair resolution: a row is a 'reason' leg when some format row references it.
    let mut format_by_reason: HashMap<String, Value> = HashMap::new();
    for a in &ai_calls_enriched {
        if let Some(rid) = nz(a.get("reason_call_id")) {
            format_by_reason.insert(qkey(rid), g(a, "id"));
        }
    }
    let reason_suffix_re = regex::Regex::new(r"\.reason$").unwrap();
    let phase_of = |a: &Value| -> &'static str {
        if format_by_reason.contains_key(&qkey(&g(a, "id"))) { return "reason"; }
        if nz(a.get("reason_call_id")).is_some() { return "format"; }
        let pn = a.get("prompt_name").and_then(|v| v.as_str()).unwrap_or("");
        if reason_suffix_re.is_match(pn) || pn == "ai.reason.generic.v1" { return "reason"; }
        "single"
    };

    for a in &ai_calls_enriched {
        let phase = phase_of(a);
        let key = qkey(&g(a, "id"));
        let attempts = attempts_by_id.get(&key).cloned().unwrap_or_default();
        // Extract before `attempts` is moved into the json! literal below (a live borrow of
        // `attempts` cannot outlast the move into the "attempts" field in the same literal).
        let validation_errors = attempts
            .iter()
            .rev()
            .find(|x| x.get("outcome").and_then(|v| v.as_str()) != Some("success"))
            .map(|x| g(x, "errors"))
            .unwrap_or(Value::Null);
        let reason_call_id = nz(a.get("reason_call_id")).cloned().unwrap_or(Value::Null);
        let pair_call_id = nz(a.get("reason_call_id"))
            .or_else(|| format_by_reason.get(&key))
            .cloned()
            .unwrap_or(Value::Null);

        merged.push(json!({
            "source": "ai",
            "id": format!("ai-{}", tmpl(Some(&g(a, "id")))),
            "ai_call_id": g(a, "id"),
            "ts": g(a, "ts"),
            "prompt_name": g(a, "prompt_name"),
            "prompt_version": g(a, "prompt_version"),
            "phase": phase,
            "reason_call_id": reason_call_id,
            "pair_call_id": pair_call_id,
            "tier": g(a, "tier"),
            "temperature": g(a, "temperature"),
            "provider": g(a, "provider"),
            "model_id": g(a, "model_id"),
            "subagent": nz(a.get("subagent")).cloned().unwrap_or(Value::Null),
            "outcome": g(a, "outcome"),
            "cost_usd_actual": number_or_zero(a.get("cost_usd_actual")),
            "tokens_in": number_or_zero(a.get("tokens_in")),
            "tokens_out": number_or_zero(a.get("tokens_out")),
            "duration_ms": g(a, "duration_ms"),
            "cached": truthy(a.get("cached")),
            "rendered_prompt_md": g(a, "rendered_prompt_md"),
            "outputs": g(a, "outputs"),
            "attempts_count": attempts.len() as i64,
            "attempts": attempts,
            "validation_errors": validation_errors,
            "report": report_by_id.get(&key).cloned().unwrap_or(Value::Null),
            "has_full_detail": true,
            "reasoning_md": nz(a.get("reasoning_md")).cloned().unwrap_or(Value::Null),
        }));
    }

    for t in &ticks {
        merged.push(json!({
            "source": "tick",
            "id": format!("tick-{}", tmpl(Some(&g(t, "id")))),
            "ts": g(t, "started_at"),
            "ended_at": g(t, "ended_at"),
            "decided_agent": g(t, "decided_agent"),
            "decided_action": g(t, "decided_action"),
            "decided_reason": g(t, "decided_reason"),
            "outcome": g(t, "outcome"),
            "error_message": g(t, "error_message"),
            "cost_usd": number_or_zero(t.get("cost_usd")),
            "duration_ms": g(t, "duration_ms"),
        }));
    }

    // Sort ts ASC (ties by event id) so the terminal reads top-to-bottom chronologically.
    merged.sort_by(|a, b| {
        let ats = a.get("ts").and_then(|v| v.as_str()).unwrap_or("");
        let bts = b.get("ts").and_then(|v| v.as_str()).unwrap_or("");
        match ats.cmp(bts) {
            std::cmp::Ordering::Equal => tmpl(Some(&g(a, "id"))).cmp(&tmpl(Some(&g(b, "id")))),
            other => other,
        }
    });

    // Cap total size; trim from the OLDEST so the most recent activity always survives.
    let len = merged.len();
    if len as i64 > cap {
        merged.drain(0..(len - cap as usize));
    }

    Ok(json!(merged))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS timeline; ATTACH ':memory:' AS brain; \
             CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, openrouter_session_id TEXT); \
             CREATE TABLE contracts (slug TEXT PRIMARY KEY, card_slug TEXT); \
             CREATE TABLE timeline.sacred_timeline (id INTEGER PRIMARY KEY, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT, entity_id TEXT, before_json TEXT, after_json TEXT, exec_env TEXT, digital_print TEXT); \
             CREATE TABLE brain.prompt_templates (id INTEGER PRIMARY KEY, name TEXT, version INTEGER); \
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, ts TEXT, prompt_template_id INTEGER, tier TEXT, temperature TEXT, provider TEXT, model_id TEXT, cost_usd_actual REAL, tokens_in INTEGER, tokens_out INTEGER, duration_ms INTEGER, cached INTEGER, outcome TEXT, envelope_json TEXT, reason_call_id INTEGER, subagent TEXT, reasoning_md TEXT, session_id TEXT, contract_slug TEXT); \
             CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY, started_at TEXT, ended_at TEXT, card_slug TEXT, decided_agent TEXT, decided_action TEXT, decided_reason TEXT, outcome TEXT, error_message TEXT, cost_usd REAL, duration_ms INTEGER); \
             CREATE TABLE brain.ai_call_attempts (ai_call_id INTEGER, attempt_no INTEGER, outcome TEXT, validation_errors_json TEXT); \
             CREATE TABLE brain.ai_call_reports (id INTEGER PRIMARY KEY, ai_call_id INTEGER, failure_class TEXT, severity TEXT, diagnosis_md TEXT, narrative_md TEXT, recommendation_md TEXT); \
             CREATE TABLE brain.ai_call_rendered (ai_call_id INTEGER PRIMARY KEY, rendered_prompt_md TEXT); \
             CREATE TABLE brain.ai_call_outputs (ai_call_id INTEGER, field_name TEXT, value_text TEXT, value_number REAL, value_boolean INTEGER, parent_output_id INTEGER, order_index INTEGER);"
        ).unwrap();
        c
    }

    #[test]
    fn merges_audit_ai_and_tick_sorted_ascending_with_diff_summary() {
        let c = db();
        c.execute("INSERT INTO cards VALUES ('CARD-A','a','sess-a')", []).unwrap();
        c.execute("INSERT INTO contracts VALUES ('CONTRACT-A','CARD-A')", []).unwrap();
        c.execute(
            "INSERT INTO timeline.sacred_timeline (id,ts,actor,operation,entity_type,entity_id,before_json,after_json,exec_env,digital_print) VALUES \
             (1,'2026-06-01 00:00:01','driver:be','card.add','card','CARD-A','{}','{\"slug\":\"CARD-A\",\"status\":\"doing\"}',NULL,'aaaaaaaaaaaa1111'), \
             (2,'2026-06-01 00:00:06','driver:be','contract.review','contract','CONTRACT-A','{\"status\":\"signed\"}','{\"status\":\"rejected\"}','live','bbbbbbbbbbbbbbbb2222')",
            [],
        ).unwrap();
        c.execute("INSERT INTO brain.prompt_templates VALUES (1,'tl.review.v1',3)", []).unwrap();
        c.execute(
            "INSERT INTO brain.ai_calls (id,ts,prompt_template_id,tier,temperature,provider,model_id,cost_usd_actual,tokens_in,tokens_out,duration_ms,cached,outcome,session_id) \
             VALUES (1,'2026-06-01 00:00:03',1,'mid','hot','openrouter','sonnet',0.03,150,75,800,1,'success','sess-a')",
            [],
        ).unwrap();
        c.execute(
            "INSERT INTO brain.driver_ticks (id,started_at,ended_at,card_slug,decided_agent,decided_action,decided_reason,outcome,cost_usd,duration_ms) \
             VALUES (1,'2026-06-01 00:00:02','2026-06-01 00:00:02','CARD-A','pl','pl.review','r','fired',0.01,15)",
            [],
        ).unwrap();

        let r = cardProcessEvents(&json!({ "slug": "CARD-A" }), &c).unwrap();
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 4);
        let sources: Vec<&str> = arr.iter().map(|e| e["source"].as_str().unwrap()).collect();
        assert_eq!(sources, vec!["audit", "tick", "ai", "audit"]);
        assert_eq!(arr[0]["id"], json!("audit-1"));
        assert_eq!(arr[0]["created"], json!(true));
        assert_eq!(arr[0]["digital_print_short"], json!("aaaaaaaaaaaa"));
        assert_eq!(arr[3]["digital_print_short"], json!("bbbbbbbbbbbb"));
        assert_eq!(arr[3]["diff_summary"], json!({ "added": [], "modified": ["status"], "removed": [] }));
        assert_eq!(arr[3]["field_changes"], json!([{ "key": "status", "from": "signed", "to": "rejected" }]));
        assert_eq!(arr[2]["ai_call_id"], json!(1));
        assert_eq!(arr[2]["phase"], json!("single"));
        assert_eq!(arr[2]["cost_usd_actual"], json!(0.03));
    }

    #[test]
    fn reason_backfill_pairs_phases_and_malformed_diff_is_null() {
        let c = db();
        c.execute("INSERT INTO cards VALUES ('CARD-C','c','sess-c')", []).unwrap();
        c.execute(
            "INSERT INTO timeline.sacred_timeline (id,ts,actor,operation,entity_type,entity_id,before_json,after_json,exec_env,digital_print) VALUES \
             (1,'2026-06-01 00:00:03','driver:be','card.update','card','CARD-C','{not json','{\"a\":1}',NULL,'malformedprint01')",
            [],
        ).unwrap();
        c.execute("INSERT INTO brain.prompt_templates VALUES (1,'ai.reason.generic.v1',1),(2,'tl.refine.v1',1)", []).unwrap();
        c.execute(
            "INSERT INTO brain.ai_calls (id,ts,prompt_template_id,tier,temperature,cached,outcome) VALUES (1,'2026-06-01 00:00:01',1,'high','warm',0,'success')",
            [],
        ).unwrap();
        c.execute(
            "INSERT INTO brain.ai_calls (id,ts,prompt_template_id,tier,temperature,cached,outcome,reason_call_id,session_id) VALUES (2,'2026-06-01 00:00:02',2,'low','cold',0,'success',1,'sess-c')",
            [],
        ).unwrap();

        let r = cardProcessEvents(&json!({ "slug": "CARD-C" }), &c).unwrap();
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 3); // reason (backfilled) + format + audit
        let reason = arr.iter().find(|e| e["id"] == json!("ai-1")).unwrap();
        let format = arr.iter().find(|e| e["id"] == json!("ai-2")).unwrap();
        assert_eq!(reason["phase"], json!("reason"));
        assert_eq!(reason["pair_call_id"], json!(2));
        assert_eq!(format["phase"], json!("format"));
        assert_eq!(format["pair_call_id"], json!(1));
        let audit = arr.iter().find(|e| e["source"] == json!("audit")).unwrap();
        assert_eq!(audit["diff_summary"], Value::Null);
        assert_eq!(audit["field_changes"], Value::Null);
    }

    #[test]
    fn limit_trims_from_the_oldest_and_empty_card_returns_empty_array() {
        let c = db();
        c.execute("INSERT INTO cards VALUES ('CARD-D','d',NULL)", []).unwrap();
        c.execute(
            "INSERT INTO timeline.sacred_timeline (id,ts,actor,operation,entity_type,entity_id,before_json,after_json,exec_env,digital_print) VALUES \
             (1,'2026-06-01 00:00:01','driver:be','card.add','card','CARD-D','{}','{\"n\":1}',NULL,'d1'), \
             (2,'2026-06-01 00:00:02','driver:be','card.update','card','CARD-D','{\"n\":1}','{\"n\":2}',NULL,'d2'), \
             (3,'2026-06-01 00:00:03','driver:be','card.update','card','CARD-D','{\"n\":2}','{\"n\":3}',NULL,'d3')",
            [],
        ).unwrap();

        let r = cardProcessEvents(&json!({ "slug": "CARD-D", "limit": 2 }), &c).unwrap();
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["id"], json!("audit-2"));
        assert_eq!(arr[1]["id"], json!("audit-3"));

        let empty = cardProcessEvents(&json!({ "slug": "CARD-NOTHING" }), &c).unwrap();
        assert_eq!(empty, json!([]));
    }
}
