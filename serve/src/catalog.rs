//! Port of `EngineService.skillCatalog()` + `board.listSkills(names)`.
//!
//! Builds the skill catalog by READING THE CRATE'S OWN SOURCE: it parses lib.rs's `#[path]`
//! attributes to map crate module -> skill file, scans `src/skills/**.rs`, extracts each skill's
//! leading doc comment, its `crate::mod::` imports and its prompt names, then closes both
//! relations transitively (uses_ai from `ai-call`, prompt_names through dependencies).
//!
//! ⚠️ SHIPPING BLOCKER, deliberately preserved rather than papered over: this reads the SOURCE
//! TREE at runtime. A shipped single binary has no `src/`, so this endpoint degrades to an empty
//! catalog there — exactly as the BE would. Before W6 it should become a build-time artifact
//! (a `build.rs` emitting the catalog as a constant); that is a design change, not a port, so it
//! is not made here.
//!
//! Cached after the first build, like the BE's `_skillCatalog`: the scan is the expensive part.

use axum::{extract::Query, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static CATALOG: OnceLock<Vec<Value>> = OnceLock::new();

/// `GET /api/v1/skills?names=a,b,c` — the FE waterfall lazily fetches only the handful of skills
/// for the stage the operator expanded, instead of the full ~200-row catalog.
pub async fn skills(Query(q): Query<HashMap<String, String>>) -> Response {
    let all = CATALOG.get_or_init(build_catalog);
    let names: Vec<&str> = q
        .get("names")
        .map(|s| s.split(',').map(|x| x.trim()).filter(|x| !x.is_empty()).collect())
        .unwrap_or_default();
    if names.is_empty() {
        return Json(all.clone()).into_response();
    }
    let want: HashSet<&str> = names.into_iter().collect();
    let filtered: Vec<Value> = all
        .iter()
        .filter(|s| s.get("name").and_then(|v| v.as_str()).is_some_and(|n| want.contains(n)))
        .cloned()
        .collect();
    Json(filtered).into_response()
}

fn project_root() -> PathBuf {
    // The BE resolves PROJECT_ROOT the same way: the substrate root's parent.
    std::env::var("ANTILOKI_SUBSTRATE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default())
}

fn build_catalog() -> Vec<Value> {
    let root = project_root();
    let skills_dir = root.join("antiloki-core-rust").join("src").join("skills");
    let lib_rs = root.join("antiloki-core-rust").join("src").join("lib.rs");

    // crate module name -> skill file basename, straight from lib.rs's own #[path] attributes.
    let mut module_to_skill: HashMap<String, String> = HashMap::new();
    if let Ok(lib) = std::fs::read_to_string(&lib_rs) {
        let re = regex::Regex::new(r#"#\[path\s*=\s*"([^"]+)"\]\s*pub mod\s+([A-Za-z0-9_]+)\s*;"#).unwrap();
        for c in re.captures_iter(&lib) {
            let base = c[1].split('/').next_back().unwrap_or("").trim_end_matches(".rs").to_string();
            module_to_skill.insert(c[2].to_string(), base);
        }
    }

    let mut rows: Vec<(String, PathBuf)> = Vec::new();
    collect_rs(&skills_dir, &mut rows);

    let import_re = regex::Regex::new(r"crate::([A-Za-z0-9_]+)::").unwrap();
    let prompt_re = regex::Regex::new(r#"['"]([a-z][a-z0-9-]*(?:\.[a-z0-9-]+)+\.v\d+)['"]"#).unwrap();
    let assign_re = regex::Regex::new(r#"PROMPT_NAME\s*[:=]\s*['"]([a-z][a-z0-9.-]+)['"]"#).unwrap();
    let ai_prefix = regex::Regex::new(r"^ai\.(reason|format)").unwrap();
    let strip_v = regex::Regex::new(r"\.v\d+$").unwrap();

    let mut deps: HashMap<String, Vec<String>> = HashMap::new();
    let mut own_prompts: HashMap<String, Vec<String>> = HashMap::new();
    let mut descs: HashMap<String, Option<String>> = HashMap::new();

    for (name, path) in &rows {
        let Ok(src) = std::fs::read_to_string(path) else {
            deps.insert(name.clone(), vec![]);
            own_prompts.insert(name.clone(), vec![]);
            descs.insert(name.clone(), None);
            continue;
        };
        // Tests are excluded: a #[cfg(test)] block mentions prompts and deps that the shipped
        // skill does not use.
        let body = match src.find("#[cfg(test)]") {
            Some(i) if i > 0 => &src[..i],
            _ => &src[..],
        };

        descs.insert(name.clone(), leading_description(body));

        let mut imports: Vec<String> = Vec::new();
        for c in import_re.captures_iter(body) {
            if let Some(dep) = module_to_skill.get(&c[1]) {
                if dep != name && !imports.contains(dep) {
                    imports.push(dep.clone());
                }
            }
        }
        deps.insert(name.clone(), imports);

        let mut prompts: Vec<String> = Vec::new();
        let mut push = |p: String| {
            let p = strip_v.replace(&p, "").to_string();
            if !ai_prefix.is_match(&p) && !prompts.contains(&p) {
                prompts.push(p);
            }
        };
        for c in prompt_re.captures_iter(body) {
            push(c[1].to_string());
        }
        for c in assign_re.captures_iter(body) {
            push(c[1].to_string());
        }
        own_prompts.insert(name.clone(), prompts);
    }

    // uses_ai closes transitively from ai-call.
    let mut uses_ai: HashSet<String> = HashSet::from(["ai-call".to_string()]);
    let mut changed = true;
    while changed {
        changed = false;
        for (name, imported) in &deps {
            if uses_ai.contains(name) {
                continue;
            }
            if imported.iter().any(|d| uses_ai.contains(d)) {
                uses_ai.insert(name.clone());
                changed = true;
            }
        }
    }

    let mut memo: HashMap<String, Vec<String>> = HashMap::new();
    rows.iter()
        .map(|(name, _)| {
            json!({
                "name": name,
                "description": descs.get(name).cloned().flatten(),
                "uses_ai": uses_ai.contains(name),
                "prompt_names": prompts_of(name, &deps, &own_prompts, &mut memo, &mut HashSet::new()),
            })
        })
        .collect()
}

/// Transitive prompt names, with the BE's cycle guard (a skill already on the stack contributes
/// nothing rather than recursing forever) and its memo.
fn prompts_of(
    name: &str,
    deps: &HashMap<String, Vec<String>>,
    own: &HashMap<String, Vec<String>>,
    memo: &mut HashMap<String, Vec<String>>,
    stack: &mut HashSet<String>,
) -> Vec<String> {
    if let Some(hit) = memo.get(name) {
        return hit.clone();
    }
    if stack.contains(name) {
        return vec![];
    }
    stack.insert(name.to_string());
    let mut out: Vec<String> = own.get(name).cloned().unwrap_or_default();
    for d in deps.get(name).cloned().unwrap_or_default() {
        for p in prompts_of(&d, deps, own, memo, stack) {
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    stack.remove(name);
    memo.insert(name.to_string(), out.clone());
    out
}

fn collect_rs(dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_rs(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                out.push((stem.to_string(), p.clone()));
            }
        }
    }
}

/// Port of `leadingDescription(src)`: the file's leading block or line comment, collapsed to one
/// line and capped at 280 chars. Rust files use `//!` module docs, which the `//` branch already
/// covers because it strips every leading slash.
fn leading_description(src: &str) -> Option<String> {
    let lines: Vec<&str> = src.split('\n').map(|l| l.trim_end_matches('\r')).collect();
    let mut i = 0;
    while i < lines.len() && (lines[i].trim().is_empty() || lines[i].starts_with("#!")) {
        i += 1;
    }
    let mut out: Vec<String> = Vec::new();
    if i < lines.len() && lines[i].trim_start().starts_with("/*") {
        while i < lines.len() {
            let end = lines[i].contains("*/");
            let t = lines[i]
                .trim_start()
                .trim_start_matches('/')
                .trim_start_matches('*')
                .trim_end_matches("*/")
                .trim_start_matches('*')
                .trim()
                .to_string();
            if !t.is_empty() {
                out.push(t);
            }
            if end {
                break;
            }
            i += 1;
        }
    } else {
        while i < lines.len() {
            let raw = lines[i].trim();
            if !raw.starts_with("//") {
                break;
            }
            let t = raw.trim_start_matches('/').trim().to_string();
            if !t.is_empty() {
                out.push(t);
            }
            i += 1;
        }
    }
    let text = out.join(" ");
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return None;
    }
    // `.slice(0, 280)` counts UTF-16 CODE UNITS, not scalar values. An astral char (or, here,
    // any non-BMP glyph in a doc comment) makes `chars().take(280)` keep one more character than
    // JS does — measured as an off-by-one on cockpit-session-narrate's description.
    let mut units = 0usize;
    let mut out = String::new();
    for ch in text.chars() {
        let w = ch.len_utf16();
        if units + w > 280 {
            break;
        }
        units += w;
        out.push(ch);
    }
    Some(out)
}
