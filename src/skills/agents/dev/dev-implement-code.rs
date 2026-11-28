//! dev-implement-code.rs — twin of skills/agents/dev/dev-implement-code.mjs. BESPOKE-NATIVE, not
//! conform-db fixtured (real fs reads/writes via cross-module twins, sandbox/worktree path
//! resolution against the real filesystem — same class as file-write.rs / file-read.rs). NAMES ARE
//! CONTRACT: `devImplementCode` (default).
//!
//! DEV is the deterministic code-applying executor: TL authors literal `microtask.patches`, DEV
//! mechanically splices them via the already-converted `applyPatchToSource` twin and writes through
//! the already-converted `fileWrite` twin (which itself resolves sandbox/worktree routing). DEV
//! makes ZERO AI calls — nothing here is non-deterministic beyond "read whatever the sandbox/
//! worktree/original file currently contains on disk", the same class of real-fs dependency as its
//! sibling bespoke-native twins.
//!
//! Cross-module reuse (per the crossing playbook — all already-converted twins, called directly,
//! never re-implemented here):
//!   - `crate::agents_dev_dev_contract_load::devContractLoad` — loads the contract + ready_for_impl gate.
//!   - `crate::agents_dev_dev_consume_qa_suggestions::devConsumeQaSuggestions` — QA feedback intake (best-effort).
//!   - `crate::agents_dev_apply_section_patch::applyPatchToSource` — the anchor-based splicer. Its own
//!     twin swallows a JS-side throw into `Value::Null` (see that file's header comment) — this twin
//!     treats a `Null` return as "patch failed to apply" and records a per-file error entry, mirroring
//!     the JS `try { ...applyPatchToSource... } catch (e) { filesModified.push({path, error: e.message}) }`
//!     control flow without reproducing the fragile exact JS error text (throw-fidelity gotcha).
//!   - `crate::external_file_write::fileWrite` — sandbox/worktree-aware write.
//!   - `crate::entities_card_phase_event_record::cardPhaseEventRecord` — best-effort dev_impl phase stamp.
//!   - `crate::entities_inbox_message_add::inboxMessageAdd` — the JS `await import(...)` dynamic-import
//!     dispatch of `entities/inbox-message-add.mjs` is called here as an ordinary direct fn call: the
//!     twin already exists, so the dynamic-import mechanism itself carries no extra semantics to
//!     preserve (per the playbook, a `DEFERRED` stub is only needed when no twin exists yet).
//!
//! Frozen clock: `datetime('now')` → the pinned literal `2026-01-01 00:00:00` per the crossing
//! playbook's datetime lane (this file has no fixture, but the literal is used consistently with
//! every other twin so a future fixture-backed caller sees the same instant).
//!
//! Throws collapse to `Err(String)` carrying `"dev-implement-code: <cause>"` (AntilokiError message
//! format, `${skill}: ${cause}`) for audit-log parity; per the throw-fidelity gotcha these are not
//! fixture-compared. `applyPatchToSource`'s own swallowed error means the per-file anchor-miss
//! message is a fixed generic string here, not the JS anchor text — documented above.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn frozen_now() -> String { crate::clock::now_ts_nodb() }

// node:path win32 `resolve(base, rel)` — same duplicated-per-file convention as file-write.rs.
fn node_join(base: &Path, rel: &str) -> PathBuf {
    let mut p = base.to_path_buf();
    for part in rel.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        p.push(part);
    }
    p
}

// JS: `PROJECT_ROOT = resolve(HERE, '..', '..', '..', '..')` where HERE is
// .antiloki-core/skills/agents/dev — 4 levels up from this module's own dir is the repo root.
// Same override seam as file-write.rs's `project_root()` (antiloki-core-rust's parent on this
// checkout layout), so a sandbox-relative or original-file read lands on the same substrate root
// the sandbox-aware `fileWrite` twin itself resolves against.
fn project_root() -> PathBuf {
    if let Ok(v) = std::env::var("ANTILOKI_SUBSTRATE_ROOT") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .to_path_buf()
}

fn err_msg(cause: &str) -> String {
    format!("dev-implement-code: {cause}")
}

