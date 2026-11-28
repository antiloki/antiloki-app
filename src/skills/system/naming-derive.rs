//! naming-derive.rs — twin of skills/system/naming-derive.mjs (db lane, default export
//! `namingDerive`; named exports `missionWords`, `appPrefix`, `deriveName`, `standardSet`,
//! `alignFromAcceptance`). NAMES ARE CONTRACT: identifiers kept exactly as JS.
//!
//! Deterministic identifier derivation from mission words + conventions (LESS_COGNITION_MORE_
//! DETERMINISTIC): a pure function, not a model call. Two modes on the default export:
//!   { kind, mission, app? }  → one derived name (pure, `app` resolved from the workspace when
//!                              db-backed and not given explicitly)
//!   { contract_slug }        → the per-contract standard identifier set, persisted once into
//!                              contracts.naming_json and returned idempotently thereafter.
//!
//! `appPrefix(root)` does a REAL filesystem read (`readFileSync(join(root,'package.json'))`) —
//! per CONVERSION-PLAYBOOK lesson 3 this is unverifiable through the normal conform-db fixture
//! harness (no fs-mutation/fs-content injection in the fixture format), so this whole module's
//! testing lane is bespoke-native: the `#[cfg(test)]` block below drives real functions directly
//! (including real tempdir `package.json` files for `appPrefix`), not the fixture harness.
//!
//! Cross-module dependency: `getWorkspaceRoot(db)` from the sibling twin of
//! skills/engine/workspace-root.mjs, converted in this same batch — called here as
//! `crate::engine_workspace_root::getWorkspaceRoot(db)`, assumed (per the playbook's db-first
//! helper convention: `fn(db, ...) -> Value`) to return the resolved root path as a JSON string
//! and to never itself throw (its own JS never throws — `getActiveWorkspace` swallows the
//! missing-table case and falls back to `SUBSTRATE_ROOT`). If the sibling twin's actual
//! signature differs (e.g. `Result<Value, String>`), the call site below needs a one-line
//! adjustment when the batch is wired into lib.rs.
//!
//! `console.log(...)` in the JS `realign` branch is a side-effect-only log line — not
//! reproduced (no observable effect on the returned JSON or db state).
//!
//! Disclosed gaps (documented at the call site too):
//!  - `missionWords(mission, cap = 4)`: the JS default parameter fires only when the 2nd arg is
//!    literally `undefined` (omitted). JSON has no `undefined`; a JSON `null` 2nd arg is treated
//!    here the same as "omitted" (default 4) rather than JS's real behavior for an *explicit*
//!    `null` (`Array.prototype.slice(0, null)` → `[]`). No real caller ever passes a literal
//!    `null` cap (every call site either omits it or passes a real number), so this is inert in
//!    practice.
//!  - `context.db missing` (`err.contextMissing('naming-derive', 'db')`, JS message
//!    "naming-derive: context.db is required"): the Rust signature always carries a real
//!    `&Connection`, so this branch has no Rust equivalent — documented, not reproduced.

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// ── standard helpers (CONVERSION-PLAYBOOK §"Standard helper functions") ──
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
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}

// mission → significant lowercase words (bounded — names must stay short).
const STOP: &[&str] = &[
    "with", "from", "that", "this", "into", "have", "when", "your", "them", "then", "the", "a",
    "an", "and", "for", "of", "to", "in", "on", "card", "feature", "feat", "fix", "add", "new",
];

fn mission_words_impl(mission: Option<&Value>, cap: usize) -> Vec<String> {
    let s = str_qq(mission).to_lowercase();
    let re = Regex::new(r"[a-z][a-z0-9]+").unwrap();
    re.find_iter(&s)
        .map(|m| m.as_str().to_string())
        .filter(|w| !STOP.contains(&w.as_str()))
        .take(cap)
        .collect()
}

