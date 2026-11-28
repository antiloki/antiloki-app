//! driver-snippet-gate.rs — twin of skills/system/driver-snippet-gate.mjs. NAMES ARE
//! CONTRACT: `snippetGateOn` (named, pure fs read), `authoredSig` (named, db-first, no fs),
//! `approvedSig` (named, pure fs read), `driverSnippetGate` (default, mode-dispatched).
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested (whole file, not just the fs-touching
//! units): the JS module doc says it plainly — "Mirrors the driver-halt flag pattern (a
//! file under .antiloki/run/)" — same family as the already-converted `driver-halt.rs` /
//! `god-driver-halt.rs` / `driver-halt-check.rs` / `reconcilers/stale-pid-files.rs`, all of
//! which read/write a REAL file under `RUN_DIR` (roots.mjs: `HOME_DIR`-anchored, NOT
//! DB-derived — not fixture-controllable per the crossing playbook). Here the fs coupling
//! goes further: almost every `driverSnippetGate` mode branch touches BOTH the real
//! filesystem (`GATE_FLAG` / `APPROVE_DIR/<safeSlug(slug)>`) AND the db in the same call
//! (`approve` writes an approval file keyed by a db-computed signature; `detail`/`pending`
//! compare a db-computed signature against an fs-read one; `edit` mutates the db then clears
//! an fs marker) — there is no clean sub-mode that is DB-only and fixture-safe, so the whole
//! default export is native-tested, matching the sibling precedent. `authoredSig` itself
//! happens to touch no fs at all (pure db read + string hashing) but is kept in this same
//! bespoke-native unit for cohesion with its 3 siblings, which it's inseparable from at the
//! call-site level (`driverSnippetGate` calls it inline throughout). Proven via `#[cfg(test)]`
//! Rust-native tests using `ANTILOKI_HOME` env-var injection + a real tempdir, copying the
//! `with_home()` pattern from `god-driver-resume.rs` exactly.
//!
//! HOME_DIR resolution mirrors every sibling in this family (no `dirs`-crate dependency):
//! `ANTILOKI_HOME` env override (plain path join, no cwd-resolve — callers never pass a
//! relative value in practice) else Node `os.homedir()` semantics (POSIX `$HOME`; Windows
//! `%USERPROFILE%` falling back to `%HOMEDRIVE%%HOMEPATH%`). roots.mjs: `HOME_DIR =
//! resolve($ANTILOKI_HOME) if set+truthy else join(homedir(), '.antiloki')`; `RUN_DIR =
//! join(HOME_DIR, 'run')`; `GATE_FLAG = join(RUN_DIR, 'snippet-gate')`; `APPROVE_DIR =
//! join(RUN_DIR, 'snippet-approved')`.
//!
//! `new Date().toISOString()` (the gate-flag content) and every executed `datetime('now')`
//! (swarm_blocks.authored_at / contracts.updated_at on the `edit` writes) are pinned to the
//! frozen-clock literals per the playbook's frozen-clock lane, even though this unit is not
//! itself fixture-compared — deterministic output is still correct behavior on its own terms.
//!
//! `context.db` missing → JS throws `err.contextMissing('driver-snippet-gate', 'db')` for
//! every mode except `get`/`set`. Not modeled: Rust's `db: &Connection` is a required,
//! non-nullable reference, so this guard is unreachable here — same "guard-throws are not
//! exercised" convention used by every sibling twin (see `stack-facts.rs`).
//!
//! `hashStr` (djb2, unexported JS helper) is ported with i64-widened intermediate arithmetic
//! so the `| 0` (ToInt32 truncation) of a JS bitwise op is exact instead of panicking Rust's
//! checked `+`/`<<` on i32 overflow; `(h >>> 0).toString(36)` is a plain unsigned-32 → base36
//! render. Cross-checked against real `node` output for 4 inputs in the test module below.
//!
//! `safeSlug` (unexported JS helper: `String(s || '').replace(/[^A-Za-z0-9_.-]/g,
//! '_').slice(0,120)`) is implemented directly over a Rust `&str` (every real call site here
//! passes an already-validated non-empty JS string) rather than porting the general `s || ''`
//! coercion — a deliberate simplification proper for a native-tested-only unit; the ASCII
//! character-class swap needs no `regex` dependency.
//!
//! `driverSnippetGate`'s `contract_slug`/`block_id` input fields are echoed back into several
//! outputs VERBATIM (JS never coerces them) — modeled by carrying the original `&Value`
//! through untouched for the echo, while deriving a `String` (via the playbook's `str_qq` /
//! `js_string`) only where the JS itself coerces (SQL binds, `safeSlug`, `String(code)`).
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::PathBuf;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } }

#[cfg(windows)]
fn homedir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("USERPROFILE") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let drive = std::env::var("HOMEDRIVE").ok().filter(|s| !s.is_empty());
    let path = std::env::var("HOMEPATH").ok().filter(|s| !s.is_empty());
    match (drive, path) {
        (Some(d), Some(p)) => Some(PathBuf::from(format!("{d}{p}"))),
        _ => None,
    }
}

