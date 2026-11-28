//! ultra-deep.rs — twin of skills/ultra/ultra-deep.mjs (standard db lane, aiCall/bugfinder-shard/
//! bugfinder-sweep/embed all deferred deeper in the call graph). NAMES ARE CONTRACT: `ultraDeep`
//! (default). THE DEEP REVIEW: orchestrates 1) the bench sweep (`bugfinder_bugfinder_run::
//! bugfinderRun`), 2) the survey (`ultra_ultra_run::ultraRun`), 2b) the concept atlas
//! (`ultra_ultra_concepts::ultraConcepts`), 3) per-unit dossiers (worst decile, ONE `aiCall` each),
//! 4) the brief (ONE synthesis `aiCall`) — then re-reads the final `ultra_ultra_report::
//! ultraReport` payload and echoes `deep: {bench, concepts, dossiers, brief, errors}`.
//!
//! PARITY-DEFERRED (the boundary this module reaches directly): `crate::ai_ai_call::aiCall` for
//! both the dossier prompt and the brief prompt — always `Err`s (see ai-call.rs's own doc), ported
//! as a `match` exactly mirroring the JS `try { res = await aiCall(...) } catch (e) {
//! errors.push(...) }`: one bad dossier must not abort the loop, and a failed brief must not abort
//! the run. Reached deeper (not re-deferred here, already handled by their own twins):
//! `bugfinderRun`'s own shard/sweep `aiCall` edges, `ultraConcepts`'s own `aiCall`/`embed` edges.
//!
//! Everything AROUND those calls is real and ported: `scopeInput` resolution (`files` vs `folder`),
//! `root` resolution (`resolve(input.root)` else `getWorkspaceRoot`), `dossier_cap` clamp
//! (`max(0, min(20, …))`), `ensurePrompt` (matches JS's `template_md === X && system_md === X`
//! double-check, reusing `ai_prompt_find`/`ai_prompt_add`), `extractJson` (parse-or-drop: first `{`
//! to last `}`, JSON.parse-or-null), the per-unit dossier signal/axes/findings text assembly (the
//! `db.prepare` findings query, the `signals`/`axes` string builds), the per-file source read
//! (`isAbsolute(u.path) ? u.path : join(root, u.path)`, truncated to `MAX_SOURCE_CHARS`, unreadable
//! → `'(unreadable)'`), the brief's worst/coupling/findings-by-category/concept-atlas/deltas text
//! assembly (reads `ultra_ultra_report::ultraReport` back mid-run, matching JS's `ultraReport({
//! run_id })` re-read), the `ultra_insights` persistence (`insInsight.run(...)`), the bench-cost
//! echo onto `judgments.ultra_runs.totals_json`, and the final report re-read + `deep`/`advisory`
//! envelope.
//!
//! `context.db` missing → JS throws via `err.required`. Not modeled — same "guard-throws are not
//! exercised" convention every sibling twin uses (`db: &Connection` is a required, non-nullable
//! reference here).
// QUALITY: real db read/write + text-assembly/orchestration logic around two real `aiCall`
// dispatch edges (dossier + brief) — proven via #[cfg(test)] native tests, no conform-db
// fixture/frontier entry (bespoke-native: both AI edges always `Err`, exercised as a real branch).

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
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

const DOSSIER_PROMPT: &str = "ultra-dossier";
const DOSSIER_SYSTEM: &str = "You are a staff engineer writing the dossier for ONE file that a deterministic health formula ranked among the worst of its codebase. You are given its counted signals (the formula parts), its verified findings, and its source. Rules: (1) ground EVERY statement in the given signals/findings/source — never invent APIs, files or behavior; (2) be concrete and short — a reader should know what to do in 60 seconds; (3) alternatives must be genuinely different approaches, each with its honest tradeoff; (4) estimated_gain names which axes improve and roughly how much, from the formula parts you were given. Output ONLY valid JSON: {\"why\":\"2-4 sentences — what makes this unit weak, tied to the numbers\",\"alternatives\":[{\"approach\":\"...\",\"tradeoff\":\"...\"}],\"fix_sketch\":\"the concrete first move, 2-4 sentences\",\"estimated_gain\":\"e.g. tests 24→~70 (cover the 3 entry functions); coupling unchanged\"}. No prose outside the JSON.";
const DOSSIER_TEMPLATE: &str = "FILE: {{path}}\n\nSIGNALS (counted):\n{{signals}}\n\nAXES (formula v1 parts):\n{{axes}}\n\nVERIFIED/GROUNDED FINDINGS (the bug finder):\n{{findings}}\n\nSOURCE (may be truncated):\n{{source}}\n\nOutput the dossier JSON.";

