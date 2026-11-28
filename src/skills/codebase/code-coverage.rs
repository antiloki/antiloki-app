//! code-coverage.rs — twin of skills/codebase/code-coverage.mjs (db lane, real filesystem read).
//! NAMES ARE CONTRACT: `codeCoverage` (default). OWNED-SURFACE RUNTIME: parses the workspace's own
//! `coverage/lcov.info` (or sibling candidate paths) into a per-file `%`/hit/total heatmap, lowest
//! coverage first, plus an optional single-file `line_hits` map for the editor gutter.
//!
//! REAL FILESYSTEM (constitution-estimate.rs precedent): `root` is DB-derived via the
//! already-converted `engine_workspace_root::getWorkspaceRoot` twin (or an `input.root` override),
//! so the fixture seeds an active `workspaces` row pointing at a real on-disk testdata dir
//! (`antiloki-crossing/fixtures/testdata/code-coverage/`, committed alongside this unit's fixture
//! JSON) and both lanes read the identical `coverage/lcov.info` bytes. Candidate-path existence
//! checks (`existsSync`/`Path::exists`) and the actual `readFileSync`/`read_to_string` use Rust's
//! native path joining (not a hand-ported `node:path.join`) — those paths are ONLY used for real OS
//! file I/O, never returned in the output, so byte-exact `node:path` string semantics don't matter
//! there (unlike the `SF:` relative-path case below, which IS returned).
//!
//! `SF:` ABSOLUTE-PATH BRANCH — win32 `relative()` ported, DISCLOSED PARTIALLY UNTESTED: when an
//! `SF:` line is absolute (or merely drive-colon-prefixed, `/^[A-Za-z]:/`, matching the JS regex
//! even for a drive-RELATIVE path with no separator after the colon), the JS re-bases it via
//! `node:path.relative(root, p)`. This twin ports Node's actual win32 `relative()` algorithm
//! (`win_relative`, verified byte-for-byte against real `node:path.win32.relative` output across
//! same-prefix, cross-drive, and parent-traversal cases via direct `node -e` ground-truth checks —
//! see conversion notes) rather than reusing `workspace-root.rs`'s simplified containment-check
//! (that twin only needed a boolean, not the relative STRING). The committed fixture's `lcov.info`
//! uses relative `SF:` paths only (matching the upstream JS test's own convention), so this
//! branch — while implemented for real, not stubbed — carries no fixture coverage. The narrower
//! drive-RELATIVE sub-case (`C:tmp\x`, matches the regex but not `isAbsolute`) falls through to
//! `win_normalize` without a `process.cwd()` join (Node's `resolve()` would use the process's
//! current drive-relative directory there); real lcov tool output never emits such paths, so this
//! is an accepted, disclosed gap, not a modeled behavior.
//!
//! `spawnSync(cmd, { shell: true, ... })` BRANCH — REAL, DISCLOSED UNTESTED (playbook's
//! flag-skippable pattern, `constitution-estimate.rs`'s git-diff precedent): reachable only when
//! `input.run === true` AND a truthy `input.cmd` are BOTH given — the fixture only exercises the
//! default (`run` absent/false) safe path. `run_coverage_cmd` shells out for real via
//! `cmd /C <cmd>` (mirroring Node's `shell:true` on this Windows-only substrate — same win32-only
//! scope as `workspace-root.rs`), but Node's `timeout`/`maxBuffer` spawn options have no direct
//! `std::process::Command` equivalent without extra threading — not implemented, since this branch
//! carries no fixture comparison either way.
//!
//! Integer-keyed `line_hits` object: V8 enumerates array-index-like string keys (all lcov line
//! numbers are positive integers) in ASCENDING NUMERIC order regardless of insertion order — NOT
//! JS object insertion order. This twin collects into a `BTreeMap<i64, f64>` (auto-sorted) and
//! serializes it in that order to match, rather than JS's literal `DA:` line insertion sequence.
//!
//! `context.db` missing → JS never guards this explicitly (destructures `context ?? {}`, but every
//! db-lane call site here provides a real `context.db`); `db: &Connection` stays a required,
//! non-nullable reference like every sibling twin.
// QUALITY: 75/100 — real fs read + real win32 relative() port (verified vs real Node output),
// one disclosed-untested subprocess branch (flag-skippable), one disclosed-untested absolute-SF
// sub-branch (no fixture coverage, fixture uses relative SF: paths like the JS test itself)

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } } // = JS String(v)

