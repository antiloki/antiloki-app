//! blueprint-judge.rs — twin of skills/blueprint/blueprint-judge.mjs. NAMES ARE CONTRACT:
//! `blueprintJudge` (default), `renderSnippetEvidence` (named db-first helper). QA's adversarial
//! judgment of a BLUEPRINT (pipeline v2): a thin adapter that renders the active blueprint as
//! evidence and delegates to the counting-court (swarm_review::review). Ahead of the court, three
//! deterministic FAIL-OPEN gates run (conformance, snippet-checks for BUILD; stub-checks for
//! PLAN) that can short-circuit straight to a RED judgment without paying for the AI court.
//!
//! aiCall deferred boundary: `review()` (already converted) always returns `Err("...PARITY-DEFERRED...")`
//! in this harness because the debate/judge calls bottom out in the deferred aiCall stub — so the
//! ONLY branch of the final court call this twin ever actually exercises here is the "AI court
//! unavailable" degrade-to-deterministic-gate path (mirrors book-run.rs's convention). BESPOKE-NATIVE:
//! proven by #[cfg(test)] native tests, not a conform-db fixture — the surface (multi-gate cascade +
//! judgments-db state mutation + an aiCall-deferred dependency) is exactly the shape the playbook
//! reserves for native tests over a fixture.
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

fn query_one(db: &Connection, sql: &str, params: &[Value]) -> Result<Option<Value>, String> {
    let rows = crate::db_json::query_rows(db, sql, params)?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned())
}

fn load_active_blueprint(db: &Connection, contract_slug: &str) -> Result<Option<Value>, String> {
    query_one(
        db,
        "SELECT id, version, blueprint_json, lint_passed FROM blueprints WHERE contract_slug = ? AND superseded_by IS NULL ORDER BY id DESC LIMIT 1",
        &[json!(contract_slug)],
    )
}

// Persist a deterministic-gate RED to judgments.sqlite and return the judgment_id. Shared by the
// conformance / snippet-checks / stub-checks gates. NOTE: propagates makeJudgmentId's Err (a JS
// throw) — the DB insert itself is fail-open (judgments DB absent), matching the JS try/catch.
fn red_gate(db: &Connection, contract_slug: &str, scope_ref: &str, reason: &str) -> Result<String, String> {
    let card_slug = query_one(db, "SELECT card_slug FROM contracts WHERE slug = ?", &[json!(contract_slug)])?
        .and_then(|r| r.get("card_slug").cloned())
        .filter(|v| truthy(Some(v)))
        .unwrap_or_else(|| json!(contract_slug));
    let judgment_id = crate::swarm_judgment_id::makeJudgmentId(&card_slug, &json!("blueprint"), &json!(scope_ref))?;
    let _ = crate::db_json::run_write(
        db,
        "INSERT INTO judgments.judgment_cases (judgment_id, card_slug, scope_kind, scope_ref, goal_line, n_items, verdict)
         VALUES (?,?,?,?,?,0,'fail')
         ON CONFLICT(judgment_id) DO UPDATE SET verdict='fail', goal_line=excluded.goal_line, updated_at=datetime('2026-01-01 00:00:00')",
        &[json!(judgment_id.clone()), card_slug, json!("blueprint"), json!(scope_ref), json!(reason)],
    );
    Ok(judgment_id)
}

// design evidence: prefers the real planned stub source; falls back to block-intent prose for a
// pre-stub / stub-less design.
fn render_design_evidence(blueprint: &Value, stub_files: &Value) -> Vec<Value> {
    if let Some(arr) = stub_files.as_array() {
        if !arr.is_empty() {
            return arr
                .iter()
                .map(|s| json!({ "path": s.get("path").cloned().unwrap_or(Value::Null), "content": str_qq(s.get("source")) }))
                .collect();
        }
    }
    let empty = vec![];
    let blocks = blueprint.get("blocks").and_then(|v| v.as_array()).unwrap_or(&empty);
    blocks
        .iter()
        .map(|fb| {
            let mut lines: Vec<String> = Vec::new();
            let children = fb.get("children").cloned().unwrap_or(json!([]));
            crate::blueprint_block_shape::walkBlocks(
                &children,
                &mut |b, _file_id| {
                    let btype = nz(b.get("type")).map(|v| str_qq(Some(v))).unwrap_or_else(|| "?".to_string());
                    lines.push(format!("- {} [{}]: {}", str_qq(b.get("id")), btype, str_qq(b.get("intent"))));
                },
                &Value::Null,
            );
            let path = nz(fb.get("path")).or_else(|| nz(fb.get("id"))).cloned().unwrap_or(Value::Null);
            json!({ "path": path, "content": format!("LOGIC: {}\nBLOCKS:\n{}", str_qq(fb.get("intent")), lines.join("\n")) })
        })
        .collect()
}