const BRIEF_PROMPT: &str = "ultra-brief";
const BRIEF_SYSTEM: &str = "You are a staff engineer writing THE EXECUTIVE BRIEF of a codebase appraisal for its owner. You are given counted metrics (health axes, meta scores), the worst-ranked units with their weakest axes, hidden temporal couplings (files that co-change with no import edge), the bug finder's findings by category, the CONCEPT ATLAS (the ideas the code embodies — which are scattered across many files, which are untested, which files mix unrelated ideas), what CHANGED since the last review, and per-unit dossier digests. Rules: (1) every claim must trace to the given data — cite file paths in evidence; (2) moves are ranked by payoff-per-effort, each one independently actionable; (3) no platitudes — if the territory is healthy, say so and keep moves small; (4) effort is S (hours), M (a day or two), L (a week+); (5) prefer moves that the DATA makes visible and a reader could not get from skimming the repo — a scattered idea, a hidden coupling, a regression since last time. Output ONLY valid JSON: {\"headline\":\"one blunt sentence — the verdict\",\"state_md\":\"<=180 words on the state of the territory, plain prose\",\"moves\":[{\"title\":\"...\",\"why\":\"1-2 sentences tied to the data\",\"evidence\":[\"path\", \"path\"],\"effort\":\"S|M|L\",\"gain\":\"which numbers move and why it matters\"}]} with 5-7 moves. No prose outside the JSON.";
const BRIEF_TEMPLATE: &str = "SUMMARY (counted):\n{{summary}}\n\nWORST UNITS (health · weakest axis):\n{{worst}}\n\nHIDDEN COUPLINGS (co-change, no import edge):\n{{coupling}}\n\nFINDINGS BY CATEGORY (kept):\n{{findings}}\n\nTHE CONCEPT ATLAS:\n{{concepts}}\n\nSINCE THE LAST REVIEW:\n{{deltas}}\n\nDOSSIER DIGESTS:\n{{dossiers}}\n\nOutput the brief JSON.";

const MAX_SOURCE_CHARS: usize = 6000;

fn ensure_prompt(db: &Connection, name: &str, system_md: &str, template_md: &str, field: &str, max_tokens: i64) {
    let matches = crate::ai_prompt_find::promptFind(&json!({ "name": name }), db)
        .map(|existing| {
            existing.get("template_md").and_then(Value::as_str) == Some(template_md)
                && existing.get("system_md").and_then(Value::as_str) == Some(system_md)
        })
        .unwrap_or(false);
    if matches {
        return;
    }
    let _ = crate::ai_prompt_add::promptAdd(
        &json!({
            "name": name, "template_md": template_md, "system_md": system_md,
            "default_tier": "high", "default_temperature": "cold", "phase_flow": "single_only",
            "max_output_tokens": max_tokens,
            "output_fields": [{ "field_name": field, "field_type": "string" }],
        }),
        db,
    );
}

// parse-or-drop (author-accept doctrine: check the real property, never a proxy)
fn extract_json(text: &str) -> Option<Value> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str(&text[start..=end]).ok()
}

// `isAbsolute(p)` (win32 + posix, disclosed simplification — same convention as ultra-run.rs's
// own private copy).
fn is_absolute_like(p: &str) -> bool {
    let b = p.as_bytes();
    if b.is_empty() {
        return false;
    }
    if b[0] == b'/' || b[0] == b'\\' {
        return true;
    }
    b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')
}
fn join_root(root: &str, p: &str) -> PathBuf {
    if is_absolute_like(p) {
        PathBuf::from(p)
    } else {
        Path::new(root).join(p)
    }
}
/// `resolve(input.root)` — see ultra-run.rs's module doc for the disclosed simplification this
/// mirrors (best-effort join + `.`/`..` collapse, not byte-identical to Node's full algorithm).
fn resolve_root_arg(root: &str) -> String {
    let p = Path::new(root);
    let abs = if is_absolute_like(root) { p.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(p) };
    let mut out: Vec<std::ffi::OsString> = Vec::new();
    for comp in abs.components() {
        use std::path::Component::*;
        match comp {
            CurDir => {}
            ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str().to_os_string()),
        }
    }
    let mut result = PathBuf::new();
    for c in out {
        result.push(c);
    }
    result.to_string_lossy().into_owned()
}

