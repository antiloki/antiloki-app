//! atlas-deps-audit.rs — twin of skills/codebase/atlas-deps-audit.mjs. NAMES ARE CONTRACT:
//! default export `atlasDepsAudit`.
//!
//! This unit is SUBSTANTIALLY REAL, not hollow: the entire lockfile-discovery + parsing surface
//! (8 ecosystem adapters: npm v1/v2/v3, yarn.lock, requirements.txt, poetry.lock/Cargo.lock TOML
//! shape, go.sum, composer.lock), the dedup/sources bookkeeping, the severity-bucketing /
//! counts / sort-order finding-shaping helpers, and `active_root` (the `workspaces` fallback
//! lookup) are all ported faithfully as real fs + db + pure logic, each covered by
//! `#[cfg(test)]`.
//!
//! PARITY-DEFERRED: `query_osv` — the real HTTP calls to `OSV_BATCH`
//! (`https://api.osv.dev/v1/querybatch`) and `OSV_VULN` (`https://api.osv.dev/v1/vulns/`) are not
//! implemented in this crate (zero-new-HTTP-client-deps discipline, same as every other
//! network-adjacent twin — see anthropic-call.rs / _helpers/web-cache.rs). JS's `queryOsv` wraps
//! its `fetchJson` calls in try/catch and on ANY failure returns `{vulns:[], scanned:0,
//! online:false}` WITHOUT throwing — the module doc itself calls this "a clean offline degrade —
//! never throws". This twin's `query_osv` always takes that exact same catch-branch shape, so
//! `atlasDepsAudit`'s own return value for a repo WITH lockfiles matches real JS run offline
//! byte-for-byte; only the case of a live, reachable OSV endpoint returning real findings is
//! unreachable here. `toFinding` / `SEV_BUCKET` / `countBySeverity` / the sort comparator are
//! still ported as real, independently fixture-tested pure fns (same "ported by inspection"
//! convention as feature-wizard.rs's post-aiCall branches) even though `atlasDepsAudit`'s own
//! top-level flow never reaches them via `query_osv`'s fixed offline result.
//!
//! `context.fetch_fn` (JS test-injection hook) has no Rust equivalent since the fetch boundary is
//! never actually invoked here — mirrors anthropic-call.rs's disclosure for the same hook.

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::OnceLock;

// ── standard helper functions (playbook) ────────────────────────────────────────────────
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
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

// ── lockfile adapters ────────────────────────────────────────────────────────────
// Each: (text) -> Result<Vec<{ecosystem,name,version}>, String>. A parse failure (e.g. JSON.parse
// throwing) surfaces as Err; `parse_lockfiles` wraps every call and turns Err into an empty list,
// matching the JS try/catch in `parseLockfiles`.

fn parse_npm(text: &str) -> Result<Vec<Value>, String> {
    let j: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    if let Some(packages) = j.get("packages").and_then(|v| v.as_object()) {
        for (p, info) in packages {
            if p.is_empty() {
                continue; // "" = the root project itself
            }
            let name = match p.rfind("node_modules/") {
                Some(idx) => &p[idx + "node_modules/".len()..],
                None => p.as_str(),
            };
            if let Some(version) = info.get("version").and_then(|v| v.as_str()) {
                if !name.is_empty() && !version.is_empty() {
                    out.push(json!({ "ecosystem": "npm", "name": name, "version": version }));
                }
            }
        }
    } else if let Some(deps) = j.get("dependencies").and_then(|v| v.as_object()) {
        walk_npm_deps(deps, &mut out);
    }
    Ok(out)
}
fn walk_npm_deps(deps: &Map<String, Value>, out: &mut Vec<Value>) {
    for (name, info) in deps {
        if let Some(version) = info.get("version").and_then(|v| v.as_str()) {
            if !version.is_empty() {
                out.push(json!({ "ecosystem": "npm", "name": name, "version": version }));
            }
        }
        if let Some(sub) = info.get("dependencies").and_then(|v| v.as_object()) {
            walk_npm_deps(sub, out);
        }
    }
}

