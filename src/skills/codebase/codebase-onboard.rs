//! codebase-onboard.rs — twin of skills/codebase/codebase-onboard.mjs. NAMES ARE CONTRACT:
//! `codebaseOnboard` (default). The full READ pipeline chained end to end — scope → ingest →
//! metrics → testmap → entrypoints → constitution → agents → (opt-in digest) → report sweep →
//! rag → memory → (opt-in) register — onboarding an existing codebase into the atlas in one call.
//!
//! PARITY-DEFERRED BOUNDARY: the semantic index lives behind `rag_rag_sweep::ragSweep`, whose own
//! `external_embed::embed` dependency Errs/degrades on its real network edge (see that module's
//! doc). This file calls `ragSweep({ implicit: true }, context, db)` exactly once, best-effort —
//! same as the JS `sub('rag', …, null)` fallback — and never uses its return value downstream
//! (the JS caller doesn't either: `rag` isn't part of the returned shape). Everything else —
//! root/scope resolution, the ingest→metrics→testmap→entrypoints spine, the constitution/agents/
//! report best-effort sweep, the memory-profile persist, and the profile assembly — is fully
//! deterministic and ported faithfully, composing the ALREADY-TWINNED sibling skills
//! (`codebase_codebase_scope`, `codebase_atlas_ingest`, `codebase_atlas_metrics`,
//! `codebase_atlas_test_map`, `codebase_atlas_entry_points`, `constitution_constitution_judge`,
//! `constitution_constitution_law`, `constitution_constitution_agents`,
//! `codebase_architecture_digest`, `codebase_atlas_report`, `codebase_code_digest_snapshot`,
//! `codebase_code_signals_detect`, `codebase_code_signals_graph`, `codebase_code_decorate`,
//! `codebase_design_tokens_extract`, `codebase_design_components_extract`,
//! `codebase_code_describe_gather`, `entities_memory_add`, `codebase_atlas_register`).
//!
//! DELIBERATE SIMPLIFICATIONS (disclosed, non-deterministic UI/orchestration concerns dropped —
//! the underlying db writes + returned shape are still faithful):
//! - The REBUILD-WIZARD `detail` telemetry (`wiz`: per-step timers, rolling log lines, live
//!   progress bars, family-by-family streaming) is UI polish over `code_onboard_jobs.detail` —
//!   not modeled. `code_onboard_jobs.phase`/`status`/`profile_json`/`error` (what the FE poll
//!   actually gates on) ARE written, best-effort, matching `setPhase`/`finishJob`/`failJob`.
//! - PAUSE/RESUME (`atlas?.paused`, `control.shouldPause`, per-family checkpoint resume) isn't
//!   modeled: the underlying `codebase_atlas_ingest::atlasIngest` twin has no pause/resume/
//!   onProgress support (a synchronous, non-streaming ingest), so that branch never fires here.
//! - `context.jobDb` (the staging-vs-live split during a REGISTER-AT-END rebuild) collapses to
//!   the in-process fallback (`jobDb === db`, the common case per the JS comment) since this twin
//!   takes one `&Connection`. The REGISTER step still requires `context.register.staging_code_path`
//!   and calls `atlasRegister` for real when present — same db is used for both stage and live
//!   attaches (a real two-database swap needs two connections, out of scope for a single-`db` twin).
//! - `code-signals-read`'s dynamic `import()` inside the JS report phase only stashes a read-only
//!   snapshot into `wiz.signals` (UI-only, no side effect) — dropped along with the rest of `wiz`.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn to_vec_str(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