#[cfg(not(windows))]
fn homedir() -> Option<PathBuf> {
    std::env::var("HOME").ok().filter(|s| !s.is_empty()).map(PathBuf::from)
}

// roots.mjs: HOME_DIR = resolve($ANTILOKI_HOME) if set+truthy, else join(homedir(), '.antiloki');
// RUN_DIR = join(HOME_DIR, 'run'); GATE_FLAG = join(RUN_DIR, 'snippet-gate');
// APPROVE_DIR = join(RUN_DIR, 'snippet-approved').
fn run_dir() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("run")
}
fn gate_flag_path() -> PathBuf { run_dir().join("snippet-gate") }
fn approve_dir_path() -> PathBuf { run_dir().join("snippet-approved") }

// = JS `function hashStr(s) { let h = 5381; const str = String(s ?? ''); for (...) h = ((h
// << 5) + h + str.charCodeAt(i)) | 0; return (h >>> 0).toString(36); }` — djb2. `h << 5` is a
// plain 32-bit bit-shift (Rust `<<` on i32 truncates high bits exactly like JS's Int32 shift,
// no panic); the surrounding `+ h + charCode` is widened to i64 so it can't hit Rust's
// checked-overflow panic on `+`, then truncated back to i32 via `as i32` — the same
// truncation `| 0` (ToInt32) performs on a value that (as here) fits safely within f64's
// exact-integer range.
fn hash_str(s: &str) -> String {
    let mut h: i32 = 5381;
    for u in s.encode_utf16() {
        let shifted = h << 5;
        let sum: i64 = (shifted as i64) + (h as i64) + (u as i64);
        h = sum as i32;
    }
    to_base36(h as u32)
}
fn to_base36(mut n: u32) -> String {
    if n == 0 {
        return "0".to_string();
    }
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut buf = Vec::new();
    while n > 0 {
        buf.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    buf.reverse();
    String::from_utf8(buf).unwrap()
}

// = JS `const safeSlug = (s) => String(s || '').replace(/[^A-Za-z0-9_.-]/g, '_').slice(0,
// 120);` specialized to a Rust `&str` input (see module doc: every real call site here
// already has a validated non-empty JS string).
fn safe_slug(s: &str) -> String {
    let replaced: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' { c } else { '_' })
        .collect();
    replaced.chars().take(120).collect()
}

/// `export function snippetGateOn()` — pure fs existence check (JS: `try { return
/// existsSync(GATE_FLAG); } catch { return false; } }`; `Path::exists()` never throws in
/// Rust either, so the catch is unreachable here, same as its JS counterpart in practice).
pub fn snippetGateOn() -> bool {
    gate_flag_path().exists()
}

/// `export function authoredSig(db, contract)` — the authored-set signature: hash of the
/// microtasks_json when every microtask has ≥1 patch; else the latest 'composition' review id
/// (swarm path); else a hash of the (possibly empty) microtasks_json. No fs at all.
pub fn authoredSig(db: &Connection, contract: &Value) -> String {
    if !truthy(Some(contract)) {
        return String::new();
    }
    let raw = contract.get("microtasks_json");
    let parse_src = match nz(raw) {
        Some(v) => str_qq(Some(v)),
        None => "[]".to_string(),
    };
    let defs: Vec<Value> = match serde_json::from_str::<Value>(&parse_src) {
        Ok(Value::Array(a)) => a,
        _ => Vec::new(),
    };
    let fully_authored = !defs.is_empty()
        && defs.iter().all(|m| m.get("patches").and_then(Value::as_array).map_or(false, |p| !p.is_empty()));
    if fully_authored {
        return hash_str(&str_qq(raw));
    }
    let slug = contract.get("slug").and_then(Value::as_str).unwrap_or("");
    if let Ok(rows) = crate::db_json::query_rows(
        db,
        "SELECT id FROM contract_qa_reviews WHERE contract_slug = ? AND review_mode = 'composition' ORDER BY id DESC LIMIT 1",
        &[json!(slug)],
    ) {
        if let Some(row) = rows.as_array().and_then(|a| a.first()) {
            if let Some(id) = row.get("id") {
                return format!("swarm:{}", tmpl(Some(id)));
            }
        }
    }
    hash_str(&str_qq(raw))
}

/// `export function approvedSig(slug)` — the currently-approved signature (or `null`), read
/// from `APPROVE_DIR/<safeSlug(slug)>`. Empty/whitespace-only content also collapses to
/// `None`, mirroring JS `(content || '').trim() || null`.
pub fn approvedSig(slug: &str) -> Option<String> {
    let path = approve_dir_path().join(safe_slug(slug));
    match std::fs::read_to_string(&path) {
        Ok(content) => {
            let trimmed = content.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Err(_) => None,
    }
}

fn set_approved(slug: &str, sig: &str) -> Result<(), String> {
    let dir = approve_dir_path();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(safe_slug(slug)), sig).map_err(|e| e.to_string())?;
    Ok(())
}
fn clear_approved(slug: &str) {
    // JS: try { unlinkSync(...) } catch { /* already gone */ }
    let _ = std::fs::remove_file(approve_dir_path().join(safe_slug(slug)));
}

// = JS `const authoredDefs = (c) => { try { const d = JSON.parse(c?.microtasks_json ?? '[]');
// return Array.isArray(d) ? d : []; } catch { return []; } };`
fn authored_defs(c: &Value) -> Vec<Value> {
    let raw = c.get("microtasks_json");
    let src = match nz(raw) {
        Some(v) => str_qq(Some(v)),
        None => "[]".to_string(),
    };
    match serde_json::from_str::<Value>(&src) {
        Ok(Value::Array(a)) => a,
        _ => Vec::new(),
    }
}
// = JS `const allAuthored = (defs) => defs.length > 0 && defs.every((m) =>
// Array.isArray(m?.patches) && m.patches.length > 0);`
fn all_authored(defs: &[Value]) -> bool {
    !defs.is_empty() && defs.iter().all(|m| m.get("patches").and_then(Value::as_array).map_or(false, |p| !p.is_empty()))
}
// = JS `function swarmPassed(db, slug) { try { return !!db.prepare(...).get(slug); } catch {
// return false; } }`
fn swarm_passed(db: &Connection, slug: &str) -> bool {
    match crate::db_json::query_rows(
        db,
        "SELECT 1 FROM contract_qa_reviews WHERE contract_slug = ? AND review_mode = 'composition' AND passed = 1 LIMIT 1",
        &[json!(slug)],
    ) {
        Ok(rows) => rows.as_array().map_or(false, |a| !a.is_empty()),
        Err(_) => false,
    }
}
// = JS `function swarmBlocks(db, slug) { ... rows ordered id DESC, keep first-seen (=
// most-recent) per block_id, map to a synthetic microtask ... }`
fn swarm_blocks(db: &Connection, slug: &str) -> Vec<Value> {
    let rows = match crate::db_json::query_rows(
        db,
        "SELECT block_id, file, content FROM swarm_blocks WHERE contract_slug = ? ORDER BY id DESC",
        &[json!(slug)],
    ) {
        Ok(Value::Array(a)) => a,
        _ => return Vec::new(),
    };
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for b in rows {
        let block_id = b.get("block_id").cloned().unwrap_or(Value::Null);
        let key = block_id.as_str().unwrap_or("").to_string();
        if seen.contains(&key) {
            continue;
        }
        seen.insert(key);
        let file = b.get("file").cloned().unwrap_or(Value::Null);
        let code_value = nz(b.get("content")).cloned().unwrap_or(json!(""));

        let mut patch = Map::new();
        patch.insert("op".into(), json!("block"));
        patch.insert("code".into(), code_value);
        patch.insert("block_id".into(), block_id.clone());

        let mut obj = Map::new();
        obj.insert("microtask_id".into(), block_id);
        obj.insert("file".into(), file);
        obj.insert("file_spec_md".into(), Value::Null);
        obj.insert("swarm".into(), json!(true));
        obj.insert("patches".into(), json!([Value::Object(patch)]));
        out.push(Value::Object(obj));
    }
    out
}

// `detail` mode's per-microtask projection: `{ microtask_id: m.microtask_id ?? null, file:
// m.file ?? null, file_spec_md: m.file_spec_md ?? null, patches: (Array.isArray(m.patches) ?
// m.patches : []).map((p) => ({ op: p.op ?? null, code: p.code ?? '', anchor_before:
// p.anchor_before ?? '', anchor_after: p.anchor_after ?? '' })) }`
fn detail_microtasks_from_defs(defs: &[Value]) -> Vec<Value> {
    defs.iter()
        .map(|m| {
            let microtask_id = nz(m.get("microtask_id")).cloned().unwrap_or(Value::Null);
            let file = nz(m.get("file")).cloned().unwrap_or(Value::Null);
            let file_spec_md = nz(m.get("file_spec_md")).cloned().unwrap_or(Value::Null);
            let patches_in = m.get("patches").and_then(Value::as_array).cloned().unwrap_or_default();
            let patches: Vec<Value> = patches_in
                .iter()
                .map(|p| {
                    let op = nz(p.get("op")).cloned().unwrap_or(Value::Null);
                    let code = nz(p.get("code")).cloned().unwrap_or(json!(""));
                    let anchor_before = nz(p.get("anchor_before")).cloned().unwrap_or(json!(""));
                    let anchor_after = nz(p.get("anchor_after")).cloned().unwrap_or(json!(""));
                    let mut po = Map::new();
                    po.insert("op".into(), op);
                    po.insert("code".into(), code);
                    po.insert("anchor_before".into(), anchor_before);
                    po.insert("anchor_after".into(), anchor_after);
                    Value::Object(po)
                })
                .collect();
            let mut mo = Map::new();
            mo.insert("microtask_id".into(), microtask_id);
            mo.insert("file".into(), file);
            mo.insert("file_spec_md".into(), file_spec_md);
            mo.insert("patches".into(), json!(patches));
            Value::Object(mo)
        })
        .collect()
}

/// `export default function driverSnippetGate(input, context)`. `context.db` is modeled as a
/// required `&Connection` (see module doc — the `!db` guard-throw is unreachable in Rust).
pub fn driverSnippetGate(input: &Value, db: &Connection) -> Result<Value, String> {
    let mode_value = nz(input.get("mode")).cloned().unwrap_or(json!("get"));
    let mode = mode_value.as_str();

    if mode == Some("get") {
        return Ok(json!({ "ok": true, "enabled": snippetGateOn() }));
    }
    if mode == Some("set") {
        let enabled = truthy(input.get("enabled"));
        if enabled {
            std::fs::create_dir_all(run_dir()).map_err(|e| e.to_string())?;
            std::fs::write(gate_flag_path(), "2026-01-01T00:00:00.000Z").map_err(|e| e.to_string())?;
        } else if gate_flag_path().exists() {
            let _ = std::fs::remove_file(gate_flag_path()); // JS: try/catch swallows a race
        }
        return Ok(json!({ "ok": true, "enabled": enabled }));
    }

    if mode == Some("approve") {
        let slug_v = input.get("contract_slug");
        if !truthy(slug_v) {
            return Ok(json!({ "ok": false, "error": "need contract_slug" }));
        }
        let slug_out = slug_v.cloned().unwrap();
        let slug = str_qq(slug_v);
        let rows = crate::db_json::query_rows(db, "SELECT slug, microtasks_json FROM contracts WHERE slug = ?", &[json!(slug)])?;
        let c = match rows.as_array().and_then(|a| a.first()) {
            Some(row) => row.clone(),
            None => return Ok(json!({ "ok": false, "error": format!("contract '{}' not found", tmpl(slug_v)) })),
        };
        let sig = authoredSig(db, &c);
        set_approved(&slug, &sig)?;
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("approved".into(), json!(true));
        out.insert("contract_slug".into(), slug_out);
        out.insert("sig".into(), json!(sig));
        return Ok(Value::Object(out));
    }
    if mode == Some("reject") {
        let slug_v = input.get("contract_slug");
        if !truthy(slug_v) {
            return Ok(json!({ "ok": false, "error": "need contract_slug" }));
        }
        let slug_out = slug_v.cloned().unwrap();
        let slug = str_qq(slug_v);
        clear_approved(&slug);
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("rejected".into(), json!(true));
        out.insert("contract_slug".into(), slug_out);
        out.insert(
            "note".into(),
            json!("approval cleared — the card stays gated (re-author routing is a later increment)"),
        );
        return Ok(Value::Object(out));
    }
    if mode == Some("detail") {
        let slug_v = input.get("contract_slug");
        if !truthy(slug_v) {
            return Ok(json!({ "ok": false, "error": "need contract_slug" }));
        }
        let slug_out = slug_v.cloned().unwrap();
        let slug = str_qq(slug_v);
        let rows = crate::db_json::query_rows(db, "SELECT slug, card_slug, microtasks_json FROM contracts WHERE slug = ?", &[json!(slug)])?;
        let c = match rows.as_array().and_then(|a| a.first()) {
            Some(row) => row.clone(),
            None => return Ok(json!({ "ok": false, "error": format!("contract '{}' not found", tmpl(slug_v)) })),
        };
        let defs = authored_defs(&c);
        let mut microtasks = detail_microtasks_from_defs(&defs);
        if microtasks.is_empty() {
            microtasks = swarm_blocks(db, &slug); // swarm path: the blocks ARE the snippets
        }
        let card_slug = c.get("card_slug").cloned().unwrap_or(Value::Null);
        let sig_authored = authoredSig(db, &c);
        let sig_approved = approvedSig(&slug);
        let approved = sig_approved.as_deref() == Some(sig_authored.as_str());
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("contract_slug".into(), slug_out);
        out.insert("card_slug".into(), card_slug);
        out.insert("microtasks".into(), json!(microtasks));
        out.insert("approved".into(), json!(approved));
        return Ok(Value::Object(out));
    }
    if mode == Some("edit") {
        let slug_v = input.get("contract_slug");
        let code_v = input.get("code");
        let code_is_nullish = code_v.map_or(true, |v| v.is_null());
        if !truthy(slug_v) || code_is_nullish {
            return Ok(json!({
                "ok": false,
                "error": "need { contract_slug, code, + microtask_index/patch_index OR block_id }"
            }));
        }
        let slug_out = slug_v.cloned().unwrap();
        let slug = str_qq(slug_v);
        let code_v = code_v.unwrap();

        let block_id_v = input.get("block_id");
        if truthy(block_id_v) {
            let block_id_v = block_id_v.unwrap();
            let content_str = js_string(code_v);
            let block_id_str = js_string(block_id_v);
            let (changes, _) = crate::db_json::run_write(
                db,
                "UPDATE swarm_blocks SET content = ?, authored_at = datetime('2026-01-01 00:00:00') WHERE contract_slug = ? AND block_id = ?",
                &[json!(content_str), json!(slug), json!(block_id_str)],
            )?;
            if changes == 0 {
                return Ok(json!({ "ok": false, "error": "no swarm block with that id for this contract" }));
            }
            clear_approved(&slug);
            let mut out = Map::new();
            out.insert("ok".into(), json!(true));
            out.insert("edited".into(), json!(true));
            out.insert("swarm".into(), json!(true));
            out.insert("contract_slug".into(), slug_out);
            out.insert("block_id".into(), block_id_v.clone());
            return Ok(Value::Object(out));
        }

        let mi_f = js_number(input.get("microtask_index"));
        let pi_f = js_number(input.get("patch_index"));
        let mi_is_int = mi_f.is_finite() && mi_f.fract() == 0.0;
        let pi_is_int = pi_f.is_finite() && pi_f.fract() == 0.0;
        if !mi_is_int || !pi_is_int {
            return Ok(json!({ "ok": false, "error": "need microtask_index + patch_index (or block_id for swarm)" }));
        }

        let rows = crate::db_json::query_rows(db, "SELECT slug, microtasks_json FROM contracts WHERE slug = ?", &[json!(slug)])?;
        let c = match rows.as_array().and_then(|a| a.first()) {
            Some(row) => row.clone(),
            None => return Ok(json!({ "ok": false, "error": format!("contract '{}' not found", tmpl(slug_v)) })),
        };
        let raw = c.get("microtasks_json");
        let parse_src = match nz(raw) { Some(v) => str_qq(Some(v)), None => "[]".to_string() };
        let parsed: Value = match serde_json::from_str(&parse_src) {
            Ok(v) => v,
            Err(_) => return Ok(json!({ "ok": false, "error": "microtasks_json is not valid JSON" })),
        };
        let mut defs_arr: Vec<Value> = match parsed { Value::Array(a) => a, _ => Vec::new() };

        let mi_idx = mi_f as i64;
        let pi_idx = pi_f as i64;
        let valid = mi_idx >= 0
            && (mi_idx as usize) < defs_arr.len()
            && defs_arr[mi_idx as usize]
                .get("patches")
                .and_then(Value::as_array)
                .map_or(false, |patches| pi_idx >= 0 && (pi_idx as usize) < patches.len());
        if !valid {
            return Ok(json!({ "ok": false, "error": "no patch at that microtask/patch index" }));
        }

        {
            let m = defs_arr.get_mut(mi_idx as usize).unwrap();
            let patches = m.get_mut("patches").and_then(Value::as_array_mut).unwrap();
            if let Value::Object(po) = patches.get_mut(pi_idx as usize).unwrap() {
                po.insert("code".into(), json!(js_string(code_v)));
            }
        }
        let new_json = serde_json::to_string(&Value::Array(defs_arr)).map_err(|e| e.to_string())?;
        crate::db_json::run_write(
            db,
            "UPDATE contracts SET microtasks_json = ?, updated_at = datetime('2026-01-01 00:00:00') WHERE slug = ?",
            &[json!(new_json), json!(slug)],
        )?;
        clear_approved(&slug);
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("edited".into(), json!(true));
        out.insert("contract_slug".into(), slug_out);
        out.insert("microtask_index".into(), json!(mi_idx));
        out.insert("patch_index".into(), json!(pi_idx));
        return Ok(Value::Object(out));
    }
    if mode == Some("pending") {
        let rows = crate::db_json::query_rows(db, "SELECT slug, card_slug, status, microtasks_json FROM contracts WHERE status = 'pl_ratified'", &[])
            .unwrap_or(json!([]));
        let rows_arr = rows.as_array().cloned().unwrap_or_default();
        let mut pending: Vec<Value> = Vec::new();
        for c in &rows_arr {
            let defs = authored_defs(c);
            let micro = all_authored(&defs);
            let slug = c.get("slug").and_then(Value::as_str).unwrap_or("").to_string();
            let swarm = !micro && swarm_passed(db, &slug);
            if !micro && !swarm {
                continue;
            }
            let sig_authored = authoredSig(db, c);
            let sig_approved = approvedSig(&slug);
            if sig_approved.as_deref() == Some(sig_authored.as_str()) {
                continue;
            }
            let microtask_count: i64 = if micro { defs.len() as i64 } else { swarm_blocks(db, &slug).len() as i64 };
            let mut po = Map::new();
            po.insert("contract_slug".into(), c.get("slug").cloned().unwrap_or(Value::Null));
            po.insert("card_slug".into(), c.get("card_slug").cloned().unwrap_or(Value::Null));
            po.insert("microtask_count".into(), json!(microtask_count));
            po.insert("swarm".into(), json!(swarm));
            pending.push(Value::Object(po));
        }
        return Ok(json!({ "ok": true, "enabled": snippetGateOn(), "pending": pending }));
    }

    Ok(json!({ "ok": false, "error": "mode must be get | set | pending | approve | reject" }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // SAFETY: ANTILOKI_HOME is process-global; serialize since `cargo test` runs threads in
    // parallel by default, and this same var is read by other converted twins' tests too.
    // shared crate-wide lock (test_env_lock.rs) — a local mutex here would not prevent a race
    // with OTHER files' tests over the same real env var.
    fn with_home<T>(antiloki_home: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_HOME", antiloki_home);
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_HOME");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-driver-snippet-gate-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (slug TEXT, card_slug TEXT, status TEXT, microtasks_json TEXT, updated_at TEXT); \
             CREATE TABLE contract_qa_reviews (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, review_mode TEXT, passed INTEGER); \
             CREATE TABLE swarm_blocks (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, block_id TEXT, file TEXT, content TEXT, authored_at TEXT);",
        )
        .unwrap();
        c
    }
    fn insert_contract(c: &Connection, slug: &str, card_slug: &str, status: &str, microtasks_json: &str) {
        c.execute(
            "INSERT INTO contracts (slug, card_slug, status, microtasks_json, updated_at) VALUES (?1, ?2, ?3, ?4, '2026-01-01 00:00:00')",
            rusqlite::params![slug, card_slug, status, microtasks_json],
        )
        .unwrap();
    }
    fn authored_microtasks() -> &'static str {
        r#"[{"microtask_id":"m1","file":"a.js","patches":[{"op":"insert","code":"x"}]}]"#
    }

    #[test]
    fn hash_str_matches_node_djb2_reference_values() {
        // node: hashStr(...) for these exact inputs (see PR: computed via `node --input-type=module -e`).
        assert_eq!(hash_str("[{\"microtask_id\":\"m1\",\"patches\":[{\"code\":\"x\"}]}]"), "1fa8k1x");
        assert_eq!(hash_str(""), "45h");
        assert_eq!(hash_str("hello world"), "eslcxt");
        assert_eq!(hash_str("[]"), "3hnx9");
    }

    #[test]
    fn snippet_gate_on_reflects_the_flag_file() {
        let dir = temp_dir("gate-on");
        with_home(dir.to_str().unwrap(), || {
            assert!(!snippetGateOn());
            fs::create_dir_all(dir.join("run")).unwrap();
            fs::write(dir.join("run").join("snippet-gate"), "x").unwrap();
            assert!(snippetGateOn());
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mode_get_and_set_toggle_the_gate_flag() {
        let dir = temp_dir("get-set");
        with_home(dir.to_str().unwrap(), || {
            let c = db();
            assert_eq!(driverSnippetGate(&json!({}), &c).unwrap(), json!({ "ok": true, "enabled": false }));
            assert_eq!(
                driverSnippetGate(&json!({ "mode": "set", "enabled": true }), &c).unwrap(),
                json!({ "ok": true, "enabled": true })
            );
            assert_eq!(driverSnippetGate(&json!({ "mode": "get" }), &c).unwrap(), json!({ "ok": true, "enabled": true }));
            assert_eq!(
                driverSnippetGate(&json!({ "mode": "set", "enabled": false }), &c).unwrap(),
                json!({ "ok": true, "enabled": false })
            );
            assert_eq!(driverSnippetGate(&json!({ "mode": "get" }), &c).unwrap(), json!({ "ok": true, "enabled": false }));
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_mode_returns_the_enum_error() {
        let dir = temp_dir("unknown-mode");
        with_home(dir.to_str().unwrap(), || {
            let c = db();
            assert_eq!(
                driverSnippetGate(&json!({ "mode": "bogus" }), &c).unwrap(),
                json!({ "ok": false, "error": "mode must be get | set | pending | approve | reject" })
            );
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn authored_sig_hashes_fully_authored_defs_and_falls_back_to_swarm_then_empty_hash() {
        let dir = temp_dir("authored-sig");
        with_home(dir.to_str().unwrap(), || {
            let c = db();
            let contract1 = json!({ "slug": "CT-1", "microtasks_json": authored_microtasks() });
            let sig1 = authoredSig(&c, &contract1);
            assert_eq!(sig1, hash_str(authored_microtasks()));

            c.execute(
                "INSERT INTO contract_qa_reviews (contract_slug, review_mode, passed) VALUES ('CT-2','composition',1)",
                [],
            )
            .unwrap();
            let id: i64 = c.query_row("SELECT id FROM contract_qa_reviews WHERE contract_slug='CT-2'", [], |r| r.get(0)).unwrap();
            let contract2 = json!({ "slug": "CT-2", "microtasks_json": "[]" });
            assert_eq!(authoredSig(&c, &contract2), format!("swarm:{id}"));

            let contract3 = json!({ "slug": "CT-3", "microtasks_json": "[]" });
            assert_eq!(authoredSig(&c, &contract3), hash_str("[]"));

            assert_eq!(authoredSig(&c, &Value::Null), "");
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn approved_sig_null_when_absent_or_whitespace_trimmed_when_present() {
        let dir = temp_dir("approved-sig");
        with_home(dir.to_str().unwrap(), || {
            assert_eq!(approvedSig("NOPE"), None);
            let approve_dir = dir.join("run").join("snippet-approved");
            fs::create_dir_all(&approve_dir).unwrap();
            fs::write(approve_dir.join("WS"), "   \n").unwrap();
            assert_eq!(approvedSig("WS"), None);
            fs::write(approve_dir.join("WS"), "  abc123  \n").unwrap();
            assert_eq!(approvedSig("WS"), Some("abc123".to_string()));
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mode_approve_then_reject_round_trips_the_approval_marker() {
        let dir = temp_dir("approve-reject");
        with_home(dir.to_str().unwrap(), || {
            let c = db();
            insert_contract(&c, "CT-A", "CARD-A", "pl_ratified", authored_microtasks());
            assert_eq!(
                driverSnippetGate(&json!({ "mode": "approve" }), &c).unwrap(),
                json!({ "ok": false, "error": "need contract_slug" })
            );
            assert_eq!(
                driverSnippetGate(&json!({ "mode": "approve", "contract_slug": "GHOST" }), &c).unwrap(),
                json!({ "ok": false, "error": "contract 'GHOST' not found" })
            );
            let expected_sig = hash_str(authored_microtasks());
            let r = driverSnippetGate(&json!({ "mode": "approve", "contract_slug": "CT-A" }), &c).unwrap();
            assert_eq!(r, json!({ "ok": true, "approved": true, "contract_slug": "CT-A", "sig": expected_sig }));
            assert_eq!(approvedSig("CT-A"), Some(expected_sig));

            let rj = driverSnippetGate(&json!({ "mode": "reject", "contract_slug": "CT-A" }), &c).unwrap();
            assert_eq!(
                rj,
                json!({ "ok": true, "rejected": true, "contract_slug": "CT-A",
                    "note": "approval cleared — the card stays gated (re-author routing is a later increment)" })
            );
            assert_eq!(approvedSig("CT-A"), None);
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mode_detail_fills_defaults_and_falls_back_to_swarm_blocks() {
        let dir = temp_dir("detail");
        with_home(dir.to_str().unwrap(), || {
            let c = db();
            insert_contract(&c, "CT-D", "CARD-D", "pl_ratified", authored_microtasks());
            assert_eq!(
                driverSnippetGate(&json!({ "mode": "detail", "contract_slug": "GHOST" }), &c).unwrap(),
                json!({ "ok": false, "error": "contract 'GHOST' not found" })
            );
            let r = driverSnippetGate(&json!({ "mode": "detail", "contract_slug": "CT-D" }), &c).unwrap();
            assert_eq!(r["ok"], json!(true));
            assert_eq!(r["card_slug"], json!("CARD-D"));
            assert_eq!(
                r["microtasks"],
                json!([{
                    "microtask_id": "m1", "file": "a.js", "file_spec_md": null,
                    "patches": [{ "op": "insert", "code": "x", "anchor_before": "", "anchor_after": "" }],
                }])
            );
            assert_eq!(r["approved"], json!(false));

            insert_contract(&c, "CT-S", "CARD-S", "pl_ratified", "[]");
            c.execute("INSERT INTO swarm_blocks (contract_slug, block_id, file, content) VALUES ('CT-S','b1','f1.js','old')", []).unwrap();
            c.execute("INSERT INTO swarm_blocks (contract_slug, block_id, file, content) VALUES ('CT-S','b1','f1.js','new')", []).unwrap();
            c.execute("INSERT INTO swarm_blocks (contract_slug, block_id, file, content) VALUES ('CT-S','b2','f2.js',NULL)", []).unwrap();
            let rs = driverSnippetGate(&json!({ "mode": "detail", "contract_slug": "CT-S" }), &c).unwrap();
            assert_eq!(
                rs["microtasks"],
                json!([
                    { "microtask_id": "b2", "file": "f2.js", "file_spec_md": null, "swarm": true,
                      "patches": [{ "op": "block", "code": "", "block_id": "b2" }] },
                    { "microtask_id": "b1", "file": "f1.js", "file_spec_md": null, "swarm": true,
                      "patches": [{ "op": "block", "code": "new", "block_id": "b1" }] },
                ])
            );
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mode_edit_microtask_patch_index_path_mutates_code_and_clears_approval() {
        let dir = temp_dir("edit-index");
        with_home(dir.to_str().unwrap(), || {
            let c = db();
            insert_contract(&c, "CT-E", "CARD-E", "pl_ratified", authored_microtasks());
            assert_eq!(
                driverSnippetGate(&json!({ "mode": "edit", "contract_slug": "CT-E" }), &c).unwrap(),
                json!({ "ok": false, "error": "need { contract_slug, code, + microtask_index/patch_index OR block_id }" })
            );
            assert_eq!(
                driverSnippetGate(&json!({ "mode": "edit", "contract_slug": "CT-E", "code": "y", "microtask_index": 1.5, "patch_index": 0 }), &c).unwrap(),
                json!({ "ok": false, "error": "need microtask_index + patch_index (or block_id for swarm)" })
            );
            assert_eq!(
                driverSnippetGate(&json!({ "mode": "edit", "contract_slug": "CT-E", "code": "y", "microtask_index": 9, "patch_index": 0 }), &c).unwrap(),
                json!({ "ok": false, "error": "no patch at that microtask/patch index" })
            );
            let okr = driverSnippetGate(&json!({ "mode": "edit", "contract_slug": "CT-E", "code": "y", "microtask_index": 0, "patch_index": 0 }), &c).unwrap();
            assert_eq!(okr, json!({ "ok": true, "edited": true, "contract_slug": "CT-E", "microtask_index": 0, "patch_index": 0 }));
            let after: String = c.query_row("SELECT microtasks_json FROM contracts WHERE slug='CT-E'", [], |r| r.get(0)).unwrap();
            let parsed: Value = serde_json::from_str(&after).unwrap();
            assert_eq!(parsed[0]["patches"][0]["code"], json!("y"));
            assert_eq!(approvedSig("CT-E"), None);

            insert_contract(&c, "CT-BAD", "CARD-BAD", "pl_ratified", "not json");
            assert_eq!(
                driverSnippetGate(&json!({ "mode": "edit", "contract_slug": "CT-BAD", "code": "y", "microtask_index": 0, "patch_index": 0 }), &c).unwrap(),
                json!({ "ok": false, "error": "microtasks_json is not valid JSON" })
            );
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mode_edit_block_id_path_updates_swarm_blocks_content() {
        let dir = temp_dir("edit-block");
        with_home(dir.to_str().unwrap(), || {
            let c = db();
            insert_contract(&c, "CT-S2", "CARD-S2", "pl_ratified", "[]");
            c.execute("INSERT INTO swarm_blocks (contract_slug, block_id, file, content) VALUES ('CT-S2','b1','f.js','old')", []).unwrap();
            let r = driverSnippetGate(&json!({ "mode": "edit", "contract_slug": "CT-S2", "code": "new-code", "block_id": "b1" }), &c).unwrap();
            assert_eq!(r, json!({ "ok": true, "edited": true, "swarm": true, "contract_slug": "CT-S2", "block_id": "b1" }));
            let content: String = c
                .query_row("SELECT content FROM swarm_blocks WHERE contract_slug='CT-S2' AND block_id='b1'", [], |r| r.get(0))
                .unwrap();
            assert_eq!(content, "new-code");

            let miss = driverSnippetGate(&json!({ "mode": "edit", "contract_slug": "CT-S2", "code": "x", "block_id": "ghost" }), &c).unwrap();
            assert_eq!(miss, json!({ "ok": false, "error": "no swarm block with that id for this contract" }));
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mode_pending_lists_unapproved_authored_or_swarm_passed_contracts_only() {
        let dir = temp_dir("pending");
        with_home(dir.to_str().unwrap(), || {
            let c = db();
            insert_contract(&c, "CT-P1", "CARD-P1", "pl_ratified", authored_microtasks());
            insert_contract(&c, "CT-P2", "CARD-P2", "pl_ratified", authored_microtasks());
            driverSnippetGate(&json!({ "mode": "approve", "contract_slug": "CT-P2" }), &c).unwrap();
            insert_contract(&c, "CT-P3", "CARD-P3", "pl_ratified", "[]");
            insert_contract(&c, "CT-P4", "CARD-P4", "proposed", authored_microtasks());
            insert_contract(&c, "CT-P5", "CARD-P5", "pl_ratified", "[]");
            c.execute("INSERT INTO contract_qa_reviews (contract_slug, review_mode, passed) VALUES ('CT-P5','composition',1)", []).unwrap();
            c.execute("INSERT INTO swarm_blocks (contract_slug, block_id, file, content) VALUES ('CT-P5','b1','f.js','c')", []).unwrap();
            c.execute("INSERT INTO swarm_blocks (contract_slug, block_id, file, content) VALUES ('CT-P5','b2','f2.js','c')", []).unwrap();

            let r = driverSnippetGate(&json!({ "mode": "pending" }), &c).unwrap();
            assert_eq!(r["ok"], json!(true));
            let mut slugs: Vec<String> = r["pending"].as_array().unwrap().iter().map(|p| p["contract_slug"].as_str().unwrap().to_string()).collect();
            slugs.sort();
            assert_eq!(slugs, vec!["CT-P1".to_string(), "CT-P5".to_string()]);
            let p1 = r["pending"].as_array().unwrap().iter().find(|p| p["contract_slug"] == json!("CT-P1")).unwrap();
            assert_eq!(p1, &json!({ "contract_slug": "CT-P1", "card_slug": "CARD-P1", "microtask_count": 1, "swarm": false }));
            let p5 = r["pending"].as_array().unwrap().iter().find(|p| p["contract_slug"] == json!("CT-P5")).unwrap();
            assert_eq!(p5, &json!({ "contract_slug": "CT-P5", "card_slug": "CARD-P5", "microtask_count": 2, "swarm": true }));
        });
        let _ = fs::remove_dir_all(&dir);
    }
}
