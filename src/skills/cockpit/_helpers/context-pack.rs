//! context-pack.rs — WHAT a code-agent pane gets to read before it explores. NO JS TWIN.
//!
//! Added 2026-09-14: a Claude/Codex/OpenCode pane used to boot bare in its worktree — the only
//! antiloki context it saw was the PANE BRIEF block (mission, scope globs, raw blackboard docs).
//! Nothing antiIDE had already measured (file reviews, the folder paper, the laws, the import
//! graph, the test map) reached it, so every agent re-derived all of that by reading source.
//!
//! A *context pack* is the per-pane selection of that measured material. It lives in
//! `cockpit_panes.settings_json.context_pack` (FE and Rust share this shape) and is materialised
//! into `<worktree>/.antiloki/context/` by `cockpit-pane-context-bake.rs`. This module is the
//! pure part: parse the selection, derive the default from the pane's scope, validate custom
//! items, and compute the identity hash the bake uses to skip no-op rewrites.
//!
//! ```jsonc
//! { "preset": "scope" | "custom" | "none",
//!   "items": [ {"kind":"laws"}, {"kind":"folder","path":"src/ui"}, {"kind":"file","path":"src/ui/A.tsx"},
//!              {"kind":"graph","path":"src/ui"}, {"kind":"tests","path":"src/ui"},
//!              {"kind":"blast","path":"src/ui/A.tsx"}, {"kind":"doc","id":12} ],
//!   "live": true,                // wire the antiloki MCP server for this pane
//!   "budget_chars": 200000 }     // hard cap over the whole pack
//! ```
//!
//! THE DEFAULT RULE (preset `scope`, printed verbatim in INDEX.md so the agent and the operator
//! can see why a file is there): laws always; one folder review per distinct scope directory
//! (max 8); a file review for every in-scope file whose latest score is below 80, worst first
//! (max 20) — the files that need work; the runtime import graph and the test map for each scope
//! folder; every blackboard `context` doc the pane has not excluded. An empty scope → laws + docs.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

pub const DEFAULT_BUDGET_CHARS: i64 = 200_000;
pub const MAX_FOLDERS: usize = 8;
pub const MAX_FILES: usize = 20;
/// "needs work" — a file whose health is below this gets its review in the default pack.
pub const FILE_SCORE_MAX: f64 = 80.0;
/// Every packable catalog source plus the blackboard `doc` (which lives in the cockpit, not the atlas).
pub fn kinds() -> Vec<&'static str> { let mut k = crate::cockpit__helpers_context_catalog::packable_keys(); k.push("doc"); k }
pub const PRESETS: &[&str] = &["scope", "custom", "none"];

pub const DEFAULT_RULE: &str = "laws · one folder review per scope directory (max 8) · file reviews for in-scope files with health < 80, worst first (max 20) · import graph + test map per scope folder · the session's context docs";

fn s(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Null) | None => String::new(),
        Some(o) => o.to_string(),
    }
}

/// `settings_json` (the raw column, or already-parsed) → the pack block, normalised. Absent or
/// malformed → `{preset:"scope", items:[], live:true, budget_chars:DEFAULT}` — a pane that never
/// chose gets the scope default, never nothing.
pub fn parse_pack(settings: &Value) -> Value {
    let settings: Value = match settings {
        Value::String(raw) => serde_json::from_str(raw).unwrap_or(Value::Null),
        o => o.clone(),
    };
    let cp = settings.get("context_pack").cloned().unwrap_or(Value::Null);
    let preset = match cp.get("preset").and_then(Value::as_str) {
        Some(p) if PRESETS.contains(&p) => p.to_string(),
        _ => "scope".to_string(),
    };
    let mut warnings: Vec<String> = Vec::new();
    let items: Vec<Value> = cp
        .get("items")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|it| normalise_item(it, &mut warnings)).collect())
        .unwrap_or_default();
    let live = cp.get("live").and_then(Value::as_bool).unwrap_or(true);
    let budget = cp.get("budget_chars").and_then(Value::as_i64).filter(|b| *b > 0).unwrap_or(DEFAULT_BUDGET_CHARS);
    json!({ "preset": preset, "items": items, "live": live, "budget_chars": budget, "warnings": warnings })
}

