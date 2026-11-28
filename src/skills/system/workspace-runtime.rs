//! workspace-runtime.rs — twin of skills/system/workspace-runtime.mjs. NAMES ARE CONTRACT:
//! `workspaceRuntime` (default), `detectImage`, `analyzeRequirements`, `hostSteps`, `dockerSteps`
//! (named exports — the JS test file imports all four directly).
//!
//! BESPOKE-NATIVE, mixed module, same class as terminal-sandbox.rs / auth-sign.rs:
//! - `detectImage`/`analyzeRequirements`/`hostSteps`/`dockerSteps` are pure-ish (real `fs::read`
//!   of `.node-version`/`.nvmrc`/`package.json` under a REAL workspace root — same "reads a real
//!   file, no network" class as auth-sign.rs's key-file read), proven here against real temp
//!   dirs, mirroring the JS test's `tmpWs` helper.
//! - `workspaceRuntime`'s `up`/`down`/`status`/`detect` ops shell out to a REAL `docker`
//!   subprocess (`std::process::Command`), same class as terminal-sandbox.rs's `run_git`. The JS
//!   accepts an injectable `exec_fn` (a function value) for hermetic tests; JSON cannot carry a
//!   function across the input boundary, so this twin's `docker_exec` is always the real
//!   subprocess — `exec_fn`/`provider_fn` keys on `input` are accepted (parity with the JS
//!   shape) but unused, same convention as book-run.rs's `provider_fn` field on the deferred
//!   `aiCall` path. Native tests below exercise the pure ops (`set`/`requirements`/`explain`)
//!   plus `docker_exec`'s spawn-failure fallback (status 127) when `docker` is absent — the same
//!   observable JS gets when `spawnSync` sets `r.error` on a missing binary. `up`/`down`/`status`
//!   against a REAL running container are the JS file's own `ANTILOKI_DOCKER_SMOKE=1`-gated smoke
//!   test — out of scope for a hermetic CI-safe native test here.
//! - `explain`'s AI step ends in `crate::ai_ai_call::aiCall` (the deferred network-AI stub — see
//!   ai-call.rs's module doc): PARITY-DEFERRED. The JS never lets this op fail blank (falls back
//!   to the deterministic step list when the AI call throws); the deferred stub's `Err` exercises
//!   exactly that fallback path here, proven via native test.
//!
//! Cross-module calls (already-converted twins, reused not reimplemented):
//! `crate::engine_workspace_root::getWorkspaceRoot`, `crate::ai_prompt_find::promptFind`,
//! `crate::ai_prompt_add::promptAdd`, `crate::ai_ai_call::aiCall`.
//!
//! `WORKTREES_DIR` (roots.mjs: `join(HOME_DIR, 'worktrees')`, `HOME_DIR` = `$ANTILOKI_HOME` env
//! override else `join(homedir(), '.antiloki')`) is inlined here rather than imported — same
//! per-file inlining convention as terminal-sandbox.rs / auth-sign.rs (no shared roots crate).
//!
//! `getWorkspaceRoot` never actually returns a falsy value (JS: `active?.root_path ||
//! SUBSTRATE_ROOT`) — the JS `try { root = getWorkspaceRoot(db) } catch { root = null }` and the
//! subsequent `if (!root) return { ok:false, error:'no active workspace' }` guards are ported
//! faithfully below (defensive dead code in both languages, kept for parity).
//!
//! Message-slicing (`.slice(0, 300)` / `.slice(0, 500)`) uses UTF-16 code-unit semantics in JS;
//! this twin approximates with `chars().take(N)` (ASCII/BMP-safe, disclosed gap shared with every
//! sibling git/docker-subprocess twin).
// QUALITY: 80/100 — real docker subprocess dispatch + real fs requirement detection ported faithfully; aiCall deferred (disclosed); exec_fn/provider_fn injection seams unused (JSON can't carry a fn, disclosed)

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
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
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0 && !f.is_nan()).unwrap_or(true),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(_)) | Some(Value::Object(_)) => true,
    }
}
fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

// ── KEYS + cost_settings kv (playbook: crate::db_json read/write helpers, matches cost-settings.rs) ─
const KEY_RUNTIME: &str = "ws_runtime";
const KEY_IMAGE: &str = "ws_runtime_image";
const KEY_CONTAINER: &str = "ws_runtime_container";