// JS `Math.round`: floor(x + 0.5) — rounds HALF UP, unlike Rust's half-away-from-zero `.round()`.
fn js_round(x: f64) -> f64 { (x + 0.5).floor() }
// `Math.round(x * 1000) / 10` — the pct-rounding idiom used for both per-file and total_pct.
fn round1(x: f64) -> f64 { js_round(x * 1000.0) / 10.0 }
// JS truthy check on a bare number: 0/-0/NaN falsy, everything else (incl. Infinity) truthy.
fn truthy_num(v: f64) -> bool { v != 0.0 && !v.is_nan() }

// `Number(str)` for a raw (already-trimmed-by-caller-or-not) string slice — trims, empty -> 0,
// else strict numeric parse -> NaN on failure. lcov DA/LF/LH fields are always plain decimal ints
// in practice; this covers that case exactly (Rust's f64 FromStr additionally accepts a few
// spellings JS's Number() would reject, e.g. lowercase "inf" — an accepted, disclosed gap for
// input this module never actually receives).
fn js_number_from_str(s: &str) -> f64 {
    let t = s.trim();
    if t.is_empty() { return 0.0; }
    match t {
        "Infinity" | "+Infinity" => f64::INFINITY,
        "-Infinity" => f64::NEG_INFINITY,
        _ => t.parse::<f64>().unwrap_or(f64::NAN),
    }
}

// Renders a computed f64 the way `JSON.stringify` would render the equivalent JS number:
// whole-valued -> bare integer (gotcha #2, e.g. `pct: 0` not `0.0`), non-finite -> null (matches
// `JSON.stringify(NaN)` / `JSON.stringify(Infinity)` both serializing as `null`).
fn js_num_to_json(f: f64) -> Value {
    if !f.is_finite() { Value::Null }
    else if f.fract() == 0.0 { json!(f as i64) }
    else { json!(f) }
}

fn js_slice0n(s: &str, n: usize) -> String { s.chars().take(n).collect() }
fn js_slice_neg(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    let start = chars.len().saturating_sub(n);
    chars[start..].iter().collect()
}

// ── win32 path port (drive-letter roots only — see module doc; same scope as workspace-root.rs
//    and constitution-estimate.rs's private per-file copies) ────────────────────────────────────
fn is_win_sep(c: u8) -> bool { c == b'/' || c == b'\\' }

fn drive_colon_prefix(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

fn win_is_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    if b.is_empty() { return false; }
    if is_win_sep(b[0]) { return true; }
    if b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && is_win_sep(b[2]) { return true; }
    false
}