fn norm_path(p: &str) -> String {
    let p = p.replace('\\', "/");
    let p = p.strip_prefix("./").unwrap_or(&p).to_string();
    p.trim_matches('/').to_string()
}

fn normalise_item(it: &Value, warnings: &mut Vec<String>) -> Option<Value> {
    let kind = s(it.get("kind"));
    if !kinds().contains(&kind.as_str()) {
        warnings.push(format!("unknown item kind `{kind}` dropped"));
        return None;
    }
    // extra args (ast `symbol`/`lines`, subgraph `hops`, …) ride along under `args`
    let args: Map<String, Value> = it.get("args").and_then(Value::as_object).cloned().unwrap_or_default();
    let with_args = |mut v: Value| { if !args.is_empty() { v.as_object_mut().unwrap().insert("args".into(), Value::Object(args.clone())); } Some(v) };
    match kind.as_str() {
        "doc" => match it.get("id").and_then(Value::as_i64) {
            Some(id) => Some(json!({ "kind": "doc", "id": id })),
            None => { warnings.push("doc item without id dropped".into()); None }
        },
        _ => {
            let needs_path = crate::cockpit__helpers_context_catalog::source(&kind).map_or(false, |src| src.args.first().map_or(false, |(n, _)| *n == "path" && !matches!(kind.as_str(), "tree" | "docs" | "coverage" | "paper")));
            let p = norm_path(&s(it.get("path")));
            if p.is_empty() && needs_path { warnings.push(format!("{kind} item without path dropped")); return None; }
            if p.is_empty() { with_args(json!({ "kind": kind })) } else { with_args(json!({ "kind": kind, "path": p })) }
        }
    }
}

/// The distinct directories a scope glob list names, in first-seen order. `src/ui/**` → `src/ui`,
/// `src/ui/A.tsx` → `src/ui`, `**` → (nothing — whole repo is not a folder).
pub fn scope_folders(scope: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for g in scope {
        let base = s(Some(&crate::cockpit__helpers_scope::literalBase(&json!(g))));
        let base = norm_path(&base);
        if base.is_empty() { continue; }
        // a literal file path → its directory; a directory prefix stays
        let dir = if base.rsplit('/').next().map_or(false, |seg| seg.contains('.')) && !g.contains('*') {
            base.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default()
        } else {
            base
        };
        if dir.is_empty() || out.contains(&dir) { continue; }
        out.push(dir);
    }
    out
}

/// The scope default, from the latest done snapshot. Pure over its inputs except for the two
/// reads (scores of the latest snapshot; the session's context docs).
pub fn default_items(db: &Connection, scope: &[String], session_id: i64, context_excludes: &[i64]) -> Vec<Value> {
    let mut items: Vec<Value> = vec![json!({ "kind": "laws" })];
    let folders: Vec<String> = scope_folders(scope).into_iter().take(MAX_FOLDERS).collect();
    for f in &folders { items.push(json!({ "kind": "folder", "path": f })); }

    if !scope.is_empty() {
        let snap_id: Option<i64> = crate::db_json::query_rows(db, "SELECT id FROM judgments.review_snapshots WHERE status='done' ORDER BY id DESC LIMIT 1", &[])
            .ok()
            .and_then(|v| v.as_array().and_then(|a| a.first()).and_then(|r| r.get("id")).and_then(Value::as_i64));
        if let Some(sid) = snap_id {
            // health (the blended headline the file review shows), law score as the fallback
            let rows = crate::db_json::query_rows(db, "SELECT path, COALESCE(health, score) AS h FROM judgments.review_scores WHERE snapshot_id=? AND unit_kind='file' ORDER BY h ASC, path ASC", &[json!(sid)])
                .ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
            let globs = json!(scope);
            let mut n = 0usize;
            for r in rows {
                let p = s(r.get("path")).replace('\\', "/");
                let h = r.get("h").and_then(Value::as_f64).unwrap_or(100.0);
                if h >= FILE_SCORE_MAX { break; }
                if !crate::cockpit__helpers_scope::inScope(&globs, &json!(p)).as_bool().unwrap_or(false) { continue; }
                items.push(json!({ "kind": "file", "path": p }));
                n += 1;
                if n >= MAX_FILES { break; }
            }
        }
        for f in &folders {
            items.push(json!({ "kind": "graph", "path": f }));
            items.push(json!({ "kind": "tests", "path": f }));
        }
    }

    let docs = crate::db_json::query_rows(db, "SELECT id FROM cockpit_blackboard WHERE session_id = ? AND kind = 'context' ORDER BY created_at, id", &[json!(session_id)])
        .ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    for d in docs {
        if let Some(id) = d.get("id").and_then(Value::as_i64) {
            if !context_excludes.contains(&id) { items.push(json!({ "kind": "doc", "id": id })); }
        }
    }
    items
}