/// JS: `export function missionWords(mission, cap = 4)`. Positional no-db helper.
pub fn missionWords(mission: &Value, cap: &Value) -> Value {
    // see module doc "Disclosed gaps" — JSON `null` treated as "omitted" (default 4).
    let cap_n: usize = match cap {
        Value::Null => 4,
        Value::Number(n) => n.as_f64().map(|f| if f.is_finite() && f > 0.0 { f as usize } else { 0 }).unwrap_or(4),
        _ => 4,
    };
    json!(mission_words_impl(Some(mission), cap_n))
}

fn kebab(ws: &[String]) -> String {
    ws.join("-")
}
fn pascal(ws: &[String]) -> String {
    ws.iter()
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("")
}
fn scream(ws: &[String]) -> String {
    ws.iter().map(|w| w.to_uppercase()).collect::<Vec<_>>().join("_")
}

/// JS: `export function appPrefix(root)`. Real fs read of `<root>/package.json` — see module
/// doc: bespoke-native testing lane, not conform-db fixturable. Whole body is one JS try/catch —
/// ANY failure (non-string root, missing file, invalid JSON, empty cleaned name) → `'app'`.
pub fn appPrefix(root: &Value) -> Value {
    let root_str = match root.as_str() {
        Some(s) => s,
        None => return json!("app"),
    };
    let pkg_path = std::path::Path::new(root_str).join("package.json");
    let text = match std::fs::read_to_string(&pkg_path) {
        Ok(t) => t,
        Err(_) => return json!("app"),
    };
    let pkg: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return json!("app"),
    };
    let name = pkg.get("name"); // pkg?.name (pkg may not even be an object — .get() then None)
    let raw0 = str_qq(name); // String(pkg?.name ?? '')
    let scope_re = Regex::new(r"^@[^/]+/").unwrap();
    let raw = scope_re.replace(&raw0, "").trim().to_lowercase();
    let clean_re = Regex::new(r"[^a-z0-9-]+").unwrap();
    let dashed = clean_re.replace_all(&raw, "-");
    let dash_trim_re = Regex::new(r"^-+|-+$").unwrap();
    let clean = dash_trim_re.replace_all(&dashed, "").into_owned();
    if !clean.is_empty() {
        json!(clean)
    } else {
        json!("app")
    }
}

/// JS: `export function deriveName({ kind, mission, app = 'app' })`. Single-object no-db helper.
/// `app` default 'app' fires only when the `app` key is absent (JS `undefined`-default
/// semantics) — an explicit JSON `null` `app` key is passed through untouched, same as JS.
pub fn deriveName(input: &Value) -> Value {
    let mission = input.get("mission");
    let ws = mission_words_impl(mission, 4); // JS calls missionWords(mission) — no cap → default 4
    if ws.is_empty() {
        return Value::Null;
    }
    let kind = str_qq(input.get("kind")).to_lowercase();
    let app_opt = input.get("app");
    let app: Value = match app_opt {
        None => json!("app"),
        Some(v) => v.clone(),
    };
    let app_s = tmpl(Some(&app));

    let (name, rationale): (String, &str) = match kind.as_str() {
        "storage-key" => (
            format!("{app_s}:{}", kebab(&ws)),
            "namespaced <app>:<feature> — collision-proof across apps on the same origin",
        ),
        "component" => (pascal(&ws), "PascalCase component from the mission words"),
        "hook" => (format!("use{}", pascal(&ws)), "React hook convention use<Feature>"),
        "const" => (scream(&ws), "SCREAMING_SNAKE constant from the mission words"),
        "event" => (format!("{app_s}:{}", kebab(&ws)), "namespaced event name"),
        "css-class" => (kebab(&ws), "kebab-case class from the mission words"),
        "branch" => (format!("antiloki/{}", kebab(&ws)), "the driver worktree branch convention"),
        "slug" => (kebab(&ws), "kebab-case slug"),
        _ => return Value::Null,
    };
    let mut out = Map::new();
    out.insert("kind".into(), json!(kind));
    out.insert("name".into(), json!(name));
    out.insert("rationale".into(), json!(rationale));
    Value::Object(out)
}