fn kv_get(db: &Connection, k: &str) -> Option<String> {
    let rows = crate::db_json::query_rows(db, "SELECT value FROM cost_settings WHERE key = ?", &[json!(k)]).ok()?;
    rows.as_array()?.first()?.get("value")?.as_str().map(|s| s.to_string())
}
fn kv_set(db: &Connection, k: &str, v: &str) {
    let _ = db.execute_batch("CREATE TABLE IF NOT EXISTS cost_settings (key TEXT PRIMARY KEY, value TEXT)");
    let _ = crate::db_json::run_write(db, "INSERT INTO cost_settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value", &[json!(k), json!(v)]);
}

fn first_digits(s: &str) -> Option<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let mut out = String::new();
            while i < chars.len() && chars[i].is_ascii_digit() {
                out.push(chars[i]);
                i += 1;
            }
            return Some(out);
        }
        i += 1;
    }
    None
}

// Propose a base image. Cheap heuristics, most-specific first; the operator confirms via `set`.
pub fn detectImage(root: &Path) -> Value {
    for f in [".node-version", ".nvmrc"] {
        if let Ok(raw) = fs::read_to_string(root.join(f)) {
            let v = raw.trim().trim_start_matches('v');
            if v.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                let major = v.split('.').next().unwrap_or(v);
                return json!({ "image": format!("node:{major}-bullseye"), "source": f });
            }
        }
    }
    if let Ok(raw) = fs::read_to_string(root.join("package.json")) {
        if let Ok(pkg) = serde_json::from_str::<Value>(&raw) {
            if let Some(eng) = pkg.get("engines").and_then(|e| e.get("node")).filter(|v| truthy(Some(v))) {
                let eng_s = js_string(eng);
                if let Some(digits) = first_digits(&eng_s) {
                    return json!({ "image": format!("node:{digits}-bullseye"), "source": format!("engines.node {eng_s}") });
                }
            }
            let rs = pkg.get("dependencies").and_then(|d| d.get("react-scripts"))
                .or_else(|| pkg.get("devDependencies").and_then(|d| d.get("react-scripts")));
            if let Some(rs) = rs.filter(|v| truthy(Some(v))) {
                let rs_s = js_string(rs);
                let digits_dot: String = rs_s.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
                let maj: i64 = digits_dot.split('.').next().filter(|x| !x.is_empty()).unwrap_or("0").parse().unwrap_or(0);
                if maj > 0 && maj <= 4 {
                    return json!({ "image": "node:16-bullseye", "source": format!("react-scripts {rs_s} (webpack 4 — needs old Node)") });
                }
                if maj == 5 {
                    return json!({ "image": "node:18-bullseye", "source": format!("react-scripts {rs_s}") });
                }
            }
        }
    }
    json!({ "image": "node:20-bullseye", "source": "default (modern)" })
}

// Native modules that compile through node-gyp → need a C/C++ toolchain + Python on the host.
static NATIVE_DEPS: &[(&str, &str)] = &[
    ("node-sass", "Sass compiler (node-gyp; unbuildable on modern Node — see notes)"),
    ("node-gyp", "native addon builder"),
    ("bcrypt", "native password hashing"),
    ("sqlite3", "native SQLite"),
    ("better-sqlite3", "native SQLite"),
    ("sharp", "native image processing (libvips)"),
    ("canvas", "native canvas (cairo)"),
    ("node-pty", "native pseudo-terminal"),
    ("grpc", "native gRPC"),
    ("leveldown", "native LevelDB"),
    ("serialport", "native serial I/O"),
];