// snippets: each authored block's code is one evidence "file" (imports header + code), matching
// what blueprintCompose ships — see the FIX-COURT-SEES-COMPOSED note in the JS source. db-first
// helper: returns a plain Value (not Result) per the SKILLS_EXPORTS_DEFAULT_FUNCTION helper
// convention (mirrors ai_run_session__ensure_card_session's db-first shape).
#[allow(non_snake_case)]
pub fn renderSnippetEvidence(db: &Connection, blueprint_id: &Value) -> Value {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT block_id, code, imports_json FROM blueprint_snippets WHERE blueprint_id = ?",
        &[blueprint_id.clone()],
    )
    .unwrap_or(json!([]));
    let out: Vec<Value> = rows
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| {
            let imports_raw = match r.get("imports_json") {
                Some(Value::String(s)) => s.clone(),
                _ => "[]".to_string(),
            };
            let imports: Value = serde_json::from_str(&imports_raw).unwrap_or(json!([]));
            let header = match imports.as_array() {
                Some(a) if !a.is_empty() => format!("{}\n\n", a.iter().map(|v| str_qq(Some(v))).collect::<Vec<_>>().join("\n")),
                _ => String::new(),
            };
            json!({ "path": r.get("block_id").cloned().unwrap_or(Value::Null), "content": format!("{}{}", header, str_qq(r.get("code"))) })
        })
        .collect();
    Value::Array(out)
}

// BUILD deterministic conformance/security gate: compose authored snippets into file source, then
// check vs the projected blueprint. Returns Ok(None) whenever there's nothing to check (no
// snippets, no files, or the blueprint was never projected) — never a false-RED for a legacy /
// un-projected contract.
fn run_conformance(db: &Connection, contract_slug: &str, blueprint: &Value, blueprint_id: &Value) -> Result<Option<Value>, String> {
    let snips = crate::db_json::query_rows(db, "SELECT block_id, code, imports_json FROM blueprint_snippets WHERE blueprint_id = ?", &[blueprint_id.clone()])?;
    let snips_arr = snips.as_array().cloned().unwrap_or_default();
    if snips_arr.is_empty() {
        return Ok(None);
    }
    let mut authored = Map::new();
    for s in &snips_arr {
        let imports_raw = match s.get("imports_json") {
            Some(Value::String(t)) => t.clone(),
            _ => "[]".to_string(),
        };
        let imports: Value = serde_json::from_str(&imports_raw).unwrap_or(json!([]));
        authored.insert(str_qq(s.get("block_id")), json!({ "code": s.get("code").cloned().unwrap_or(Value::Null), "imports": imports }));
    }
    let authored_val = Value::Object(authored);

    let empty = vec![];
    let blocks = blueprint.get("blocks").and_then(|v| v.as_array()).unwrap_or(&empty);
    let mut files: Vec<Value> = Vec::new();
    for fb in blocks {
        if fb.get("type").and_then(|v| v.as_str()) != Some("file") {
            continue;
        }
        let compose_input = json!({ "block": fb, "authored": authored_val });
        let composed = crate::blueprint_blueprint_compose::blueprintCompose(&compose_input, db)?;
        let path = composed.get("path").cloned();
        let source = composed.get("source").cloned();
        if let (Some(p), Some(s)) = (path, source) {
            if truthy(Some(&p)) && truthy(Some(&s)) {
                files.push(json!({ "path": p, "source": s }));
            }
        }
    }
    if files.is_empty() {
        return Ok(None);
    }
    let conf = crate::codebase_conformance_verify::conformanceVerify(&json!({ "contract_slug": contract_slug, "files": files }), db)?;
    let blueprint_nodes = conf.get("checked").and_then(|c| c.get("blueprint_nodes")).and_then(|v| v.as_i64()).unwrap_or(0);
    Ok(if blueprint_nodes > 0 { Some(conf) } else { None })
}