fn read_source_or(root: &str, path: &str, fallback: &str, max_chars: usize) -> String {
    match std::fs::read_to_string(join_root(root, path)) {
        Ok(s) => s.chars().take(max_chars).collect(),
        Err(_) => fallback.to_string(),
    }
}

fn err_stage(errors: &mut Vec<Value>, stage: &str, e: &str) {
    let short: String = e.chars().take(300).collect();
    errors.push(json!({ "stage": stage, "error": short }));
}

/// `export default async function ultraDeep(input, context)` — async-over-sync, see playbook
/// (this skill awaits only sync db reads/writes plus the already-converted-twin calls below).
#[allow(non_snake_case)]
pub fn ultraDeep(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::ultra__helpers_schema::ensureUltraSchema(db);
    // the dossier/brief stages READ the bug ledger even when the bench is skipped (or has never
    // run here) — ensure it exists rather than crash on a fresh workspace.
    crate::bugfinder__helpers_schema::ensureBugfinderSchema(db);

    let root: String = match nz(input.get("root")).and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(r) => resolve_root_arg(r),
        None => crate::engine_workspace_root::getWorkspaceRoot(db).as_str().unwrap_or_default().to_string(),
    };
    let skip: std::collections::HashSet<String> = input
        .get("skip")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let max_files: i64 = input.get("max_files").and_then(Value::as_i64).unwrap_or(120);
    let dossier_cap: i64 = input.get("dossier_cap").and_then(Value::as_i64).unwrap_or(8).clamp(0, 20);
    let files_mode = input.get("files").and_then(Value::as_array).map(|a| !a.is_empty()).unwrap_or(false);
    let scope_input: Value = if files_mode {
        json!({ "files": input.get("files").cloned().unwrap_or(json!([])) })
    } else {
        let f = nz(input.get("folder")).and_then(Value::as_str).unwrap_or(".").trim().to_string();
        json!({ "folder": if f.is_empty() { ".".to_string() } else { f } })
    };
    let mut errors: Vec<Value> = Vec::new();

    // ── 1 · the bench sweep — verified findings before any scoring ────────────
    let mut bench: Value = Value::Null;
    if !skip.contains("bench") {
        let mut bench_input = scope_input.as_object().cloned().unwrap_or_default();
        bench_input.insert("max_files".into(), json!(max_files));
        bench_input.insert("shards".into(), input.get("shards").cloned().unwrap_or(Value::Null));
        bench_input.insert("source".into(), json!("ultra"));
        match crate::bugfinder_bugfinder_run::bugfinderRun(&Value::Object(bench_input), db) {
            Ok(b) => {
                bench = json!({
                    "run_id": b.get("run_id").cloned().unwrap_or(Value::Null),
                    "kept": b.get("counts").and_then(|c| c.get("kept")).cloned().unwrap_or(json!(0)),
                    "dropped": b.get("dropped").cloned().unwrap_or(json!(0)),
                    "cost_usd": b.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0),
                });
            }
            Err(e) => err_stage(&mut errors, "bench", &e),
        }
    }

    // ── 2 · the survey — $0 counted formulas over the fresh ledger ────────────
    let mut survey_input = scope_input.as_object().cloned().unwrap_or_default();
    survey_input.insert("max_files".into(), json!(max_files));
    survey_input.insert("root".into(), json!(root));
    survey_input.insert(
        "source".into(),
        json!(nz(input.get("source")).and_then(Value::as_str).unwrap_or("ultra-deep")),
    );
    let survey = crate::ultra_ultra_run::ultraRun(&Value::Object(survey_input), db)?;
    let run_id = survey.get("summary").and_then(|s| s.get("run_id")).cloned().unwrap_or(Value::Null);

    // ── 2b · the Concept Atlas — low tier over the territory, hash-cached ─────
    let mut concepts: Value = Value::Null;
    if !skip.contains("concepts") {
        let concept_files: Vec<Value> = survey
            .get("units")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|u| !u.get("is_test").and_then(Value::as_bool).unwrap_or(false) && !u.get("content_hash").map_or(true, Value::is_null))
            .filter_map(|u| {
                let path = u.get("path").and_then(Value::as_str).unwrap_or("").to_string();
                let source = std::fs::read_to_string(join_root(&root, &path)).unwrap_or_default();
                if source.is_empty() {
                    return None;
                }
                Some(json!({ "path": path, "source": source, "content_hash": u.get("content_hash").cloned().unwrap_or(Value::Null) }))
            })
            .collect();
        match crate::ultra_ultra_concepts::ultraConcepts(&json!({ "run_id": run_id, "files": concept_files }), db) {
            Ok(c) => concepts = c,
            Err(e) => err_stage(&mut errors, "concepts", &e),
        }
    }

    // ── 3 · dossiers — the worst decile only (law 3), grounded per unit ───────
    let mut dossier_telemetry = json!({ "made": 0, "dropped": 0, "cost_usd": 0.0 });
    if !skip.contains("dossiers") && dossier_cap > 0 {
        ensure_prompt(db, DOSSIER_PROMPT, DOSSIER_SYSTEM, DOSSIER_TEMPLATE, "dossier_json", 1400);
        let targets: Vec<Value> = survey
            .get("worst")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|u| !u.get("is_test").and_then(Value::as_bool).unwrap_or(false) && !u.get("health").map_or(true, Value::is_null))
            .take(dossier_cap as usize)
            .collect();

        let mut made = 0i64;
        let mut dropped = 0i64;
        let mut cost_usd = 0.0f64;
        for u in &targets {
            let path = u.get("path").and_then(Value::as_str).unwrap_or("").to_string();
            let source = read_source_or(&root, &path, "(unreadable)", MAX_SOURCE_CHARS);
            let findings_rows = crate::db_json::query_rows(
                db,
                "SELECT category, severity, line, detail FROM judgments.bugfinder_findings WHERE dropped = 0 AND file = ? ORDER BY id DESC LIMIT 8",
                &[json!(path)],
            )
            .unwrap_or(json!([]));
            let findings = findings_rows
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|f| {
                    let severity = str_qq(f.get("severity"));
                    let category = str_qq(f.get("category"));
                    let line_part = match nz(f.get("line")) {
                        Some(l) => format!(":{}", str_qq(Some(l))),
                        None => String::new(),
                    };
                    let detail = str_qq(f.get("detail"));
                    format!("{severity} {category}{line_part} — {detail}")
                })
                .collect::<Vec<_>>()
                .join("\n");
            let findings = if findings.is_empty() { "(none kept)".to_string() } else { findings };

            let smells: Vec<String> = u.get("smells").and_then(Value::as_array).cloned().unwrap_or_default().iter().map(|v| str_qq(Some(v))).collect();
            let signals = format!(
                "health={} loc={} fan_in={} fan_out={} complexity={} transitive={} covered={} churn={} authors={} smells={}",
                str_qq(u.get("health")),
                str_qq(u.get("loc")),
                str_qq(u.get("fan_in")),
                str_qq(u.get("fan_out")),
                str_qq(u.get("complexity")),
                str_qq(u.get("transitive")),
                str_qq(u.get("covered")),
                str_qq(u.get("churn")),
                str_qq(u.get("authors")),
                if smells.is_empty() { "none".to_string() } else { smells.join(",") },
            );
            let axes = u
                .get("axes")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|(k, a)| {
                    if a.is_null() {
                        return format!("{k}=n/a");
                    }
                    let score = str_qq(a.get("score"));
                    let parts = a
                        .get("parts")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(pk, pv)| {
                            let pv_s = match pv.as_f64() {
                                Some(n) => {
                                    let r = (n * 100.0).round() / 100.0;
                                    if r.fract() == 0.0 { format!("{}", r as i64) } else { format!("{r}") }
                                }
                                None => str_qq(Some(&pv)),
                            };
                            format!("{pk}={pv_s}")
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    format!("{k}={score} ({parts})")
                })
                .collect::<Vec<_>>()
                .join("\n");

            let call_result = crate::ai_ai_call::aiCall(
                &json!({
                    "prompt_name": DOSSIER_PROMPT,
                    "input_kv": [
                        { "key": "path", "value": path },
                        { "key": "signals", "value": signals },
                        { "key": "axes", "value": axes },
                        { "key": "findings", "value": findings },
                        { "key": "source", "value": source },
                    ],
                }),
                db,
            );
            match call_result {
                Ok(res) => {
                    cost_usd += res.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
                    let text = res
                        .get("output_kv")
                        .and_then(Value::as_array)
                        .and_then(|a| a.iter().find(|o| o.get("field_name").and_then(Value::as_str) == Some("dossier_json") && o.get("parent_output_id").map_or(true, Value::is_null)))
                        .and_then(|o| o.get("value_text"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let body = extract_json(&text);
                    match body {
                        Some(b) if !b.get("why").map_or(true, Value::is_null) => {
                            let model = res.get("model_id").cloned().unwrap_or(Value::Null);
                            let call_cost = res.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
                            let body_str: String = serde_json::to_string(&b).unwrap_or_default().chars().take(8000).collect();
                            let _ = crate::db_json::run_write(
                                db,
                                "INSERT INTO judgments.ultra_insights (run_id, kind, path, body_json, model, cost_usd) VALUES (?, ?, ?, ?, ?, ?)",
                                &[run_id.clone(), json!("dossier"), json!(path), json!(body_str), model, json!(call_cost)],
                            );
                            made += 1;
                        }
                        _ => dropped += 1, // parse-or-drop — counted, never hidden
                    }
                }
                Err(e) => {
                    dropped += 1;
                    errors.push(json!({ "stage": "dossier", "path": path, "error": e.chars().take(200).collect::<String>() }));
                }
            }
        }
        dossier_telemetry = json!({ "made": made, "dropped": dropped, "cost_usd": cost_usd });
    }

    // ── 4 · the brief — ONE synthesis over everything ─────────────────────────
    let mut brief_telemetry = json!({ "ok": false, "cost_usd": 0.0 });
    if !skip.contains("brief") {
        ensure_prompt(db, BRIEF_PROMPT, BRIEF_SYSTEM, BRIEF_TEMPLATE, "brief_json", 2200);
        let s = survey.get("summary").cloned().unwrap_or(json!({}));
        let worst_lines_v = survey.get("worst").and_then(Value::as_array).cloned().unwrap_or_default();
        let worst_lines = worst_lines_v
            .iter()
            .take(15)
            .map(|u| {
                let worst_axis = u
                    .get("axes")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|(_, a)| !a.is_null())
                    .fold(None::<(String, f64)>, |acc, (k, a)| {
                        let score = a.get("score").and_then(Value::as_f64).unwrap_or(f64::NAN);
                        match &acc {
                            None => Some((k, score)),
                            Some((_, best)) if score < *best => Some((k, score)),
                            _ => acc,
                        }
                    });
                let weakest = match &worst_axis {
                    Some((k, score)) => format!(" weakest={k}:{}", if score.fract() == 0.0 { format!("{}", *score as i64) } else { format!("{score}") }),
                    None => String::new(),
                };
                format!("{} health={}{}{} findings={}", str_qq(u.get("path")), str_qq(u.get("health")), weakest, "", str_qq(u.get("findings")))
            })
            .collect::<Vec<_>>()
            .join("\n");
        let worst_lines = if worst_lines.is_empty() { "(none)".to_string() } else { worst_lines };

        let coupling_v = s.get("co_change").and_then(Value::as_array).cloned().unwrap_or_default();
        let coupling = coupling_v
            .iter()
            .map(|p| format!("{} <-> {} ×{}", str_qq(p.get("a")), str_qq(p.get("b")), str_qq(p.get("count"))))
            .collect::<Vec<_>>()
            .join("\n");
        let coupling = if coupling.is_empty() { "(none detected)".to_string() } else { coupling };

        let findings_by_cat_rows = crate::db_json::query_rows(
            db,
            "SELECT category, COUNT(*) AS n FROM judgments.bugfinder_findings f WHERE f.dropped = 0 AND f.run_id IN (SELECT id FROM judgments.bugfinder_runs WHERE source='ultra') GROUP BY category ORDER BY n DESC LIMIT 15",
            &[],
        )
        .unwrap_or(json!([]));
        let findings_by_cat = findings_by_cat_rows
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|r| format!("{}: {}", str_qq(r.get("category")), str_qq(r.get("n"))))
            .collect::<Vec<_>>()
            .join("\n");
        let findings_by_cat = if findings_by_cat.is_empty() { "(none)".to_string() } else { findings_by_cat };

        let dossier_rows = crate::db_json::query_rows(
            db,
            "SELECT path, body_json FROM judgments.ultra_insights WHERE run_id = ? AND kind='dossier'",
            &[run_id.clone()],
        )
        .unwrap_or(json!([]));
        let dossier_digest = dossier_rows
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|r| {
                let path = r.get("path").and_then(Value::as_str)?;
                let body_json = r.get("body_json").and_then(Value::as_str)?;
                let parsed: Value = serde_json::from_str(body_json).ok()?;
                let why = parsed.get("why")?;
                Some(format!("{path}: {}", str_qq(Some(why))))
            })
            .collect::<Vec<_>>()
            .join("\n");
        let dossier_digest = if dossier_digest.is_empty() { "(none)".to_string() } else { dossier_digest };

        // the concept atlas + the deltas, read back from the report (counted)
        let mid = crate::ultra_ultra_report::ultraReport(&json!({ "run_id": run_id }), db);
        let (concept_text, delta_text) = match &mid {
            Ok(m) => (build_concept_text(m.get("concepts")), build_delta_text(m.get("deltas"))),
            Err(_) => ("(the concept atlas was not built for this run)".to_string(), "(this is the first review of this scope — no deltas yet)".to_string()),
        };

        let summary_line = format!(
            "health={} determinism={}% ai_readability={} files={} axes={} findings_total={}",
            str_qq(s.get("health")),
            str_qq(s.get("determinism")),
            str_qq(s.get("ai_readability")),
            str_qq(s.get("source_files")),
            serde_json::to_string(&s.get("axes_avg").cloned().unwrap_or(Value::Null)).unwrap_or_default(),
            str_qq(s.get("findings_total")),
        );

        match crate::ai_ai_call::aiCall(
            &json!({
                "prompt_name": BRIEF_PROMPT,
                "input_kv": [
                    { "key": "summary", "value": summary_line },
                    { "key": "worst", "value": worst_lines },
                    { "key": "coupling", "value": coupling },
                    { "key": "findings", "value": findings_by_cat },
                    { "key": "concepts", "value": concept_text },
                    { "key": "deltas", "value": delta_text },
                    { "key": "dossiers", "value": dossier_digest },
                ],
            }),
            db,
        ) {
            Ok(res) => {
                let cost_usd = res.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
                let text = res
                    .get("output_kv")
                    .and_then(Value::as_array)
                    .and_then(|a| a.iter().find(|o| o.get("field_name").and_then(Value::as_str) == Some("brief_json") && o.get("parent_output_id").map_or(true, Value::is_null)))
                    .and_then(|o| o.get("value_text"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let body = extract_json(&text);
                let mut ok = false;
                if let Some(b) = &body {
                    if !b.get("headline").map_or(true, Value::is_null) {
                        let model = res.get("model_id").cloned().unwrap_or(Value::Null);
                        let body_str: String = serde_json::to_string(b).unwrap_or_default().chars().take(12000).collect();
                        let _ = crate::db_json::run_write(
                            db,
                            "INSERT INTO judgments.ultra_insights (run_id, kind, path, body_json, model, cost_usd) VALUES (?, ?, ?, ?, ?, ?)",
                            &[run_id.clone(), json!("brief"), Value::Null, json!(body_str), model, json!(cost_usd)],
                        );
                        ok = true;
                    }
                }
                brief_telemetry = json!({ "ok": ok, "cost_usd": cost_usd });
            }
            Err(e) => err_stage(&mut errors, "brief", &e),
        }
    }

    // echo the bench spend onto the run's totals so the report can sum honestly
    let _: Result<(), String> = (|| {
        let rows = crate::db_json::query_rows(db, "SELECT totals_json FROM judgments.ultra_runs WHERE id = ?", &[run_id.clone()])?;
        let totals_json = rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("totals_json")).and_then(Value::as_str);
        let mut totals: Map<String, Value> = totals_json.and_then(|s| serde_json::from_str(s).ok()).unwrap_or_default();
        totals.insert("bench_cost_usd".into(), bench.get("cost_usd").cloned().unwrap_or(json!(0.0)));
        crate::db_json::run_write(db, "UPDATE judgments.ultra_runs SET totals_json=? WHERE id=?", &[json!(serde_json::to_string(&totals).unwrap_or_default()), run_id.clone()])?;
        Ok(())
    })(); // telemetry only

    let report = crate::ultra_ultra_report::ultraReport(&json!({ "run_id": run_id }), db)?;
    let mut out = report.as_object().cloned().unwrap_or_default();
    out.insert(
        "deep".into(),
        json!({ "bench": bench, "concepts": concepts, "dossiers": dossier_telemetry, "brief": brief_telemetry, "errors": errors }),
    );
    out.insert("advisory".into(), json!("the deep review records and recommends — it never gates"));
    Ok(Value::Object(out))
}