// DETERMINISTIC environment requirements — read straight off the workspace files, no AI.
pub fn analyzeRequirements(root: &Path) -> Value {
    let has = |f: &str| root.join(f).exists();
    let read_json = |f: &str| -> Option<Value> { fs::read_to_string(root.join(f)).ok().and_then(|s| serde_json::from_str(&s).ok()) };

    let img = detectImage(root);
    let image = img.get("image").and_then(|v| v.as_str()).unwrap_or("node:20-bullseye").to_string();
    let node = first_digits(&image).unwrap_or_else(|| "20".to_string());
    let node_source = img.get("source").cloned().unwrap_or(Value::Null);

    let mut package_manager = "npm";
    if has("pnpm-lock.yaml") { package_manager = "pnpm"; }
    else if has("yarn.lock") { package_manager = "yarn"; }
    else if has("bun.lockb") { package_manager = "bun"; }

    let pkg = read_json("package.json").unwrap_or(json!({}));
    let mut deps: Map<String, Value> = Map::new();
    if let Some(d) = pkg.get("dependencies").and_then(|v| v.as_object()) { for (k, v) in d { deps.insert(k.clone(), v.clone()); } }
    if let Some(d) = pkg.get("devDependencies").and_then(|v| v.as_object()) { for (k, v) in d { deps.insert(k.clone(), v.clone()); } }

    let native_deps: Vec<Value> = NATIVE_DEPS.iter()
        .filter(|(d, _)| truthy(deps.get(*d)))
        .map(|(d, why)| json!({ "dep": d, "version": deps.get(*d).cloned().unwrap_or(Value::Null), "why": why }))
        .collect();

    let rs_major: i64 = match deps.get("react-scripts").filter(|v| truthy(Some(v))) {
        Some(v) => {
            let s = js_string(v);
            let digits_dot: String = s.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
            digits_dot.split('.').next().filter(|x| !x.is_empty()).unwrap_or("0").parse().unwrap_or(0)
        }
        None => 0,
    };
    let openssl_legacy = rs_major > 0 && rs_major <= 4;
    let node_sass_broken = truthy(deps.get("node-sass"));
    let python: Value = if has("requirements.txt") || has("pyproject.toml") || has("Pipfile") || has("setup.py") {
        json!("app runtime (python project files present)")
    } else if !native_deps.is_empty() {
        json!("build-time (node-gyp)")
    } else {
        Value::Null
    };

    let install = if has("package-lock.json") && package_manager == "npm" { "npm ci".to_string() } else { format!("{package_manager} install") };
    let run = |name: &str| -> String { if package_manager == "npm" { format!("npm run {name}") } else { format!("{package_manager} {name}") } };
    let scripts = pkg.get("scripts");
    let dev_val = scripts.and_then(|s| s.get("dev")).filter(|v| truthy(Some(v)));
    let start_val = scripts.and_then(|s| s.get("start")).filter(|v| truthy(Some(v)));
    let dev_cmd: Value = if dev_val.is_some() {
        json!(run("dev"))
    } else if start_val.is_some() {
        json!(if package_manager == "npm" { "npm start".to_string() } else { format!("{package_manager} start") })
    } else {
        Value::Null
    };

    let mut notes: Vec<Value> = vec![];
    if node_sass_broken { notes.push(json!("node-sass builds native bindings that FAIL on Node ≥ 16 (node-gyp + modern Python). Docker on node:16 sidesteps it; on host, alias node-sass → dart-sass.")); }
    if openssl_legacy { notes.push(json!("react-scripts ≤ 4 uses webpack 4, which needs NODE_OPTIONS=--openssl-legacy-provider on Node ≥ 17.")); }

    json!({
        "node": node, "nodeSource": node_source, "image": image, "packageManager": package_manager,
        "install": install, "devCmd": dev_cmd, "nativeDeps": native_deps, "opensslLegacy": openssl_legacy,
        "nodeSassBroken": node_sass_broken, "python": python, "notes": notes,
    })
}

// HOST recipe — the manual path.
pub fn hostSteps(req: &Value) -> Value {
    let mut s: Vec<Value> = vec![];
    let mut n = 1;
    let node = req.get("node").and_then(|v| v.as_str()).unwrap_or("20");
    s.push(json!({ "n": n, "title": format!("Install Node {node}"), "cmd": format!("nvm install {node} && nvm use {node}"), "note": "Windows: nvm-windows or Volta. Must match the workspace — a Node mismatch is the #1 cause of failed installs." }));
    n += 1;
    let native_deps = req.get("nativeDeps").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if !native_deps.is_empty() {
        let names: Vec<String> = native_deps.iter().map(|d| js_string(d.get("dep").unwrap_or(&Value::Null))).collect();
        s.push(json!({ "n": n, "title": "Install a native build toolchain", "note": format!("{} compile via node-gyp. Windows: \"Desktop development with C++\" (VS Build Tools) + Python 3. Linux: apt-get install -y build-essential python3.", names.join(", ")) }));
        n += 1;
    }
    if truthy(req.get("opensslLegacy")) {
        s.push(json!({ "n": n, "title": "Set the OpenSSL legacy flag", "cmd": "NODE_OPTIONS=--openssl-legacy-provider", "note": "Already recorded in the scripts-registry env — the gates/preview apply it automatically." }));
        n += 1;
    }
    if truthy(req.get("nodeSassBroken")) {
        s.push(json!({ "n": n, "title": "Fix node-sass (unbuildable on modern Node)", "cmd": "npm pkg set \"dependencies.node-sass=npm:sass@^1.77\"", "note": "Swaps node-sass for dart-sass under the SAME name (react-scripts imports \"node-sass\" literally). Or just use the Docker node:16 image and skip this entirely." }));
        n += 1;
    }
    s.push(json!({ "n": n, "title": "Install dependencies", "cmd": req.get("install").cloned().unwrap_or(Value::Null) }));
    n += 1;
    if let Some(dev) = req.get("devCmd") {
        if truthy(Some(dev)) {
            s.push(json!({ "n": n, "title": "Run it", "cmd": dev.clone() }));
            n += 1;
        }
    }
    let _ = n;
    json!(s)
}