/// The pane's edit whitelist as `pane/update` last wrote it (`scope_json`, the column the scope
/// sentry reads too — claims are only written at add time and go stale on a scope edit).
pub fn pane_scope(pane: &Value) -> Vec<String> {
    let raw = pane.get("scope_json");
    let v: Value = match raw {
        Some(Value::String(x)) => serde_json::from_str(x).unwrap_or(Value::Null),
        Some(o) => o.clone(),
        None => Value::Null,
    };
    v.as_array().map(|a| a.iter().map(|g| s(Some(g))).filter(|g| !g.is_empty()).collect()).unwrap_or_default()
}

/// The pane row → the pack it should get right now. `preset: scope` re-derives every time (so a
/// scope edit or a new judge run changes the pack); `custom` keeps the stored items; `none` is empty.
pub fn resolve_pack(db: &Connection, pane: &Value) -> Value {
    let parsed = parse_pack(pane.get("settings_json").unwrap_or(&Value::Null));
    let preset = s(parsed.get("preset"));
    let scope = pane_scope(pane);
    let excludes: Vec<i64> = crate::cockpit__helpers_context_brief::parseExcludes(pane.get("context_excludes_json").unwrap_or(&Value::Null))
        .as_array().map(|a| a.iter().filter_map(Value::as_i64).collect()).unwrap_or_default();
    let session_id = pane.get("session_id").and_then(Value::as_i64).unwrap_or(0);
    let items: Vec<Value> = match preset.as_str() {
        "none" => Vec::new(),
        "custom" => parsed.get("items").and_then(Value::as_array).cloned().unwrap_or_default(),
        _ => default_items(db, &scope, session_id, &excludes),
    };
    let mut out = parsed.as_object().cloned().unwrap_or_default();
    out.insert("items".into(), json!(items));
    out.insert("scope".into(), json!(scope));
    out.insert("default_rule".into(), json!(DEFAULT_RULE));
    Value::Object(out)
}

/// The identity of one bake: items + what they were rendered from. Same hash → same bytes →
/// the bake is skipped. Order-insensitive over items (a reordered selection is the same pack).
/// Bumped whenever the pack's FILE LAYOUT changes (manifest fields, INDEX wording, engine wiring) so
/// an already-baked pack is rewritten on the next bake even when nothing about the code moved.
pub const PACK_FORMAT: &str = "2026-09-16.scope-in-manifest";

pub fn pack_hash(items: &[Value], snapshot_id: Option<i64>, engineering_version: &str, constitution_hash: &str, head_sha: &str, budget: i64) -> String {
    let mut keys: Vec<String> = items.iter().map(|it| it.to_string()).collect();
    keys.sort();
    let mut h = Sha256::new();
    h.update(keys.join("\n").as_bytes());
    h.update(format!("\n#{}|{}|{}|{}|{}|{PACK_FORMAT}", snapshot_id.unwrap_or(0), engineering_version, constitution_hash, head_sha, budget).as_bytes());
    let out = h.finalize();
    out.iter().take(12).map(|b| format!("{b:02x}")).collect()
}

/// `src/ui/A.tsx` → `src__ui__A.tsx` — one flat file per item, path recoverable by eye.
pub fn slug(path: &str) -> String {
    norm_path(path).replace('/', "__")
}