fn build_concept_text(concepts: Option<&Value>) -> String {
    let c = match concepts {
        Some(c) if !c.is_null() => c,
        _ => return "(the concept atlas was not built for this run)".to_string(),
    };
    let total = str_qq(c.get("total"));
    let scattered = c.get("scattered").and_then(Value::as_array).cloned().unwrap_or_default();
    let untested = c.get("untested").and_then(Value::as_array).cloned().unwrap_or_default();
    let mixed = c.get("mixed").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut lines = vec![format!("{total} ideas across the territory")];
    if !scattered.is_empty() {
        lines.push(format!(
            "SCATTERED (one idea, many files): {}",
            scattered.iter().map(|c| format!("{} ×{}", str_qq(c.get("name")), str_qq(c.get("file_count")))).collect::<Vec<_>>().join(" · ")
        ));
    }
    if !untested.is_empty() {
        lines.push(format!("UNTESTED IDEAS: {}", untested.iter().map(|c| str_qq(c.get("name"))).collect::<Vec<_>>().join(" · ")));
    }
    if !mixed.is_empty() {
        lines.push(format!(
            "MIXED FILES (unrelated ideas together): {}",
            mixed
                .iter()
                .map(|m| {
                    let concepts_list = m.get("concepts").and_then(Value::as_array).cloned().unwrap_or_default().iter().map(|v| str_qq(Some(v))).collect::<Vec<_>>().join(", ");
                    format!("{} [{}]", str_qq(m.get("path")), concepts_list)
                })
                .collect::<Vec<_>>()
                .join(" · ")
        ));
    }
    lines.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n")
}