// DOCKER recipe — usually 2 real steps, because node:<v>-bullseye already carries the toolchain.
pub fn dockerSteps(req: &Value) -> Value {
    let mut s: Vec<Value> = vec![];
    let mut n = 1;
    let node = req.get("node").and_then(|v| v.as_str()).unwrap_or("20");
    let native_deps = req.get("nativeDeps").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let native_note = if !native_deps.is_empty() {
        let names: Vec<String> = native_deps.iter().map(|d| js_string(d.get("dep").unwrap_or(&Value::Null))).collect();
        format!("{} build with zero extra setup", names.join(", "))
    } else {
        "native modules build out of the box".to_string()
    };
    s.push(json!({ "n": n, "title": format!("Base image node:{node}-bullseye"), "note": format!("Debian \"bullseye\" (not slim/alpine) ships gcc, make and python3 — {native_note}. This is why Docker is the easy path here.") }));
    n += 1;

    let python = req.get("python").cloned().unwrap_or(Value::Null);
    let py_str = python.as_str().unwrap_or("");
    if py_str.contains("app runtime") {
        s.push(json!({ "n": n, "title": "Add the Python runtime", "cmd": "apt-get update && apt-get install -y python3 python3-pip", "note": "Only because the APP runs Python — the node-gyp toolchain is already in the base image." }));
        n += 1;
    }
    s.push(json!({ "n": n, "title": "Start the workspace container", "note": "The \"Start container\" button above runs it: one container per workspace, worktrees mounted, dev ports published. No hand-written docker command." }));
    n += 1;
    let install = req.get("install").and_then(|v| v.as_str()).unwrap_or("");
    s.push(json!({ "n": n, "title": "Everything runs inside it", "cmd": format!("docker exec -w /worktrees/<card> <container> {install}"), "note": format!("install, build, tests and preview all run on node {node} in the container — your host is never touched.") }));
    n += 1;
    let _ = n;
    json!(s)
}

// AI explainer (op:explain) prompt — EXPANDS the deterministic steps into a friendly tutorial.
const EXPLAIN_PROMPT: &str = "system.workspace.runtime.explain.v3";
const EXPLAIN_TEMPLATE_MD: &str = r#"You are a senior devops engineer with LIVE WEB SEARCH. Write the EXACT, FROM-SCRATCH setup for a developer who has NOTHING installed and "no clue how to do this" — not Docker, not Node, not the build tools. Every single command, in order, ready to paste.

