//! with-audit.rs — twin of skills/engine/with-audit.mjs (db WRITE lane). NAMES ARE CONTRACT:
//! `withAudit` (default), `createAuditPack` (named, no-db). This round converts ONLY the
//! DEFAULT / immediate-seal code path of `withAudit` — the branch JS reaches when neither
//! `context.audit_session` nor `context.audit_pack` is set and `ANTILOKI_AUDIT_SKILL_UNLOCKED`
//! is unset (i.e. every call the conform harness ever makes, since its context is always bare
//! `{ db }`). The BEGIN IMMEDIATE / auditTxnChain mutex machinery that serializes concurrent
//! HTTP requests in production is NOT ported — the harness runs one case at a time against a
//! fresh in-memory db, so an ordinary rusqlite call reproduces the same net effect for a single
//! call. `flushAuditPack` / `flushAuditStage` / `flushAllStaged` are OUT OF SCOPE this round (see
//! the conversion manifest) — no currently-pending caller of this twin needs the staging path.
//!
//! DISPATCH: real with-audit resolves `input.skill` (a bare skill NAME string) via
//! `importSkill()`, a filename-index lookup that can reach ANY skill in the tree. The twin can
//! only dispatch to skills that already have a Rust twin — `dispatch_skill` below is a hand-kept
//! match arm per already-converted target, extended as more twins land (see the arm's own Err
//! message). This round's fixture proves the mechanism end-to-end against three already-converted
//! card-entity twins (card-status-change, card-update, card-reorder); it does not attempt to cover
//! every skill name with-audit can wrap in production.
//!
//! entity_type: all EIGHT ENTITY_QUERIES keys now have a working readEntityState query
//! (card/agent/action/memory/law/file/pending_diff/contract). It was card-only while the fixture
//! scope was card-only; that stopped being a scoping note and became a live outage the moment the
//! Rust engine became the DEFAULT, since every audited mutation to any other entity 500s'd. See
//! read_entity_state for the memory dual-key rule and the filesystem branch.
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

// JS String(v) coercion (== the playbook's js_string helper) — used for `String(entity_id)`,
// the exact coercion with the comment in the JS source explaining why (TEXT column + hash
// agreement regardless of whether the caller passed an integer or a string entity_id).
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

// The full ENTITY_QUERIES key set (JS object literal order) — used only for the enum-guard
// error message (`err.enumVal` joins `allowed` in THIS order), matching every other converted
// twin's `field must be one of: ... (got '...')` message shape.
const ENTITY_TYPES: [&str; 8] = ["card", "agent", "action", "memory", "law", "file", "pending_diff", "contract"];

// AUDIT_FIELD_MAX_BYTES: JS reads `process.env.ANTILOKI_AUDIT_FIELD_MAX_BYTES ?? 4096` at module
// load. The conform harness never sets that env var, so the twin hardcodes the default (per the
// playbook's env-flag simplification for the frozen-clock lane).
const AUDIT_FIELD_MAX_BYTES: usize = 4096;

// Audit-seal clock. cfg!(test): antiloki_rs's own unit tests get the frozen instant so their
// digital_print/ts assertions stay deterministic (no per-test env churn). Otherwise query SQLite's
// datetime('now') — the conform harness rewrites this to the frozen instant (fixtures match JS),
// while the production runtime gets real UTC wall-clock, which the sacred_timeline chain requires.
fn now_ts(db: &Connection) -> String {
    // This module used to carry its OWN copy of the frozen-vs-wall-clock decision, written before
    // crate::clock existed. Two implementations of one rule drift; the audit chain is the last
    // place that should happen, so it delegates.
    crate::clock::now_ts(db)
}

// truncMarker(s) = `__audit_truncated__:${s.length}:${sha256(s).slice(0,16)}`. JS `.length` is a
// UTF-16 code-unit count; fixtures stay ASCII/BMP (playbook gotcha #5), where that equals
// `s.len()` (bytes) == `s.chars().count()`, so `.len()` is safe here.
fn trunc_marker(s: &str) -> String {
    let digest = Sha256::digest(s.as_bytes());
    let sha16 = hex::encode(digest).chars().take(16).collect::<String>();
    format!("__audit_truncated__:{}:{}", s.len(), sha16)
}

// capAuditSnapshot: lazy-clone the row, replacing only oversized STRING fields with a trunc
// marker. Non-string fields (numbers, null) are never capped — mirrors the JS `typeof v ===
// 'string'` guard. `snapshot` here is always a `SELECT *` row (a JSON object), so the JS
// `snapshot == null || typeof !== 'object'` passthrough branch never applies in this twin's
// caller (before/after are only capped when `Some`).
fn cap_audit_snapshot(snapshot: &Value) -> Value {
    let Some(obj) = snapshot.as_object() else { return snapshot.clone() };
    let mut capped: Option<serde_json::Map<String, Value>> = None;
    for (k, v) in obj {
        if let Value::String(s) = v {
            if s.len() > AUDIT_FIELD_MAX_BYTES {
                let c = capped.get_or_insert_with(|| obj.clone());
                c.insert(k.clone(), json!(trunc_marker(s)));
            }
        }
    }
    capped.map(Value::Object).unwrap_or_else(|| snapshot.clone())
}

// capAuditPayload: JSON.stringify(wrappedInput) whole, capped as ONE unit (no per-field diff
// semantics — just a size ceiling). Returns the JSON STRING to store in the payload column (or
// None when wrappedInput is null/absent, matching JS `wrappedInput == null → null`).
fn cap_audit_payload(wrapped_input: &Value) -> Option<String> {
    if wrapped_input.is_null() {
        return None;
    }
    let json_str = serde_json::to_string(wrapped_input).unwrap_or_else(|_| "null".into());
    if json_str.len() <= AUDIT_FIELD_MAX_BYTES {
        return Some(json_str);
    }
    let digest = Sha256::digest(json_str.as_bytes());
    let sha16 = hex::encode(digest).chars().take(16).collect::<String>();
    // JS: JSON.stringify({ __audit_truncated__: json.length, sha256: sha16 }) — key order
    // __audit_truncated__ then sha256; both values are primitives (number, string), so a direct
    // format! reproduces JSON.stringify's output byte-for-byte without a re-serialization step.
    Some(format!("{{\"__audit_truncated__\":{},\"sha256\":\"{}\"}}", json_str.len(), sha16))
}

// computeDigitalPrint: sha256 hex of the 9 event fields joined by '|', in EXACT JS field order.
// `?? ''` fields (before_json/after_json/payload) pass an empty string when None.
#[allow(clippy::too_many_arguments)]
fn compute_digital_print(
    prev_print: &str,
    ts: &str,
    actor: &str,
    operation: &str,
    entity_type: &str,
    entity_id: &str,
    before_json: Option<&str>,
    after_json: Option<&str>,
    payload: Option<&str>,
) -> String {
    let content = [
        prev_print,
        ts,
        actor,
        operation,
        entity_type,
        entity_id,
        before_json.unwrap_or(""),
        after_json.unwrap_or(""),
        payload.unwrap_or(""),
    ]
    .join("|");
    hex::encode(Sha256::digest(content.as_bytes()))
}

// getLastPrint: head of the chain, or 'GENESIS' when sacred_timeline is empty.
fn get_last_print(db: &Connection) -> Result<String, String> {
    let rows = crate::db_json::query_rows(db, "SELECT digital_print FROM sacred_timeline ORDER BY id DESC LIMIT 1", &[])?;
    Ok(rows
        .as_array()
        .and_then(|a| a.first())
        .and_then(|r| r.get("digital_print"))
        .and_then(|v| v.as_str())
        .unwrap_or("GENESIS")
        .to_string())
}