// `{ blueprint_id, mode, ...r }` in JS: base keys first, then r's keys in r's own order (a key
// already present keeps its ORIGINAL position on overwrite — serde_json's preserve_order `insert`
// does exactly this), then any trailing overrides/extra keys appended after.
fn merge_result(base: Vec<(&str, Value)>, r: &Value, extra: Vec<(&str, Value)>) -> Value {
    let mut m = Map::new();
    for (k, v) in base {
        m.insert(k.to_string(), v);
    }
    if let Some(robj) = r.as_object() {
        for (k, v) in robj {
            m.insert(k.clone(), v.clone());
        }
    }
    for (k, v) in extra {
        m.insert(k.to_string(), v);
    }
    Value::Object(m)
}

#[allow(non_snake_case)]
pub fn blueprintJudge(input: &Value, db: &Connection) -> Result<Value, String> {
    let contract_slug_v = input.get("contract_slug").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&contract_slug_v)) {
        return Err("blueprint-judge: contract_slug is required".into());
    }
    let contract_slug = str_qq(Some(&contract_slug_v));
    let mode = if input.get("mode").and_then(|v| v.as_str()) == Some("snippets") { "snippets" } else { "design" };

    let row = load_active_blueprint(db, &contract_slug)?
        .ok_or_else(|| format!("blueprint-judge: blueprint '{}' not found", contract_slug))?;
    let row_id = row.get("id").cloned().unwrap_or(Value::Null);
    let version_num = row.get("version").and_then(|v| v.as_i64()).unwrap_or(1);
    let lint_passed = row.get("lint_passed").and_then(|v| v.as_i64()).unwrap_or(0);

    let raw_bp: Value = row
        .get("blueprint_json")
        .and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(json!({}));
    let blueprint = crate::blueprint_block_shape::normalizeBlueprint(&raw_bp);

    let snippets: Value = if mode == "snippets" {
        renderSnippetEvidence(db, &row_id)
    } else {
        Value::Array(render_design_evidence(&blueprint, raw_bp.get("stub_files").unwrap_or(&Value::Null)))
    };
    let scope_ref = format!("{}-v{}", mode, version_num);

    // Stamped onto the judgment case at every snippets-mode exit — see snippets-hash.rs.
    let snip_hash: Option<Value> = if mode == "snippets" {
        Some(crate::blueprint__helpers_snippets_hash::snippetsContentHash(db, &row_id)?)
    } else {
        None
    };
    let stamp_hash = |db: &Connection, judgment_id: &str| {
        if let Some(h) = &snip_hash {
            let _ = crate::db_json::run_write(db, "UPDATE judgments.judgment_cases SET snippets_hash = ? WHERE judgment_id = ?", &[h.clone(), json!(judgment_id)]);
        }
    };

    // DETERMINISTIC CONFORMANCE/SECURITY GATE (BUILD only) — fail-OPEN on runConformance itself,
    // but a real fail verdict short-circuits (NOT wrapped in the same try as runConformance in JS).
    if mode == "snippets" {
        let conf = run_conformance(db, &contract_slug, &blueprint, &row_id).unwrap_or(None);
        if let Some(c) = &conf {
            if c.get("verdict").and_then(|v| v.as_str()) == Some("fail") {
                let summary = c
                    .get("findings")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .take(4)
                    .map(|f| {
                        let kind = str_qq(f.get("kind"));
                        match nz(f.get("detail")) {
                            Some(d) => format!("{}:{}", kind, str_qq(Some(d))),
                            None => kind,
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                let judgment_id = red_gate(db, &contract_slug, &scope_ref, &format!("(conformance gate RED: {})", summary))?;
                stamp_hash(db, &judgment_id);
                return Ok(json!({
                    "blueprint_id": row_id, "mode": mode, "judgment_id": judgment_id, "verdict": "fail",
                    "conformance": c, "n": 0, "conformance_red": true,
                }));
            }
        }

        // SNIPPET-CHECKS GATE (BUILD): fail-open over the WHOLE block (call + redGate + return),
        // matching JS's single enclosing try/catch.
        let sc_input = json!({ "contract_slug": contract_slug, "blueprint_id": row_id });
        let snippet_gate: Result<Option<Value>, String> = (|| {
            let sc = crate::blueprint_snippet_checks::snippetChecks(&sc_input, db)?;
            if sc.get("ok").and_then(|v| v.as_bool()) == Some(false) {
                let summary = sc
                    .get("findings")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter(|f| f.get("severity").and_then(|v| v.as_str()) == Some("error"))
                    .take(4)
                    .map(|f| format!("{}: {}", str_qq(f.get("file")), str_qq(f.get("detail"))))
                    .collect::<Vec<_>>()
                    .join("; ");
                let judgment_id = red_gate(db, &contract_slug, &scope_ref, &format!("(snippet-checks gate RED: {})", summary))?;
                stamp_hash(db, &judgment_id);
                return Ok(Some(json!({
                    "blueprint_id": row_id, "mode": mode, "judgment_id": judgment_id, "verdict": "fail",
                    "snippet_checks": sc, "n": 0, "snippet_checks_red": true,
                })));
            }
            Ok(None)
        })();
        if let Ok(Some(v)) = snippet_gate {
            return Ok(v);
        }
    }

    // STUB-CHECKS GATE (DESIGN): fail-open over the WHOLE block, same pattern.
    if mode == "design" {
        let sc_input = json!({ "contract_slug": contract_slug, "blueprint_id": row_id });
        let stub_gate: Result<Option<Value>, String> = (|| {
            let sc = crate::blueprint_stub_checks::stubChecks(&sc_input, db)?;
            if sc.get("ok").and_then(|v| v.as_bool()) == Some(false) {
                let summary = sc
                    .get("findings")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter(|f| f.get("severity").and_then(|v| v.as_str()) == Some("error"))
                    .take(4)
                    .map(|f| format!("{}: {}", str_qq(f.get("file")), str_qq(f.get("detail"))))
                    .collect::<Vec<_>>()
                    .join("; ");
                let judgment_id = red_gate(db, &contract_slug, &scope_ref, &format!("(stub-checks gate RED: {})", summary))?;
                return Ok(Some(json!({
                    "blueprint_id": row_id, "mode": mode, "judgment_id": judgment_id, "verdict": "fail",
                    "stub_checks": sc, "n": 0, "stub_checks_red": true,
                })));
            }
            Ok(None)
        })();
        if let Ok(Some(v)) = stub_gate {
            return Ok(v);
        }
    }

    // Defect carry-over (redesigns only): fail-open.
    let mut prior_defects_md = String::new();
    if mode == "design" && version_num > 1 {
        let carried: Result<String, String> = (|| {
            let card_slug = query_one(db, "SELECT card_slug FROM contracts WHERE slug = ?", &[json!(contract_slug)])?
                .and_then(|r| r.get("card_slug").cloned())
                .filter(|v| truthy(Some(v)))
                .map(|v| str_qq(Some(&v)))
                .unwrap_or_else(|| contract_slug.clone());
            let prior_scope_ref = format!("design-v{}", version_num - 1);
            let items = crate::db_json::query_rows(
                db,
                "SELECT ji.statement, ji.judge_why FROM judgments.judgment_items ji
                 JOIN judgments.judgment_cases jc ON jc.judgment_id = ji.judgment_id
                 WHERE jc.card_slug = ? AND jc.scope_kind = 'blueprint' AND jc.scope_ref = ?
                   AND ji.side = 'prosecutor' AND ji.judge_value = 1",
                &[json!(card_slug), json!(prior_scope_ref)],
            )?;
            let items_arr = items.as_array().cloned().unwrap_or_default();
            if items_arr.is_empty() {
                return Ok(String::new());
            }
            let body = items_arr
                .iter()
                .map(|i| {
                    let stmt = str_qq(i.get("statement"));
                    match nz(i.get("judge_why")).filter(|v| truthy(Some(v))) {
                        Some(w) => format!("- {} ({})", stmt, str_qq(Some(w))),
                        None => format!("- {}", stmt),
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            Ok(format!(
                "═══ PRIOR SUSTAINED DEFECTS (the previous design version was REJECTED for these — verify each is addressed; if one is not, RE-RAISE it) ═══\n{}",
                body
            ))
        })();
        prior_defects_md = carried.unwrap_or_default();
    }

    // DETERMINISTIC PRIOR-DEFECT RECHECK: fail-open. When a prior sustained defect QUOTES a
    // literal that still appears verbatim in the new blueprint, RED now — no court, no tokens.
    if mode == "design" && !prior_defects_md.is_empty() {
        let recheck: Result<Option<Value>, String> = (|| {
            let card_slug2 = query_one(db, "SELECT card_slug FROM contracts WHERE slug = ?", &[json!(contract_slug)])?
                .and_then(|r| r.get("card_slug").cloned())
                .filter(|v| truthy(Some(v)))
                .map(|v| str_qq(Some(&v)))
                .unwrap_or_else(|| contract_slug.clone());
            let prior_scope_ref = format!("design-v{}", version_num - 1);
            let prior_items = crate::db_json::query_rows(
                db,
                "SELECT ji.statement, ji.judge_why FROM judgments.judgment_items ji
                 JOIN judgments.judgment_cases jc ON jc.judgment_id = ji.judgment_id
                 WHERE jc.card_slug = ? AND jc.scope_kind = 'blueprint' AND jc.scope_ref = ?
                   AND ji.side = 'prosecutor' AND ji.judge_value = 1",
                &[json!(card_slug2), json!(prior_scope_ref)],
            )?;
            let bp_text = str_qq(row.get("blueprint_json"));
            let literal_re = regex::Regex::new(r#"['"`]([^'"`]{4,80})['"`]"#).map_err(|e| e.to_string())?;
            let mut persisting: Vec<(String, String, String)> = Vec::new();
            for it in prior_items.as_array().cloned().unwrap_or_default() {
                let text = format!("{} {}", str_qq(it.get("statement")), str_qq(it.get("judge_why")));
                let hit = literal_re.captures_iter(&text).map(|c| c[1].to_string()).find(|l| bp_text.contains(l.as_str()));
                if let Some(hit) = hit {
                    persisting.push((str_qq(it.get("statement")), str_qq(it.get("judge_why")), hit));
                }
            }
            if persisting.is_empty() {
                return Ok(None);
            }
            let judgment_id = format!("{}::blueprint:{}", card_slug2, scope_ref);
            let goal = format!(
                "(prior-defect recheck RED: {} defect(s) from design-v{} persist verbatim in v{})",
                persisting.len(),
                version_num - 1,
                version_num
            );
            crate::db_json::run_write(
                db,
                "INSERT INTO judgments.judgment_cases (judgment_id, card_slug, scope_kind, scope_ref, goal_line, n_items, verdict)
                 VALUES (?,?,?,?,?,?, 'fail')
                 ON CONFLICT(judgment_id) DO UPDATE SET verdict='fail', goal_line=excluded.goal_line, updated_at=datetime('2026-01-01 00:00:00')",
                &[json!(judgment_id), json!(card_slug2), json!("blueprint"), json!(scope_ref), json!(goal), json!(persisting.len() as i64)],
            )?;
            for (i, (statement, _judge_why, literal)) in persisting.iter().enumerate() {
                let _ = crate::db_json::run_write(
                    db,
                    "INSERT INTO judgments.judgment_items (judgment_id, side, item_id, statement, links_json, grounded, judge_value, judge_why) VALUES (?,?,?,?,'[]',1,1,?)",
                    &[
                        json!(judgment_id),
                        json!("prosecutor"),
                        json!(format!("recheck-{}", i + 1)),
                        json!(statement),
                        json!(format!("deterministic recheck: '{}' still present in the redesigned blueprint", literal)),
                    ],
                );
            }
            println!(
                "[gate judge.design] {} session=card-{} RECHECK RED — {} prior defect(s) persist verbatim (no court call)",
                "2026-01-01T00:00:00.000Z", card_slug2, persisting.len()
            );
            Ok(Some(json!({
                "blueprint_id": row_id, "mode": mode, "judgment_id": judgment_id, "verdict": "fail",
                "recheck": true, "persisting": persisting.len() as i64,
            })))
        })();
        if let Ok(Some(v)) = recheck {
            return Ok(v);
        }
    }

    let review_input = json!({
        "contract_slug": contract_slug, "scope_kind": "blueprint", "scope_ref": scope_ref,
        "snippets": snippets, "scope_class": if mode == "design" { "design" } else { "code" },
        "prior_defects_md": prior_defects_md,
    });
    match crate::swarm_review::review(&review_input, db) {
        Ok(r) => {
            let base = vec![("blueprint_id", row_id.clone()), ("mode", json!(mode))];
            let measurable = r.get("measurable").and_then(|v| v.as_bool()).unwrap_or(false);
            let judgment_id = r.get("judgment_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if !measurable {
                let det_pass = if mode == "snippets" { snippets.as_array().map(|a| !a.is_empty()).unwrap_or(false) } else { lint_passed == 1 };
                let verdict = if det_pass { "pass" } else { "fail" };
                let _ = crate::db_json::run_write(
                    db,
                    "UPDATE judgments.judgment_cases SET verdict = ?, goal_line = ?, updated_at = datetime('2026-01-01 00:00:00') WHERE judgment_id = ?",
                    &[json!(verdict), json!(format!("(no measurable goal — deferred to the deterministic gate: {})", verdict)), json!(judgment_id)],
                );
                stamp_hash(db, &judgment_id);
                return Ok(merge_result(base, &r, vec![("verdict", json!(verdict)), ("degraded", json!(true)), ("non_measurable", json!(true))]));
            }

            // DEGENERATE-COURT GUARD: fail-open over query + update + return, same pattern.
            let degenerate: Result<Option<Value>, String> = (|| {
                let jc = query_one(db, "SELECT defender_trues, prosecutor_trues, n_items FROM judgments.judgment_cases WHERE judgment_id = ?", &[json!(judgment_id.clone())])?;
                if let Some(jc) = jc {
                    let n_items = jc.get("n_items").and_then(|v| v.as_i64()).unwrap_or(0);
                    let defender_trues = jc.get("defender_trues").and_then(|v| v.as_i64()).unwrap_or(0);
                    let prosecutor_trues = jc.get("prosecutor_trues").and_then(|v| v.as_i64()).unwrap_or(0);
                    if n_items > 0 && defender_trues == 0 && prosecutor_trues == 0 {
                        let det_pass = if mode == "snippets" { snippets.as_array().map(|a| !a.is_empty()).unwrap_or(false) } else { lint_passed == 1 };
                        let verdict = if det_pass { "pass" } else { "fail" };
                        crate::db_json::run_write(
                            db,
                            "UPDATE judgments.judgment_cases SET verdict = ?, goal_line = ?, updated_at = datetime('2026-01-01 00:00:00') WHERE judgment_id = ?",
                            &[json!(verdict), json!(format!("(degenerate court: 0 grounded claims either side — deferred to the deterministic gate: {})", verdict)), json!(judgment_id.clone())],
                        )?;
                        stamp_hash(db, &judgment_id);
                        return Ok(Some(merge_result(base.clone(), &r, vec![("verdict", json!(verdict)), ("degraded", json!(true)), ("degenerate_court", json!(true))])));
                    }
                }
                Ok(None)
            })();
            if let Ok(Some(v)) = degenerate {
                return Ok(v);
            }
            stamp_hash(db, &judgment_id);
            Ok(merge_result(base, &r, vec![]))
        }
        Err(e) => {
            // The adversarial AI court is unavailable — degrade to the deterministic gate that
            // already ran (lint for the design; authored code present for snippets).
            let card_slug = query_one(db, "SELECT card_slug FROM contracts WHERE slug = ?", &[json!(contract_slug)])
                .ok()
                .flatten()
                .and_then(|r| r.get("card_slug").cloned())
                .filter(|v| truthy(Some(v)))
                .unwrap_or_else(|| json!(contract_slug));
            let judgment_id = crate::swarm_judgment_id::makeJudgmentId(&card_slug, &json!("blueprint"), &json!(scope_ref))
                .unwrap_or_else(|_| format!("{}::blueprint:{}", str_qq(Some(&card_slug)), scope_ref));
            let det_pass = if mode == "snippets" { snippets.as_array().map(|a| !a.is_empty()).unwrap_or(false) } else { lint_passed == 1 };
            let verdict = if det_pass { "pass" } else { "fail" };
            let _ = crate::db_json::run_write(
                db,
                "INSERT INTO judgments.judgment_cases (judgment_id, card_slug, scope_kind, scope_ref, goal_line, n_items, verdict)
                 VALUES (?,?,?,?,?,0,?)
                 ON CONFLICT(judgment_id) DO UPDATE SET verdict=excluded.verdict, goal_line=excluded.goal_line, updated_at=datetime('2026-01-01 00:00:00')",
                &[json!(judgment_id.clone()), card_slug, json!("blueprint"), json!(scope_ref), json!(format!("(AI court unavailable — deterministic gate: {})", verdict)), json!(verdict)],
            );
            stamp_hash(db, &judgment_id);
            Ok(json!({
                "blueprint_id": row_id, "mode": mode, "judgment_id": judgment_id, "verdict": verdict,
                "n": 0, "degraded": true, "error": e,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "
            CREATE TABLE contracts (id INTEGER PRIMARY KEY, slug TEXT, card_slug TEXT, acceptance_json TEXT, microtasks_json TEXT, files_touched_json TEXT);
            CREATE TABLE cards (id INTEGER PRIMARY KEY, slug TEXT, title TEXT, body_json TEXT, acceptance_json TEXT);
            CREATE TABLE agents (id INTEGER PRIMARY KEY, name TEXT, persona_md TEXT, system_md TEXT);
            CREATE TABLE blueprints (id INTEGER PRIMARY KEY, contract_slug TEXT, version INTEGER, lint_passed INTEGER, blueprint_json TEXT, superseded_by INTEGER);
            CREATE TABLE blueprint_snippets (id INTEGER PRIMARY KEY AUTOINCREMENT, blueprint_id INTEGER NOT NULL, block_id TEXT NOT NULL, code TEXT, imports_json TEXT);
            CREATE TABLE pl_screenings (id INTEGER PRIMARY KEY, card_slug TEXT, screening_json TEXT);
            ",
        )
        .unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS knowledge;").unwrap();
        c.execute_batch(
            "
            CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL DEFAULT 1, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
            CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL DEFAULT 0, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
            CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL DEFAULT 0, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
            ",
        )
        .unwrap();
        c.execute_batch(
            "
            CREATE TABLE judgments.judgment_cases (judgment_id TEXT PRIMARY KEY, card_slug TEXT NOT NULL, scope_kind TEXT NOT NULL, scope_ref TEXT NOT NULL DEFAULT '', goal_line TEXT, n_items INTEGER, defender_trues INTEGER, prosecutor_trues INTEGER, defender_rate REAL, prosecutor_rate REAL, defender_accuracy REAL, prosecutor_accuracy REAL, verdict TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), updated_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), snippets_hash TEXT);
            CREATE TABLE judgments.judgment_items (id INTEGER PRIMARY KEY AUTOINCREMENT, judgment_id TEXT NOT NULL, side TEXT NOT NULL, item_id TEXT NOT NULL, statement TEXT NOT NULL DEFAULT '', links_json TEXT NOT NULL DEFAULT '[]', grounded INTEGER NOT NULL DEFAULT 1, judge_value INTEGER, judge_accuracy REAL, judge_why TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), UNIQUE (judgment_id, side, item_id));
            CREATE TABLE judgments.judgment_calls (id INTEGER PRIMARY KEY AUTOINCREMENT, judgment_id TEXT NOT NULL, role TEXT NOT NULL, tier TEXT, prompt_md TEXT, response_md TEXT, ai_call_id INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')));
            CREATE TABLE judgments.court_evidence (judgment_id TEXT PRIMARY KEY, contract_slug TEXT, scope_kind TEXT, scope_ref TEXT, corpus_json TEXT, evidence_md TEXT, n_blocks INTEGER, n_acceptance INTEGER, has_card INTEGER, has_blueprint INTEGER, has_screening INTEGER, created_at TEXT DEFAULT '2026-01-01 00:00:00');
            ",
        )
        .unwrap();
        c
    }

    fn seed_contract(c: &Connection, slug: &str, card_slug: &str) {
        c.execute(
            "INSERT INTO contracts (slug, card_slug, acceptance_json, files_touched_json) VALUES (?1, ?2, '[]', '[]')",
            rusqlite::params![slug, card_slug],
        )
        .unwrap();
        c.execute("INSERT INTO cards (slug, body_json) VALUES (?1, '{\"kind\":\"feature\",\"size\":\"S\",\"why\":\"x\"}')", rusqlite::params![card_slug]).unwrap();
    }

    #[test]
    fn missing_contract_slug_is_required() {
        let c = conn();
        let err = blueprintJudge(&json!({}), &c).unwrap_err();
        assert!(err.contains("contract_slug is required"));
    }

    #[test]
    fn blueprint_not_found() {
        let c = conn();
        let err = blueprintJudge(&json!({ "contract_slug": "cs1" }), &c).unwrap_err();
        assert!(err.contains("blueprint 'cs1' not found"), "unexpected: {err}");
    }

    #[test]
    fn design_mode_degrades_to_lint_pass_when_ai_court_unavailable() {
        let c = conn();
        seed_contract(&c, "cs1", "card1");
        c.execute(
            "INSERT INTO blueprints (contract_slug, version, lint_passed, blueprint_json) VALUES ('cs1', 1, 1, '{\"blocks\":[]}')",
            [],
        )
        .unwrap();
        let r = blueprintJudge(&json!({ "contract_slug": "cs1", "mode": "design" }), &c).unwrap();
        assert_eq!(r["verdict"], json!("pass"));
        assert_eq!(r["degraded"], json!(true));
        assert_eq!(r["mode"], json!("design"));
        assert!(r["error"].as_str().unwrap().contains("ai-call"), "{}", r["error"]);
        // side effect: a judgments row was persisted with that verdict.
        let jid = r["judgment_id"].as_str().unwrap().to_string();
        let row = crate::db_json::query_rows(&c, "SELECT verdict FROM judgments.judgment_cases WHERE judgment_id = ?", &[json!(jid)]).unwrap();
        assert_eq!(row[0]["verdict"], json!("pass"));
    }

    #[test]
    fn design_mode_degrades_to_fail_when_lint_failed() {
        let c = conn();
        seed_contract(&c, "cs2", "card2");
        c.execute(
            "INSERT INTO blueprints (contract_slug, version, lint_passed, blueprint_json) VALUES ('cs2', 1, 0, '{\"blocks\":[]}')",
            [],
        )
        .unwrap();
        let r = blueprintJudge(&json!({ "contract_slug": "cs2", "mode": "design" }), &c).unwrap();
        assert_eq!(r["verdict"], json!("fail"));
        assert_eq!(r["degraded"], json!(true));
    }

    #[test]
    fn snippets_mode_degrades_pass_when_snippets_present() {
        let c = conn();
        seed_contract(&c, "cs3", "card3");
        c.execute(
            "INSERT INTO blueprints (contract_slug, version, lint_passed, blueprint_json) VALUES ('cs3', 1, 0, '{\"blocks\":[]}')",
            [],
        )
        .unwrap();
        let bp_id: i64 = c.query_row("SELECT id FROM blueprints WHERE contract_slug='cs3'", [], |r| r.get(0)).unwrap();
        c.execute(
            "INSERT INTO blueprint_snippets (blueprint_id, block_id, code, imports_json) VALUES (?1, 'b1', 'fn x() {}', '[\"use a;\"]')",
            rusqlite::params![bp_id],
        )
        .unwrap();
        let r = blueprintJudge(&json!({ "contract_slug": "cs3", "mode": "snippets" }), &c).unwrap();
        assert_eq!(r["verdict"], json!("pass"), "unexpected result: {r}");
        assert_eq!(r["mode"], json!("snippets"));
        // snippets_hash was stamped on the persisted row.
        let jid = r["judgment_id"].as_str().unwrap().to_string();
        let row = crate::db_json::query_rows(&c, "SELECT snippets_hash FROM judgments.judgment_cases WHERE judgment_id = ?", &[json!(jid)]).unwrap();
        assert!(row[0]["snippets_hash"].as_str().is_some());
    }

    #[test]
    fn snippets_mode_degrades_fail_when_no_snippets() {
        let c = conn();
        seed_contract(&c, "cs4", "card4");
        c.execute(
            "INSERT INTO blueprints (contract_slug, version, lint_passed, blueprint_json) VALUES ('cs4', 1, 1, '{\"blocks\":[]}')",
            [],
        )
        .unwrap();
        let r = blueprintJudge(&json!({ "contract_slug": "cs4", "mode": "snippets" }), &c).unwrap();
        assert_eq!(r["verdict"], json!("fail"));
    }

    #[test]
    fn stub_checks_gate_reds_a_degenerate_stub_before_the_court() {
        let c = conn();
        seed_contract(&c, "cs5", "card5");
        let bp = json!({
            "blocks": [],
            "stub_files": [ { "path": "src/a.js", "source": "// TODO" } ],
        });
        c.execute(
            "INSERT INTO blueprints (contract_slug, version, lint_passed, blueprint_json) VALUES ('cs5', 1, 1, ?1)",
            rusqlite::params![bp.to_string()],
        )
        .unwrap();
        let r = blueprintJudge(&json!({ "contract_slug": "cs5", "mode": "design" }), &c).unwrap();
        assert_eq!(r["verdict"], json!("fail"));
        assert_eq!(r["stub_checks_red"], json!(true));
        assert!(r.get("error").is_none(), "should short-circuit before the AI court: {r}");
    }

    #[test]
    fn render_snippet_evidence_prepends_import_header_when_present() {
        let c = conn();
        c.execute(
            "INSERT INTO blueprint_snippets (blueprint_id, block_id, code, imports_json) VALUES (1, 'b1', 'console.log(1)', '[\"import a from \\\"a\\\"\"]')",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO blueprint_snippets (blueprint_id, block_id, code, imports_json) VALUES (1, 'b2', 'return 2', NULL)", []).unwrap();
        let out = renderSnippetEvidence(&c, &json!(1));
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["path"], json!("b1"));
        assert_eq!(arr[0]["content"], json!("import a from \"a\"\n\nconsole.log(1)"));
        assert_eq!(arr[1]["content"], json!("return 2"));
    }

    #[test]
    fn render_snippet_evidence_no_rows_is_empty_array() {
        let c = conn();
        let out = renderSnippetEvidence(&c, &json!(999));
        assert_eq!(out, json!([]));
    }
}