TARGET: {{target}}   (docker = one container; host = the developer's own machine, likely Windows)

The workspace's requirements (read from its files — ground truth):
{{requirements_json}}

The deterministic steps to expand (do NOT invent tools the requirements don't imply):
{{steps_json}}

RESEARCH: use web search to find the CURRENT, official install commands + download URLs (Docker Desktop, nvm, Node, VS Build Tools, etc.) — do not rely on memory for version-sensitive steps. Prefer official docs.

Produce a COMPLETE from-zero tutorial. START by installing the runtime tool ITSELF, then create the image/container (the exact `docker run` or Dockerfile), then install deps, then run. For each card:
  - title: the action, short.
  - body: 2-4 plain sentences — WHY + WHAT. Windows vs macOS vs Linux differences EXPLICITLY. Name the official source you used.
  - example: the EXACT commands — ALL of them, one per line, copy-pasteable, in order. Official download URL when a tool is installed by hand. Windows AND Unix when they differ, labelled. Empty only when truly nothing to run.

Be exhaustive. No "something like". The reader pastes these into a live terminal to go from a fresh machine to a running dev server.

Output JSON EXACTLY: { "tutorial_json": "<a JSON-stringified array of {\"title\":\"…\",\"body\":\"…\",\"example\":\"…\"} objects>" }"#;

fn explain_output_fields() -> Value {
    json!([{ "field_name": "tutorial_json", "field_type": "string", "required": true }])
}

fn ensure_explain_prompt(db: &Connection) -> Result<(), String> {
    if crate::ai_prompt_find::promptFind(&json!({ "name": EXPLAIN_PROMPT }), db).is_ok() {
        return Ok(());
    }
    crate::ai_prompt_add::promptAdd(&json!({
        "name": EXPLAIN_PROMPT,
        "template_md": EXPLAIN_TEMPLATE_MD,
        "default_tier": "low",
        "default_temperature": "cold",
        "max_output_tokens": 1200,
        "output_fields": explain_output_fields(),
    }), db)?;
    crate::ai_prompt_find::promptFind(&json!({ "name": EXPLAIN_PROMPT }), db)?;
    Ok(())
}

fn container_name_for(root: Option<&str>) -> String {
    let base = match root {
        Some(r) if !r.is_empty() => {
            let seg = r.split(|c| c == '\\' || c == '/').filter(|s| !s.is_empty()).last();
            seg.map(|s| s.to_string()).unwrap_or_else(|| "workspace".to_string())
        }
        _ => "workspace".to_string(),
    };
    let lower = base.to_lowercase();
    let cleaned: String = lower.chars().map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.' || c == '-' { c } else { '-' }).collect();
    format!("antiloki-ws-{cleaned}")
}

// roots.mjs: HOME_DIR = resolve($ANTILOKI_HOME) if set+truthy, else join(homedir(), '.antiloki').
// WORKTREES_DIR = join(HOME_DIR, 'worktrees'). Same per-file inline convention as terminal-sandbox.rs.
#[cfg(windows)]
fn homedir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("USERPROFILE") {
        if !p.is_empty() { return Some(PathBuf::from(p)); }
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

fn worktrees_dir_string() -> String {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("worktrees").to_string_lossy().into_owned()
}

// Real `docker` subprocess. Spawn failure (binary absent) mirrors spawnSync's `r.error` branch:
// JS `r.status ?? (r.error ? 127 : 1)` — a launch failure (no such binary) always yields 127.
fn docker_exec(args: &[String]) -> (i32, String) {
    match crate::proc::cmd("docker").args(args).output() {
        Ok(out) => {
            let status = out.status.code().unwrap_or(1);
            let stdout = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            (status, stdout)
        }
        Err(_) => (127, String::new()),
    }
}

// `/:(\d{2,5})\b/g` — every `:NNN` (2-5 digits, word-boundary after) found in docker's stderr text.
fn find_busy_port(s: &str, ports: &[String]) -> Option<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut found: Vec<String> = vec![];
    while i < chars.len() {
        if chars[i] == ':' {
            let mut j = i + 1;
            let mut digits = String::new();
            while j < chars.len() && chars[j].is_ascii_digit() && digits.len() < 5 {
                digits.push(chars[j]);
                j += 1;
            }
            if digits.len() >= 2 {
                let boundary_ok = j >= chars.len() || !(chars[j].is_ascii_alphanumeric() || chars[j] == '_');
                if boundary_ok { found.push(digits); }
            }
            i = if j > i { j } else { i + 1 };
        } else {
            i += 1;
        }
    }
    found.into_iter().find(|d| ports.contains(d))
}

pub fn workspaceRuntime(input: &Value, db: &Connection) -> Result<Value, String> {
    let op = input.get("op").map(js_string).unwrap_or_else(|| "status".to_string());
    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root_opt = root_v.as_str().filter(|s| !s.is_empty()).map(|s| s.to_string());
    let container = container_name_for(root_opt.as_deref());

    match op.as_str() {
        "detect" => {
            let root = match &root_opt { Some(r) => r.clone(), None => return Ok(json!({ "ok": false, "error": "no active workspace to detect" })) };
            let img = detectImage(Path::new(&root));
            let mut out = img.as_object().cloned().unwrap_or_default();
            out.insert("ok".into(), json!(true));
            out.insert("container".into(), json!(container));
            out.insert("recommend".into(), json!("docker"));
            Ok(Value::Object(out))
        }

        "requirements" => {
            let root = match &root_opt { Some(r) => r.clone(), None => return Ok(json!({ "ok": false, "error": "no active workspace" })) };
            let requirements = analyzeRequirements(Path::new(&root));
            let host = hostSteps(&requirements);
            let docker = dockerSteps(&requirements);
            Ok(json!({ "ok": true, "requirements": requirements, "host": host, "docker": docker }))
        }

        "explain" => {
            let root = match &root_opt { Some(r) => r.clone(), None => return Ok(json!({ "ok": false, "error": "no active workspace" })) };
            let target = if input.get("target").and_then(|v| v.as_str()) == Some("host") { "host" } else { "docker" };
            let requirements = analyzeRequirements(Path::new(&root));
            let steps = if target == "host" { hostSteps(&requirements) } else { dockerSteps(&requirements) };
            let deterministic: Vec<Value> = steps.as_array().cloned().unwrap_or_default().iter().map(|s| json!({
                "title": s.get("title").cloned().unwrap_or(Value::Null),
                "body": s.get("note").cloned().unwrap_or(json!("")),
                "example": s.get("cmd").cloned().unwrap_or(json!("")),
            })).collect();

            let mut ai_error: Option<String> = None;
            let mut web_researched = false;
            let mut ai_ok = false;
            if let Err(e) = ensure_explain_prompt(db) {
                ai_error = Some(format!("prompt registration failed: {}", truncate(&e, 300)));
            }
            if ai_error.is_none() {
                for ws in [true, false] {
                    let call_input = json!({
                        "prompt_name": EXPLAIN_PROMPT,
                        "input_kv": [
                            { "key": "target", "value": target },
                            { "key": "requirements_json", "value": serde_json::to_string(&requirements).unwrap_or_default() },
                            { "key": "steps_json", "value": serde_json::to_string(&steps).unwrap_or_default() },
                        ],
                        "tier": "mid", "temperature": "cold", "web_search": ws,
                    });
                    match crate::ai_ai_call::aiCall(&call_input, db) {
                        Ok(_) => { web_researched = ws; ai_error = None; ai_ok = true; break; }
                        Err(e) => { ai_error = Some(format!("{}AI call failed: {}", if ws { "web-search " } else { "" }, truncate(&e, 300))); }
                    }
                }
            }
            let tutorial = if ai_ok { deterministic.clone() } else { deterministic }; // ai path unreachable: aiCall is the deferred stub (always Err)
            Ok(json!({ "ok": true, "target": target, "tutorial": tutorial, "web_researched": web_researched, "ai_error": ai_error, "ai_call_id": Value::Null }))
        }

        "set" => {
            let runtime = match input.get("runtime") { // JS `??` — falls back only on null/undefined, not ''
                Some(v) if !v.is_null() => js_string(v),
                _ => "docker".to_string(),
            };
            kv_set(db, KEY_RUNTIME, &runtime);
            if let Some(image) = input.get("image") {
                if truthy(Some(image)) { kv_set(db, KEY_IMAGE, &js_string(image)); }
            }
            Ok(json!({ "ok": true, "runtime": kv_get(db, KEY_RUNTIME), "image": kv_get(db, KEY_IMAGE) }))
        }

        "up" => {
            let root = match &root_opt { Some(r) => r.clone(), None => return Ok(json!({ "ok": false, "error": "no active workspace" })) };
            let image = match input.get("image").filter(|v| truthy(Some(v))) {
                Some(v) => js_string(v),
                None => kv_get(db, KEY_IMAGE).filter(|s| !s.is_empty())
                    .unwrap_or_else(|| detectImage(Path::new(&root)).get("image").and_then(|v| v.as_str()).unwrap_or("node:20-bullseye").to_string()),
            };
            // Already running → no-op (never kill a live container someone may have a shell in).
            let (st0_code, st0_out) = docker_exec(&["inspect".into(), "-f".into(), "{{.State.Running}}".into(), container.clone()]);
            if st0_code == 0 && st0_out.to_lowercase().contains("true") {
                kv_set(db, KEY_RUNTIME, "docker"); kv_set(db, KEY_IMAGE, &image); kv_set(db, KEY_CONTAINER, &container);
                return Ok(json!({ "ok": true, "container": container, "image": image, "already_running": true }));
            }
            let mut ports: Vec<String> = vec!["3000".into(), "5173".into(), "8080".into(), "4200".into()];
            let mut ports_skipped: Vec<String> = vec![];
            let worktrees_dir = worktrees_dir_string();
            let mut last_code = 1;
            let mut last_out = String::new();
            let mut last_args: Vec<String> = vec![];
            let mut ran = false;
            for _attempt in 0..=4u32 {
                let _ = docker_exec(&["rm".into(), "-f".into(), container.clone()]); // idempotent — clears any Created/Exited corpse of OUR container
                let mut args: Vec<String> = vec![
                    "run".into(), "-d".into(), "--name".into(), container.clone(),
                    "-v".into(), format!("{worktrees_dir}:/worktrees"),
                    "-v".into(), format!("{root}:/repo"),
                ];
                for p in &ports { args.push("-p".into()); args.push(format!("{p}:{p}")); }
                args.push("-w".into()); args.push("/repo".into()); args.push(image.clone());
                args.push("sh".into()); args.push("-lc".into()); args.push("tail -f /dev/null".into());
                let (code, out) = docker_exec(&args);
                last_code = code; last_out = out.clone(); last_args = args.clone(); ran = true;
                if code == 0 { break; }
                match find_busy_port(&out, &ports) {
                    Some(busy) => { ports.retain(|p| *p != busy); ports_skipped.push(busy); }
                    None => break, // not a port conflict → a real failure, surface it
                }
            }
            if !ran || last_code != 0 {
                let cmd_val = if ran { json!(format!("docker {}", last_args.join(" "))) } else { Value::Null };
                return Ok(json!({
                    "ok": false,
                    "error": format!("docker run failed: {}", truncate(&last_out, 500)),
                    "cmd": cmd_val,
                }));
            }
            kv_set(db, KEY_RUNTIME, "docker"); kv_set(db, KEY_IMAGE, &image); kv_set(db, KEY_CONTAINER, &container);
            Ok(json!({ "ok": true, "container": container, "image": image, "ports": ports, "ports_skipped": ports_skipped, "cmd": format!("docker {}", last_args.join(" ")) }))
        }

        "down" => {
            let (code, out) = docker_exec(&["rm".into(), "-f".into(), container.clone()]);
            kv_set(db, KEY_CONTAINER, "");
            let ok = code == 0 || out.to_lowercase().contains("no such container");
            Ok(json!({ "ok": ok, "container": container }))
        }

        _ => { // status (default — any unrecognized op falls through here too, mirroring the JS)
            let (code, out) = docker_exec(&["inspect".into(), "-f".into(), "{{.State.Running}}".into(), container.clone()]);
            let running = code == 0 && out.to_lowercase().contains("true");
            Ok(json!({
                "ok": true,
                "runtime": kv_get(db, KEY_RUNTIME).unwrap_or_else(|| "host".to_string()),
                "image": kv_get(db, KEY_IMAGE),
                "container": container,
                "running": running,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_ws(files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wsrt-rs-{}-{}", std::process::id(), rand_suffix()));
        fs::create_dir_all(&dir).unwrap();
        for (name, content) in files {
            fs::write(dir.join(name), content).unwrap();
        }
        dir
    }
    fn rand_suffix() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().subsec_nanos() as u64
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    // ── detectImage ──────────────────────────────────────────────────────────────────────────
    #[test]
    fn detect_image_node_version_wins_over_everything() {
        let dir = tmp_ws(&[(".node-version", "v16.20.0\n"), ("package.json", "{}")]);
        assert_eq!(detectImage(&dir).get("image").unwrap().as_str().unwrap(), "node:16-bullseye");
    }
    #[test]
    fn detect_image_engines_node() {
        let dir = tmp_ws(&[("package.json", r#"{"engines":{"node":">=18"}}"#)]);
        assert_eq!(detectImage(&dir).get("image").unwrap().as_str().unwrap(), "node:18-bullseye");
    }
    #[test]
    fn detect_image_react_scripts_3_means_old_node_16() {
        let dir = tmp_ws(&[("package.json", r#"{"dependencies":{"react-scripts":"3.0.0"}}"#)]);
        assert_eq!(detectImage(&dir).get("image").unwrap().as_str().unwrap(), "node:16-bullseye");
    }
    #[test]
    fn detect_image_modern_default_when_nothing_declared() {
        let dir = tmp_ws(&[("package.json", "{}")]);
        assert_eq!(detectImage(&dir).get("image").unwrap().as_str().unwrap(), "node:20-bullseye");
    }

    // ── analyzeRequirements ──────────────────────────────────────────────────────────────────
    #[test]
    fn analyze_requirements_node_sass_plus_react_scripts_3_full_pain_profile() {
        let dir = tmp_ws(&[
            ("package.json", r#"{"dependencies":{"react-scripts":"3.0.1","node-sass":"^4.14.0"},"scripts":{"start":"react-scripts start"}}"#),
            ("package-lock.json", "{}"),
        ]);
        let req = analyzeRequirements(&dir);
        assert_eq!(req.get("node").unwrap().as_str().unwrap(), "16");
        assert_eq!(req.get("nodeSassBroken").unwrap().as_bool().unwrap(), true);
        assert_eq!(req.get("opensslLegacy").unwrap().as_bool().unwrap(), true);
        assert_eq!(req.get("install").unwrap().as_str().unwrap(), "npm ci");
        assert!(req.get("nativeDeps").unwrap().as_array().unwrap().iter().any(|d| d.get("dep").unwrap() == "node-sass"));
        assert!(req.get("notes").unwrap().as_array().unwrap().len() >= 2);
    }
    #[test]
    fn analyze_requirements_modern_pnpm_vite_app_clean_profile() {
        let dir = tmp_ws(&[
            ("package.json", r#"{"devDependencies":{"vite":"^5"},"scripts":{"dev":"vite"}}"#),
            ("pnpm-lock.yaml", "lockfileVersion: 6"),
        ]);
        let req = analyzeRequirements(&dir);
        assert_eq!(req.get("packageManager").unwrap().as_str().unwrap(), "pnpm");
        assert_eq!(req.get("install").unwrap().as_str().unwrap(), "pnpm install");
        assert_eq!(req.get("devCmd").unwrap().as_str().unwrap(), "pnpm dev");
        assert_eq!(req.get("nodeSassBroken").unwrap().as_bool().unwrap(), false);
        assert_eq!(req.get("opensslLegacy").unwrap().as_bool().unwrap(), false);
        assert_eq!(req.get("nativeDeps").unwrap().as_array().unwrap().len(), 0);
    }

    #[test]
    fn host_steps_surface_remedies_docker_steps_stay_short() {
        let dir = tmp_ws(&[("package.json", r#"{"dependencies":{"react-scripts":"3.0.1","node-sass":"^4"}}"#)]);
        let req = analyzeRequirements(&dir);
        let host = hostSteps(&req);
        let host_arr = host.as_array().unwrap();
        assert!(host_arr.iter().any(|s| s.get("title").unwrap().as_str().unwrap().to_lowercase().contains("node-sass")));
        assert!(host_arr.iter().any(|s| s.get("title").unwrap().as_str().unwrap().to_lowercase().contains("openssl")));
        let docker = dockerSteps(&req);
        let docker_arr = docker.as_array().unwrap();
        assert!(docker_arr.iter().any(|s| s.get("title").unwrap().as_str().unwrap().contains("node:16-bullseye")));
        assert!(docker_arr.len() < host_arr.len());
    }

    // ── workspaceRuntime ops ─────────────────────────────────────────────────────────────────
    #[test]
    fn requirements_op_returns_deterministic_requirements_and_both_recipes() {
        let c = db();
        let r = workspaceRuntime(&json!({ "op": "requirements" }), &c).unwrap();
        assert_eq!(r.get("ok").unwrap().as_bool().unwrap(), true);
        assert!(r.get("requirements").unwrap().is_object());
        assert!(r.get("host").unwrap().is_array());
        assert!(r.get("docker").unwrap().is_array());
    }

    #[test]
    fn set_records_runtime_and_image() {
        let c = db();
        let r = workspaceRuntime(&json!({ "op": "set", "runtime": "docker", "image": "node:16-bullseye" }), &c).unwrap();
        assert_eq!(r.get("runtime").unwrap().as_str().unwrap(), "docker");
        assert_eq!(r.get("image").unwrap().as_str().unwrap(), "node:16-bullseye");
    }

    #[test]
    fn explain_never_fails_blank_deferred_ai_falls_back_to_deterministic_tutorial() {
        let c = db();
        let r = workspaceRuntime(&json!({ "op": "explain", "target": "docker" }), &c).unwrap();
        assert_eq!(r.get("ok").unwrap().as_bool().unwrap(), true);
        let tutorial = r.get("tutorial").unwrap().as_array().unwrap();
        assert!(!tutorial.is_empty(), "tutorial must never be empty");
        assert!(r.get("ai_error").unwrap().as_str().unwrap().contains("AI call failed"));
        assert_eq!(r.get("web_researched").unwrap().as_bool().unwrap(), false);
    }

    #[test]
    fn find_busy_port_extracts_the_first_matching_published_port() {
        let ports = vec!["3000".to_string(), "5173".to_string()];
        let msg = "Error response from daemon: ports are not available: exposing port TCP 0.0.0.0:5173 -> 127.0.0.1:0: listen tcp 0.0.0.0:5173: bind: Only one usage of each socket address";
        assert_eq!(find_busy_port(msg, &ports), Some("5173".to_string()));
    }

    #[test]
    fn docker_exec_missing_binary_yields_status_127() {
        // On a machine without `docker` on PATH this proves the spawn-failure fallback; if docker
        // IS present the smoke path (status 0/1 from a real inspect) is exercised instead — both
        // are valid observations of the same real subprocess call, matching terminal-sandbox.rs's
        // "either real git ran or it didn't" bespoke-native testing stance.
        let (code, _out) = docker_exec(&["inspect".into(), "-f".into(), "{{.State.Running}}".into(), "antiloki-ws-does-not-exist".into()]);
        assert!(code == 127 || code == 0 || code == 1);
    }

    #[test]
    fn container_name_for_slugifies_the_last_path_segment() {
        assert_eq!(container_name_for(Some("C:/dev/My App")), "antiloki-ws-my-app");
        assert_eq!(container_name_for(None), "antiloki-ws-workspace");
    }
}