fn yarn_header_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^[^\s#].*:\s*$").unwrap())
}
fn yarn_version_line_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^\s+version\s").unwrap())
}
fn yarn_version_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"version\s+"?([^"\s]+)"?"#).unwrap())
}
fn parse_yarn(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut pending: Option<String> = None;
    for line in text.lines() {
        if yarn_header_re().is_match(line) {
            let stripped = line.trim_end();
            let stripped = stripped.strip_suffix(':').unwrap_or(stripped).trim_end();
            let first_key = stripped.split(',').next().unwrap_or("").trim();
            let first_key = first_key.trim_matches('"');
            pending = match first_key.rfind('@') {
                Some(at) if at > 0 => Some(first_key[..at].to_string()),
                _ => None,
            };
        } else if pending.is_some() && yarn_version_line_re().is_match(line) {
            if let Some(caps) = yarn_version_re().captures(line) {
                out.push(json!({ "ecosystem": "npm", "name": pending.clone().unwrap(), "version": caps[1].to_string() }));
            }
            pending = None;
        }
    }
    out
}

fn requirements_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^([A-Za-z0-9._-]+)\s*==\s*([0-9][^\s;,]*)").unwrap())
}
fn parse_requirements(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('-') {
            continue;
        }
        if let Some(caps) = requirements_re().captures(line) {
            out.push(json!({ "ecosystem": "PyPI", "name": caps[1].to_string(), "version": caps[2].to_string() }));
        }
    }
    out
}

fn toml_package_split_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\[\[package\]\]").unwrap())
}
fn toml_name_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?m)^\s*name\s*=\s*"([^"]+)""#).unwrap())
}
fn toml_version_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?m)^\s*version\s*=\s*"([^"]+)""#).unwrap())
}
fn parse_toml_packages(text: &str, ecosystem: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut blocks: Vec<&str> = toml_package_split_re().split(text).collect();
    if !blocks.is_empty() {
        blocks.remove(0); // .slice(1)
    }
    for block in blocks {
        let name = toml_name_re().captures(block).map(|c| c[1].to_string());
        let version = toml_version_re().captures(block).map(|c| c[1].to_string());
        if let (Some(name), Some(version)) = (name, version) {
            out.push(json!({ "ecosystem": ecosystem, "name": name, "version": version }));
        }
    }
    out
}

fn parse_go_sum(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let parts: Vec<&str> = raw.trim().split_whitespace().collect();
        if parts.len() < 3 {
            continue;
        }
        let (module, ver) = (parts[0], parts[1]);
        if ver.ends_with("/go.mod") {
            continue;
        }
        let ver = ver.strip_prefix('v').unwrap_or(ver);
        out.push(json!({ "ecosystem": "Go", "name": module, "version": ver }));
    }
    out
}

fn parse_composer(text: &str) -> Result<Vec<Value>, String> {
    let j: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for key in ["packages", "packages-dev"] {
        if let Some(arr) = j.get(key).and_then(|v| v.as_array()) {
            for p in arr {
                let name = p.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
                let version = p.get("version");
                if let (Some(name), Some(version)) = (name, version) {
                    if truthy(Some(version)) {
                        let vs = str_qq(Some(version));
                        let vs = vs.strip_prefix('v').unwrap_or(&vs).to_string();
                        out.push(json!({ "ecosystem": "Packagist", "name": name, "version": vs }));
                    }
                }
            }
        }
    }
    Ok(out)
}

const LOCKFILE_NAMES: [&str; 8] = [
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "requirements.txt",
    "poetry.lock",
    "Cargo.lock",
    "go.sum",
    "composer.lock",
];