// Same `normalizeString` algorithm as workspace-root.rs's private copy (drive-letter roots only).
fn normalize_string_win(path: &[u8], allow_above_root: bool) -> String {
    let mut res: Vec<u8> = Vec::new();
    let mut last_segment_length: isize = 0;
    let mut last_slash: isize = -1;
    let mut dots: isize = 0;
    let mut ch: u8 = 0;
    let n = path.len() as isize;
    let mut i: isize = 0;
    while i <= n {
        if i < n {
            ch = path[i as usize];
        } else if is_win_sep(ch) {
            break;
        } else {
            ch = b'\\';
        }
        if is_win_sep(ch) {
            if last_slash == i - 1 || dots == 1 {
                // NOOP
            } else if dots == 2 {
                if res.len() < 2 || last_segment_length != 2
                    || res[res.len() - 1] != b'.' || res[res.len() - 2] != b'.'
                {
                    if res.len() > 2 {
                        match res.iter().rposition(|&c| is_win_sep(c)) {
                            Some(lsi) => {
                                res.truncate(lsi);
                                let new_last = res.iter().rposition(|&c| is_win_sep(c)).map(|x| x as isize).unwrap_or(-1);
                                last_segment_length = res.len() as isize - 1 - new_last;
                            }
                            None => { res.clear(); last_segment_length = 0; }
                        }
                        last_slash = i;
                        dots = 0;
                        i += 1;
                        continue;
                    } else if !res.is_empty() {
                        res.clear();
                        last_segment_length = 0;
                        last_slash = i;
                        dots = 0;
                        i += 1;
                        continue;
                    }
                }
                if allow_above_root {
                    if !res.is_empty() { res.push(b'\\'); res.push(b'.'); res.push(b'.'); }
                    else { res.push(b'.'); res.push(b'.'); }
                    last_segment_length = 2;
                }
            } else {
                let seg = &path[(last_slash + 1) as usize..i as usize];
                if !res.is_empty() { res.push(b'\\'); res.extend_from_slice(seg); }
                else { res.extend_from_slice(seg); }
                last_segment_length = i - last_slash - 1;
            }
            last_slash = i;
            dots = 0;
        } else if ch == b'.' && dots != -1 {
            dots += 1;
        } else {
            dots = -1;
        }
        i += 1;
    }
    String::from_utf8(res).unwrap_or_default()
}

/// node:path win32.normalize, drive-letter roots only (no UNC).
fn win_normalize(path: &str) -> String {
    let bytes = path.as_bytes();
    let len = bytes.len();
    if len == 0 { return ".".into(); }
    if len == 1 {
        return if is_win_sep(bytes[0]) { "\\".into() } else { path.to_string() };
    }
    let mut root_end: usize = 0;
    let mut device: Option<String> = None;
    let mut is_absolute = false;
    if is_win_sep(bytes[0]) {
        is_absolute = true;
        root_end = 1;
    } else if bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        device = Some(String::from_utf8(bytes[0..2].to_vec()).unwrap_or_default());
        root_end = 2;
        if len > 2 && is_win_sep(bytes[2]) {
            is_absolute = true;
            root_end = 3;
        }
    }
    let mut tail = if root_end < len { normalize_string_win(&bytes[root_end..], !is_absolute) } else { String::new() };
    if tail.is_empty() && !is_absolute { tail = ".".into(); }
    if !tail.is_empty() && is_win_sep(bytes[len - 1]) { tail.push('\\'); }
    match device {
        None => if is_absolute { if !tail.is_empty() { format!("\\{tail}") } else { "\\".into() } } else { tail },
        Some(d) => if is_absolute { if !tail.is_empty() { format!("{d}\\{tail}") } else { format!("{d}\\") } } else { format!("{d}{tail}") },
    }
}