/// JS: `export function standardSet({ title, app })`. Single-object no-db helper. `app` has NO
/// default here — if the `app` key is absent, the JS result OMITS the `app` key entirely (`{app,
/// ...}` with `app === undefined` is dropped by JSON.stringify) while `${app}` inside
/// `storage_key.value`/`event_prefix` still template-stringifies to the literal `"undefined"`.
pub fn standardSet(input: &Value) -> Value {
    let title = input.get("title");
    let ws = mission_words_impl(title, 4); // JS: missionWords(title) — default cap 4
    if ws.is_empty() {
        return Value::Null;
    }
    let feature = kebab(&ws);
    let app_opt = input.get("app");
    let app_s = tmpl(app_opt); // `${app}` — "undefined" when the key is absent

    let mut out = Map::new();
    if let Some(v) = app_opt {
        out.insert("app".into(), v.clone());
    } // else: undefined → key omitted, matching JSON.stringify
    out.insert("feature".into(), json!(feature));
    out.insert("component".into(), json!(pascal(&ws)));
    out.insert("hook".into(), json!(format!("use{}", pascal(&ws))));
    let mut storage_key = Map::new();
    storage_key.insert("const".into(), json!("STORAGE_KEY"));
    storage_key.insert("value".into(), json!(format!("{app_s}:{feature}")));
    out.insert("storage_key".into(), Value::Object(storage_key));
    out.insert("event_prefix".into(), json!(format!("{app_s}:{feature}")));
    out.insert("css_class".into(), json!(feature));
    Value::Object(out)
}