// ── how long each phase took (Grokbot QA 2026-10-12, item 24: Settings > Substrate said "no rebuilds recorded yet" beside History's "1 rebuild recorded"; the panel reads `detail.phase_ms` and nothing ever wrote it) ──
// One clock per job id, on the thread that runs the job (the pipeline below is one synchronous call). `phase_tick(id, Some(next))` closes the running phase and opens `next`;
// `phase_tick(id, None)` closes it and returns every phase with its milliseconds, in the order they ran.
thread_local! {
    static PHASE_CLOCK: std::cell::RefCell<std::collections::HashMap<String, (String, std::time::Instant, Vec<(String, u128)>)>> = std::cell::RefCell::new(std::collections::HashMap::new());
}
fn phase_tick(id: &str, next: Option<&str>) -> Vec<(String, u128)> {
    PHASE_CLOCK.with(|c| {
        let mut m = c.borrow_mut();
        let now = std::time::Instant::now();
        let mut done = match m.remove(id) {
            Some((name, started, mut acc)) => { acc.push((name, now.duration_since(started).as_millis())); acc }
            None => Vec::new(),
        };
        match next {
            Some(n) => { m.insert(id.to_string(), (n.to_string(), now, std::mem::take(&mut done))); Vec::new() }
            None => done,
        }
    })
}
/// `detail` with `phase_ms` merged in, keeping whatever progress the row already carried. Pure - tested.
fn detail_with_phases(existing: Option<&str>, phases: &[(String, u128)]) -> Option<String> {
    if phases.is_empty() { return None }
    let mut d: Map<String, Value> = existing.filter(|s| !s.is_empty()).and_then(|s| serde_json::from_str::<Value>(s).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default();
    let mut pm = Map::new();
    for (name, ms) in phases { let prev = pm.get(name).and_then(Value::as_u64).unwrap_or(0); pm.insert(name.clone(), json!(prev + (*ms as u64))); }
    d.insert("phase_ms".into(), Value::Object(pm));
    serde_json::to_string(&Value::Object(d)).ok()
}

// ── job-row control (best-effort, matches the JS try/catch-swallow convention) ──
fn set_phase(db: &Connection, job_id: Option<&str>, phase: &str) {
    if let Some(id) = job_id {
        let _ = phase_tick(id, Some(phase));
        let _ = crate::db_json::run_write(
            db,
            &format!("UPDATE code_onboard_jobs SET phase = ?, updated_at = {} WHERE job_id = ?", crate::clock::now_sql()),
            &[json!(phase), json!(id)],
        );
    }
}
fn finish_job(db: &Connection, job_id: Option<&str>, profile: &Value) {
    if let Some(id) = job_id {
        let phases = phase_tick(id, None);
        let existing: Option<String> = crate::db_json::query_rows(db, "SELECT detail FROM code_onboard_jobs WHERE job_id = ?", &[json!(id)]).ok()
            .and_then(|v| v.as_array().and_then(|a| a.first().cloned())).and_then(|r| r.get("detail").and_then(Value::as_str).map(String::from));
        let detail = detail_with_phases(existing.as_deref(), &phases);
        let _ = crate::db_json::run_write(
            db,
            &format!("UPDATE code_onboard_jobs SET phase = 'done', status = 'done', profile_json = ?, detail = COALESCE(?, detail), updated_at = {} WHERE job_id = ?", crate::clock::now_sql()),
            &[json!(serde_json::to_string(profile).unwrap_or_default()), detail.map(Value::from).unwrap_or(Value::Null), json!(id)],
        );
    }
}
fn fail_job(db: &Connection, job_id: Option<&str>, e: &str) {
    if let Some(id) = job_id { let _ = phase_tick(id, None); }   // a failed rebuild records no timings
    let phase = job_id.and_then(|id| {
        crate::db_json::query_rows(db, "SELECT phase FROM code_onboard_jobs WHERE job_id = ?", &[json!(id)])
            .ok()
            .and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
            .and_then(|r| r.get("phase").and_then(Value::as_str).map(String::from))
    }).unwrap_or_else(|| "?".to_string());
    if let Some(id) = job_id {
        let msg: String = format!("[{phase}] {e}").chars().take(800).collect();
        let _ = crate::db_json::run_write(
            db,
            &format!("UPDATE code_onboard_jobs SET status = 'error', error = ?, updated_at = {} WHERE job_id = ?", crate::clock::now_sql()),
            &[json!(msg), json!(id)],
        );
    }
}
fn job_root(db: &Connection, job_id: Option<&str>) -> Option<String> {
    job_id.and_then(|id| {
        crate::db_json::query_rows(db, "SELECT root FROM code_onboard_jobs WHERE job_id = ?", &[json!(id)])
            .ok()
            .and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
            .and_then(|r| r.get("root").and_then(Value::as_str).map(String::from))
            .filter(|s| !s.is_empty())
    })
}

// `gitHistory(root) → { pth: commits }` reduction (the churn-input sub-step), then the JS `m.size
// ? m : gitChurn(root)` fallback. Owners = `churnHistory?.byFile` verbatim.
fn churn_and_owners(root: &str, hist: &Value) -> (Value, Value) {
    let mut m = Map::new();
    if let Some(by_file) = hist.get("by_file").and_then(Value::as_object) {
        for (p, e) in by_file {
            m.insert(p.clone(), e.get("commits").cloned().unwrap_or(json!(0)));
        }
    }
    let churn = if !m.is_empty() { Value::Object(m) } else { crate::codebase_atlas_metrics::gitChurn(&json!(root)) };
    let owners = if hist.is_null() { Value::Null } else { hist.get("by_file").cloned().unwrap_or(Value::Null) };
    (churn, owners)
}

fn top_n(metrics: &Value, key: &str, fields: &[&str], n: usize) -> Value {
    let arr = metrics.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
    let out: Vec<Value> = arr.into_iter().take(n).map(|e| {
        let mut m = Map::new();
        for f in fields { m.insert((*f).to_string(), e.get(*f).cloned().unwrap_or(Value::Null)); }
        Value::Object(m)
    }).collect();
    json!(out)
}

pub fn codebaseOnboard(input: &Value, context: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — the db lane always injects a live connection, so that guard is unreachable given this
    // fn's `(input, context, db)` signature (same convention as `ragSweep`/`codebaseRefresh`).
    let origin_v = nz(input.get("origin")).cloned().unwrap_or(json!("atlas"));
    let origin = origin_v.as_str().unwrap_or("atlas").to_string();
    let job_id: Option<String> = input.get("job_id").and_then(|v| if v.is_null() { None } else { Some(str_qq(Some(v))) });
    let job_id_ref = job_id.as_deref();

    // root: explicit input.root → the root THIS job recorded on start → the active workspace root.
    let root_v = match nz(input.get("root")) {
        Some(r) => r.clone(),
        None => match job_root(db, job_id_ref) {
            Some(r) => json!(r),
            None => crate::engine_workspace_root::getWorkspaceRoot(db),
        },
    };
    let root = str_qq(Some(&root_v));

    // scope: explicit include/exclude wins; otherwise the saved per-workspace code_scope.
    let saved = crate::codebase_code_scope_get::loadScope(db, &origin_v);
    let include: Vec<String> = match nz(input.get("include")) { Some(v) => to_vec_str(v), None => to_vec_str(&saved["include"]) };
    let exclude: Vec<String> = match nz(input.get("exclude")) { Some(v) => to_vec_str(v), None => to_vec_str(&saved["exclude"]) };

    let run = (|| -> Result<Value, String> {
        // 1. SCOPE — mandatory (JS `sub(..., undefined, ...)`: no fallback, a throw here fails onboard).
        set_phase(db, job_id_ref, "scope");
        let scope = crate::codebase_codebase_scope::codebaseScope(
            &json!({ "root": root, "include": include, "exclude": exclude }),
            &Value::Null,
        )?;

        // 2. INGEST — mandatory (parse every in-scope file into the atlas). Heavy.
        set_phase(db, job_id_ref, "ingest");
        // job_id rides along ONLY so ingest can heartbeat the job row per family — see the
        // heartbeat note in atlas-ingest.rs. Ingest does no other job bookkeeping.
        let atlas = crate::codebase_atlas_ingest::atlasIngest(
            &json!({ "root": root, "include": include, "exclude": exclude, "origin": origin, "lanes": true,
                     "job_id": job_id_ref.map(|s| json!(s)).unwrap_or(Value::Null) }),
            db,
        )?;
        let lanes: Option<Value> = atlas.get("lanes").cloned();

        // 3. METRICS + 4. TESTMAP — best-effort (JS falls back to empty/zeroed shapes on error).
        set_phase(db, job_id_ref, "metrics");
        let hist = crate::codebase_code_signals_graph::gitHistory(&root, 3000, 500);
        let (churn_map, owners) = churn_and_owners(&root, &hist);
        let metrics = crate::codebase_atlas_metrics::atlasMetrics(
            &json!({ "origin": origin, "root": root, "churn": churn_map, "owners": owners }),
            db,
        ).unwrap_or_else(|_| json!({ "top_central": [], "top_risky": [], "top_churn": [], "top_complex": [], "metrics_by_file": {} }));

        set_phase(db, job_id_ref, "testmap");
        let coverage = crate::codebase_atlas_test_map::atlasTestMap(&json!({ "origin": origin }), db)
            .unwrap_or_else(|_| json!({ "covered_count": 0, "uncovered_count": 0, "test_count": 0 }));

        // 4b. ENTRY POINTS — best-effort: write from the family lane (atlas.lanes present, JS
        // always requests `lanes:true`) or fall back to a full extract sweep.
        set_phase(db, job_id_ref, "entrypoints");
        let entry_points: Value = match &lanes {
            Some(l) => crate::codebase_atlas_entry_points::writeEntryPoints(
                db, &origin_v, l.get("entries").unwrap_or(&json!([])), &json!({ "capture": true, "writeRows": false }),
            ),
            None => crate::codebase_atlas_entry_points::atlasEntryPoints(&json!({ "origin": origin, "root": root, "capture": true }), db)
                .unwrap_or_else(|e| json!({ "ok": false, "error": e })),
        };

        // 4c. CONSTITUTION — seed-if-empty (best-effort) then judge (best-effort).
        set_phase(db, job_id_ref, "constitution");
        let law_count: i64 = crate::db_json::query_rows(db, "SELECT COUNT(*) n FROM laws.review_laws", &[])
            .ok()
            .and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
            .and_then(|r| r.get("n").and_then(Value::as_i64))
            .unwrap_or(0);
        if law_count == 0 {
            let _ = crate::constitution_constitution_law::constitutionLaw(&json!({ "op": "seed" }), db);
        } else {
            // a workspace that already has its laws still receives a CORRECTED seed law (nothing is inserted, nothing the operator removed comes back)
            let _ = crate::constitution_constitution_law::constitutionLaw(&json!({ "op": "seed", "upgrade_only": true }), db);
        }
        let constitution: Value = (|| -> Result<Value, String> {
            let cj = crate::constitution_constitution_judge::constitutionJudge(
                &json!({ "root": root, "source": "rebuild", "max_files": 3000 }),
                db,
            )?;
            Ok(json!({
                "snapshot_id": cj.get("snapshot").and_then(|s| s.get("id")).cloned().unwrap_or(Value::Null),
                "health": cj.get("summary").and_then(|s| s.get("health")).cloned().unwrap_or(Value::Null),
                "violations": cj.get("summary").and_then(|s| s.get("violations")).cloned().unwrap_or(Value::Null),
                "judged": cj.get("judged").cloned().unwrap_or(Value::Null),
                "carried": cj.get("carried").cloned().unwrap_or(Value::Null),
            }))
        })().unwrap_or_else(|e| json!({ "ok": false, "error": e.chars().take(200).collect::<String>() }));

        // 4d. AGENTS — derive the specialists registry, best-effort.
        set_phase(db, job_id_ref, "agents");
        let agents: Value = (|| -> Result<Value, String> {
            let reg = crate::constitution_constitution_agents::constitutionAgents(&Value::Null, db)?;
            let mut by_category = Map::new();
            if let Some(arr) = reg.get("agents").and_then(Value::as_array) {
                for a in arr {
                    let cat = a.get("category").and_then(Value::as_str).unwrap_or("").to_string();
                    let cur = by_category.get(&cat).and_then(Value::as_i64).unwrap_or(0);
                    by_category.insert(cat, json!(cur + 1));
                }
            }
            Ok(json!({
                "count": reg.get("agents").and_then(Value::as_array).map(|a| a.len() as i64).unwrap_or(0),
                "by_category": Value::Object(by_category),
                "languages": reg.get("derived_from").and_then(|d| d.get("languages")).cloned().unwrap_or(json!([])),
                "frameworks": reg.get("derived_from").and_then(|d| d.get("frameworks")).cloned().unwrap_or(json!([])),
            }))
        })().unwrap_or_else(|e| json!({ "ok": false, "error": e.chars().take(160).collect::<String>() }));

        // 5. DIGEST — opt-in only (`input.summarize === true`); best-effort when opted in.
        set_phase(db, job_id_ref, "digest");
        let digest: Option<Value> = if matches!(input.get("summarize"), Some(Value::Bool(true))) {
            Some(
                crate::codebase_architecture_digest::architectureDigest(&json!({ "origin": origin }), db)
                    .unwrap_or_else(|e| json!({ "ok": false, "error": e })),
            )
        } else {
            None
        };

        let mut profile = json!({
            "root": atlas.get("root").cloned().unwrap_or(Value::Null),
            "languages": scope.get("by_lang").cloned().unwrap_or(Value::Null),
            "files": atlas.get("files").cloned().unwrap_or(Value::Null),
            "nodes": atlas.get("nodes").cloned().unwrap_or(Value::Null),
            "edges": atlas.get("edges").cloned().unwrap_or(Value::Null),
            "call_edges": atlas.get("call_edges").cloned().unwrap_or(Value::Null),
            "unresolved_imports": atlas.get("unresolved").cloned().unwrap_or(Value::Null),
            "parse_errors": atlas.get("errors").and_then(Value::as_array).map(|a| a.len() as i64).unwrap_or(0),
            "top_central": top_n(&metrics, "top_central", &["path", "fan_in"], 5),
            "top_risky": top_n(&metrics, "top_risky", &["path", "fan_in"], 5),
            "top_churn": top_n(&metrics, "top_churn", &["path", "churn"], 5),
            "top_complex": top_n(&metrics, "top_complex", &["path", "complexity"], 5),
            "coverage": {
                "covered": coverage.get("covered_count").cloned().unwrap_or(json!(0)),
                "uncovered": coverage.get("uncovered_count").cloned().unwrap_or(json!(0)),
                "tests": coverage.get("test_count").cloned().unwrap_or(json!(0)),
            },
            "entry_points": entry_points.get("counts").cloned().unwrap_or(Value::Null),
            "constitution": constitution,
            "agents": agents,
            "digest_md": digest.as_ref().and_then(|d| d.get("digest_md")).cloned().unwrap_or(Value::Null),
        });

        // 7. REPORT — the graded x-ray + surface writers. Every call here is best-effort
        // (JS `sub(..., null, ...)`): a failure here degrades silently, never fails onboard.
        set_phase(db, job_id_ref, "report");
        if let Some(l) = &lanes {
            let _ = crate::codebase_code_signals_detect::writeFileSignals(db, &origin_v, l.get("signals").unwrap_or(&json!([])));
        }
        let _ = crate::codebase_code_signals_graph::codeSignalsGraph(&json!({ "origin": origin, "root": root, "history": hist }), db);
        let _ = crate::codebase_code_digest_snapshot::codeDigestSnapshot(&json!({ "origin": origin, "root": root, "skip_narrative": true }), db);
        let _ = crate::codebase_atlas_report::atlasReport(&json!({ "origin": origin, "root": root }), db);
        let _ = crate::codebase_code_decorate::codeDecorate(&json!({ "all": true, "origin": origin }), db);
        let _ = crate::codebase_code_describe_gather::codeDescribeGather(&json!({ "all": true, "origin": origin }), db);
        match &lanes {
            Some(l) => {
                let tokens = l.get("design_tokens").and_then(Value::as_array).cloned().unwrap_or_default();
                let _ = crate::codebase_design_tokens_extract::finalizeTokens(db, &origin, tokens, Some(root.as_str()));
            }
            None => { let _ = crate::codebase_design_tokens_extract::designTokensExtract(&json!({ "all": true, "origin": origin }), db); }
        }
        match &lanes {
            Some(l) => { let _ = crate::codebase_design_components_extract::writeComponents(db, &origin_v, l.get("design_components").unwrap_or(&json!([]))); }
            None => { let _ = crate::codebase_design_components_extract::designComponentsExtract(&json!({ "all": true, "origin": origin }), db); }
        }

        // 7b. HOW MUCH OF THIS HAS PROSE, AND HOW MUCH PARSED - said, not implied (2026-10-01: "X-ray summaries empty (0% prose)"
        // sat unexplained because onboarding reported neither; a file the parser could not read and a file nobody described
        // both looked like "nothing to show"). Facts are gathered above; sentences are written only on request, so a fresh
        // onboarding reports 0 and says how to ask. The gather ran in step 7, so the count is taken after it.
        profile["descriptions"] = crate::codebase_code_describe::describeCoverage(db, &origin).unwrap_or_else(|e| json!({ "ok": false, "error": e.chars().take(160).collect::<String>() }));
        profile["parse_health"] = atlas.get("parse_health").cloned().unwrap_or(Value::Null);

        // 8. RAG — best-effort semantic sweep. PARITY-DEFERRED BOUNDARY (see module doc): its own
        // `embed` edge degrades independently; the return value is unused here, same as JS.
        set_phase(db, job_id_ref, "rag");
        let _ = crate::rag_rag_sweep::ragSweep(&json!({ "implicit": true }), context, db);

        // 9. MEMORY — LAST by design: the profile snapshot of everything the run produced.
        // Best-effort; `remember: false` skips it entirely.
        set_phase(db, job_id_ref, "memory");
        let remember = !matches!(input.get("remember"), Some(Value::Bool(false)));
        let memory_id: Value = if remember {
            (|| -> Result<Value, String> {
                let langs = profile.get("languages").and_then(Value::as_object)
                    .map(|o| o.iter().map(|(k, v)| format!("{k}:{v}")).collect::<Vec<_>>().join(", "))
                    .unwrap_or_default();
                let central_paths: Vec<String> = profile.get("top_central").and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|c| c.get("path").and_then(Value::as_str).map(String::from)).collect())
                    .unwrap_or_default();
                let central = if central_paths.is_empty() { "—".to_string() } else { central_paths.join(", ") };
                let risky_paths: Vec<String> = profile.get("top_risky").and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|c| c.get("path").and_then(Value::as_str).map(String::from)).collect())
                    .unwrap_or_default();
                let risky = if risky_paths.is_empty() { "—".to_string() } else { risky_paths.join(", ") };
                let root_disp = profile.get("root").and_then(Value::as_str).unwrap_or("").to_string();
                let digest_md = profile.get("digest_md").cloned().unwrap_or(Value::Null);
                let covered = profile.get("coverage").and_then(|c| c.get("covered")).cloned().unwrap_or(json!(0));
                let uncovered = profile.get("coverage").and_then(|c| c.get("uncovered")).cloned().unwrap_or(json!(0));
                let guidance = match digest_md.as_str() {
                    Some(s) => s.to_string(),
                    None => format!("Query this project's structure via system.codebase.{{get,subgraph,find,blast,metrics,testmap}}. {covered} source files have tests, {uncovered} do not."),
                };
                let situation = format!(
                    "Existing codebase at {} onboarded into the atlas — {} files ({}), {} nodes / {} edges. Most depended-on: {}. Risky (depended-on but untested): {}.",
                    root_disp,
                    profile.get("files").cloned().unwrap_or(json!(0)),
                    langs,
                    profile.get("nodes").cloned().unwrap_or(json!(0)),
                    profile.get("edges").cloned().unwrap_or(json!(0)),
                    central, risky,
                );
                let row = crate::entities_memory_add::memoryAdd(&json!({
                    "type": "reference",
                    "title": format!("Codebase onboarded: {root_disp}"),
                    "situation": situation,
                    "guidance": guidance,
                    "tags": ["codebase", "atlas", "onboard"],
                    "confidence": 0.6,
                    "created_by": "system:codebase-onboard",
                }), db)?;
                Ok(row.get("id").cloned().unwrap_or(Value::Null))
            })().unwrap_or(Value::Null)
        } else {
            Value::Null
        };

        // 10. REGISTER — the ONLY live-atlas write.
        //
        // IT CANNOT RUN HERE WHEN THE CALLER STAGES. atlas-register copies `stage.* → code.*`, but
        // during a staged rebuild this connection's `code.` schema IS the staging file (the runner
        // opened it that way so every atlas write lands off the live DB). Copying staging onto
        // staging is a no-op, and the live atlas stayed empty after every "successful" rebuild —
        // node_count 0 in the FE while the staging file held the whole tree.
        //
        // `register.defer` is the caller saying "I will re-open with `code.` on the LIVE file and
        // dispatch system.codebase.atlas.register myself". The in-line branch is kept for callers
        // that pass a staging path on a connection already pointing at live (the tests do this).
        if let Some(staging) = context.get("register").and_then(|r| r.get("staging_code_path")).filter(|v| !v.is_null()) {
            let deferred = context.get("register").and_then(|r| r.get("defer")).and_then(Value::as_bool).unwrap_or(false);
            if !deferred {
                set_phase(db, job_id_ref, "register");
                crate::codebase_atlas_register::atlasRegister(&json!({ "staging_code_path": staging.clone(), "origin": origin }), db)?;
            }
        }

        finish_job(db, job_id_ref, &profile);
        Ok(json!({
            "root": atlas.get("root").cloned().unwrap_or(Value::Null),
            "profile": profile,
            "scope": scope,
            "atlas": atlas,
            "metrics": metrics,
            "coverage": coverage,
            "entry_points": entry_points,
            "digest": digest.unwrap_or(Value::Null),
            "memory_id": memory_id,
        }))
    })();

    if let Err(e) = &run {
        fail_job(db, job_id_ref, e);
    }
    run
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs as stdfs;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-codebase-onboard-rs-{tag}-{}", std::process::id()));
        let _ = stdfs::remove_dir_all(&dir);
        stdfs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db_with_schema() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::codebase_code_schema::ensureCodeSchema(&c, &Value::Null);
        c
    }

    #[test]
    fn churn_and_owners_falls_back_to_git_churn_when_history_is_null() {
        let (churn, owners) = churn_and_owners("C:/nowhere-real", &Value::Null);
        // gitChurn's execFileSync throws on a non-repo root -> catches to null (verified against
        // real JS: `node -e "gitChurn('C:/nowhere-real')"` -> null). The JS `m.size ? m :
        // gitChurn(root)` fallback therefore resolves to null, not an empty object.
        assert_eq!(churn, Value::Null);
        assert_eq!(owners, Value::Null);
    }

    #[test]
    fn churn_and_owners_reduces_by_file_commits_from_history() {
        let hist = json!({ "by_file": { "a.js": { "commits": 3 }, "b.js": { "commits": 1 } } });
        let (churn, owners) = churn_and_owners("C:/whatever", &hist);
        assert_eq!(churn, json!({ "a.js": 3, "b.js": 1 }));
        assert_eq!(owners, hist["by_file"]);
    }

    #[test]
    fn top_n_projects_only_the_requested_fields_and_caps_at_n() {
        let metrics = json!({ "top_central": [
            { "path": "a.js", "fan_in": 3, "other": "x" },
            { "path": "b.js", "fan_in": 2 },
            { "path": "c.js", "fan_in": 1 },
        ] });
        let r = top_n(&metrics, "top_central", &["path", "fan_in"], 2);
        assert_eq!(r, json!([{ "path": "a.js", "fan_in": 3 }, { "path": "b.js", "fan_in": 2 }]));
    }

    #[test]
    fn onboards_a_tiny_real_codebase_end_to_end() {
        let dir = temp_dir("e2e");
        stdfs::write(dir.join("a.mjs"), "export function foo() { return 1; }\n").unwrap();
        let db = db_with_schema();
        let r = codebaseOnboard(&json!({ "root": dir.to_string_lossy(), "remember": false }), &Value::Null, &db).unwrap();

        assert_eq!(r["profile"]["files"], json!(1));
        assert_eq!(r["profile"]["parse_errors"], json!(0));
        assert!(r["profile"]["coverage"].is_object());
        // best-effort phases (no judgments/laws/memory schema seeded) degrade to ok:false, never
        // fail the whole onboard.
        assert_eq!(r["profile"]["constitution"]["ok"], json!(false));
        assert_eq!(r["profile"]["agents"]["ok"], json!(false));
        assert_eq!(r["digest"], Value::Null); // summarize not opted in
        assert_eq!(r["memory_id"], Value::Null); // remember:false
        // THE REPORT SAYS HOW MUCH HAS PROSE AND HOW MUCH PARSED (2026-10-01: "X-ray summaries empty (0% prose)" went unexplained for
        // weeks because onboarding reported neither) - a fresh onboarding has facts and no sentences, and the profile says so in words
        let d = &r["profile"]["descriptions"];
        assert_eq!(d["with_prose"], json!(0), "{d}");
        assert!(d["describable"].as_i64().unwrap() >= 1, "the one file with a function has facts to describe: {d}");
        assert!(d["note"].as_str().unwrap().contains("(0%)") && d["note"].as_str().unwrap().contains("describe/plan"), "{d}");
        assert_eq!(r["profile"]["parse_health"], json!({ "fail": 0, "degraded": 0, "skipped": 0 }));
        // the atlas landed for real.
        let count: i64 = db.query_row("SELECT COUNT(*) FROM code_nodes WHERE origin='atlas' AND type='file'", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn job_row_tracks_phase_transitions_and_finishes_done() {
        let dir = temp_dir("job");
        stdfs::write(dir.join("a.mjs"), "export const x = 1;\n").unwrap();
        let db = db_with_schema();
        db.execute(
            "INSERT INTO code_onboard_jobs (job_id, root, phase, status) VALUES ('j1', ?, 'queued', 'running')",
            rusqlite::params![dir.to_string_lossy().to_string()],
        ).unwrap();
        let r = codebaseOnboard(&json!({ "job_id": "j1", "remember": false }), &Value::Null, &db).unwrap();
        assert_eq!(r["profile"]["files"], json!(1));
        let (phase, status): (String, String) = db.query_row(
            "SELECT phase, status FROM code_onboard_jobs WHERE job_id = 'j1'", [], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(phase, "done");
        assert_eq!(status, "done");
        // …and every phase it ran has its milliseconds on the row, which is what Settings > Substrate counts a rebuild by
        let detail: String = db.query_row("SELECT detail FROM code_onboard_jobs WHERE job_id = 'j1'", [], |row| row.get(0)).unwrap();
        let d: Value = serde_json::from_str(&detail).unwrap();
        let pm = d["phase_ms"].as_object().expect("phase_ms is written");
        for ph in ["scope", "ingest", "metrics"] { assert!(pm.contains_key(ph), "{ph} in {detail}"); }
    }

    #[test]
    fn phase_timings_merge_into_what_the_row_already_carries_and_a_failed_job_records_none() {
        let phases = vec![("scope".to_string(), 12u128), ("ingest".to_string(), 340), ("scope".to_string(), 3)];
        let d: Value = serde_json::from_str(&detail_with_phases(Some(r#"{"pct":50,"current":"ingest"}"#), &phases).unwrap()).unwrap();
        assert_eq!(d["pct"], json!(50));                                  // the progress the panel reads is kept
        assert_eq!(d["current"], json!("ingest"));
        assert_eq!(d["phase_ms"], json!({ "scope": 15, "ingest": 340 })); // a phase that ran twice is summed
        assert!(detail_with_phases(Some("{}"), &[]).is_none());           // nothing ran: nothing written
        assert!(detail_with_phases(Some("not json"), &phases).is_some()); // unreadable old detail is replaced, not fatal
        // the clock: open, switch, close
        let id = "clock-test";
        assert!(phase_tick(id, Some("a")).is_empty());
        std::thread::sleep(std::time::Duration::from_millis(3));
        assert!(phase_tick(id, Some("b")).is_empty());
        let done = phase_tick(id, None);
        assert_eq!(done.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), vec!["a", "b"]);
        assert!(done[0].1 >= 2, "a ran about 3 ms: {done:?}");
        assert!(phase_tick(id, None).is_empty(), "closing twice yields nothing");
    }

    #[test]
    fn root_falls_back_to_the_jobs_own_recorded_root_when_input_root_is_absent() {
        let dir = temp_dir("jobroot");
        stdfs::write(dir.join("a.mjs"), "export const x = 1;\n").unwrap();
        let db = db_with_schema();
        db.execute(
            "INSERT INTO code_onboard_jobs (job_id, root, phase, status) VALUES ('j2', ?, 'queued', 'running')",
            rusqlite::params![dir.to_string_lossy().to_string()],
        ).unwrap();
        let r = codebaseOnboard(&json!({ "job_id": "j2", "remember": false }), &Value::Null, &db).unwrap();
        assert_eq!(r["profile"]["files"], json!(1));
    }

    #[test]
    fn failure_in_a_mandatory_phase_propagates_and_marks_the_job_errored() {
        let db = db_with_schema();
        db.execute(
            "INSERT INTO code_onboard_jobs (job_id, root, phase, status) VALUES ('j3', 'placeholder', 'queued', 'running')",
            [],
        ).unwrap();
        // an empty-string root fails codebase-scope's `root is required` guard — mandatory phase.
        let r = codebaseOnboard(&json!({ "job_id": "j3", "root": "" }), &Value::Null, &db);
        assert!(r.is_err());
        let (status, error): (String, Option<String>) = db.query_row(
            "SELECT status, error FROM code_onboard_jobs WHERE job_id = 'j3'", [], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(status, "error");
        assert!(error.unwrap().starts_with("[scope]"));
    }

    #[test]
    fn scope_defaults_come_from_the_saved_code_scope_row() {
        let dir = temp_dir("scope");
        stdfs::write(dir.join("a.mjs"), "export const x = 1;\n").unwrap();
        stdfs::write(dir.join("b.ts"), "export const y = 1;\n").unwrap();
        let db = db_with_schema();
        db.execute("INSERT INTO code_scope (origin, exclude_json, include_json) VALUES ('atlas', '[]', '[\"*.mjs\"]')", []).unwrap();
        let r = codebaseOnboard(&json!({ "root": dir.to_string_lossy(), "remember": false }), &Value::Null, &db).unwrap();
        assert_eq!(r["profile"]["files"], json!(1)); // only the .mjs file matches the saved include glob
    }

    #[test]
    fn digest_stays_null_unless_summarize_is_opted_in() {
        let dir = temp_dir("nosum");
        stdfs::write(dir.join("a.mjs"), "export const x = 1;\n").unwrap();
        let db = db_with_schema();
        let r = codebaseOnboard(&json!({ "root": dir.to_string_lossy(), "remember": false }), &Value::Null, &db).unwrap();
        assert_eq!(r["digest"], Value::Null);
        assert_eq!(r["profile"]["digest_md"], Value::Null);
    }
}