/// node:path.win32.relative(from, to) — ported from Node's actual algorithm (see module doc for
/// verification method). Assumes ASCII input (fixtures stay ASCII/BMP per playbook gotcha #5) so
/// byte offsets computed against the lowercased copy stay valid against the case-preserved
/// original (ASCII lowercasing never changes byte length or position).
fn win_relative(from_raw: &str, to_raw: &str) -> String {
    if from_raw == to_raw { return String::new(); }
    let from_orig = win_normalize(from_raw);
    let to_orig = win_normalize(to_raw);
    if from_orig == to_orig { return String::new(); }
    let from_lc = from_orig.to_ascii_lowercase();
    let to_lc = to_orig.to_ascii_lowercase();
    if from_lc == to_lc { return String::new(); }
    let from_b = from_lc.as_bytes();
    let to_b = to_lc.as_bytes();

    let mut from_start = 0usize;
    while from_start < from_b.len() && from_b[from_start] == b'\\' { from_start += 1; }
    let mut from_end = from_b.len();
    while from_end > from_start + 1 && from_b[from_end - 1] == b'\\' { from_end -= 1; }
    let from_len = from_end - from_start;

    let mut to_start = 0usize;
    while to_start < to_b.len() && to_b[to_start] == b'\\' { to_start += 1; }
    let mut to_end = to_b.len();
    while to_end > to_start + 1 && to_b[to_end - 1] == b'\\' { to_end -= 1; }
    let to_len = to_end - to_start;

    let length = from_len.min(to_len);
    let mut last_common_sep: i64 = -1;
    let mut i: usize = 0;
    loop {
        if i == length {
            if to_len > length {
                if to_b[to_start + i] == b'\\' {
                    return to_orig[to_start + i + 1..].to_string();
                } else if i == 2 {
                    return to_orig[to_start + i..].to_string();
                }
            }
            if from_len > length {
                if from_b[from_start + i] == b'\\' {
                    last_common_sep = i as i64;
                } else if i == 2 {
                    last_common_sep = 3;
                }
            }
            break;
        }
        let fc = from_b[from_start + i];
        let tc = to_b[to_start + i];
        if fc != tc { break; }
        if fc == b'\\' { last_common_sep = i as i64; }
        i += 1;
    }

    if i != length && last_common_sep == -1 {
        return to_orig;
    }

    let lcs = if last_common_sep < 0 { 0 } else { last_common_sep as usize };
    let mut out = String::new();
    let mut j = from_start + lcs + 1;
    while j <= from_end {
        if j == from_end || from_b[j] == b'\\' {
            if out.is_empty() { out.push_str(".."); } else { out.push_str("\\.."); }
        }
        j += 1;
    }

    let mut to_start2 = to_start + lcs;
    if !out.is_empty() {
        format!("{out}{}", &to_orig[to_start2..to_end])
    } else {
        if to_start2 < to_b.len() && to_b[to_start2] == b'\\' { to_start2 += 1; }
        to_orig[to_start2..to_end].to_string()
    }
}

struct FileCov {
    path: String,
    total: f64,
    hit: f64,
    line_hits: BTreeMap<i64, f64>,
}

/// `function parseLcov(text, root)`
fn parse_lcov(text: &str, root: &str) -> Vec<FileCov> {
    let mut files: Vec<FileCov> = Vec::new();
    let mut cur: Option<FileCov> = None;
    for raw in text.split('\n') {
        let line = raw.trim_end();
        if let Some(rest) = line.strip_prefix("SF:") {
            let trimmed = rest.trim();
            let mut p = trimmed.replace('\\', "/");
            if win_is_absolute(&p) || drive_colon_prefix(&p) {
                p = win_relative(root, &p).replace('\\', "/");
            }
            cur = Some(FileCov { path: p, total: 0.0, hit: 0.0, line_hits: BTreeMap::new() });
        } else if let Some(rest) = line.strip_prefix("DA:") {
            if let Some(c) = cur.as_mut() {
                let parts: Vec<&str> = rest.splitn(2, ',').collect();
                let ln = js_number_from_str(parts.first().copied().unwrap_or(""));
                // No comma at all -> JS `Number(undefined)` = NaN, distinct from an empty second
                // field (`Number('')` = 0).
                let count = if parts.len() < 2 { f64::NAN } else { js_number_from_str(parts[1]) };
                if ln.is_finite() {
                    c.total += 1.0;
                    if count > 0.0 { c.hit += 1.0; }
                    c.line_hits.insert(ln as i64, count);
                }
            }
        } else if let Some(rest) = line.strip_prefix("LF:") {
            if let Some(c) = cur.as_mut() {
                let v = js_number_from_str(rest);
                if truthy_num(v) { c.total = v; }
            }
        } else if let Some(rest) = line.strip_prefix("LH:") {
            if let Some(c) = cur.as_mut() {
                let v = js_number_from_str(rest);
                if v.is_finite() { c.hit = v; }
            }
        } else if line.starts_with("end_of_record") {
            if let Some(c) = cur.take() {
                files.push(c);
            }
        }
    }
    files
}