pub fn devImplementCode(input: &Value, db: &Connection) -> Result<Value, String> {
    let empty = json!({});
    let input = if input.is_object() { input } else { &empty };

    let contract_slug = match input.get("contract_slug").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => return Err(err_msg("contract_slug is required")),
    };
    let dry_run = truthy(input.get("dry_run"));

    // 1. Load contract (already-converted twin).
    let loaded = crate::agents_dev_dev_contract_load::devContractLoad(&json!(contract_slug), db)?;
    let mut contract = loaded.get("contract").cloned().unwrap_or(Value::Null);
    let ready_for_impl = loaded.get("ready_for_impl").and_then(Value::as_bool).unwrap_or(false);
    if !ready_for_impl {
        let status = contract.get("status").and_then(Value::as_str).unwrap_or("");
        return Err(err_msg(&format!(
            "contract '{contract_slug}' status='{status}', expected tl_approved, human_approved, or impl (retry)"
        )));
    }

    let allowed: Vec<String> = contract
        .get("files_touched")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    if allowed.is_empty() {
        return Err(err_msg("contract.files_touched is empty — nothing to implement"));
    }
    let is_allowed = |f: &str| allowed.iter().any(|a| a == f);

    let card_slug = contract.get("card_slug").cloned();

    // EPIC-SWARM C2: swarm_blocks materialization path (best-effort read — tables may be missing
    // on legacy DBs, matching the JS `try { ... } catch { swarmBlocks = []; }`).
    let swarm_blocks: Vec<Value> = (|| -> Option<Vec<Value>> {
        let man = crate::db_json::query_rows(
            db,
            "SELECT id FROM swarm_manifests WHERE contract_slug = ? AND superseded_by IS NULL ORDER BY version DESC, id DESC LIMIT 1",
            &[json!(contract_slug)],
        ).ok()?;
        let man_id = man.as_array().and_then(|a| a.first()).and_then(|r| r.get("id")).cloned()?;
        let blocks = crate::db_json::query_rows(
            db,
            "SELECT block_id, file, content FROM swarm_blocks WHERE manifest_id = ? ORDER BY id",
            &[man_id],
        ).ok()?;
        blocks.as_array().cloned()
    })().unwrap_or_default();

    if !swarm_blocks.is_empty() {
        let offenders: Vec<String> = swarm_blocks
            .iter()
            .filter_map(|b| b.get("file").and_then(Value::as_str))
            .filter(|f| !is_allowed(f))
            .map(|s| s.to_string())
            .collect();
        if !offenders.is_empty() {
            return Err(err_msg(&format!(
                "swarm authored out-of-scope file(s) ({}): {} — the manifest/block set drifted from contract.files_touched",
                offenders.len(),
                offenders.join(", ")
            )));
        }
        let mut files_modified = Vec::new();
        for b in &swarm_blocks {
            let file = b.get("file").and_then(Value::as_str).unwrap_or("").to_string();
            let content = b.get("content").and_then(Value::as_str).unwrap_or("").to_string();
            if !dry_run {
                crate::external_file_write::fileWrite(
                    &json!({ "path": file, "content": content, "contract_slug": contract_slug }),
                    db,
                )?;
            }
            files_modified.push(json!({ "path": file, "bytes_written": content.chars().count() as i64 }));
        }
        if !dry_run {
            crate::db_json::run_write(
                db,
                &format!("UPDATE contracts SET status='impl', updated_at=datetime('{}') WHERE slug = ?", frozen_now()),
                &[json!(contract_slug)],
            )?;
        }
        let status_before = contract.get("status").cloned().unwrap_or(Value::Null);
        return Ok(json!({
            "contract_slug": contract_slug,
            "planned": false,
            "swarm_materialized": files_modified.len() as i64,
            "files_modified": files_modified,
            "dry_run": dry_run,
            "new_status": if dry_run { status_before } else { json!("impl") },
        }));
    }

    // FEAT-QA-DEV-FEEDBACK-LOOP: best-effort QA-suggestion intake (never blocks DEV).
    match crate::agents_dev_dev_consume_qa_suggestions::devConsumeQaSuggestions(
        &json!({ "contract_slug": contract_slug }),
        db,
    ) {
        Ok(r) => {
            let appended = r.get("appended").and_then(Value::as_i64).unwrap_or(0);
            if appended > 0 {
                if let Some(obj) = contract.as_object_mut() {
                    obj.insert("microtasks_json".into(), r.get("new_microtasks_json").cloned().unwrap_or(json!("[]")));
                }
            }
        }
        Err(_) => { /* swallow — DEV proceeds with whatever microtasks already exist */ }
    }

    // FEAT-DEV-PER-MICROTASK-ITERATION
    let microtasks_list: Vec<Value> = match contract.get("microtasks_json") {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default(),
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    };
    let progress = contract.get("dev_microtask_progress").and_then(Value::as_i64).unwrap_or(0);
    let microtask_mode = !microtasks_list.is_empty();
    let current_microtask: Value = if microtask_mode && (progress as usize) < microtasks_list.len() {
        microtasks_list[progress as usize].clone()
    } else {
        input.get("microtask").cloned().unwrap_or(Value::Null)
    };

    // EPIC-QA-THINKS-DEV-EXECUTES / FEAT-DEV-REFUSES-NON-EXECUTOR-MICROTASKS
    if current_microtask.get("kind").and_then(Value::as_str) == Some("contract-amendment") {
        let amendment_type = current_microtask.get("amendment_type").and_then(Value::as_str);
        let microtask_id = current_microtask.get("microtask_id").cloned().unwrap_or(Value::Null);
        let _ = crate::db_json::run_write(
            db,
            "INSERT INTO notifications (kind, severity, source, message, related_card_slug, payload_json) VALUES ('generic', 'warn', 'agent:dev', ?, ?, ?)",
            &[
                json!(format!("DEV refused a contract-amendment microtask on {contract_slug} — TL should handle it via tl.contract.amend")),
                card_slug.clone().unwrap_or(Value::Null),
                json!(json!({
                    "subkind": "dev-refuses-non-executor",
                    "contract_slug": contract_slug,
                    "microtask_id": microtask_id,
                    "amendment_type": amendment_type,
                }).to_string()),
            ],
        );
        return Err(err_msg(&format!(
            "CONTRACT_AMENDMENT_NOT_FOR_DEV: microtask kind='contract-amendment' is TL's domain (route via tl.contract.amend), not DEV's. amendment_type='{}'",
            amendment_type.unwrap_or("(none)")
        )));
    }

    // WS-B (DEV_REFUSED_NO_PATCHES)
    let microtask_patches: Vec<Value> = current_microtask.get("patches").and_then(Value::as_array).cloned().unwrap_or_default();
    if current_microtask.is_null() || microtask_patches.is_empty() {
        let microtask_id = current_microtask.get("microtask_id").cloned().unwrap_or(Value::Null);
        let _ = crate::db_json::run_write(
            db,
            "INSERT INTO notifications (kind, severity, source, message, related_card_slug, payload_json) VALUES ('generic', 'warn', 'agent:dev', ?, ?, ?)",
            &[
                json!(format!("DEV refused: no patches to apply on {contract_slug} — TL must author microtask.patches")),
                card_slug.clone().unwrap_or(Value::Null),
                json!(json!({
                    "subkind": "dev-refuses-no-patches",
                    "contract_slug": contract_slug,
                    "microtask_id": microtask_id,
                }).to_string()),
            ],
        );
        let subject = format!(
            "DEV refused: {} on {contract_slug} has no patches",
            microtask_id.as_str().map(|s| s.to_string()).unwrap_or_else(|| "?".to_string())
        );
        let body_md = format!(
            "## What happened\nDEV refused to execute microtask **{}** on contract `{contract_slug}` because it carries no `patches`.\n\nDEV is a deterministic applier — it does not author code. TL must author the literal patches into the microtask before DEV can apply them.\n\n## What to do\n1. Press **x** → Fixit and look for the auto-suggested prevention card.\n2. Re-route to TL (`tl.refine.microtasks`) so it re-authors the microtask with concrete `patches`.",
            microtask_id.as_str().map(|s| s.to_string()).unwrap_or_else(|| "?".to_string())
        );
        let _ = crate::entities_inbox_message_add::inboxMessageAdd(
            &json!({
                "kind": "dev-refused-no-patches",
                "subject": subject,
                "body_md": body_md,
                "severity": "error",
                "related_card_slug": card_slug.clone().unwrap_or(Value::Null),
                "related_contract_slug": contract_slug,
                "suggested_reconciler": "escalated-contract-no-more-work",
                "payload_json": { "microtask_id": microtask_id },
            }),
            db,
        );
        return Err(err_msg("DEV_REFUSED_NO_PATCHES: microtask carries no patches — TL must author the literal code (microtask.patches) before DEV can apply. DEV does not write code."));
    }

    // Read TL-authored patches — no AI.
    let planned_patches: Vec<Value> = microtask_patches
        .iter()
        .enumerate()
        .map(|(i, p)| {
            json!({
                "file": p.get("file").cloned().unwrap_or(Value::Null),
                "op": p.get("op").and_then(Value::as_str).unwrap_or("replace_section"),
                "anchor_before": p.get("anchor_before").and_then(Value::as_str).unwrap_or(""),
                "anchor_after": p.get("anchor_after").and_then(Value::as_str).unwrap_or(""),
                "replacement": p.get("replacement").and_then(Value::as_str).unwrap_or(""),
                "rationale_md": p.get("rationale_md").and_then(Value::as_str).unwrap_or(""),
                "order_index": i as i64,
            })
        })
        .collect();

    // SUBSTRATE GATE
    let offenders: Vec<String> = planned_patches
        .iter()
        .filter_map(|p| p.get("file").and_then(Value::as_str))
        .filter(|f| !is_allowed(f))
        .map(|s| s.to_string())
        .collect();
    if !offenders.is_empty() {
        return Err(err_msg(&format!(
            "patch targets out-of-scope file(s) ({}): {}",
            offenders.len(),
            offenders.join(", ")
        )));
    }

    // Apply patches per file (grouped, preserving first-seen file order — JS Map semantics).
    let mut files_modified: Vec<Value> = Vec::new();
    if !dry_run {
        let mut order: Vec<String> = Vec::new();
        let mut by_file: std::collections::HashMap<String, Vec<Value>> = std::collections::HashMap::new();
        for p in &planned_patches {
            let f = p.get("file").and_then(Value::as_str).unwrap_or("").to_string();
            if !by_file.contains_key(&f) {
                order.push(f.clone());
            }
            by_file.entry(f).or_default().push(p.clone());
        }

        let worktree_path = contract.get("worktree_path").and_then(Value::as_str).filter(|s| !s.is_empty());
        let sandbox_path = contract.get("sandbox_path").and_then(Value::as_str).filter(|s| !s.is_empty());
        let root = project_root();
        let sandbox_abs_root: Option<PathBuf> = worktree_path
            .map(PathBuf::from)
            .or_else(|| sandbox_path.map(|sp| node_join(&root, sp)));

        for file_path in order {
            let patches = by_file.get(&file_path).cloned().unwrap_or_default();
            let is_new_op = patches.first().and_then(|p| p.get("op")).and_then(Value::as_str) == Some("create");

            let (mut current, is_new) = if is_new_op {
                (String::new(), true)
            } else {
                let sb_abs = sandbox_abs_root.as_ref().map(|r| node_join(r, &file_path));
                if let Some(sb) = sb_abs.as_ref().filter(|p| p.exists()) {
                    match std::fs::read_to_string(sb) {
                        Ok(s) => (s, false),
                        Err(e) => { files_modified.push(json!({ "path": file_path, "error": e.to_string() })); continue; }
                    }
                } else {
                    let orig = node_join(&root, &file_path);
                    if orig.exists() {
                        match std::fs::read_to_string(&orig) {
                            Ok(s) => (s, false),
                            Err(e) => { files_modified.push(json!({ "path": file_path, "error": e.to_string() })); continue; }
                        }
                    } else {
                        (String::new(), true)
                    }
                }
            };

            let mut failed = false;
            for p in &patches {
                match crate::agents_dev_apply_section_patch::applyPatchToSource(&json!(current), p) {
                    Value::String(s) => current = s,
                    _ => { failed = true; break; }
                }
            }
            if failed {
                files_modified.push(json!({ "path": file_path, "error": "patch application failed (anchor not found or ambiguous)" }));
                continue;
            }

            match crate::external_file_write::fileWrite(
                &json!({
                    "path": file_path,
                    "content": current,
                    "contract_slug": contract_slug,
                    "mode": if is_new { "create" } else { "overwrite" },
                }),
                db,
            ) {
                Ok(_) => files_modified.push(json!({
                    "path": file_path,
                    "patches_applied": patches.len() as i64,
                    "bytes_written": current.chars().count() as i64,
                })),
                Err(e) => files_modified.push(json!({ "path": file_path, "error": e })),
            }
        }
    }

    // FEAT-AUTO-PHASE-EVENTS
    let wrote_something = files_modified.iter().any(|f| {
        f.get("error").is_none()
            && f.get("bytes_written").and_then(Value::as_i64).unwrap_or(0) > 0
    });
    let mut phase_event_id: Value = Value::Null;
    if !dry_run && truthy(card_slug.as_ref()) && wrote_something {
        let files: Vec<Value> = files_modified
            .iter()
            .filter(|f| f.get("error").is_none())
            .filter_map(|f| f.get("path").cloned())
            .collect();
        if let Ok(ev) = crate::entities_card_phase_event_record::cardPhaseEventRecord(
            &json!({
                "card_slug": card_slug.clone().unwrap_or(Value::Null),
                "phase": "dev_impl",
                "actor": "agent:dev",
                "evidence": {
                    "source": "dev-implement-code",
                    "contract_slug": contract_slug,
                    "files_modified": files,
                },
            }),
            db,
        ) {
            phase_event_id = ev.get("id").cloned().unwrap_or(Value::Null);
        }
    }

    // FIX-PIPELINE-DEV-IMPL-ADVANCES-STATUS / FEAT-DEV-PER-MICROTASK-ITERATION
    let mut new_progress = progress;
    let should_advance = !dry_run && wrote_something;
    if should_advance {
        if microtask_mode {
            new_progress = progress + 1;
            let _ = crate::db_json::run_write(
                db,
                "UPDATE contracts SET dev_microtask_progress = ?, updated_at = datetime(?) WHERE slug = ?",
                &[json!(new_progress), json!(frozen_now()), json!(contract_slug)],
            );
            if (new_progress as usize) >= microtasks_list.len() {
                crate::db_json::run_write(
                    db,
                    "UPDATE contracts SET status = 'impl', updated_at = datetime(?) WHERE slug = ?",
                    &[json!(frozen_now()), json!(contract_slug)],
                )?;
            }
        } else {
            crate::db_json::run_write(
                db,
                "UPDATE contracts SET status = 'impl', updated_at = datetime(?) WHERE slug = ?",
                &[json!(frozen_now()), json!(contract_slug)],
            )?;
        }
    }

    let files_modified_out: Value = if dry_run {
        Value::Array(
            planned_patches
                .iter()
                .map(|p| {
                    let mut m = p.as_object().cloned().unwrap_or_default();
                    m.insert("dry_run".into(), json!(true));
                    Value::Object(m)
                })
                .collect(),
        )
    } else {
        Value::Array(files_modified)
    };

    let mut out = Map::new();
    out.insert("contract_slug".into(), json!(contract_slug));
    out.insert("planned".into(), json!(planned_patches.len() as i64));
    out.insert("files_modified".into(), files_modified_out);
    out.insert("dry_run".into(), json!(dry_run));
    out.insert("phase_event_id".into(), phase_event_id);
    out.insert("microtask_mode".into(), json!(microtask_mode));
    out.insert("microtask_index".into(), if microtask_mode { json!(progress) } else { Value::Null });
    out.insert("microtask_index_next".into(), if microtask_mode { json!(new_progress) } else { Value::Null });
    out.insert("microtask_total".into(), if microtask_mode { json!(microtasks_list.len() as i64) } else { Value::Null });
    out.insert(
        "current_microtask_id".into(),
        current_microtask.get("microtask_id").cloned().unwrap_or(Value::Null),
    );
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (
                slug TEXT PRIMARY KEY, status TEXT, card_slug TEXT,
                files_touched_json TEXT DEFAULT '[]', microtasks_json TEXT DEFAULT '[]',
                dev_microtask_progress INTEGER DEFAULT 0, worktree_path TEXT, sandbox_path TEXT,
                updated_at TEXT
             );
             CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, body_json TEXT);
             CREATE TABLE notifications (id INTEGER PRIMARY KEY, kind TEXT, severity TEXT, source TEXT, message TEXT, related_card_slug TEXT, payload_json TEXT, ts TEXT DEFAULT (datetime('now')));
             CREATE TABLE inbox_messages (id INTEGER PRIMARY KEY, kind TEXT, subject TEXT, body_md TEXT, severity TEXT, related_card_slug TEXT, related_contract_slug TEXT, suggested_reconciler TEXT, suggested_card_action TEXT, payload_json TEXT, ts TEXT DEFAULT (datetime('now')));
             CREATE TABLE card_phase_events (id INTEGER PRIMARY KEY, card_slug TEXT, phase TEXT, actor TEXT, evidence_json TEXT, ts TEXT DEFAULT (datetime('now')));
             CREATE TABLE swarm_manifests (id INTEGER PRIMARY KEY, contract_slug TEXT, version INTEGER DEFAULT 1, superseded_by INTEGER);
             CREATE TABLE swarm_blocks (id INTEGER PRIMARY KEY, contract_slug TEXT, manifest_id INTEGER, block_id TEXT, file TEXT, content TEXT);
             CREATE TABLE qa_runs (id INTEGER PRIMARY KEY, contract_slug TEXT, all_passed INTEGER, suggested_microtasks_json TEXT, created_at TEXT DEFAULT (datetime('now')));",
        )
        .unwrap();
        c
    }

    fn seed_contract(c: &Connection, slug: &str, status: &str, files: &[&str], microtasks: &Value, sandbox_path: &str) {
        c.execute("INSERT INTO cards (slug, title) VALUES (?1, 'card')", [slug]).unwrap();
        c.execute(
            "INSERT INTO contracts (slug, status, card_slug, files_touched_json, microtasks_json, sandbox_path) VALUES (?1, ?2, ?1, ?3, ?4, ?5)",
            rusqlite::params![slug, status, serde_json::to_string(files).unwrap(), microtasks.to_string(), sandbox_path],
        )
        .unwrap();
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-dev-implement-code-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn with_substrate_root<T>(root: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", root); }
        let r = f();
        unsafe { std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT"); }
        r
    }

    #[test]
    fn missing_contract_slug_is_required() {
        let c = db();
        let e = devImplementCode(&json!({}), &c).unwrap_err();
        assert_eq!(e, "dev-implement-code: contract_slug is required");
    }

    #[test]
    fn refuses_when_microtask_has_no_patches() {
        let c = db();
        let mt = json!([{ "microtask_id": "m1", "patches": [] }]);
        seed_contract(&c, "CON-A", "tl_approved", &["docs/x.md"], &mt, ".antiloki/sandbox/CON-A");
        let e = devImplementCode(&json!({ "contract_slug": "CON-A" }), &c).unwrap_err();
        assert!(e.contains("DEV_REFUSED_NO_PATCHES"), "got: {e}");
        let progress: i64 = c.query_row("SELECT dev_microtask_progress FROM contracts WHERE slug='CON-A'", [], |r| r.get(0)).unwrap();
        assert_eq!(progress, 0);
        let inbox: i64 = c.query_row("SELECT COUNT(*) FROM inbox_messages WHERE kind='dev-refused-no-patches'", [], |r| r.get(0)).unwrap();
        assert_eq!(inbox, 1);
        let notif: i64 = c.query_row("SELECT COUNT(*) FROM notifications WHERE source='agent:dev'", [], |r| r.get(0)).unwrap();
        assert_eq!(notif, 1);
    }

    #[test]
    fn refuses_contract_amendment_microtask() {
        let c = db();
        let mt = json!([{ "microtask_id": "amend-1", "kind": "contract-amendment", "amendment_type": "widen-files-touched" }]);
        seed_contract(&c, "CON-B", "tl_approved", &["docs/z.md"], &mt, ".antiloki/sandbox/CON-B");
        let e = devImplementCode(&json!({ "contract_slug": "CON-B" }), &c).unwrap_err();
        assert!(e.contains("CONTRACT_AMENDMENT_NOT_FOR_DEV"), "got: {e}");
        assert!(e.contains("widen-files-touched"));
        let row: (String, String) = c.query_row("SELECT severity, message FROM notifications WHERE source='agent:dev'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(row.0, "warn");
        assert!(row.1.contains("contract-amendment"));
    }

    #[test]
    fn empty_files_touched_is_rejected() {
        let c = db();
        seed_contract(&c, "CON-C", "tl_approved", &[], &json!([]), "");
        let e = devImplementCode(&json!({ "contract_slug": "CON-C" }), &c).unwrap_err();
        assert!(e.contains("files_touched is empty"), "got: {e}");
    }

    #[test]
    fn not_ready_status_is_rejected() {
        let c = db();
        seed_contract(&c, "CON-D", "proposed", &["a.md"], &json!([]), "");
        let e = devImplementCode(&json!({ "contract_slug": "CON-D" }), &c).unwrap_err();
        assert!(e.contains("status='proposed'"), "got: {e}");
    }

    #[test]
    fn applies_create_patch_writes_real_file_and_advances_status() {
        let dir = temp_dir("create");
        let mt = json!([{
            "microtask_id": "m1",
            "patches": [{ "file": "docs/new.md", "op": "create", "replacement": "# Hello\n" }],
        }]);
        let r = with_substrate_root(dir.to_str().unwrap(), || {
            let c = db();
            seed_contract(&c, "CON-E", "tl_approved", &["docs/new.md"], &mt, ".antiloki/sandbox/CON-E");
            let out = devImplementCode(&json!({ "contract_slug": "CON-E" }), &c).unwrap();
            let status: String = c.query_row("SELECT status FROM contracts WHERE slug='CON-E'", [], |row| row.get(0)).unwrap();
            (out, status)
        });
        let (out, status) = r;
        assert_eq!(status, "impl");
        let written = dir.join(".antiloki/sandbox/CON-E/docs/new.md");
        assert_eq!(fs::read_to_string(&written).unwrap(), "# Hello\n");
        assert_eq!(out["files_modified"][0]["patches_applied"], json!(1));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dry_run_does_not_write_or_advance() {
        let dir = temp_dir("dryrun");
        let mt = json!([{
            "microtask_id": "m1",
            "patches": [{ "file": "docs/new.md", "op": "create", "replacement": "content" }],
        }]);
        let r = with_substrate_root(dir.to_str().unwrap(), || {
            let c = db();
            seed_contract(&c, "CON-F", "tl_approved", &["docs/new.md"], &mt, ".antiloki/sandbox/CON-F");
            let out = devImplementCode(&json!({ "contract_slug": "CON-F", "dry_run": true }), &c).unwrap();
            let status: String = c.query_row("SELECT status FROM contracts WHERE slug='CON-F'", [], |row| row.get(0)).unwrap();
            (out, status)
        });
        let (out, status) = r;
        assert_eq!(status, "tl_approved");
        assert_eq!(out["dry_run"], json!(true));
        assert!(!dir.join(".antiloki/sandbox/CON-F/docs/new.md").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn out_of_scope_patch_file_is_rejected() {
        let dir = temp_dir("scope");
        let mt = json!([{
            "microtask_id": "m1",
            "patches": [{ "file": "src/SNEAKY.ts", "op": "create", "replacement": "x" }],
        }]);
        let r = with_substrate_root(dir.to_str().unwrap(), || {
            let c = db();
            seed_contract(&c, "CON-G", "tl_approved", &["src/in.ts"], &mt, ".antiloki/sandbox/CON-G");
            devImplementCode(&json!({ "contract_slug": "CON-G" }), &c)
        });
        let e = r.unwrap_err();
        assert!(e.contains("out-of-scope"), "got: {e}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn swarm_blocks_materialize_and_advance_to_impl() {
        let dir = temp_dir("swarm");
        let r = with_substrate_root(dir.to_str().unwrap(), || {
            let c = db();
            seed_contract(&c, "CON-H", "tl_approved", &["src/in.ts"], &json!([]), ".antiloki/sandbox/CON-H");
            let man_id: i64 = c.query_row(
                "INSERT INTO swarm_manifests (contract_slug) VALUES ('CON-H') RETURNING id", [], |row| row.get(0),
            ).unwrap();
            c.execute(
                "INSERT INTO swarm_blocks (contract_slug, manifest_id, block_id, file, content) VALUES ('CON-H', ?1, 'a1', 'src/in.ts', 'OK')",
                [man_id],
            ).unwrap();
            let out = devImplementCode(&json!({ "contract_slug": "CON-H" }), &c).unwrap();
            let status: String = c.query_row("SELECT status FROM contracts WHERE slug='CON-H'", [], |row| row.get(0)).unwrap();
            (out, status)
        });
        let (out, status) = r;
        assert_eq!(status, "impl");
        assert_eq!(out["swarm_materialized"], json!(1));
        let written = dir.join(".antiloki/sandbox/CON-H/src/in.ts");
        assert_eq!(fs::read_to_string(&written).unwrap(), "OK");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn swarm_out_of_scope_block_throws_and_does_not_advance() {
        let dir = temp_dir("swarm-drop");
        let r = with_substrate_root(dir.to_str().unwrap(), || {
            let c = db();
            seed_contract(&c, "CON-I", "tl_approved", &["src/in.ts"], &json!([]), ".antiloki/sandbox/CON-I");
            let man_id: i64 = c.query_row(
                "INSERT INTO swarm_manifests (contract_slug) VALUES ('CON-I') RETURNING id", [], |row| row.get(0),
            ).unwrap();
            c.execute(
                "INSERT INTO swarm_blocks (contract_slug, manifest_id, block_id, file, content) VALUES ('CON-I', ?1, 'a1', 'src/in.ts', 'OK')",
                [man_id],
            ).unwrap();
            c.execute(
                "INSERT INTO swarm_blocks (contract_slug, manifest_id, block_id, file, content) VALUES ('CON-I', ?1, 'a2', 'src/SNEAKY.ts', 'DROPPED')",
                [man_id],
            ).unwrap();
            let e = devImplementCode(&json!({ "contract_slug": "CON-I" }), &c).unwrap_err();
            let status: String = c.query_row("SELECT status FROM contracts WHERE slug='CON-I'", [], |row| row.get(0)).unwrap();
            (e, status)
        });
        let (e, status) = r;
        assert!(e.contains("out-of-scope") || e.contains("SNEAKY"), "got: {e}");
        assert_eq!(status, "tl_approved");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn microtask_mode_advances_progress_without_flipping_status_until_last() {
        let dir = temp_dir("microtask-progress");
        let mt = json!([
            { "microtask_id": "m1", "patches": [{ "file": "a.md", "op": "create", "replacement": "1" }] },
            { "microtask_id": "m2", "patches": [{ "file": "a.md", "op": "insert_after", "anchor_before": "1", "replacement": "2" }] },
        ]);
        let r = with_substrate_root(dir.to_str().unwrap(), || {
            let c = db();
            seed_contract(&c, "CON-J", "tl_approved", &["a.md"], &mt, ".antiloki/sandbox/CON-J");
            let out1 = devImplementCode(&json!({ "contract_slug": "CON-J" }), &c).unwrap();
            let status1: String = c.query_row("SELECT status FROM contracts WHERE slug='CON-J'", [], |row| row.get(0)).unwrap();
            let progress1: i64 = c.query_row("SELECT dev_microtask_progress FROM contracts WHERE slug='CON-J'", [], |row| row.get(0)).unwrap();
            let out2 = devImplementCode(&json!({ "contract_slug": "CON-J" }), &c).unwrap();
            let status2: String = c.query_row("SELECT status FROM contracts WHERE slug='CON-J'", [], |row| row.get(0)).unwrap();
            (out1, status1, progress1, out2, status2)
        });
        let (out1, status1, progress1, out2, status2) = r;
        assert_eq!(status1, "tl_approved");
        assert_eq!(progress1, 1);
        assert_eq!(out1["microtask_total"], json!(2));
        assert_eq!(status2, "impl");
        assert_eq!(out2["current_microtask_id"], json!("m2"));
        let written = dir.join(".antiloki/sandbox/CON-J/a.md");
        assert_eq!(fs::read_to_string(&written).unwrap(), "12");
        let _ = fs::remove_dir_all(&dir);
    }
}