// insertSeal — SIMPLIFICATION vs JS: the real insertSeal introspects `PRAGMA table_info` to
// build a column list that tolerates a pre-migration db missing user_goal_id/exec_env/tick_id.
// The twin's fixture always CREATEs sacred_timeline with the full modern column set (per the
// conversion manifest), so every column is always present and always inserted — no introspection
// needed, and the columns are irrelevant to digital_print (JS comment: none of the three is
// hashed), so this sidesteps the introspection honestly.
#[allow(clippy::too_many_arguments)]
fn insert_seal(
    db: &Connection,
    ts: &str,
    actor: &str,
    operation: &str,
    entity_type: &str,
    entity_id: &str,
    before_json: Option<&str>,
    after_json: Option<&str>,
    payload: Option<&str>,
    prev_print: &str,
    digital_print: &str,
    user_goal_id: Option<&str>,
    exec_env: Option<&str>,
    tick_id: Option<&str>,
) -> Result<(), String> {
    db.execute(
        "INSERT INTO sacred_timeline \
         (ts, actor, operation, entity_type, entity_id, before_json, after_json, payload, prev_print, digital_print, user_goal_id, exec_env, tick_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![ts, actor, operation, entity_type, entity_id, before_json, after_json, payload, prev_print, digital_print, user_goal_id, exec_env, tick_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

// PROJECT_ROOT for entity_type='file'. JS resolves it from the module's own location
// (skills/engine/../../.. = the core package root); the Rust lane has no equivalent module path,
// so it uses the same ANTILOKI_SUBSTRATE_ROOT-else-CARGO_MANIFEST_DIR-parent derivation every
// other filesystem-touching twin in this crate uses.
fn project_root() -> std::path::PathBuf {
    if let Ok(v) = std::env::var("ANTILOKI_SUBSTRATE_ROOT") {
        if !v.is_empty() { return std::path::PathBuf::from(v); }
    }
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    std::path::Path::new(manifest_dir).parent().map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from(manifest_dir))
}

// readFileState — entity_type='file' seals the CONTENT DIGEST, not a row: a missing file is None
// (JS returns null), and an existing one contributes {path, sha256, bytes}. `bytes` is the byte
// length, not a char count, because it is describing a buffer.
//
// The root is a PARAMETER rather than read from the environment inside, so the test can point it
// at a temp directory without mutating ANTILOKI_SUBSTRATE_ROOT. A test that sets a process-global
// env var is a test that breaks whichever unrelated test happens to read it concurrently — this
// crate has already paid for that lesson three times.
fn read_file_state_in(root: &std::path::Path, rel_path: &str) -> Option<Value> {
    let abs = root.join(rel_path);
    if !abs.exists() { return None; }
    let buf = std::fs::read(&abs).ok()?;
    Some(json!({
        "path": rel_path,
        "sha256": hex::encode(Sha256::digest(&buf)),
        "bytes": buf.len(),
    }))
}

// readEntityState — all 8 ENTITY_QUERIES keys.
//
// This used to support ONLY 'card' and returned a loud Err for the other seven, which was the
// right call while the fixture scope really was card-only: silently returning None would have
// computed a digital_print over a fabricated before/after, sealing a lie into an append-only
// chain. But the loud Err stopped being a scoping note the moment the Rust engine became the
// DEFAULT — every audited mutation to an agent, action, memory, law, file, pending_diff or
// contract 500s, which is most of the substrate's write surface. Found by a route-snapshot
// fixture that could not seed a memory row.
//
// MEMORY IS DUAL-KEY, and the reason is worth keeping: memory-add seals by TITLE (the integer id
// does not exist until after the insert) while supersede/validate/link-add/use-* seal by the
// integer id. A static `WHERE id = ?` matched only the id callers, so every memory.add seal
// captured null before/after. A numeric entity_id keys on the PK; a non-numeric one takes the
// LATEST row with that title — titles are not unique, and most-recent is the right answer for an
// append-only add seal.
//
// The final arm is NOT unreachable in practice: withAuditAs validates entity_type against
// ENTITY_TYPES before ever calling this fn, but flushAuditStage's crash-recovery path (reading a
// NULL after_json staged row) calls this with entity_type read straight off the DB row, with no
// prior validation — a NULL/garbage entity_type on that row would panic the whole process via
// unreachable!() if this stayed a hard match. JS's readEntityState has no such guard either: an
// entity_type absent from ENTITY_QUERIES just falls through `if (!sql || ...) return null;` — so
// this mirrors that graceful None, matching JS byte-for-byte instead of crashing on bad staged data.
fn read_entity_state(db: &Connection, entity_type: &str, entity_id: &str) -> Result<Option<Value>, String> {
    // JS: `Number.isInteger(idNum) && String(idNum) === String(entity_id)` — a STRICT numeric
    // test, so '12abc', '1.5' and ' 1' all fall through to the title branch.
    let numeric_id = |s: &str| -> Option<i64> {
        s.parse::<i64>().ok().filter(|n| n.to_string() == s)
    };
    let one = |sql: &str, p: Value| -> Result<Option<Value>, String> {
        let rows = crate::db_json::query_rows(db, sql, &[p])?;
        Ok(rows.as_array().and_then(|a| a.first()).cloned())
    };
    match entity_type {
        "card" => one("SELECT * FROM cards WHERE slug = ?", json!(entity_id)),
        "agent" => one("SELECT * FROM agents WHERE name = ?", json!(entity_id)),
        "action" => one("SELECT * FROM actions WHERE name = ?", json!(entity_id)),
        "law" => one("SELECT * FROM laws.laws WHERE slug = ?", json!(entity_id)),
        "contract" => one("SELECT * FROM contracts WHERE slug = ?", json!(entity_id)),
        "pending_diff" => one(
            "SELECT id, card_slug, file_path, status, decided_at, applied_at FROM pending_diffs WHERE id = ?",
            json!(entity_id),
        ),
        "memory" => match numeric_id(entity_id) {
            Some(n) => one("SELECT * FROM memory.memories WHERE id = ?", json!(n)),
            None => one(
                "SELECT * FROM memory.memories WHERE title = ? ORDER BY id DESC LIMIT 1",
                json!(entity_id),
            ),
        },
        "file" => Ok(read_file_state_in(&project_root(), entity_id)),
        _ => Ok(None),
    }
}

// dispatch_skill — stands in for JS's `importSkill(skill)` (a filename-index lookup that can
// resolve ANY skill on disk) followed by `targetSkill(wrappedInput, context)`. The twin can only
// reach skills that already have a Rust twin; extend this match arm-by-arm as more twins land.
fn dispatch_skill(name: &str, input: &Value, db: &Connection) -> Result<Value, String> {
    // DELEGATE TO THE REGISTRY. This used to be a hand-kept table of seven card skills, with a
    // note saying "real with-audit would importSkill() it — add an arm here as more twins are
    // needed". The registry IS that importSkill: 781 generated arms, the same table action-run
    // dispatches through, so the two lanes cannot disagree about what a skill name resolves to.
    //
    // The hand-kept list was fine while the fixture scope was card-only. It became an outage when
    // the Rust engine turned default: `keeper.memory.add` (and every other audited non-card
    // mutation) reached this function and got "dispatch table has no entry" — a 500 for a skill
    // that was fully ported and sitting in the registry the whole time.
    //
    // The actor/source are the audit row's own, not the caller's: this is the skill being wrapped,
    // and with-audit is what attributes it.
    let ctx = crate::skill_registry::RunCtx { db, actor: "with-audit", source: "with-audit" };
    crate::skill_registry::dispatch(name, input, &ctx).unwrap_or_else(|| {
        Err(format!(
            "with-audit: no skill named '{name}' is registered (the registry has {} arms; a skill \
             with no Rust twin cannot be wrapped)",
            crate::skill_registry::registered_names().len()
        ))
    })
}

// PUBLIC ENTRY (the fixture-dispatched unit): `context.actor` is always its JS default here,
// because the conform-db harness only ever calls the standard lane with bare context={db}
// (conform-db.mjs: `fn(c.args?.[0] ?? null, { db: freezeDb(sqlite) })`) — so this fixture can
// never observe a non-default actor. Real production callers that DO thread a custom actor
// (e.g. a reconciler's `fix(finding, db)` composing withAudit with `actor: 'system:reconciler'`)
// are Rust code calling INTO this crate directly (no dynamic import(), no JSON-only boundary) —
// they call `withAuditAs` below instead, which this function is a thin default-actor wrapper over.
pub fn withAudit(input: &Value, db: &Connection) -> Result<Value, String> {
    withAuditAs(input, db, "unknown")
}

// INTERNAL ENTRY for other twins to compose with a caller-supplied actor (mirrors JS
// `context.actor ?? 'unknown'`, where actor travels as a JS closure variable, not JSON input —
// so it is a Rust fn parameter here, not an `input` field). Same logic as `withAudit`, otherwise.
// pub(crate) (not pub): no JS counterpart — internal API for other twins that thread a real actor
// (reconcilers, pl-session-approve), not public surface the names-ok parity gate should compare
// against with-audit.mjs's exports. Visible crate-wide via crate::.
pub(crate) fn withAuditAs(input: &Value, db: &Connection, actor: &str) -> Result<Value, String> {
    // user_goal_id: JS `context.user_goal_id = null` default. No current caller (fixture or
    // composing twin) threads a non-null value — extend with a parameter if one needs to.
    let user_goal_id: Option<&str> = None;

    let skill = match input.get("skill").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => return Err("with-audit: skill is required".into()),
    };
    let operation = match input.get("operation").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => return Err("with-audit: operation is required".into()),
    };
    let entity_type = match input.get("entity_type").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => return Err("with-audit: entity_type is required".into()),
    };
    // JS: `entity_id == null` (loose) — an empty string or 0 passes; only absent/null fails.
    let entity_id_val = match input.get("entity_id").filter(|v| !v.is_null()) {
        Some(v) => v,
        None => return Err("with-audit: entity_id is required".into()),
    };
    if !ENTITY_TYPES.contains(&entity_type) {
        return Err(format!(
            "with-audit: entity_type must be one of: {} (got '{}')",
            ENTITY_TYPES.join(", "),
            entity_type
        ));
    }

    let wrapped_input = input.get("wrappedInput").cloned().unwrap_or(Value::Null);
    let entity_id_str = js_string(entity_id_val);

    let before = read_entity_state(db, entity_type, &entity_id_str)?;
    let result = dispatch_skill(skill, &wrapped_input, db)?;
    let after = read_entity_state(db, entity_type, &entity_id_str)?;

    // exec_env: only computed for entity_type='contract' (unexercised this round — see module
    // doc comment). JS truthy checks on worktree_path/sandbox_path; empty string is falsy.
    let exec_env: Option<String> = if entity_type == "contract" {
        let src = after.as_ref().or(before.as_ref());
        let worktree = src.and_then(|s| s.get("worktree_path")).and_then(|v| v.as_str()).filter(|s| !s.is_empty());
        let sandbox = src.and_then(|s| s.get("sandbox_path")).and_then(|v| v.as_str()).filter(|s| !s.is_empty());
        Some(match (worktree, sandbox) {
            (Some(w), _) => format!("worktree:{w}"),
            (None, Some(s)) => format!("sandbox:{s}"),
            (None, None) => "live".to_string(),
        })
    } else {
        None
    };

    // Clock: JS `db.prepare("SELECT datetime('now') AS ts").get().ts`. For the RUNTIME cutover this
    // must be REAL time (a frozen audit ts breaks the Merkle chain's freshness + collides every
    // seal). now_ts(db) keeps determinism where it's needed: antiloki_rs's own unit tests (cfg!(test))
    // get the frozen literal (their hash assertions stay valid, no test churn); the conform binary
    // and the production bin take the SQL path — the conform harness rewrites datetime('now') to the
    // frozen instant (so fixtures still match JS), while production gets real wall-clock time.
    let ts = now_ts(db);
    let ts = ts.as_str();

    let before_json = before.as_ref().map(|b| serde_json::to_string(&cap_audit_snapshot(b)).unwrap());
    let after_json = after.as_ref().map(|a| serde_json::to_string(&cap_audit_snapshot(a)).unwrap());
    let payload = cap_audit_payload(&wrapped_input);

    // NOTE (mirrors the JS comment): user_goal_id is intentionally NOT hashed into digital_print
    // — it's correlation metadata (why), not part of what was done.
    let prev_print = get_last_print(db)?;
    let digital_print = compute_digital_print(
        &prev_print,
        ts,
        actor,
        operation,
        entity_type,
        &entity_id_str,
        before_json.as_deref(),
        after_json.as_deref(),
        payload.as_deref(),
    );

    insert_seal(
        db,
        ts,
        actor,
        operation,
        entity_type,
        &entity_id_str,
        before_json.as_deref(),
        after_json.as_deref(),
        payload.as_deref(),
        &prev_print,
        &digital_print,
        user_goal_id,
        exec_env.as_deref(),
        None,
    )?;

    Ok(result)
}