/// The item's file inside `.antiloki/context/`. Any catalog source lands under `<kind>/<slug>.md`
/// (`graph` as JSON); args that narrow an item (a partial AST) are folded into the slug.
pub fn item_file(item: &Value) -> String {
    let kind = s(item.get("kind"));
    let path = s(item.get("path"));
    let args_tag = item.get("args").and_then(Value::as_object).filter(|a| !a.is_empty()).map(|a| {
        let mut bits: Vec<String> = a.iter().map(|(k, v)| format!("{k}-{}", s(Some(v)))).collect();
        bits.sort();
        format!("__{}", bits.join("__").chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' }).collect::<String>())
    }).unwrap_or_default();
    let base = if path.is_empty() { "all".to_string() } else { slug(&path) };
    match kind.as_str() {
        "laws" => "laws.md".into(),
        "doc" => format!("docs/{}.md", item.get("id").and_then(Value::as_i64).unwrap_or(0)),
        "folder" => format!("folders/{base}.md"),
        "file" => format!("files/{base}.md"),
        "graph" => format!("graph/{base}.json"),
        _ => format!("{kind}/{base}{args_tag}.md"),
    }
}

/// Write the pack back into a settings object (keeps every other key).
pub fn with_pack(settings: &Value, pack: &Value) -> Value {
    let mut m: Map<String, Value> = match settings {
        Value::String(raw) => serde_json::from_str::<Value>(raw).ok().and_then(|v| v.as_object().cloned()).unwrap_or_default(),
        Value::Object(o) => o.clone(),
        _ => Map::new(),
    };
    m.insert("context_pack".into(), pack.clone());
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_defaults_and_validates() {
        let p = parse_pack(&Value::Null);
        assert_eq!(p["preset"], json!("scope"));
        assert_eq!(p["live"], json!(true));
        assert_eq!(p["budget_chars"], json!(DEFAULT_BUDGET_CHARS));
        let p = parse_pack(&json!(r#"{"x":1,"context_pack":{"preset":"custom","items":[{"kind":"laws"},{"kind":"file","path":"./src/a.ts"},{"kind":"bogus"},{"kind":"folder"},{"kind":"doc","id":4},{"kind":"ast","path":"src/a.ts","args":{"symbol":"go"}},{"kind":"git"},{"kind":"describe","path":"x"}],"live":false,"budget_chars":10}}"#));
        assert_eq!(p["preset"], json!("custom"));
        assert_eq!(p["items"], json!([{ "kind": "laws" }, { "kind": "file", "path": "src/a.ts" }, { "kind": "doc", "id": 4 }, { "kind": "ast", "path": "src/a.ts", "args": { "symbol": "go" } }, { "kind": "git" }]), "AI sources are not packable");
        assert_eq!(p["live"], json!(false));
        assert_eq!(p["budget_chars"], json!(10));
        assert_eq!(p["warnings"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn scope_folders_dedupe_and_drop_whole_repo() {
        let f = scope_folders(&["src/ui/**", "src/ui/A.tsx", "**", "lib/x/*.ts", "src/ui"].map(String::from));
        assert_eq!(f, vec!["src/ui".to_string(), "lib/x".to_string()]);
    }

    #[test]
    fn hash_is_order_insensitive_and_input_sensitive() {
        let a = [json!({"kind":"laws"}), json!({"kind":"file","path":"a"})];
        let b = [json!({"kind":"file","path":"a"}), json!({"kind":"laws"})];
        assert_eq!(pack_hash(&a, Some(1), "v3", "h", "sha", 1), pack_hash(&b, Some(1), "v3", "h", "sha", 1));
        assert_ne!(pack_hash(&a, Some(1), "v3", "h", "sha", 1), pack_hash(&a, Some(2), "v3", "h", "sha", 1));
        assert_ne!(pack_hash(&a, Some(1), "v3", "h", "sha", 1), pack_hash(&a, Some(1), "v3", "h", "sha2", 1));
    }

    #[test]
    fn item_files_are_flat_and_recoverable() {
        assert_eq!(item_file(&json!({"kind":"file","path":"src/ui/A.tsx"})), "files/src__ui__A.tsx.md");
        assert_eq!(item_file(&json!({"kind":"graph","path":"src/ui"})), "graph/src__ui.json");
        assert_eq!(item_file(&json!({"kind":"doc","id":7})), "docs/7.md");
        assert_eq!(item_file(&json!({"kind":"laws"})), "laws.md");
        assert_eq!(item_file(&json!({"kind":"ast","path":"src/ui/A.tsx","args":{"symbol":"tabify"}})), "ast/src__ui__A.tsx__symbol-tabify.md");
        assert_eq!(item_file(&json!({"kind":"git"})), "git/all.md");
    }

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments").unwrap();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        c
    }

    #[test]
    fn default_items_follow_the_printed_rule() {
        let c = conn();
        c.execute("INSERT INTO judgments.review_snapshots (id, created_at, scope_json, manifest_json, constitution_hash, status) VALUES (1,'2026-01-01','{}','[]','h','done')", []).unwrap();
        for (p, sc) in [("src/ui/A.tsx", 42), ("src/ui/B.tsx", 90), ("src/ui/C.tsx", 79), ("src/other/D.tsx", 10)] {
            c.execute("INSERT INTO judgments.review_scores (snapshot_id, unit_kind, path, score, health, archetype, axes_json) VALUES (1,'file',?1,?2,?2,'logic','{}')", rusqlite::params![p, sc]).unwrap();
        }
        c.execute("INSERT INTO cockpit_sessions (id, name) VALUES (1, 's')", []).unwrap();
        c.execute("INSERT INTO cockpit_blackboard (session_id, kind, body_md) VALUES (1, 'context', 'doc one')", []).unwrap();
        c.execute("INSERT INTO cockpit_blackboard (session_id, kind, body_md) VALUES (1, 'context', 'doc two')", []).unwrap();
        c.execute("INSERT INTO cockpit_blackboard (session_id, kind, body_md) VALUES (1, 'note', 'not a doc')", []).unwrap();
        let items = default_items(&c, &["src/ui/**".to_string()], 1, &[2]);
        assert_eq!(items, json!([
            { "kind": "laws" },
            { "kind": "folder", "path": "src/ui" },
            { "kind": "file", "path": "src/ui/A.tsx" },
            { "kind": "file", "path": "src/ui/C.tsx" },
            { "kind": "graph", "path": "src/ui" },
            { "kind": "tests", "path": "src/ui" },
            { "kind": "doc", "id": 1 },
        ]).as_array().unwrap().clone(), "worst-first, < 80 only, out-of-scope D excluded, doc 2 excluded, notes ignored");

        // empty scope → laws + docs only
        let items = default_items(&c, &[], 1, &[]);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["kind"], json!("laws"));
        assert_eq!(items[1]["kind"], json!("doc"));
    }

    #[test]
    fn resolve_pack_honours_preset() {
        let c = conn();
        c.execute("INSERT INTO cockpit_sessions (id, name) VALUES (1, 's')", []).unwrap();
        let pane = json!({ "id": 5, "session_id": 1, "scope_json": "[\"src/ui/**\"]", "settings_json": "{\"context_pack\":{\"preset\":\"none\"}}" });
        assert_eq!(resolve_pack(&c, &pane)["items"], json!([]));
        let pane = json!({ "id": 5, "session_id": 1, "scope_json": "[\"src/ui/**\"]", "settings_json": "{\"context_pack\":{\"preset\":\"custom\",\"items\":[{\"kind\":\"file\",\"path\":\"x.ts\"}]}}" });
        assert_eq!(resolve_pack(&c, &pane)["items"], json!([{ "kind": "file", "path": "x.ts" }]));
        let pane = json!({ "id": 5, "session_id": 1, "scope_json": "[\"src/ui/**\"]", "settings_json": Value::Null });
        let r = resolve_pack(&c, &pane);
        assert_eq!(r["preset"], json!("scope"));
        assert_eq!(r["items"][0], json!({ "kind": "laws" }));
        assert_eq!(r["items"][1], json!({ "kind": "folder", "path": "src/ui" }));
        assert_eq!(r["scope"], json!(["src/ui/**"]));
    }
}