/// JS: `export function alignFromAcceptance(db, slug, names)`. db-first helper. Mutates `names`
/// in place (fields the acceptance text cites as authoritative) and returns the list of changed
/// field names. Whole body is one try/catch — any db error swallows to "no changes".
pub fn alignFromAcceptance(db: &Connection, slug: &Value, names: &mut Value) -> Value {
    let slug_s = str_qq(Some(slug));
    let mut changed: Vec<String> = Vec::new();

    let rows = match crate::db_json::query_rows(
        db,
        "SELECT acceptance_json, goal_md FROM contracts WHERE slug = ?",
        &[json!(slug_s)],
    ) {
        Ok(r) => r,
        Err(_) => return json!(changed),
    };
    let row = rows.as_array().and_then(|a| a.first());
    // `${row?.acceptance_json ?? ''}\n${row?.goal_md ?? ''}` — both columns are TEXT/NULL, so
    // `?? ''` and "missing/non-string → ''" coincide.
    let acceptance = row.and_then(|r| r.get("acceptance_json")).and_then(|v| v.as_str()).unwrap_or("");
    let goal_md = row.and_then(|r| r.get("goal_md")).and_then(|v| v.as_str()).unwrap_or("");
    let text = format!("{acceptance}\n{goal_md}");

    let comp_re1 =
        Regex::new(r"(?:src|app|pages|components?)[/\\][\w/\\.-]*?([A-Z][A-Za-z0-9]+)\.(?:jsx|tsx|js|ts)\b").unwrap();
    let comp_re2 = Regex::new(
        r"(?:src|app|pages|components?)[/\\](?:[\w.-]+[/\\])*([A-Z][A-Za-z0-9]+)[/\\]index\.(?:jsx|tsx|js|ts)\b",
    )
    .unwrap();
    let comp_match = comp_re1.captures(&text).or_else(|| comp_re2.captures(&text));
    if let Some(caps) = comp_match {
        if let Some(g1) = caps.get(1) {
            let comp = g1.as_str().to_string();
            let cur = names.get("component").and_then(|v| v.as_str());
            if cur != Some(comp.as_str()) {
                if let Value::Object(map) = names {
                    map.insert("component".into(), json!(comp));
                    map.insert("hook".into(), json!(format!("use{comp}")));
                }
                changed.push("component".into());
            }
        }
    }

    let km_re = Regex::new(r#"localStorage[^'"]{0,80}['"]([A-Za-z0-9_.:\-]{3,64})['"]"#).unwrap();
    if let Some(caps) = km_re.captures(&text) {
        if let Some(g1) = caps.get(1) {
            let key = g1.as_str().to_string();
            let cur = names.get("storage_key").and_then(|sk| sk.get("value")).and_then(|v| v.as_str());
            if cur != Some(key.as_str()) {
                if let Value::Object(map) = names {
                    let mut sk = Map::new();
                    sk.insert("const".into(), json!("STORAGE_KEY"));
                    sk.insert("value".into(), json!(key));
                    map.insert("storage_key".into(), Value::Object(sk));
                }
                changed.push("storage_key".into());
            }
        }
    }
    json!(changed)
}

// Resolve `app` the way `getWorkspaceRoot(db)` + `appPrefix(root)` would, tolerating either a
// plain-Value or Result-returning sibling twin signature without hard-coding one (see module doc
// "Cross-module dependency").
fn resolve_workspace_app(db: &Connection) -> Value {
    let root = crate::engine_workspace_root::getWorkspaceRoot(db);
    appPrefix(&root)
}

/// JS: `export default function namingDerive(input, context)`. Standard lane.
pub fn namingDerive(input: &Value, db: &Connection) -> Result<Value, String> {
    // ── single-name mode (pure; app resolved from the workspace when not given explicitly) ──
    if truthy(input.get("kind")) {
        let mission = input.get("mission");
        if !truthy(mission) {
            return Err("naming-derive: mission is required".into());
        }
        // `let app = input?.app ?? 'app';` then, only `if (!input?.app && db)`, overridden.
        let app_field = input.get("app");
        let mut app: Value = match app_field {
            None | Some(Value::Null) => json!("app"),
            Some(v) => v.clone(),
        };
        if !truthy(app_field) {
            // db is always present on the Rust side (no `&& db` guard needed); JS wraps this in
            // try/catch to "keep default" — both callee fns already self-catch, so no failure path.
            app = resolve_workspace_app(db);
        }
        let derive_input = json!({
            "kind": input.get("kind").cloned().unwrap_or(Value::Null),
            "mission": mission.cloned().unwrap_or(Value::Null),
            "app": app.clone(),
        });
        let r = deriveName(&derive_input);
        if r.is_null() {
            let kind_s = tmpl(input.get("kind"));
            return Err(format!(
                "naming-derive: a derivable kind+mission (kind '{kind_s}' unknown or mission empty) is required"
            ));
        }
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        if let Value::Object(m) = r {
            for (k, v) in m {
                out.insert(k, v);
            }
        }
        out.insert("app".into(), app);
        return Ok(Value::Object(out));
    }

    // ── contract-authority mode ──
    let slug_field = input.get("contract_slug");
    if !truthy(slug_field) {
        return Err("naming-derive: kind+mission OR contract_slug is required".into());
    }
    let slug = str_qq(slug_field);
    // JS: `if (!db) throw err.contextMissing(...)` — no Rust equivalent, db always present.

    let rows = crate::db_json::query_rows(
        db,
        "SELECT slug, title, card_slug, naming_json FROM contracts WHERE slug = ?",
        &[json!(slug.clone())],
    )?;
    let contract = match rows.as_array().and_then(|a| a.first()) {
        Some(c) => c.clone(),
        None => return Err(format!("naming-derive: contract '{slug}' not found")),
    };

    if input.get("op").and_then(|v| v.as_str()) == Some("realign") {
        let raw = contract.get("naming_json").and_then(|v| v.as_str()).unwrap_or("{}");
        let parsed: Value = serde_json::from_str(raw).unwrap_or(json!({}));
        let mut existing = if truthy(Some(&parsed)) { parsed } else { json!({}) };
        let keys_len = match &existing {
            Value::Object(m) => m.len(),
            Value::Array(a) => a.len(),
            _ => 0,
        };
        if keys_len == 0 {
            let mut out = Map::new();
            out.insert("ok".into(), json!(true));
            out.insert("contract_slug".into(), json!(slug));
            out.insert("names".into(), Value::Null);
            out.insert("realigned".into(), json!([]));
            out.insert("reason".into(), json!("no decided set yet — nothing to realign"));
            return Ok(Value::Object(out));
        }
        let realigned = alignFromAcceptance(db, &json!(slug.clone()), &mut existing);
        if realigned.as_array().map_or(false, |a| !a.is_empty()) {
            crate::db_json::run_write(
                db,
                "UPDATE contracts SET naming_json = ?, updated_at = datetime('2026-01-01 00:00:00') WHERE slug = ?",
                &[json!(serde_json::to_string(&existing).map_err(|e| e.to_string())?), json!(slug.clone())],
            )?;
            // console.log(...) — logging only, no observable effect on return value/db state.
        }
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("contract_slug".into(), json!(slug));
        out.insert("names".into(), existing);
        out.insert("realigned".into(), realigned);
        return Ok(Value::Object(out));
    }

    // Idempotent: an already-decided set is THE answer — never re-derive over it.
    // JS: `typeof existing === 'object'` is true for null too, but `Object.keys(null)` THROWS,
    // caught by the outer try/catch → falls through to re-derive. Rust: skip early-return when
    // parse fails or the parsed value isn't a non-empty object/array.
    if let Some(raw) = contract.get("naming_json").and_then(|v| v.as_str()) {
        if let Ok(existing) = serde_json::from_str::<Value>(raw) {
            let objectish_len = match &existing {
                Value::Object(m) => Some(m.len()),
                Value::Array(a) => Some(a.len()),
                _ => None,
            };
            if let Some(len) = objectish_len {
                if len > 0 {
                    let mut out = Map::new();
                    out.insert("ok".into(), json!(true));
                    out.insert("contract_slug".into(), json!(slug));
                    out.insert("names".into(), existing);
                    out.insert("ensured".into(), json!(false));
                    return Ok(Value::Object(out));
                }
            }
        }
    }

    let app = resolve_workspace_app(db); // `let app = 'app'; try { app = appPrefix(getWorkspaceRoot(db)); } catch {}`

    let card_slug = contract.get("card_slug");
    let card: Option<Value> = if truthy(card_slug) {
        let cs = str_qq(card_slug);
        let rows = crate::db_json::query_rows(db, "SELECT title, slug FROM cards WHERE slug = ?", &[json!(cs)])?;
        rows.as_array().and_then(|a| a.first()).cloned()
    } else {
        None
    };
    // title = card?.title ?? contract.title ?? contract.card_slug ?? slug
    let title: Value = nz(card.as_ref().and_then(|c| c.get("title")))
        .or_else(|| nz(contract.get("title")))
        .or_else(|| nz(contract.get("card_slug")))
        .cloned()
        .unwrap_or(json!(slug.clone()));

    let names_input = json!({ "title": title, "app": app.clone() });
    let names = standardSet(&names_input);
    if names.is_null() {
        let mut out = Map::new();
        out.insert("ok".into(), json!(false));
        out.insert("contract_slug".into(), json!(slug));
        out.insert("names".into(), Value::Null);
        out.insert("ensured".into(), json!(false));
        out.insert("reason".into(), json!("no derivable mission words"));
        return Ok(Value::Object(out));
    }

    let mut names = names;
    alignFromAcceptance(db, &json!(slug.clone()), &mut names);

    crate::db_json::run_write(
        db,
        "UPDATE contracts SET naming_json = ?, updated_at = datetime('2026-01-01 00:00:00') WHERE slug = ?",
        &[json!(serde_json::to_string(&names).map_err(|e| e.to_string())?), json!(slug.clone())],
    )?;

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("contract_slug".into(), json!(slug));
    out.insert("names".into(), names);
    out.insert("ensured".into(), json!(true));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (
                slug TEXT PRIMARY KEY, title TEXT, card_slug TEXT,
                naming_json TEXT NOT NULL DEFAULT '{}',
                acceptance_json TEXT NOT NULL DEFAULT '[]',
                goal_md TEXT, updated_at TEXT
             );
             CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn mission_words_stop_words_drop_output_bounded() {
        let r = missionWords(&json!("add a new centered click me button for the counter"), &Value::Null);
        assert_eq!(r, json!(["centered", "click", "me", "button"]));
    }

    #[test]
    fn mission_words_explicit_cap_overrides_default() {
        let r = missionWords(&json!("centered click me button counter"), &json!(2));
        assert_eq!(r, json!(["centered", "click"]));
    }

    #[test]
    fn derive_name_every_kind_follows_its_convention() {
        let app = "meli";
        let mission = "centered Click Me button with localStorage count";
        let mk = |kind: &str| deriveName(&json!({ "kind": kind, "mission": mission, "app": app }));
        assert_eq!(mk("storage-key")["name"], json!("meli:centered-click-me-button"));
        assert_eq!(mk("component")["name"], json!("CenteredClickMeButton"));
        assert_eq!(mk("hook")["name"], json!("useCenteredClickMeButton"));
        assert_eq!(mk("const")["name"], json!("CENTERED_CLICK_ME_BUTTON"));
        assert_eq!(mk("css-class")["name"], json!("centered-click-me-button"));
        assert_eq!(mk("branch")["name"], json!("antiloki/centered-click-me-button"));
        assert_eq!(mk("slug")["name"], json!("centered-click-me-button"));
        assert_eq!(mk("event")["name"], json!("meli:centered-click-me-button"));
        assert_eq!(deriveName(&json!({ "kind": "nonsense", "mission": mission })), Value::Null);
        assert_eq!(deriveName(&json!({ "kind": "slug", "mission": "the and of" })), Value::Null);
    }

    #[test]
    fn standard_set_per_contract_identifier_set() {
        let s = standardSet(&json!({ "title": "Centered Click Me button", "app": "meli" }));
        assert_eq!(s["component"], json!("CenteredClickMeButton"));
        assert_eq!(s["storage_key"], json!({ "const": "STORAGE_KEY", "value": "meli:centered-click-me-button" }));
        assert_eq!(s["css_class"], json!("centered-click-me-button"));
    }

    #[test]
    fn standard_set_omits_app_key_when_absent() {
        let s = standardSet(&json!({ "title": "Centered Click Me button" }));
        assert!(s.as_object().unwrap().get("app").is_none());
        assert_eq!(s["storage_key"]["value"], json!("undefined:centered-click-me-button"));
    }

    #[test]
    fn app_prefix_reads_and_sanitizes_package_json() {
        let dir = std::env::temp_dir().join(format!("antiloki-naming-derive-rs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("package.json"), r#"{"name":"@scope/My_Cool App"}"#).unwrap();
        let r = appPrefix(&json!(dir.to_string_lossy().into_owned()));
        assert_eq!(r, json!("my-cool-app"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn app_prefix_falls_back_to_app_on_missing_manifest() {
        let dir = std::env::temp_dir().join(format!("antiloki-naming-derive-rs-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let r = appPrefix(&json!(dir.to_string_lossy().into_owned()));
        assert_eq!(r, json!("app"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn contract_mode_derives_once_persists_and_is_idempotent() {
        let db = make_db();
        db.execute("INSERT INTO cards (slug, title, status) VALUES ('CARD-N','Centered Click Me button','doing')", [])
            .unwrap();
        db.execute(
            "INSERT INTO contracts (slug, card_slug, title, acceptance_json, naming_json) VALUES ('CARD-N-C1','CARD-N','t','[]','{}')",
            [],
        )
        .unwrap();

        let a = namingDerive(&json!({ "contract_slug": "CARD-N-C1" }), &db).unwrap();
        assert_eq!(a["ensured"], json!(true));
        assert_eq!(a["names"]["component"], json!("CenteredClickMeButton"));

        let stored: String = db.query_row("SELECT naming_json FROM contracts WHERE slug='CARD-N-C1'", [], |r| r.get(0)).unwrap();
        let stored: Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(stored["storage_key"]["const"], json!("STORAGE_KEY"));

        let mut overridden = stored.clone();
        overridden["component"] = json!("OperatorRenamedIt");
        db.execute(
            "UPDATE contracts SET naming_json = ?1 WHERE slug='CARD-N-C1'",
            [serde_json::to_string(&overridden).unwrap()],
        )
        .unwrap();
        let b = namingDerive(&json!({ "contract_slug": "CARD-N-C1" }), &db).unwrap();
        assert_eq!(b["ensured"], json!(false));
        assert_eq!(b["names"]["component"], json!("OperatorRenamedIt"));
    }

    #[test]
    fn single_name_mode_requires_known_kind_and_mission_words() {
        let db = make_db();
        let err = namingDerive(&json!({ "kind": "nonsense", "mission": "x y z" }), &db).unwrap_err();
        assert!(err.contains("derivable"), "{err}");
    }

    #[test]
    fn missing_mission_and_missing_kind_and_slug_throw_exact_messages() {
        let db = make_db();
        assert_eq!(
            namingDerive(&json!({}), &db).unwrap_err(),
            "naming-derive: kind+mission OR contract_slug is required"
        );
        assert_eq!(
            namingDerive(&json!({ "kind": "storage-key" }), &db).unwrap_err(),
            "naming-derive: mission is required"
        );
        assert_eq!(
            namingDerive(&json!({ "contract_slug": "ghost" }), &db).unwrap_err(),
            "naming-derive: contract 'ghost' not found"
        );
    }

    #[test]
    fn single_name_mode_pure_when_app_given_explicitly() {
        let db = make_db();
        let r = namingDerive(&json!({ "kind": "component", "mission": "click me button", "app": "meli" }), &db).unwrap();
        assert_eq!(r["name"], json!("ClickMeButton"));
        assert_eq!(r["app"], json!("meli"));
    }

    #[test]
    fn acceptance_cited_localstorage_key_overrides_derived_key() {
        let db = make_db();
        db.execute(
            "INSERT INTO cards (slug, title, status) VALUES ('CARD-K','Centered Click Me button with localStorage','doing')",
            [],
        )
        .unwrap();
        let acceptance = serde_json::to_string(&json!([
            { "text": "Component uses useState hook initialized with value from localStorage.getItem('click_me_button_count')" },
            { "text": "Click handler calls localStorage.setItem('click_me_button_count', newCount)" },
        ]))
        .unwrap();
        db.execute(
            "INSERT INTO contracts (slug, card_slug, title, acceptance_json, naming_json) VALUES ('CARD-K-C1','CARD-K','t',?1,'{}')",
            [acceptance],
        )
        .unwrap();
        let r = namingDerive(&json!({ "contract_slug": "CARD-K-C1" }), &db).unwrap();
        assert_eq!(r["names"]["storage_key"]["value"], json!("click_me_button_count"));
        let stored: String = db.query_row("SELECT naming_json FROM contracts WHERE slug='CARD-K-C1'", [], |row| row.get(0)).unwrap();
        assert!(stored.contains("click_me_button_count"));
    }

    #[test]
    fn prose_form_key_and_directory_form_component_align_r21_class() {
        let db = make_db();
        db.execute(
            "INSERT INTO cards (slug, title, status) VALUES ('CARD-P','Centered Click Me button with localStorage count','doing')",
            [],
        )
        .unwrap();
        let acceptance = serde_json::to_string(&json!([
            { "intent": "Use the exact localStorage key 'click_me_button_count'", "done_criteria": "grep" },
            { "intent": "the design creates the file src/components/ClickMeButton/index.jsx" },
        ]))
        .unwrap();
        db.execute(
            "INSERT INTO contracts (slug, card_slug, title, acceptance_json, naming_json) VALUES ('CARD-P-C1','CARD-P','t',?1,'{}')",
            [acceptance],
        )
        .unwrap();
        let r = namingDerive(&json!({ "contract_slug": "CARD-P-C1" }), &db).unwrap();
        assert_eq!(r["names"]["storage_key"]["value"], json!("click_me_button_count"));
        assert_eq!(r["names"]["component"], json!("ClickMeButton"));
    }

    #[test]
    fn realign_self_heals_poisoned_set_from_acceptance_only_cited_fields() {
        let db = make_db();
        db.execute("INSERT INTO cards (slug, title, status) VALUES ('CARD-R','x','doing')", []).unwrap();
        let acceptance =
            serde_json::to_string(&json!([{ "intent": "Use the exact localStorage key 'click_me_button_count'" }])).unwrap();
        let poisoned = serde_json::to_string(&json!({
            "app": "setup", "feature": "centered-click-me-button", "component": "OperatorChoseThis",
            "storage_key": { "const": "STORAGE_KEY", "value": "setup:centered-click-me-button" }
        }))
        .unwrap();
        db.execute(
            "INSERT INTO contracts (slug, card_slug, title, acceptance_json, naming_json) VALUES ('CARD-R-C1','CARD-R','t',?1,?2)",
            [acceptance, poisoned],
        )
        .unwrap();
        let r = namingDerive(&json!({ "contract_slug": "CARD-R-C1", "op": "realign" }), &db).unwrap();
        assert_eq!(r["realigned"], json!(["storage_key"]));
        assert_eq!(r["names"]["storage_key"]["value"], json!("click_me_button_count"));
        assert_eq!(r["names"]["component"], json!("OperatorChoseThis"));
    }

    #[test]
    fn realign_with_no_decided_set_reports_nothing_to_realign() {
        let db = make_db();
        db.execute(
            "INSERT INTO contracts (slug, title, acceptance_json, naming_json) VALUES ('CARD-E-C1','t','[]','{}')",
            [],
        )
        .unwrap();
        let r = namingDerive(&json!({ "contract_slug": "CARD-E-C1", "op": "realign" }), &db).unwrap();
        assert_eq!(r["names"], Value::Null);
        assert_eq!(r["realigned"], json!([]));
    }

    #[test]
    fn align_from_acceptance_mutates_in_place_and_returns_changed_fields() {
        let db = make_db();
        let acceptance =
            serde_json::to_string(&json!([{ "text": "src/components/Foo/index.tsx and localStorage.getItem('bar_key')" }])).unwrap();
        db.execute(
            "INSERT INTO contracts (slug, title, acceptance_json, naming_json) VALUES ('CARD-A-C1','t',?1,'{}')",
            [acceptance],
        )
        .unwrap();
        let mut names = json!({ "component": "Old", "hook": "useOld", "storage_key": { "const": "STORAGE_KEY", "value": "old:val" } });
        let changed = alignFromAcceptance(&db, &json!("CARD-A-C1"), &mut names);
        assert_eq!(changed, json!(["component", "storage_key"]));
        assert_eq!(names["component"], json!("Foo"));
        assert_eq!(names["hook"], json!("useFoo"));
        assert_eq!(names["storage_key"]["value"], json!("bar_key"));
    }
}