// createAuditPack(tick_id = null) → { tick_id, events: [] }. Pure, no-db. `tick_id` here is
// whatever the fixture passes positionally (JS default only applies when the arg is `undefined`,
// i.e. omitted — an explicit `null` argument stays `null`, same observable result either way).
pub fn createAuditPack(tick_id: &Value) -> Value {
    json!({ "tick_id": tick_id.clone(), "events": [] })
}

// Value → Option<String> for binding into a nullable column via insert_seal's Option<&str>
// params. None passes SQL NULL (JS binds `null`, not the string "null"); Some(s) binds TEXT — for
// an INTEGER-affinity column (tick_id, user_goal_id) SQLite coerces a numeric-looking TEXT bind to
// the same INTEGER storage class a raw numeric bind would produce, so the stored value matches the
// JS lane. Mirrors the playbook's str_qq but keeps None distinct from "" (str_qq collapses both).
fn opt_text(v: Option<&Value>) -> Option<String> {
    match v {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        // node:sqlite binds ANY plain JS number param (regardless of whether the value came from
        // an INTEGER or REAL column read) as SQLite REAL — verified live: reading tick_id back off
        // an INTEGER column gives the JS number 1, and re-binding that same value into a TEXT-
        // affinity column (sacred_timeline.tick_id/user_goal_id, exactly this fn's callers) stores
        // the REAL-to-TEXT coercion "1.0", not "1". A whole-number Value::Number must render with
        // the trailing ".0" to match; a genuinely fractional one already round-trips via to_string().
        Some(n @ Value::Number(_)) => {
            let f = n.as_f64().unwrap_or(0.0);
            if f.is_finite() && f.fract() == 0.0 {
                Some(format!("{f:.1}"))
            } else {
                Some(n.to_string())
            }
        }
        Some(Value::Bool(b)) => Some(b.to_string()),
        Some(o) => Some(o.to_string()),
    }
}