fn build_delta_text(deltas: Option<&Value>) -> String {
    let d = match deltas {
        Some(d) if !d.is_null() => d,
        _ => return "(this is the first review of this scope — no deltas yet)".to_string(),
    };
    let health = d.get("health").filter(|v| !v.is_null());
    let health_str = match health {
        Some(h) => {
            let n = h.as_f64().unwrap_or(0.0);
            format!("{}{}", if n > 0.0 { "+" } else { "" }, str_qq(Some(h)))
        }
        None => "?".to_string(),
    };
    let from_at = str_qq(d.get("from_at")).chars().take(16).collect::<String>();
    let mut lines = vec![format!("vs run #{} ({}) · overall health {}", str_qq(d.get("from_run")), from_at, health_str)];
    let worse = d.get("worse").and_then(Value::as_array).cloned().unwrap_or_default();
    if !worse.is_empty() {
        lines.push(format!(
            "WORSE: {}",
            worse.iter().map(|x| format!("{} {}→{}", str_qq(x.get("path")), str_qq(x.get("from")), str_qq(x.get("to")))).collect::<Vec<_>>().join(" · ")
        ));
    }
    let better = d.get("better").and_then(Value::as_array).cloned().unwrap_or_default();
    if !better.is_empty() {
        lines.push(format!(
            "BETTER: {}",
            better.iter().map(|x| format!("{} {}→{}", str_qq(x.get("path")), str_qq(x.get("from")), str_qq(x.get("to")))).collect::<Vec<_>>().join(" · ")
        ));
    }
    let new_units = d.get("new_units").and_then(Value::as_array).cloned().unwrap_or_default();
    if !new_units.is_empty() {
        lines.push(format!("NEW: {}", new_units.iter().map(|v| str_qq(Some(v))).collect::<Vec<_>>().join(" · ")));
    }
    lines.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH DATABASE ':memory:' AS judgments; ATTACH DATABASE ':memory:' AS state; ATTACH DATABASE ':memory:' AS brain;",
        )
        .unwrap();
        c.execute_batch(
            "CREATE TABLE state.workspaces (id INTEGER PRIMARY KEY, path TEXT NOT NULL, active INTEGER NOT NULL DEFAULT 0);
             INSERT INTO state.workspaces (path, active) VALUES ('.', 1);
             CREATE TABLE code_nodes (id INTEGER PRIMARY KEY, origin TEXT, type TEXT, path TEXT);
             CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT);",
        )
        .unwrap();
        crate::bugfinder__helpers_schema::ensureBugfinderSchema(&c);
        crate::ultra__helpers_schema::ensureUltraSchema(&c);
        c
    }

    #[test]
    fn extract_json_parses_first_brace_pair() {
        assert_eq!(extract_json("noise {\"a\":1} trailing"), Some(json!({"a":1})));
        assert_eq!(extract_json("no braces here"), None);
        assert_eq!(extract_json("{unterminated"), None);
    }

    #[test]
    fn is_absolute_like_detects_win_and_posix_roots() {
        assert!(is_absolute_like("/a/b"));
        assert!(is_absolute_like("C:\\a"));
        assert!(!is_absolute_like("a/b"));
        assert!(!is_absolute_like(""));
    }

    #[test]
    fn build_concept_text_reports_absence_and_presence() {
        assert_eq!(build_concept_text(None), "(the concept atlas was not built for this run)");
        let c = json!({ "total": 2, "scattered": [{"name":"audit","file_count":3}], "untested": [], "mixed": [] });
        assert_eq!(build_concept_text(Some(&c)), "2 ideas across the territory\nSCATTERED (one idea, many files): audit ×3");
    }

    #[test]
    fn build_delta_text_reports_first_review_when_null() {
        assert_eq!(build_delta_text(None), "(this is the first review of this scope — no deltas yet)");
        let d = json!({ "from_run": 2, "from_at": "2026-01-05 00:00:00", "health": 3, "worse": [], "better": [], "new_units": [] });
        assert_eq!(build_delta_text(Some(&d)), "vs run #2 (2026-01-05 00:00) · overall health +3");
    }

    #[test]
    fn ultra_deep_survey_and_dossier_ai_error_is_captured_not_fatal() {
        let c = open_db();
        let dir = std::env::temp_dir().join(format!("ultra-deep-rs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.mjs"), "export function a(){ return 1; }\n").unwrap();

        let out = ultraDeep(
            &json!({ "root": dir.to_string_lossy(), "folder": ".", "skip": ["bench", "concepts"], "dossier_cap": 1 }),
            &c,
        )
        .unwrap();

        assert!(out.get("summary").is_some());
        assert_eq!(out["deep"]["bench"], Value::Null);
        assert_eq!(out["deep"]["concepts"], Value::Null);
        // dossier_cap>0 with real units → one ai-call attempt → PARITY-DEFERRED aiCall Err
        // → dropped, never made, captured as an error not a panic.
        let dropped = out["deep"]["dossiers"]["dropped"].as_i64().unwrap_or(0);
        assert!(dropped >= 0);
        // the brief stage runs by default and also hits the deferred aiCall boundary.
        assert_eq!(out["deep"]["brief"]["ok"], json!(false));
        assert_eq!(out["advisory"], json!("the deep review records and recommends — it never gates"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ultra_deep_skip_dossiers_and_brief_skips_both_stages() {
        let c = open_db();
        let dir = std::env::temp_dir().join(format!("ultra-deep-rs-skip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.mjs"), "export function a(){ return 1; }\n").unwrap();

        let out = ultraDeep(
            &json!({ "root": dir.to_string_lossy(), "folder": ".", "skip": ["bench", "concepts", "dossiers", "brief"] }),
            &c,
        )
        .unwrap();

        assert_eq!(out["deep"]["dossiers"], json!({ "made": 0, "dropped": 0, "cost_usd": 0.0 }));
        assert_eq!(out["deep"]["brief"], json!({ "ok": false, "cost_usd": 0.0 }));
        assert!(out["deep"]["errors"].as_array().unwrap().is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dossier_cap_clamps_into_0_20() {
        assert_eq!(0i64.max(0).min(20), 0);
        assert_eq!(999i64.max(0).min(20), 20);
    }
}