fn parse_one_lockfile(file: &str, text: &str) -> Vec<Value> {
    match file {
        "package-lock.json" | "npm-shrinkwrap.json" => parse_npm(text).unwrap_or_default(),
        "yarn.lock" => parse_yarn(text),
        "requirements.txt" => parse_requirements(text),
        "poetry.lock" => parse_toml_packages(text, "PyPI"),
        "Cargo.lock" => parse_toml_packages(text, "crates.io"),
        "go.sum" => parse_go_sum(text),
        "composer.lock" => parse_composer(text).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// `function parseLockfiles(root)` — real fs reads at the repo root, best-effort per file
/// (missing / unparseable -> skipped), deduped `{ecosystem,name,version}` tuples + source list.
fn parse_lockfiles(root: &str) -> (Vec<Value>, Vec<String>) {
    let mut tuples = Vec::new();
    let mut sources = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for file in LOCKFILE_NAMES {
        let path = std::path::Path::new(root).join(file);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let parsed = parse_one_lockfile(file, &text);
        if parsed.is_empty() {
            continue;
        }
        sources.push(file.to_string());
        for t in parsed {
            let name = t.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
            let version = t.get("version").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
            let (name, version) = match (name, version) {
                (Some(n), Some(v)) => (n, v),
                _ => continue,
            };
            let ecosystem = t.get("ecosystem").and_then(|v| v.as_str()).unwrap_or("");
            let key = format!("{ecosystem}|{name}|{version}");
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key);
            tuples.push(t);
        }
    }
    (tuples, sources)
}

// ── OSV ─────────────────────────────────────────────────────────────────────────
fn sev_bucket(s: Option<&str>) -> &'static str {
    let u = s.unwrap_or("").to_uppercase();
    if u.contains("CRIT") {
        "critical"
    } else if u == "HIGH" {
        "high"
    } else if u.starts_with("MOD") || u == "MEDIUM" {
        "moderate"
    } else if u == "LOW" {
        "low"
    } else {
        "unknown"
    }
}
fn empty_counts() -> Value {
    json!({ "critical": 0, "high": 0, "moderate": 0, "low": 0, "unknown": 0, "total": 0 })
}
// JS `c[v.severity] = (c[v.severity] ?? 0) + 1` — keyed only by the 5 buckets `sev_bucket` ever
// returns, so no unexpected key ever lands in the fixed emptyCounts() shape.
fn count_by_severity(vulns: &[Value]) -> Value {
    let mut c = empty_counts();
    let obj = c.as_object_mut().unwrap();
    let mut total: i64 = 0;
    for v in vulns {
        let sev = v.get("severity").and_then(|x| x.as_str()).unwrap_or("unknown");
        let cur = obj.get(sev).and_then(|x| x.as_i64()).unwrap_or(0);
        obj.insert(sev.to_string(), json!(cur + 1));
        total += 1;
    }
    obj.insert("total".to_string(), json!(total));
    c
}