/// The `spawnSync(cmd, { shell: true, ... })` branch — see module doc. Real, disclosed untested.
fn run_coverage_cmd(cmd: &str, cwd: &str) -> Option<String> {
    match crate::proc::cmd("cmd").arg("/C").arg(cmd).current_dir(cwd).output() {
        Ok(o) => {
            if o.status.success() {
                None
            } else {
                let code = o.status.code().map(|c| c.to_string()).unwrap_or_else(|| "null".to_string());
                let stderr = String::from_utf8_lossy(&o.stderr);
                if stderr.is_empty() { Some(format!("exit {code}")) }
                else { Some(format!("exit {code}: {}", js_slice_neg(&stderr, 600))) }
            }
        }
        Err(e) => Some(js_slice0n(&e.to_string(), 300)),
    }
}

/// `export default function codeCoverage(input, context)`
pub fn codeCoverage(input: &Value, db: &Connection) -> Result<Value, String> {
    let workspace_root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root_v: Value = nz(input.get("root")).cloned().unwrap_or(workspace_root_v);
    if !truthy(Some(&root_v)) {
        return Ok(json!({
            "files": [], "total_pct": Value::Null, "count": 0, "ran": false,
            "error": "no active workspace root"
        }));
    }
    let root = js_string(&root_v);

    let cmd_opt: Option<String> = if truthy(input.get("cmd")) { Some(js_string(input.get("cmd").unwrap())) } else { None };
    let mut ran = false;
    let mut run_error: Option<String> = None;
    if matches!(input.get("run"), Some(Value::Bool(true))) {
        if let Some(cmd_str) = &cmd_opt {
            ran = true;
            run_error = run_coverage_cmd(cmd_str, &root);
        }
    }

    const CANDIDATES: [&str; 4] = ["coverage/lcov.info", "lcov.info", "coverage/lcov/lcov.info", ".nyc_output/lcov.info"];
    let mut lcov_path: Option<std::path::PathBuf> = None;
    for c in CANDIDATES {
        let full = Path::new(&root).join(c);
        if full.exists() { lcov_path = Some(full); break; }
    }
    let Some(lcov_path) = lcov_path else {
        let mut out = Map::new();
        out.insert("files".into(), json!([]));
        out.insert("total_pct".into(), Value::Null);
        out.insert("count".into(), json!(0));
        out.insert("ran".into(), json!(ran));
        if let Some(c) = &cmd_opt { out.insert("cmd".into(), json!(c)); }
        if let Some(e) = &run_error { out.insert("run_error".into(), json!(e)); }
        let err_msg = if ran {
            "no coverage/lcov.info found after the run".to_string()
        } else {
            "no coverage/lcov.info found — run coverage first (e.g. `npx c8 --reporter=lcovonly npm test`)".to_string()
        };
        out.insert("error".into(), json!(err_msg));
        return Ok(Value::Object(out));
    };

    let text = match std::fs::read_to_string(&lcov_path) {
        Ok(t) => t,
        Err(e) => {
            return Ok(json!({
                "files": [], "total_pct": Value::Null, "count": 0, "ran": ran,
                "error": format!("failed to parse lcov: {}", js_slice0n(&e.to_string(), 160))
            }));
        }
    };
    let files = parse_lcov(&text, &root);

    let tot_t: f64 = files.iter().map(|f| f.total).sum();
    let tot_h: f64 = files.iter().map(|f| f.hit).sum();
    let total_pct: Value = if truthy_num(tot_t) { js_num_to_json(round1(tot_h / tot_t)) } else { Value::Null };

    // lowest coverage first (the riskiest untested), then biggest — stable sort (gotcha #8).
    let mut summary: Vec<(String, f64, f64, f64)> = files
        .iter()
        .map(|f| {
            let pct = if truthy_num(f.total) { round1(f.hit / f.total) } else { 0.0 };
            (f.path.clone(), pct, f.hit, f.total)
        })
        .collect();
    summary.sort_by(|a, b| {
        let d = a.1 - b.1;
        if d != 0.0 {
            a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)
        } else {
            b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal)
        }
    });
    summary.truncate(800);
    let files_json: Vec<Value> = summary
        .iter()
        .map(|(p, pct, hit, total)| json!({ "path": p, "pct": js_num_to_json(*pct), "hit": js_num_to_json(*hit), "total": js_num_to_json(*total) }))
        .collect();

    let mut out = Map::new();
    out.insert("root_rel".into(), Value::Null);
    out.insert("ran".into(), json!(ran));
    if let Some(c) = &cmd_opt { out.insert("cmd".into(), json!(c)); }
    if let Some(e) = &run_error { out.insert("run_error".into(), json!(e)); }
    out.insert("total_pct".into(), total_pct);
    out.insert("count".into(), json!(files.len() as i64));
    out.insert("files".into(), json!(files_json));

    // one file's per-line hits → the editor's executed-line heatmap
    if truthy(input.get("path")) {
        let want = js_string(input.get("path").unwrap()).replace('\\', "/");
        let found = files.iter().find(|f| {
            f.path == want || f.path.ends_with(&format!("/{want}")) || want.ends_with(&format!("/{}", f.path))
        });
        let line_hits_v = match found {
            Some(f) => {
                let mut m = Map::new();
                for (ln, cnt) in &f.line_hits {
                    m.insert(ln.to_string(), js_num_to_json(*cnt));
                }
                Value::Object(m)
            }
            None => json!({}),
        };
        out.insert("line_hits".into(), line_hits_v);
    }

    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = concat!(env!("ANTILOKI_FIXTURES"), "/code-coverage");

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')",
        )
        .unwrap();
        c
    }
    fn seed_active(c: &Connection, root: &str) {
        c.execute("INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', ?1, 1)", rusqlite::params![root]).unwrap();
    }

    #[test]
    fn parses_lcov_lowest_coverage_first_with_total_pct() {
        let c = db();
        seed_active(&c, ROOT);
        let r = codeCoverage(&json!({}), &c).unwrap();
        assert_eq!(r["count"], json!(2));
        assert_eq!(r["total_pct"], json!(40));
        assert_eq!(r["files"][0]["path"], json!("src/b.ts"));
        assert_eq!(r["files"][0]["pct"], json!(0));
        assert_eq!(r["files"][1]["path"], json!("src/a.ts"));
        assert_eq!(r["files"][1]["pct"], json!(66.7));
        assert_eq!(r["ran"], json!(false));
        assert!(r.get("cmd").is_none());
        assert!(r.get("run_error").is_none());
        assert!(r.get("line_hits").is_none());
    }

    #[test]
    fn path_input_returns_line_hits_for_that_file() {
        let c = db();
        seed_active(&c, ROOT);
        let r = codeCoverage(&json!({ "path": "src/a.ts" }), &c).unwrap();
        assert_eq!(r["line_hits"], json!({ "1": 5, "2": 0, "3": 5 }));
    }

    #[test]
    fn no_lcov_found_returns_clean_guidance_no_throw() {
        let c = db();
        seed_active(&c, &format!("{ROOT}/no-such-subdir"));
        let r = codeCoverage(&json!({}), &c).unwrap();
        assert_eq!(r["count"], json!(0));
        assert_eq!(r["files"], json!([]));
        assert_eq!(r["total_pct"], Value::Null);
        assert!(r["error"].as_str().unwrap().contains("lcov"));
    }

    #[test]
    fn empty_root_override_reports_no_active_workspace_root() {
        let c = db();
        let r = codeCoverage(&json!({ "root": "" }), &c).unwrap();
        assert_eq!(r, json!({ "files": [], "total_pct": null, "count": 0, "ran": false, "error": "no active workspace root" }));
    }

    #[test]
    fn win_relative_matches_real_node_output() {
        assert_eq!(win_relative("C:/tmp/proj", "C:/tmp/proj/src/a.ts"), "src\\a.ts");
        assert_eq!(win_relative("C:/tmp/proj", "D:/other/x.ts"), "D:\\other\\x.ts");
        assert_eq!(win_relative("C:/tmp/proj", "C:/tmp/proj"), "");
        assert_eq!(win_relative("C:/tmp/proj", "C:/tmp/other/x.ts"), "..\\other\\x.ts");
    }
}