// JS truthy() coercion for a bare Value (not Option) — used for `if (!session_id) ...` guards.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

// EPIC-AUDIT-PACK (#45): flush a buffered pack (see createAuditPack) into sacred_timeline in ONE
// pass, chaining the hash off the CURRENT db head — reuses withAudit's own machinery
// (get_last_print / compute_digital_print / insert_seal). The twin skips the auditTxnChain mutex
// (see the module doc comment): the harness runs one case at a time against a fresh in-memory db,
// so there is no concurrent caller to serialize against. It does NOT skip the transaction itself —
// JS wraps the whole flush in BEGIN IMMEDIATE / COMMIT with ROLLBACK on any failure (with-audit.mjs
// flushAuditPack), and with-audit-pack.test.mjs asserts exactly that: a failure partway through the
// batch must leave NOTHING written and the pack buffer NOT drained (no half-written chain, matching
// the doc comment above JS's export). BEGIN/COMMIT/ROLLBACK here follow the same manual
// execute_batch pattern this crate already uses for a multi-statement all-or-nothing write (see
// heimdall-ledger-rebuild.rs's DELETE+INSERT* body).
// JS: `flushAuditPack(db, pack)`. A falsy/empty pack short-circuits to `{ flushed: 0 }` with no DB
// write (matches JS's `!pack || !Array.isArray(pack.events) || pack.events.length === 0` guard).
// pack.tick_id is shared across every seal in the batch (JS: `tick_id: pack.tick_id` inside the
// loop) — computed once, reused per insert_seal call below.
//
// SIGNATURE NOTE for callers/dispatch: `pack: &mut Value` (not `&Value`) — JS drains the buffer on
// success (`pack.events.length = 0`, with-audit.mjs ~443; tested as "drained — never re-flush" in
// with-audit-pack.test.mjs ~42,70-74) so a caller can safely call flush again without re-sealing
// already-flushed events. An immutable pack could never reproduce that destructive-drain semantic,
// so any dispatch arm that wires this fn must pass a mutable binding through.
pub fn flushAuditPack(db: &Connection, pack: &mut Value) -> Result<Value, String> {
    let events = match pack.get("events").and_then(|v| v.as_array()) {
        Some(e) if !e.is_empty() => e.clone(),
        _ => return Ok(json!({ "flushed": 0 })),
    };
    let tick_id = opt_text(pack.get("tick_id"));

    db.execute_batch("BEGIN IMMEDIATE").map_err(|e| e.to_string())?;
    let body: Result<i64, String> = (|| {
        let mut head = get_last_print(db)?;
        let mut flushed: i64 = 0;
        for item in &events {
            let event = item.get("event").cloned().unwrap_or(Value::Null);
            // JS accesses `event.ts` etc. directly inside computeDigitalPrint/insertSeal with no
            // null guard — a null/undefined event throws TypeError there, which is what triggers
            // the ROLLBACK (with-audit-pack.test.mjs's "a failed flush rolls back..." case sets
            // `pack.events[1] = { event: null, ... }`). Reproduce that failure explicitly instead
            // of silently coercing missing fields to "" and inserting a garbage-but-"successful" seal.
            // JS: `computeDigitalPrint(prevPrint, event)` immediately reads `event.ts` with no
            // guard — for a null event that throws "Cannot read properties of null (reading
            // 'ts')" verbatim (verified live against the real JS). Reproduce that exact message,
            // not a twin-authored description, since the db lane compares thrown text byte-for-byte.
            if event.is_null() {
                return Err("Cannot read properties of null (reading 'ts')".to_string());
            }
            let ts = event.get("ts").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let actor = event.get("actor").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let operation = event.get("operation").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let entity_type = event.get("entity_type").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let entity_id = event.get("entity_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let before_json = event.get("before_json").and_then(|v| v.as_str()).map(str::to_string);
            let after_json = event.get("after_json").and_then(|v| v.as_str()).map(str::to_string);
            let payload = event.get("payload").and_then(|v| v.as_str()).map(str::to_string);
            let user_goal_id = opt_text(item.get("user_goal_id"));
            let exec_env = opt_text(item.get("exec_env"));

            let prev_print = head.clone();
            let digital_print = compute_digital_print(
                &prev_print, &ts, &actor, &operation, &entity_type, &entity_id,
                before_json.as_deref(), after_json.as_deref(), payload.as_deref(),
            );
            insert_seal(
                db, &ts, &actor, &operation, &entity_type, &entity_id,
                before_json.as_deref(), after_json.as_deref(), payload.as_deref(),
                &prev_print, &digital_print, user_goal_id.as_deref(), exec_env.as_deref(), tick_id.as_deref(),
            )?;
            head = digital_print;
            flushed += 1;
        }
        Ok(flushed)
    })();

    match body {
        Ok(flushed) => {
            db.execute_batch("COMMIT").map_err(|e| e.to_string())?;
            // Drain only on success — mirrors JS's `pack.events.length = 0` placed after COMMIT.
            if let Some(arr) = pack.get_mut("events").and_then(|v| v.as_array_mut()) {
                arr.clear();
            }
            Ok(json!({ "flushed": flushed }))
        }
        Err(e) => {
            let _ = db.execute_batch("ROLLBACK");
            Err(e) // pack NOT drained → caller may retry, same as JS
        }
    }
}

// EVENT-SOURCED-AUDIT (Phase 2): seal a session's STAGED rows (timeline.audit_stage) into
// sacred_timeline in seq (id ASC) order, chaining off the fresh head — same machinery as
// flushAuditPack above, and the same transaction requirement: JS wraps the SELECT, every insertSeal,
// and the final DELETE in one BEGIN IMMEDIATE / COMMIT, with ROLLBACK on any failure
// (with-audit.mjs flushAuditStage). JS: `flushAuditStage(db, session_id)`. A falsy session_id (JS
// `if (!session_id) return { flushed: 0 }`) short-circuits with no DB write and no transaction at
// all (matches the early return below, before BEGIN). Zero staged rows still opens+commits an
// (empty, no-op) transaction in JS — mirrored here by returning Ok(0) from the transaction body,
// which the success arm below COMMITs. Rows are drained (DELETEd) only after every seal in the
// batch has been inserted, in the SAME transaction as the inserts — on an error midway the whole
// transaction ROLLBACKs (nothing sealed, nothing deleted), leaving the staged rows untouched for
// retry / boot recovery.
pub fn flushAuditStage(db: &Connection, session_id: &Value) -> Result<Value, String> {
    if !truthy(session_id) {
        return Ok(json!({ "flushed": 0 }));
    }

    db.execute_batch("BEGIN IMMEDIATE").map_err(|e| e.to_string())?;
    let body: Result<i64, String> = (|| {
        let rows = crate::db_json::query_rows(
            db,
            "SELECT * FROM timeline.audit_stage WHERE session_id = ? ORDER BY id ASC",
            &[session_id.clone()],
        )?;
        let rows = rows.as_array().cloned().unwrap_or_default();
        if rows.is_empty() {
            return Ok(0);
        }

        let mut head = get_last_print(db)?;
        for r in &rows {
            let ts = r.get("ts").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let actor = r.get("actor").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let operation = r.get("operation").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let entity_type = r.get("entity_type").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let entity_id = r.get("entity_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let before_json = r.get("before_json").and_then(|v| v.as_str()).map(str::to_string);
            // AI-CALL-OUTSIDE-LOCK crash recovery: a NULL staged after_json means the skill crashed
            // between recording intent and finalizing — re-read the live entity (source of truth),
            // same as the JS `if (after_json === null) { const live = readEntityState(...); ... }`.
            let mut after_json = r.get("after_json").and_then(|v| v.as_str()).map(str::to_string);
            if after_json.is_none() {
                let live = read_entity_state(db, &entity_type, &entity_id)?;
                after_json = live.as_ref().map(|l| serde_json::to_string(&cap_audit_snapshot(l)).unwrap());
            }
            let payload = r.get("payload").and_then(|v| v.as_str()).map(str::to_string);
            let user_goal_id = opt_text(r.get("user_goal_id"));
            let exec_env = opt_text(r.get("exec_env"));
            let tick_id = opt_text(r.get("tick_id"));

            let prev_print = head.clone();
            let digital_print = compute_digital_print(
                &prev_print, &ts, &actor, &operation, &entity_type, &entity_id,
                before_json.as_deref(), after_json.as_deref(), payload.as_deref(),
            );
            insert_seal(
                db, &ts, &actor, &operation, &entity_type, &entity_id,
                before_json.as_deref(), after_json.as_deref(), payload.as_deref(),
                &prev_print, &digital_print, user_goal_id.as_deref(), exec_env.as_deref(), tick_id.as_deref(),
            )?;
            head = digital_print;
        }

        // JS: `const maxId = rows[rows.length - 1].id; DELETE ... WHERE session_id = ? AND id <= ?`.
        let max_id = rows.last().and_then(|r| r.get("id")).cloned().unwrap_or(Value::Null);
        crate::db_json::run_write(
            db,
            "DELETE FROM timeline.audit_stage WHERE session_id = ? AND id <= ?",
            &[session_id.clone(), max_id],
        )?;
        Ok(rows.len() as i64)
    })();

    match body {
        Ok(flushed) => {
            db.execute_batch("COMMIT").map_err(|e| e.to_string())?;
            Ok(json!({ "flushed": flushed }))
        }
        Err(e) => {
            let _ = db.execute_batch("ROLLBACK");
            Err(e) // staged rows NOT drained → boot recovery / retry will re-seal them
        }
    }
}

// EVENT-SOURCED-AUDIT (Phase 2): boot recovery. Seal every session's orphaned staged rows — a
// crash between a mutation and its phase flush leaves the trail in audit_stage. Idempotent: a
// clean shutdown leaves nothing to do. JS: `flushAllStaged(db)` — a degenerate db-first case (no
// extra positional args). The `SELECT DISTINCT` runs inside its own try/catch in JS (a
// pre-migration db missing the audit_stage table) — matched here by treating a failure of THIS
// query as `{ sessions: 0, flushed: 0 }`; a failure raised by flushAuditStage inside the loop is
// NOT swallowed (JS: only the initial `try` wraps the SELECT, the loop's `await` is unguarded).
pub fn flushAllStaged(db: &Connection) -> Result<Value, String> {
    let sessions = match crate::db_json::query_rows(
        db,
        "SELECT DISTINCT session_id FROM timeline.audit_stage WHERE session_id IS NOT NULL",
        &[],
    ) {
        Ok(rows) => rows.as_array().cloned().unwrap_or_default(),
        Err(_) => return Ok(json!({ "sessions": 0, "flushed": 0 })),
    };
    let mut flushed: i64 = 0;
    for row in &sessions {
        let session_id = row.get("session_id").cloned().unwrap_or(Value::Null);
        let r = flushAuditStage(db, &session_id)?;
        flushed += r.get("flushed").and_then(|v| v.as_i64()).unwrap_or(0);
    }
    Ok(json!({ "sessions": sessions.len() as i64, "flushed": flushed }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// read_entity_state over ALL EIGHT entity types.
    ///
    /// This function was card-only, and the seven Err arms read as a harmless scoping note right
    /// up until the Rust engine became the default — at which point every audited mutation to a
    /// memory, law, contract, agent, action, pending_diff or file 500s'd. These tests exist so the
    /// arms cannot quietly regress to that state.
    mod entity_state {
        use super::*;

        fn db_with_entities() -> Connection {
            let c = Connection::open_in_memory().unwrap();
            // memory and laws live in ATTACHed databases; in-memory attachments give the twin the
            // same `memory.memories` / `laws.laws` names it resolves in production.
            c.execute_batch("ATTACH ':memory:' AS memory; ATTACH ':memory:' AS laws;").unwrap();
            c.execute_batch(
                "CREATE TABLE cards (slug TEXT, title TEXT);
                 CREATE TABLE agents (name TEXT, description TEXT);
                 CREATE TABLE actions (name TEXT, owner_agent TEXT);
                 CREATE TABLE contracts (slug TEXT, card_slug TEXT);
                 CREATE TABLE pending_diffs (id INTEGER PRIMARY KEY, card_slug TEXT, file_path TEXT,
                                             status TEXT, decided_at TEXT, applied_at TEXT, diff_body TEXT);
                 CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, title TEXT, body TEXT);
                 CREATE TABLE laws.laws (slug TEXT, title TEXT);
                 INSERT INTO cards VALUES ('C-1','a card');
                 INSERT INTO agents VALUES ('dev','the dev agent');
                 INSERT INTO actions VALUES ('dev.file.write','dev');
                 INSERT INTO contracts VALUES ('CT-1','C-1');
                 INSERT INTO pending_diffs VALUES (7,'C-1','src/x.ts','pending',NULL,NULL,'@@ huge diff @@');
                 INSERT INTO memory.memories VALUES (1,'first','body one'),(2,'dup','older'),(3,'dup','newer');
                 INSERT INTO laws.laws VALUES ('NEVER_X','never do x');",
            )
            .unwrap();
            c
        }

        #[test]
        fn every_entity_type_resolves_its_row() {
            let c = db_with_entities();
            let got = |t: &str, id: &str| read_entity_state(&c, t, id).unwrap();
            assert_eq!(got("card", "C-1").unwrap()["title"], json!("a card"));
            assert_eq!(got("agent", "dev").unwrap()["description"], json!("the dev agent"));
            assert_eq!(got("action", "dev.file.write").unwrap()["owner_agent"], json!("dev"));
            assert_eq!(got("law", "NEVER_X").unwrap()["title"], json!("never do x"));
            assert_eq!(got("contract", "CT-1").unwrap()["card_slug"], json!("C-1"));
        }

        #[test]
        fn a_missing_entity_is_none_not_an_error() {
            // None means "there was no before state" — the legitimate answer for an INSERT, and
            // the reason an `add` can be sealed at all.
            let c = db_with_entities();
            for (t, id) in [("card", "NOPE"), ("agent", "nope"), ("law", "NOPE"), ("memory", "404")] {
                assert!(read_entity_state(&c, t, id).unwrap().is_none(), "{t} should be None");
            }
        }

        #[test]
        fn pending_diff_selects_ONLY_the_named_columns() {
            // Deliberately NOT `SELECT *`: the row carries the diff body, and sealing that into
            // before/after on every mutation is what once grew sacred_timeline to 2.6 GB.
            let c = db_with_entities();
            let row = read_entity_state(&c, "pending_diff", "7").unwrap().unwrap();
            let keys: Vec<&String> = row.as_object().unwrap().keys().collect();
            assert_eq!(keys, ["id", "card_slug", "file_path", "status", "decided_at", "applied_at"]);
            assert!(row.get("diff_body").is_none(), "the diff body must not be sealed");
        }

        #[test]
        fn memory_is_DUAL_KEY_by_id_and_by_title() {
            // memory-add seals by TITLE (no integer id exists until after the insert) while
            // supersede/validate/link-add seal by the integer id. Matching only one of the two is
            // how every memory.add seal came to capture a null before/after.
            let c = db_with_entities();
            assert_eq!(read_entity_state(&c, "memory", "1").unwrap().unwrap()["body"], json!("body one"));
            assert_eq!(read_entity_state(&c, "memory", "first").unwrap().unwrap()["id"], json!(1));
        }

        #[test]
        fn a_repeated_memory_title_resolves_to_the_NEWEST() {
            // Titles are not unique. Most-recent is the right answer for an append-only add seal.
            let c = db_with_entities();
            assert_eq!(read_entity_state(&c, "memory", "dup").unwrap().unwrap()["body"], json!("newer"));
        }

        #[test]
        fn a_non_integer_memory_key_takes_the_TITLE_branch() {
            // JS guards with `String(Number(id)) === String(id)`, so '1.5' and '1abc' are TITLES,
            // not ids — a loose parse would send them down the id branch and seal the wrong row.
            let c = db_with_entities();
            c.execute("INSERT INTO memory.memories VALUES (9, '1.5', 'titled like a number')", []).unwrap();
            assert!(read_entity_state(&c, "memory", "1abc").unwrap().is_none());
            assert_eq!(read_entity_state(&c, "memory", "1.5").unwrap().unwrap()["body"], json!("titled like a number"));
        }

        #[test]
        fn file_seals_a_content_DIGEST_and_a_missing_file_is_none() {
            let dir = std::env::temp_dir().join(format!("antiloki-audit-file-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("seal-me.txt"), b"hello").unwrap();
            let got = read_file_state_in(&dir, "seal-me.txt").unwrap();
            // sha256("hello") — a published constant, so a wrong implementation cannot pass by
            // agreeing with itself.
            assert_eq!(got["sha256"], json!("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"));
            assert_eq!(got["bytes"], json!(5));
            assert_eq!(got["path"], json!("seal-me.txt"));
            assert!(read_file_state_in(&dir, "no-such-file.txt").is_none());
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn an_unknown_entity_type_is_none_rather_than_a_panic() {
            // flushAuditStage's crash-recovery path reads entity_type straight off a staged row
            // with no validation, so garbage must degrade, not crash the process.
            let c = db_with_entities();
            assert!(read_entity_state(&c, "not_a_type", "x").unwrap().is_none());
        }
    }

    fn seeded_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               slug TEXT UNIQUE, title TEXT, body_json TEXT, parent_slug TEXT,
               status TEXT, display_order REAL, created_at TEXT, updated_at TEXT
             );
             CREATE TABLE sacred_timeline (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               ts TEXT, actor TEXT, operation TEXT, entity_type TEXT, entity_id TEXT,
               before_json TEXT, after_json TEXT, payload TEXT,
               prev_print TEXT, digital_print TEXT,
               user_goal_id TEXT, exec_env TEXT, tick_id TEXT
             );
             INSERT INTO cards (id, slug, title, body_json, parent_slug, status, display_order, created_at, updated_at)
               VALUES (1, 'A', 'a', NULL, NULL, 'backlog', NULL, '2025-12-01 00:00:00', '2025-12-01 00:00:00');",
        )
        .unwrap();
        c
    }

    #[test]
    fn dispatch_and_seal_round_trip_from_genesis() {
        let c = seeded_db();
        let input = json!({
            "skill": "card-status-change",
            "wrappedInput": { "slug": "A", "status": "doing" },
            "operation": "card.status.change",
            "entity_type": "card",
            "entity_id": "A",
        });
        let result = withAudit(&input, &c).unwrap();
        assert_eq!(result["status"], json!("doing"));

        let tl = crate::db_json::query_rows(&c, "SELECT * FROM sacred_timeline", &[]).unwrap();
        let rows = tl.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row["prev_print"], json!("GENESIS"));
        assert_eq!(row["ts"], json!("2026-01-01 00:00:00"));
        assert_eq!(row["actor"], json!("unknown"));
        assert_eq!(row["entity_id"], json!("A"));
        let dp = row["digital_print"].as_str().unwrap();
        assert_eq!(dp.len(), 64);
        assert!(dp.chars().all(|ch| ch.is_ascii_hexdigit()));

        // hand-verify the hash math independently of compute_digital_print itself.
        let before_json = row["before_json"].as_str().unwrap();
        let after_json = row["after_json"].as_str().unwrap();
        let payload = row["payload"].as_str().unwrap();
        assert!(before_json.contains("\"status\":\"backlog\""));
        assert!(after_json.contains("\"status\":\"doing\""));
        let content = ["GENESIS", "2026-01-01 00:00:00", "unknown", "card.status.change", "card", "A", before_json, after_json, payload].join("|");
        let expected = hex::encode(Sha256::digest(content.as_bytes()));
        assert_eq!(dp, expected);
    }

    #[test]
    fn chains_off_an_existing_head() {
        let c = seeded_db();
        c.execute(
            "INSERT INTO sacred_timeline (ts, actor, operation, entity_type, entity_id, before_json, after_json, payload, prev_print, digital_print) \
             VALUES ('2025-12-01 00:00:00', 'seed', 'card.add', 'card', 'A', NULL, '{}', NULL, 'GENESIS', 'seedhash')",
            [],
        )
        .unwrap();
        let input = json!({
            "skill": "card-update",
            "wrappedInput": { "slug": "A", "title": "new title" },
            "operation": "card.update",
            "entity_type": "card",
            "entity_id": "A",
        });
        withAudit(&input, &c).unwrap();
        let tl = crate::db_json::query_rows(&c, "SELECT * FROM sacred_timeline ORDER BY id", &[]).unwrap();
        let rows = tl.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1]["prev_print"], json!("seedhash"));
    }

    #[test]
    fn card_reorder_dispatch_seals_display_order_change() {
        let c = seeded_db();
        let input = json!({
            "skill": "card-reorder",
            "wrappedInput": { "slug": "A", "position": { "absolute": 42.5 } },
            "operation": "card.reorder",
            "entity_type": "card",
            "entity_id": "A",
        });
        let result = withAudit(&input, &c).unwrap();
        assert_eq!(result["display_order"], json!(42.5));
        let tl = crate::db_json::query_rows(&c, "SELECT after_json FROM sacred_timeline", &[]).unwrap();
        let after_json = tl[0]["after_json"].as_str().unwrap();
        assert!(after_json.contains("\"display_order\":42.5"));
    }

    #[test]
    fn rejects_unknown_entity_type_before_dispatch() {
        let c = seeded_db();
        let input = json!({ "skill": "card-status-change", "wrappedInput": {}, "operation": "x", "entity_type": "martian", "entity_id": "A" });
        let err = withAudit(&input, &c).unwrap_err();
        assert!(err.contains("entity_type must be one of: card, agent, action, memory, law, file, pending_diff, contract (got 'martian')"), "{err}");
    }

    #[test]
    fn rejects_missing_required_fields() {
        let c = seeded_db();
        assert!(withAudit(&json!({ "operation": "x", "entity_type": "card", "entity_id": "A" }), &c).unwrap_err().contains("skill is required"));
        assert!(withAudit(&json!({ "skill": "card-status-change", "entity_type": "card", "entity_id": "A" }), &c).unwrap_err().contains("operation is required"));
        assert!(withAudit(&json!({ "skill": "card-status-change", "operation": "x", "entity_id": "A" }), &c).unwrap_err().contains("entity_type is required"));
        assert!(withAudit(&json!({ "skill": "card-status-change", "operation": "x", "entity_type": "card" }), &c).unwrap_err().contains("entity_id is required"));
    }

    #[test]
    fn an_unregistered_skill_is_a_loud_error() {
        // A skill name with no Rust twin cannot be wrapped, and saying so beats sealing an audit
        // row for something that never ran.
        let c = seeded_db();
        let input = json!({ "skill": "not-a-converted-skill", "wrappedInput": {}, "operation": "x", "entity_type": "card", "entity_id": "A" });
        let err = withAudit(&input, &c).unwrap_err();
        assert!(err.contains("no skill named 'not-a-converted-skill' is registered"), "{err}");
    }

    #[test]
    fn with_audit_can_wrap_ANY_registered_skill_not_a_hand_kept_list() {
        // The regression this guards: dispatch_skill was seven hand-written card arms, so every
        // audited NON-card mutation 500'd with "dispatch table has no entry" even though the skill
        // was ported and sitting in the registry. Delegating to the registry is what real
        // with-audit's importSkill() does.
        assert!(crate::skill_registry::is_registered("memory-add"), "memory-add must be reachable");
        assert!(crate::skill_registry::is_registered("card-update"), "the original card arms still resolve");
        assert!(crate::skill_registry::registered_names().len() > 700);
    }

    #[test]
    fn create_audit_pack_shape() {
        assert_eq!(createAuditPack(&Value::Null), json!({ "tick_id": null, "events": [] }));
        assert_eq!(createAuditPack(&json!(7)), json!({ "tick_id": 7, "events": [] }));
    }

    fn seeded_db_with_stage() -> Connection {
        let c = seeded_db();
        c.execute_batch(
            "ATTACH ':memory:' AS timeline;
             CREATE TABLE timeline.audit_stage (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               session_id TEXT, tick_id TEXT, phase TEXT, ts TEXT, actor TEXT, operation TEXT,
               entity_type TEXT, entity_id TEXT, before_json TEXT, after_json TEXT, payload TEXT,
               exec_env TEXT, user_goal_id TEXT
             );",
        )
        .unwrap();
        c
    }

    #[test]
    fn flush_audit_pack_chains_and_reports_count() {
        let c = seeded_db();
        let mut pack = json!({
            "tick_id": 5,
            "events": [
                { "event": { "ts": "2026-01-01 00:00:00", "actor": "driver", "operation": "card.update", "entity_type": "card", "entity_id": "A", "before_json": "{\"a\":1}", "after_json": "{\"a\":2}", "payload": null }, "user_goal_id": null, "exec_env": null },
                { "event": { "ts": "2026-01-01 00:00:01", "actor": "driver", "operation": "card.update", "entity_type": "card", "entity_id": "A", "before_json": "{\"a\":2}", "after_json": "{\"a\":3}", "payload": null }, "user_goal_id": 9, "exec_env": "live" },
            ],
        });
        let result = flushAuditPack(&c, &mut pack).unwrap();
        assert_eq!(result, json!({ "flushed": 2 }));
        // drained on success — mirrors with-audit-pack.test.mjs "drained — never re-flush".
        assert_eq!(pack["events"].as_array().unwrap().len(), 0);

        let tl = crate::db_json::query_rows(&c, "SELECT * FROM sacred_timeline ORDER BY id", &[]).unwrap();
        let rows = tl.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["prev_print"], json!("GENESIS"));
        // node:sqlite binds a plain JS number param as SQLite REAL regardless of its source
        // column's affinity — the JS lane's real re-bind of a raw numeric tick_id/user_goal_id
        // into this TEXT column stores "5.0"/"9.0" (verified live), not "5"/"9" — see opt_text.
        assert_eq!(rows[0]["tick_id"], json!("5.0"));
        assert_eq!(rows[1]["prev_print"], rows[0]["digital_print"]);
        assert_eq!(rows[1]["user_goal_id"], json!("9.0"));
        assert_eq!(rows[1]["exec_env"], json!("live"));

        // re-flushing the drained pack is a no-op — never re-seals the same events.
        assert_eq!(flushAuditPack(&c, &mut pack).unwrap(), json!({ "flushed": 0 }));
        let tl2 = crate::db_json::query_rows(&c, "SELECT * FROM sacred_timeline", &[]).unwrap();
        assert_eq!(tl2.as_array().unwrap().len(), 2);
    }

    #[test]
    fn flush_audit_pack_empty_is_a_noop() {
        let c = seeded_db();
        assert_eq!(flushAuditPack(&c, &mut json!({ "tick_id": null, "events": [] })).unwrap(), json!({ "flushed": 0 }));
        assert_eq!(flushAuditPack(&c, &mut Value::Null).unwrap(), json!({ "flushed": 0 }));
        let tl = crate::db_json::query_rows(&c, "SELECT * FROM sacred_timeline", &[]).unwrap();
        assert_eq!(tl.as_array().unwrap().len(), 0);
    }

    #[test]
    fn flush_audit_pack_null_event_rolls_back_the_whole_batch_and_leaves_pack_undrained() {
        // Mirrors with-audit-pack.test.mjs "a failed flush rolls back and leaves the buffer
        // intact for retry (no half-written chain)": a valid first event, a malformed (null)
        // second event — nothing must land in sacred_timeline and the pack must keep both events.
        let c = seeded_db();
        let mut pack = json!({
            "tick_id": null,
            "events": [
                { "event": { "ts": "2026-01-01 00:00:00", "actor": "driver", "operation": "card.update", "entity_type": "card", "entity_id": "A", "before_json": "{\"a\":1}", "after_json": "{\"a\":2}", "payload": null }, "user_goal_id": null, "exec_env": null },
                { "event": null, "user_goal_id": null, "exec_env": null },
            ],
        });
        let err = flushAuditPack(&c, &mut pack).unwrap_err();
        // Exact V8 message (verified live): computeDigitalPrint/insertSeal read `event.ts` with
        // no guard, so a null event throws this TypeError verbatim — not a twin-authored summary.
        assert_eq!(err, "Cannot read properties of null (reading 'ts')");

        let tl = crate::db_json::query_rows(&c, "SELECT * FROM sacred_timeline", &[]).unwrap();
        assert_eq!(tl.as_array().unwrap().len(), 0, "the first (valid) event must NOT have been permanently committed");
        assert_eq!(pack["events"].as_array().unwrap().len(), 2, "pack must stay undrained for retry");
    }

    #[test]
    fn flush_audit_stage_seals_in_seq_order_and_drains_only_its_session() {
        let c = seeded_db_with_stage();
        c.execute_batch(
            "INSERT INTO timeline.audit_stage (session_id, tick_id, ts, actor, operation, entity_type, entity_id, before_json, after_json, payload)
               VALUES ('s1', '3', '2026-01-01 00:00:00', 'driver', 'card.update', 'card', 'A', '{\"a\":1}', '{\"a\":2}', NULL);
             INSERT INTO timeline.audit_stage (session_id, tick_id, ts, actor, operation, entity_type, entity_id, before_json, after_json, payload)
               VALUES ('s1', '3', '2026-01-01 00:00:01', 'driver', 'card.update', 'card', 'A', '{\"a\":2}', '{\"a\":3}', NULL);
             INSERT INTO timeline.audit_stage (session_id, tick_id, ts, actor, operation, entity_type, entity_id, before_json, after_json, payload)
               VALUES ('s2', NULL, '2026-01-01 00:00:02', 'driver', 'card.update', 'card', 'A', '{\"a\":9}', '{\"a\":10}', NULL);",
        )
        .unwrap();

        let result = flushAuditStage(&c, &json!("s1")).unwrap();
        assert_eq!(result, json!({ "flushed": 2 }));

        let tl = crate::db_json::query_rows(&c, "SELECT * FROM sacred_timeline ORDER BY id", &[]).unwrap();
        let rows = tl.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["prev_print"], json!("GENESIS"));
        assert_eq!(rows[1]["prev_print"], rows[0]["digital_print"]);

        let remaining = crate::db_json::query_rows(&c, "SELECT session_id FROM timeline.audit_stage", &[]).unwrap();
        let remaining = remaining.as_array().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0]["session_id"], json!("s2"));
    }

    #[test]
    fn flush_audit_stage_falsy_session_id_is_a_noop() {
        let c = seeded_db_with_stage();
        assert_eq!(flushAuditStage(&c, &Value::Null).unwrap(), json!({ "flushed": 0 }));
        assert_eq!(flushAuditStage(&c, &json!("")).unwrap(), json!({ "flushed": 0 }));
    }

    #[test]
    fn flush_audit_stage_recovers_null_after_json_from_live_entity() {
        let c = seeded_db_with_stage();
        c.execute(
            "UPDATE cards SET status = 'doing' WHERE slug = 'A'",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO timeline.audit_stage (session_id, ts, actor, operation, entity_type, entity_id, before_json, after_json, payload)
               VALUES ('s1', '2026-01-01 00:00:00', 'driver', 'card.status.change', 'card', 'A', '{\"status\":\"backlog\"}', NULL, NULL)",
            [],
        )
        .unwrap();

        flushAuditStage(&c, &json!("s1")).unwrap();
        let tl = crate::db_json::query_rows(&c, "SELECT after_json FROM sacred_timeline", &[]).unwrap();
        let after_json = tl[0]["after_json"].as_str().unwrap();
        assert!(after_json.contains("\"status\":\"doing\""), "{after_json}");
    }

    #[test]
    fn flush_audit_stage_mid_batch_failure_rolls_back_and_leaves_staged_rows_for_retry() {
        // Mirrors flushAuditPack's rollback guarantee (issue 4): a valid first row followed by a
        // row whose recovery path FAILS must roll back the WHOLE batch — nothing sealed, nothing
        // deleted from audit_stage — rather than skipping the bad row and sealing the good one.
        //
        // The failure is induced by staging an `agent` row against a database with no `agents`
        // table, so the recovery re-read errors. This test used to induce it via
        // read_entity_state's "not ported this round" arm, back when entity_type='agent' had no
        // query at all; that arm is gone now that all eight types are ported, but the property
        // being guarded — all-or-nothing — is unchanged and is what matters here.
        let c = seeded_db_with_stage();
        c.execute_batch(
            "INSERT INTO timeline.audit_stage (session_id, ts, actor, operation, entity_type, entity_id, before_json, after_json, payload)
               VALUES ('s1', '2026-01-01 00:00:00', 'driver', 'card.update', 'card', 'A', '{\"a\":1}', '{\"a\":2}', NULL);
             INSERT INTO timeline.audit_stage (session_id, ts, actor, operation, entity_type, entity_id, before_json, after_json, payload)
               VALUES ('s1', '2026-01-01 00:00:01', 'driver', 'agent.update', 'agent', 'X', '{}', NULL, NULL);",
        )
        .unwrap();

        let err = flushAuditStage(&c, &json!("s1")).unwrap_err();
        assert!(err.contains("no such table: agents"), "{err}");

        let tl = crate::db_json::query_rows(&c, "SELECT * FROM sacred_timeline", &[]).unwrap();
        assert_eq!(tl.as_array().unwrap().len(), 0, "the first (valid) row must NOT have been permanently sealed");
        let remaining = crate::db_json::query_rows(&c, "SELECT * FROM timeline.audit_stage WHERE session_id = 's1'", &[]).unwrap();
        assert_eq!(remaining.as_array().unwrap().len(), 2, "staged rows must remain undeleted for retry");
    }

    #[test]
    fn flush_audit_stage_unrecognized_entity_type_in_recovery_path_is_graceful_not_a_panic() {
        // Issue 5: a staged row with after_json IS NULL triggers the live-entity re-read via
        // read_entity_state, using entity_type read straight off the DB row with NO prior
        // ENTITY_TYPES validation. A NULL/garbage entity_type here must NOT panic the process
        // (the old `_ => unreachable!()` arm) — it must behave like JS's readEntityState, which
        // returns null for any entity_type outside ENTITY_QUERIES.
        let c = seeded_db_with_stage();
        c.execute(
            "INSERT INTO timeline.audit_stage (session_id, ts, actor, operation, entity_type, entity_id, before_json, after_json, payload)
               VALUES ('s1', '2026-01-01 00:00:00', 'driver', 'weird.op', 'martian', 'Z', NULL, NULL, NULL)",
            [],
        )
        .unwrap();

        let result = flushAuditStage(&c, &json!("s1")).unwrap(); // must not panic
        assert_eq!(result, json!({ "flushed": 1 }));
        let tl = crate::db_json::query_rows(&c, "SELECT after_json FROM sacred_timeline", &[]).unwrap();
        assert!(tl[0]["after_json"].is_null());
    }

    #[test]
    fn flush_all_staged_sweeps_every_session_and_drains_the_buffer() {
        let c = seeded_db_with_stage();
        c.execute_batch(
            "INSERT INTO timeline.audit_stage (session_id, ts, actor, operation, entity_type, entity_id, before_json, after_json, payload)
               VALUES ('s1', '2026-01-01 00:00:00', 'driver', 'card.update', 'card', 'A', NULL, '{\"a\":1}', NULL);
             INSERT INTO timeline.audit_stage (session_id, ts, actor, operation, entity_type, entity_id, before_json, after_json, payload)
               VALUES ('s2', '2026-01-01 00:00:01', 'driver', 'card.update', 'card', 'A', NULL, '{\"a\":2}', NULL);",
        )
        .unwrap();

        let result = flushAllStaged(&c).unwrap();
        assert_eq!(result, json!({ "sessions": 2, "flushed": 2 }));

        let remaining = crate::db_json::query_rows(&c, "SELECT * FROM timeline.audit_stage", &[]).unwrap();
        assert_eq!(remaining.as_array().unwrap().len(), 0);
        let tl = crate::db_json::query_rows(&c, "SELECT * FROM sacred_timeline", &[]).unwrap();
        assert_eq!(tl.as_array().unwrap().len(), 2);
    }

    #[test]
    fn flush_all_staged_swallows_a_missing_audit_stage_table() {
        let c = seeded_db(); // no `timeline` ATTACH at all
        assert_eq!(flushAllStaged(&c).unwrap(), json!({ "sessions": 0, "flushed": 0 }));
    }
}