/// `function toFinding(tuple, id, detail)` — real, pure. Unreachable from `atlasDepsAudit`'s own
/// top-level flow in this twin (see module doc), ported by inspection + independently tested.
fn to_finding(tuple: &Value, id: &str, detail: Option<&Value>) -> Value {
    let aliases: Vec<Value> = detail.and_then(|d| d.get("aliases")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let cve = aliases
        .iter()
        .find(|a| a.as_str().map_or(false, |s| s.len() >= 4 && s[..4].eq_ignore_ascii_case("CVE-")))
        .cloned()
        .unwrap_or_else(|| json!(id));

    let mut raw_sev: Option<String> = None;
    let mut fixed_in: Option<Value> = None;
    if let Some(detail) = detail {
        let tname = tuple.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let tecosystem = tuple.get("ecosystem").and_then(|v| v.as_str()).unwrap_or("");
        let affected: Vec<Value> = detail.get("affected").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        for a in affected.iter().filter(|a| {
            let pkg = a.get("package");
            let name_match = pkg.and_then(|p| p.get("name")).and_then(|v| v.as_str()) == Some(tname);
            let eco = pkg.and_then(|p| p.get("ecosystem")).and_then(|v| v.as_str());
            name_match && (eco.is_none() || eco == Some(tecosystem))
        }) {
            if raw_sev.is_none() {
                if let Some(sev) = a.get("database_specific").and_then(|ds| ds.get("severity")).and_then(|v| v.as_str()) {
                    raw_sev = Some(sev.to_string());
                }
            }
            let ranges: Vec<Value> = a.get("ranges").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            for range in &ranges {
                let events: Vec<Value> = range.get("events").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                for ev in &events {
                    if fixed_in.is_none() {
                        let f = ev.get("fixed");
                        if truthy(f) {
                            fixed_in = f.cloned();
                        }
                    }
                }
            }
        }
        if raw_sev.is_none() {
            if let Some(sev) = detail.get("database_specific").and_then(|ds| ds.get("severity")).and_then(|v| v.as_str()) {
                raw_sev = Some(sev.to_string());
            }
        }
    }

    let summary = match detail.and_then(|d| d.get("summary")) {
        Some(v) if !v.is_null() => v.clone(),
        _ => {
            let details = detail.and_then(|d| d.get("details"));
            if truthy(details) {
                json!(str_qq(details).chars().take(200).collect::<String>())
            } else {
                Value::Null
            }
        }
    };

    json!({
        "ecosystem": tuple.get("ecosystem").cloned().unwrap_or(Value::Null),
        "package": tuple.get("name").cloned().unwrap_or(Value::Null),
        "version": tuple.get("version").cloned().unwrap_or(Value::Null),
        "cve": cve,
        "osv_id": id,
        "severity": sev_bucket(raw_sev.as_deref()),
        "fixed_in": fixed_in.unwrap_or(Value::Null),
        "summary": summary,
    })
}

/// PARITY-DEFERRED: `queryOsv` — real network calls not implemented (see module doc). JS's own
/// try/catch-driven offline degrade is reproduced unconditionally: this always returns the exact
/// shape JS returns when the OSV batch POST fails.
fn query_osv(_tuples: &[Value]) -> Value {
    json!({ "vulns": [], "scanned": 0, "online": false })
}

/// `function activeRoot(db)` — best-effort: no db / no active workspace / any query error -> null.
fn active_root(db: Option<&Connection>) -> Option<String> {
    let db = db?;
    db.query_row("SELECT root_path FROM workspaces WHERE is_active = 1 LIMIT 1", [], |r| r.get::<_, String>(0)).ok()
}

const SEV_ORDER: [&str; 4] = ["critical", "high", "moderate", "low"];
fn sev_order(s: &str) -> usize {
    SEV_ORDER.iter().position(|&x| x == s).unwrap_or(4)
}

/// `export default async function atlasDepsAudit(input, context = {})` — NAMES ARE CONTRACT.
/// `context.db` -> `db: Option<&Connection>` (mirrors the local `activeRoot` null-safety); JS's
/// `context.fetch_fn` hook has no equivalent (see module doc — `query_osv` is the fixed boundary).
pub fn atlasDepsAudit(input: &Value, db: Option<&Connection>) -> Value {
    let root_from_input = match input.get("root") {
        None | Some(Value::Null) => None,
        Some(v) => Some(str_qq(Some(v))),
    };
    let root = root_from_input.or_else(|| active_root(db));
    let root = match root {
        Some(r) if !r.is_empty() => r,
        _ => return json!({ "vulns": [], "scanned": 0, "sources": [], "online": true, "counts": empty_counts() }),
    };

    let (tuples, sources) = parse_lockfiles(&root);
    // `fetchFn = fetch_fn ?? globalThis.fetch` — no injected hook and no HTTP client in this twin;
    // `globalThis.fetch` always exists in the real Node 24 runtime, so `typeof fetchFn === 'function'`
    // is mirrored as unconditionally true. The real deferred boundary is `query_osv` below.
    if tuples.is_empty() {
        return json!({ "vulns": [], "scanned": 0, "sources": sources, "online": true, "counts": empty_counts() });
    }

    let osv = query_osv(&tuples);
    let mut vulns: Vec<Value> = osv.get("vulns").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let scanned = osv.get("scanned").cloned().unwrap_or(json!(0));
    let online = osv.get("online").cloned().unwrap_or(json!(false));

    // High severity first, then ecosystem/package for a stable order. Unreachable from
    // `query_osv`'s fixed empty result in this twin — ported by inspection, tested directly below.
    vulns.sort_by(|a, b| {
        let sa = a.get("severity").and_then(|v| v.as_str()).unwrap_or("unknown");
        let sb = b.get("severity").and_then(|v| v.as_str()).unwrap_or("unknown");
        sev_order(sa)
            .cmp(&sev_order(sb))
            .then_with(|| str_qq(a.get("ecosystem")).cmp(&str_qq(b.get("ecosystem"))))
            .then_with(|| str_qq(a.get("package")).cmp(&str_qq(b.get("package"))))
    });

    json!({ "vulns": vulns, "scanned": scanned, "sources": sources, "online": online, "counts": count_by_severity(&vulns) })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TmpDir(std::path::PathBuf);
    impl TmpDir {
        fn new(tag: &str) -> Self {
            let mut dir = std::env::temp_dir();
            dir.push(format!("antiloki-atlas-deps-audit-rs-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TmpDir(dir)
        }
        fn write(&self, name: &str, contents: &str) {
            std::fs::write(self.0.join(name), contents).unwrap();
        }
        fn path(&self) -> &str {
            self.0.to_str().unwrap()
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // ── parse_npm ────────────────────────────────────────────────────────
    #[test]
    fn parse_npm_v2v3_packages_map_skips_root_and_missing_version() {
        let text = r#"{"packages":{"":{"version":"9.9.9"},"node_modules/lodash":{"version":"4.17.21"},"node_modules/@scope/pkg":{"version":"1.0.0"},"node_modules/bad":{}}}"#;
        let out = parse_npm(text).unwrap();
        assert_eq!(out, vec![
            json!({"ecosystem":"npm","name":"lodash","version":"4.17.21"}),
            json!({"ecosystem":"npm","name":"@scope/pkg","version":"1.0.0"}),
        ]);
    }

    #[test]
    fn parse_npm_v1_dependencies_tree_recurses() {
        let text = r#"{"dependencies":{"a":{"version":"1.0.0","dependencies":{"b":{"version":"2.0.0"}}}}}"#;
        let out = parse_npm(text).unwrap();
        assert_eq!(out, vec![
            json!({"ecosystem":"npm","name":"a","version":"1.0.0"}),
            json!({"ecosystem":"npm","name":"b","version":"2.0.0"}),
        ]);
    }

    #[test]
    fn parse_npm_invalid_json_errs() {
        assert!(parse_npm("not json").is_err());
    }

    // ── parse_yarn ───────────────────────────────────────────────────────
    #[test]
    fn parse_yarn_reads_header_and_version() {
        let text = "\"lodash@^4.17.0\", lodash@^4.17.21:\n  version \"4.17.21\"\n  resolved \"...\"\n\n\"@scope/pkg@^1.0.0\":\n  version \"1.0.0\"\n";
        let out = parse_yarn(text);
        assert_eq!(out, vec![
            json!({"ecosystem":"npm","name":"lodash","version":"4.17.21"}),
            json!({"ecosystem":"npm","name":"@scope/pkg","version":"1.0.0"}),
        ]);
    }

    // ── parse_requirements ──────────────────────────────────────────────
    #[test]
    fn parse_requirements_only_exact_pins() {
        let text = "flask==2.3.1\ndjango>=4.0\n-r other.txt\n# comment\nrequests==2.31.0  # pinned\n";
        let out = parse_requirements(text);
        assert_eq!(out, vec![
            json!({"ecosystem":"PyPI","name":"flask","version":"2.3.1"}),
            json!({"ecosystem":"PyPI","name":"requests","version":"2.31.0"}),
        ]);
    }

    // ── parse_toml_packages ──────────────────────────────────────────────
    #[test]
    fn parse_toml_packages_reads_name_and_version_blocks() {
        let text = "[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\n\n[[package]]\nname = \"tokio\"\nversion = \"1.2.3\"\nsource = \"registry\"\n";
        let out = parse_toml_packages(text, "crates.io");
        assert_eq!(out, vec![
            json!({"ecosystem":"crates.io","name":"serde","version":"1.0.0"}),
            json!({"ecosystem":"crates.io","name":"tokio","version":"1.2.3"}),
        ]);
    }

    // ── parse_go_sum ─────────────────────────────────────────────────────
    #[test]
    fn parse_go_sum_skips_go_mod_lines_and_strips_v() {
        let text = "github.com/pkg/errors v0.9.1 h1:abc=\ngithub.com/pkg/errors v0.9.1/go.mod h1:def=\n";
        let out = parse_go_sum(text);
        assert_eq!(out, vec![json!({"ecosystem":"Go","name":"github.com/pkg/errors","version":"0.9.1"})]);
    }

    // ── parse_composer ───────────────────────────────────────────────────
    #[test]
    fn parse_composer_reads_packages_and_dev_and_strips_v() {
        let text = r#"{"packages":[{"name":"vendor/pkg","version":"v1.2.3"}],"packages-dev":[{"name":"vendor/dev","version":"2.0.0"}]}"#;
        let out = parse_composer(text).unwrap();
        assert_eq!(out, vec![
            json!({"ecosystem":"Packagist","name":"vendor/pkg","version":"1.2.3"}),
            json!({"ecosystem":"Packagist","name":"vendor/dev","version":"2.0.0"}),
        ]);
    }

    // ── parse_lockfiles ──────────────────────────────────────────────────
    #[test]
    fn parse_lockfiles_reads_present_files_dedups_and_lists_sources() {
        let dir = TmpDir::new("parse-lockfiles");
        dir.write(
            "package-lock.json",
            r#"{"packages":{"node_modules/lodash":{"version":"4.17.21"},"node_modules/dup":{"version":"1.0.0"}}}"#,
        );
        dir.write("requirements.txt", "flask==2.3.1\n");
        let (tuples, sources) = parse_lockfiles(dir.path());
        assert_eq!(sources, vec!["package-lock.json".to_string(), "requirements.txt".to_string()]);
        assert_eq!(tuples.len(), 3);
        assert!(tuples.contains(&json!({"ecosystem":"npm","name":"lodash","version":"4.17.21"})));
        assert!(tuples.contains(&json!({"ecosystem":"PyPI","name":"flask","version":"2.3.1"})));
    }

    #[test]
    fn parse_lockfiles_missing_dir_yields_empty() {
        let (tuples, sources) = parse_lockfiles("C:/definitely/not/a/real/path/xyz");
        assert!(tuples.is_empty());
        assert!(sources.is_empty());
    }

    // ── sev_bucket / counts ──────────────────────────────────────────────
    #[test]
    fn sev_bucket_buckets_known_severities() {
        assert_eq!(sev_bucket(Some("CRITICAL")), "critical");
        assert_eq!(sev_bucket(Some("HIGH")), "high");
        assert_eq!(sev_bucket(Some("MODERATE")), "moderate");
        assert_eq!(sev_bucket(Some("MEDIUM")), "moderate");
        assert_eq!(sev_bucket(Some("LOW")), "low");
        assert_eq!(sev_bucket(None), "unknown");
        assert_eq!(sev_bucket(Some("weird")), "unknown");
    }

    #[test]
    fn count_by_severity_tallies_and_totals() {
        let vulns = vec![json!({"severity":"critical"}), json!({"severity":"high"}), json!({"severity":"critical"})];
        let c = count_by_severity(&vulns);
        assert_eq!(c, json!({"critical":2,"high":1,"moderate":0,"low":0,"unknown":0,"total":3}));
    }

    // ── to_finding ───────────────────────────────────────────────────────
    #[test]
    fn to_finding_prefers_cve_alias_and_reads_severity_and_fixed_in() {
        let tuple = json!({"ecosystem":"npm","name":"lodash","version":"4.17.20"});
        let detail = json!({
            "aliases": ["GHSA-xxxx", "CVE-2021-23337"],
            "affected": [{
                "package": {"name":"lodash","ecosystem":"npm"},
                "database_specific": {"severity":"HIGH"},
                "ranges": [{"events":[{"introduced":"0"},{"fixed":"4.17.21"}]}],
            }],
            "summary": "Command Injection in lodash",
        });
        let f = to_finding(&tuple, "GHSA-xxxx", Some(&detail));
        assert_eq!(f["cve"], json!("CVE-2021-23337"));
        assert_eq!(f["osv_id"], json!("GHSA-xxxx"));
        assert_eq!(f["severity"], json!("high"));
        assert_eq!(f["fixed_in"], json!("4.17.21"));
        assert_eq!(f["summary"], json!("Command Injection in lodash"));
        assert_eq!(f["package"], json!("lodash"));
    }

    #[test]
    fn to_finding_falls_back_to_id_and_truncated_details_when_no_cve_or_summary() {
        let tuple = json!({"ecosystem":"npm","name":"x","version":"1.0.0"});
        let long_details = "d".repeat(250);
        let detail = json!({"aliases": [], "details": long_details});
        let f = to_finding(&tuple, "OSV-1", Some(&detail));
        assert_eq!(f["cve"], json!("OSV-1"));
        assert_eq!(f["severity"], json!("unknown"));
        assert_eq!(f["fixed_in"], Value::Null);
        assert_eq!(f["summary"].as_str().unwrap().len(), 200);
    }

    #[test]
    fn to_finding_with_no_detail_is_all_unknown() {
        let tuple = json!({"ecosystem":"npm","name":"x","version":"1.0.0"});
        let f = to_finding(&tuple, "OSV-2", None);
        assert_eq!(f["cve"], json!("OSV-2"));
        assert_eq!(f["severity"], json!("unknown"));
        assert_eq!(f["fixed_in"], Value::Null);
        assert_eq!(f["summary"], Value::Null);
    }

    // ── query_osv (deferred) ─────────────────────────────────────────────
    #[test]
    fn query_osv_always_returns_the_offline_degrade_shape() {
        let out = query_osv(&[json!({"ecosystem":"npm","name":"x","version":"1.0.0"})]);
        assert_eq!(out, json!({"vulns":[],"scanned":0,"online":false}));
    }

    // ── active_root ──────────────────────────────────────────────────────
    fn workspaces_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');",
        ).unwrap();
        c
    }

    #[test]
    fn active_root_none_db_is_null() {
        assert_eq!(active_root(None), None);
    }

    #[test]
    fn active_root_no_active_workspace_is_null() {
        let c = workspaces_db();
        assert_eq!(active_root(Some(&c)), None);
    }

    #[test]
    fn active_root_finds_the_active_workspace_root_path() {
        let c = workspaces_db();
        c.execute("INSERT INTO workspaces (name, root_path, is_active) VALUES ('a','/repo/a',0)", []).unwrap();
        c.execute("INSERT INTO workspaces (name, root_path, is_active) VALUES ('b','/repo/b',1)", []).unwrap();
        assert_eq!(active_root(Some(&c)), Some("/repo/b".to_string()));
    }

    // ── atlasDepsAudit ───────────────────────────────────────────────────
    #[test]
    fn atlas_deps_audit_no_root_and_no_active_workspace_returns_clean_empty() {
        let c = workspaces_db();
        let out = atlasDepsAudit(&json!({}), Some(&c));
        assert_eq!(out, json!({"vulns":[],"scanned":0,"sources":[],"online":true,"counts":empty_counts()}));
    }

    #[test]
    fn atlas_deps_audit_root_with_no_lockfiles_is_online_true_empty() {
        let dir = TmpDir::new("audit-no-lockfiles");
        let out = atlasDepsAudit(&json!({"root": dir.path()}), None);
        assert_eq!(out, json!({"vulns":[],"scanned":0,"sources":[],"online":true,"counts":empty_counts()}));
    }

    #[test]
    fn atlas_deps_audit_root_with_lockfile_reaches_the_deferred_offline_degrade() {
        let dir = TmpDir::new("audit-with-lockfile");
        dir.write("requirements.txt", "flask==2.3.1\n");
        let out = atlasDepsAudit(&json!({"root": dir.path()}), None);
        assert_eq!(out["vulns"], json!([]));
        assert_eq!(out["scanned"], json!(0));
        assert_eq!(out["sources"], json!(["requirements.txt"]));
        assert_eq!(out["online"], json!(false));
        assert_eq!(out["counts"], empty_counts());
    }

    #[test]
    fn atlas_deps_audit_uses_active_workspace_root_when_input_root_omitted() {
        let dir = TmpDir::new("audit-active-root");
        dir.write("requirements.txt", "flask==2.3.1\n");
        let c = workspaces_db();
        c.execute("INSERT INTO workspaces (name, root_path, is_active) VALUES ('a', ?1, 1)", [dir.path()]).unwrap();
        let out = atlasDepsAudit(&json!({}), Some(&c));
        assert_eq!(out["sources"], json!(["requirements.txt"]));
        assert_eq!(out["online"], json!(false));
    }
}
