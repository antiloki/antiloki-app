//! CONNECTORS (RFC 0022): one generic way to connect a service (an API or an MCP server) to antiloki, asked for by an agent and finished by the
//! person in the Connectors pane. The agent only PROPOSES a recipe (name, what it gives, the steps, the inputs, where it calls); it never
//! connects, never reads a secret, never approves. The person pastes a key (or clicks Connect for an OAuth service); the secret goes to
//! `<project>/.antiloki/.env` (mode 0600, kept out of git) and nowhere else: not the SQLite rows, not the YAML recipe, not the call trail.
//! Truth = rows in `<project>/.antiloki/connectors.db`; the YAML recipe is a rendering of the row.
//! After that every agent calls the connector through ONE tool, `antiloki_connector` (list · status · tools · call), and only toward the
//! host the recipe names. A read-only connector refuses anything that writes.

use crate::AppState;
use axum::{body::Bytes, extract::Query, http::StatusCode, response::{IntoResponse, Response}, Json};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const TOOLS: [&str; 1] = ["antiloki_connector"];
const MAX_RECIPE: usize = 16_384;
const MAX_REPLY: usize = 20_000;

fn s(v: &Value) -> String { v.as_str().unwrap_or("").trim().to_string() }

// ── the recipe: what an agent may propose, checked before anything is stored ────────────────────────────────────────────────

/// A web address a connector may talk to: https, or http on this machine (a local service, or a test).
pub fn safe_url(u: &str) -> Result<url::Url, String> {
    let p = url::Url::parse(u).map_err(|_| format!("`{u}` is not a web address"))?;
    let host = p.host_str().unwrap_or("").to_string();
    let loopback = host == "localhost" || host == "127.0.0.1" || host == "[::1]" || host == "::1";
    match p.scheme() { "https" => {}, "http" if loopback => {}, _ => return Err(format!("`{u}`: only https (or http on this machine) is allowed")) }
    if host.is_empty() { return Err(format!("`{u}` has no host")); }
    if !p.username().is_empty() || p.password().is_some() { return Err("a web address must not carry a user name or password".into()); }
    Ok(p)
}
fn ident(x: &str, max: usize, dash: bool) -> bool { !x.is_empty() && x.len() <= max && x.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || (dash && c == '-')) }
fn token_name(x: &str) -> bool { !x.is_empty() && x.len() <= 60 && x.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') }
fn placeholders(t: &str) -> Vec<String> {
    let mut out = vec![]; let mut rest = t;
    while let Some(i) = rest.find('{') { let Some(j) = rest[i..].find('}') else { break }; out.push(rest[i + 1..i + j].to_string()); rest = &rest[i + j + 1..]; }
    out
}

/// The recipe an agent proposed, checked and cut down to the fields antiloki knows. `Err` says what to fix.
pub fn validate(v: &Value) -> Result<Value, String> {
    if serde_json::to_string(v).map(|t| t.len()).unwrap_or(0) > MAX_RECIPE { return Err(format!("the recipe is longer than {MAX_RECIPE} bytes")); }
    let title = s(&v["title"]); if title.is_empty() || title.chars().count() > 60 { return Err("`title` is needed (at most 60 characters)".into()); }
    let slug = { let g = s(&v["slug"]); if g.is_empty() { slugify(&title) } else { g } };
    if !ident(&slug, 40, true) { return Err("`slug`: lower case letters, digits and dashes, at most 40".into()); }
    let kind = s(&v["kind"]); if kind != "api" && kind != "mcp" { return Err("`kind` is `api` (an HTTP API) or `mcp` (a remote MCP server)".into()); }
    let auth = { let a = s(&v["auth"]); if a.is_empty() { "key".to_string() } else { a } };
    if !matches!(auth.as_str(), "key" | "oauth" | "none") { return Err("`auth` is `key`, `oauth` or `none`".into()); }
    let access = { let a = s(&v["access"]); if a.is_empty() { "read-only".to_string() } else { a } };
    if access != "read-only" && access != "read-write" { return Err("`access` is `read-only` or `read-write`".into()); }
    let site = s(&v["site"]); if !site.is_empty() { safe_url(&site)?; }
    let endpoint_key = if kind == "api" { "base_url" } else { "url" };
    let endpoint = s(&v[endpoint_key]); if endpoint.is_empty() { return Err(format!("`{endpoint_key}` is needed for kind `{kind}`")); }
    let ep = safe_url(&endpoint)?; if ep.query().is_some() || ep.fragment().is_some() { return Err(format!("`{endpoint_key}` must not carry a query or fragment")); }
    let mut inputs = vec![]; let mut ids: Vec<String> = vec![];
    for i in v["inputs"].as_array().into_iter().flatten().take(12) {
        let id = s(&i["id"]); if !ident(&id, 30, false) { return Err(format!("input id `{id}`: lower case letters, digits and underscores, at most 30")); }
        if ids.contains(&id) { return Err(format!("input id `{id}` is listed twice")); }
        let label = s(&i["label"]); if label.is_empty() || label.chars().count() > 80 { return Err(format!("input `{id}` needs a `label` (at most 80 characters)")); }
        let link = s(&i["link"]); if !link.is_empty() { safe_url(&link)?; }
        inputs.push(json!({ "id": id, "label": label, "secret": i["secret"] != json!(false), "help": s(&i["help"]).chars().take(300).collect::<String>(), "link": link, "optional": i["optional"] == json!(true) }));
        ids.push(id);
    }
    let mut out = json!({ "slug": slug, "title": title, "site": site, "kind": kind, "auth": auth, "access": access, endpoint_key: endpoint, "inputs": inputs,
        "summary": s(&v["summary"]).chars().take(300).collect::<String>() });
    // how a call is signed: a header whose value is a template over the inputs, e.g. Authorization: Bearer {token}
    let h = &v["auth_header"];
    if h.is_object() {
        let name = s(&h["name"]); let template = s(&h["template"]);
        if !token_name(&name) { return Err("`auth_header.name` is not a header name".into()); }
        for p in placeholders(&template) { if !ids.contains(&p) && !(auth == "oauth" && p == "access_token") { return Err(format!("`auth_header.template` uses {{{p}}}, which is not an input")); } }
        if template.contains('\n') || template.contains('\r') { return Err("`auth_header.template` is one line".into()); }
        out["auth_header"] = json!({ "name": name, "template": template });
    } else if auth != "none" { return Err("`auth_header` {name, template} is needed: it says how a call is signed, e.g. {\"name\":\"Authorization\",\"template\":\"Bearer {token}\"}".into()); }
    if auth == "key" && !inputs_len_ok(&out) { return Err("auth `key` needs at least one input (the key to paste)".into()); }
    if auth == "oauth" {
        let o = &v["oauth"]; let (a, t) = (s(&o["authorize_url"]), s(&o["token_url"])); safe_url(&a)?; safe_url(&t)?;
        let (cid, csec) = (s(&o["client_id_input"]), s(&o["client_secret_input"]));
        let (builtin, reg) = (s(&o["builtin"]), s(&o["register_url"]));
        if !builtin.is_empty() && !ident(&builtin, 30, false) { return Err("`oauth.builtin` is a short name such as `google`".into()); }
        if !reg.is_empty() { safe_url(&reg)?; }
        // a built-in or auto-registered client needs no inputs from the person; otherwise they paste the client id (and secret) of their own
        if builtin.is_empty() && reg.is_empty() && !ids.contains(&cid) { return Err("`oauth.client_id_input` must name one of the inputs (or give `oauth.builtin` / `oauth.register_url`)".into()); }
        if !cid.is_empty() && !ids.contains(&cid) { return Err("`oauth.client_id_input` must name one of the inputs".into()); }
        if !csec.is_empty() && !ids.contains(&csec) { return Err("`oauth.client_secret_input` must name one of the inputs".into()); }
        // extra sign-in parameters some services need (Google hands out a refresh token only with access_type=offline&prompt=consent)
        let mut extra = serde_json::Map::new();
        for (k, x) in o["extra"].as_object().into_iter().flatten().take(6) {
            let v = x.as_str().unwrap_or("").trim().to_string();
            if !ident(k, 30, false) || v.is_empty() || v.len() > 200 || v.contains('\n') { return Err(format!("`oauth.extra.{k}`: a short single-line text, key in lower case letters and underscores")); }
            if matches!(k.as_str(), "response_type" | "client_id" | "redirect_uri" | "state" | "scope") { return Err(format!("`oauth.extra.{k}` is set by antiloki")); }
            extra.insert(k.clone(), json!(v));
        }
        out["oauth"] = json!({ "authorize_url": a, "token_url": t, "scopes": o["scopes"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).take(20).map(String::from).collect::<Vec<_>>()).unwrap_or_default(), "client_id_input": cid, "client_secret_input": csec, "builtin": builtin, "register_url": reg, "extra": extra });
    }
    let steps: Vec<String> = v["steps"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).take(12).map(|x| x.trim().chars().take(300).collect()).filter(|x: &String| !x.is_empty()).collect();
    if steps.is_empty() { return Err("`steps`: write the short tutorial the person follows (where to click, what to copy), 1 to 12 lines".into()); }
    out["steps"] = json!(steps);
    // the test: one harmless read that proves the connection works
    let t = &v["test"];
    if kind == "api" {
        let p = s(&t["path"]); if !path_ok(&p) { return Err("`test.path` is needed for an api (a harmless GET that proves the key works, starting with /)".into()); }
        out["test"] = json!({ "path": p });
    }
    Ok(out)
}
fn inputs_len_ok(r: &Value) -> bool { r["inputs"].as_array().map_or(false, |a| !a.is_empty()) }
pub fn slugify(t: &str) -> String {
    let mut o = String::new(); for c in t.to_lowercase().chars() { if c.is_ascii_alphanumeric() { o.push(c); } else if !o.ends_with('-') && !o.is_empty() { o.push('-'); } }
    o.trim_matches('-').chars().take(40).collect()
}
/// A path that can only stay under the connector's host and base: starts with one /, no `..`, no backslash, no `@`, no second slash up front.
pub fn path_ok(p: &str) -> bool { p.starts_with('/') && !p.starts_with("//") && !p.contains("..") && !p.contains('\\') && !p.contains('@') && !p.contains('\n') && !p.contains('\r') && p.len() <= 2000 }

// ── the YAML rendering of a recipe (the row is the truth; this is for reading, sharing and diffing) ─────────────────────────────

pub fn to_yaml(recipe: &Value) -> String {
    fn conv(v: &Value) -> yaml_rust2::Yaml {
        use yaml_rust2::Yaml;
        match v {
            Value::Null => Yaml::Null, Value::Bool(b) => Yaml::Boolean(*b),
            Value::Number(n) => if let Some(i) = n.as_i64() { Yaml::Integer(i) } else { Yaml::Real(n.to_string()) },
            Value::String(t) => Yaml::String(t.clone()),
            Value::Array(a) => Yaml::Array(a.iter().map(conv).collect()),
            Value::Object(o) => { let mut h = yaml_rust2::yaml::Hash::new(); for (k, x) in o { h.insert(Yaml::String(k.clone()), conv(x)); } Yaml::Hash(h) }
        }
    }
    let mut out = String::new();
    let _ = yaml_rust2::YamlEmitter::new(&mut out).dump(&conv(recipe));
    out.trim_start_matches("---").trim_start().to_string() + "\n"
}

// ── the store: <project>/.antiloki/connectors.db ──────────────────────────────────────────────────────────────────────────

fn dir_of(root: &str) -> PathBuf { Path::new(root).join(".antiloki") }
fn open(root: &str) -> Result<Connection, String> {
    if root.is_empty() { return Err("no project is open".into()); }
    let d = dir_of(root); std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    let c = Connection::open(d.join("connectors.db")).map_err(|e| e.to_string())?;
    c.execute_batch("CREATE TABLE IF NOT EXISTS connectors (slug TEXT PRIMARY KEY, title TEXT NOT NULL, recipe_json TEXT NOT NULL, status TEXT NOT NULL, note TEXT NOT NULL DEFAULT '', created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS connector_events (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL, at INTEGER NOT NULL, actor TEXT NOT NULL, op TEXT NOT NULL, note TEXT NOT NULL DEFAULT '');
        CREATE TABLE IF NOT EXISTS connector_logos (slug TEXT PRIMARY KEY, mime TEXT NOT NULL, bytes BLOB NOT NULL);
        CREATE TRIGGER IF NOT EXISTS connector_events_keep BEFORE UPDATE ON connector_events BEGIN SELECT RAISE(ABORT, 'connector events are append-only'); END;").map_err(|e| e.to_string())?;
    Ok(c)
}
fn now() -> i64 { crate::cli::now_ms() }
fn event(c: &Connection, slug: &str, actor: &str, op: &str, note: &str) { let _ = c.execute("INSERT INTO connector_events (slug, at, actor, op, note) VALUES (?1, ?2, ?3, ?4, ?5)", params![slug, now(), actor, op, note.chars().take(300).collect::<String>()]); }

/// The agent proposes: a new draft, or a rewrite of a draft that is not connected yet (a connected one is never overwritten from here).
pub fn propose(root: &str, recipe: &Value, actor: &str) -> Result<(String, bool), String> {
    let r = validate(recipe)?; let slug = s(&r["slug"]);
    let c = open(root)?;
    let old: Option<String> = c.query_row("SELECT status FROM connectors WHERE slug = ?1", params![slug], |x| x.get(0)).ok();
    if old.as_deref() == Some("connected") { return Err(format!("`{slug}` is already connected; the person removes it in the Connectors pane before a new recipe can replace it")); }
    let t = now();
    c.execute("INSERT INTO connectors (slug, title, recipe_json, status, note, created_at, updated_at) VALUES (?1, ?2, ?3, 'draft', '', ?4, ?4)
        ON CONFLICT(slug) DO UPDATE SET title = excluded.title, recipe_json = excluded.recipe_json, status = 'draft', note = '', updated_at = excluded.updated_at",
        params![slug, s(&r["title"]), serde_json::to_string(&r).unwrap_or_default(), t]).map_err(|e| e.to_string())?;
    event(&c, &slug, actor, if old.is_some() { "reproposed" } else { "proposed" }, &s(&r["summary"]));
    Ok((slug, old.is_none()))
}

fn row(c: &Connection, slug: &str) -> Result<Value, String> {
    c.query_row("SELECT slug, title, recipe_json, status, note, created_at, updated_at FROM connectors WHERE slug = ?1", params![slug], |x| {
        Ok(json!({ "slug": x.get::<_, String>(0)?, "title": x.get::<_, String>(1)?, "recipe": serde_json::from_str::<Value>(&x.get::<_, String>(2)?).unwrap_or(Value::Null), "status": x.get::<_, String>(3)?, "note": x.get::<_, String>(4)?, "created_at": x.get::<_, i64>(5)?, "updated_at": x.get::<_, i64>(6)? }))
    }).map_err(|_| format!("no connector called `{slug}`"))
}
fn rows(c: &Connection) -> Vec<Value> {
    let Ok(mut st) = c.prepare("SELECT slug FROM connectors ORDER BY updated_at DESC") else { return vec![] };
    let slugs: Vec<String> = st.query_map([], |x| x.get(0)).map(|r| r.flatten().collect()).unwrap_or_default();
    slugs.iter().filter_map(|g| row(c, g).ok()).collect()
}
fn set_status(c: &Connection, slug: &str, status: &str, note: &str) { let _ = c.execute("UPDATE connectors SET status = ?2, note = ?3, updated_at = ?4 WHERE slug = ?1", params![slug, status, note.chars().take(300).collect::<String>(), now()]); }

/// A row as the screen and the agent see it: never a secret, only which inputs are filled.
fn public(root: &str, r: &Value) -> Value {
    let env = read_env(root); let slug = s(&r["slug"]);
    let inputs: Vec<Value> = r["recipe"]["inputs"].as_array().into_iter().flatten().map(|i| { let mut i = i.clone(); i["filled"] = json!(env.contains_key(&env_key(&slug, &s(&i["id"])))); i }).collect();
    let mut recipe = r["recipe"].clone(); recipe["inputs"] = json!(inputs.clone());
    let oauth_ready = s(&recipe["auth"]) == "oauth" && oauth_ready(&env, &slug, &recipe);
    let missing: Vec<String> = if oauth_ready { vec![] } else { inputs.iter().filter(|i| i["filled"] != json!(true) && i["optional"] != json!(true)).map(|i| s(&i["id"])).collect() };
    json!({ "slug": slug, "title": r["title"], "status": r["status"], "note": r["note"], "kind": recipe["kind"], "auth": recipe["auth"], "access": recipe["access"], "site": recipe["site"], "summary": recipe["summary"], "recipe": recipe, "yaml": to_yaml(&r["recipe"]), "redirect_uri": if recipe["auth"] == "oauth" { json!(redirect_uri()) } else { Value::Null }, "oauth_ready": oauth_ready, "missing": missing, "updated_at": r["updated_at"], "has_logo": has_logo(root, &slug) })
}
fn has_logo(root: &str, slug: &str) -> bool { open(root).ok().and_then(|c| c.query_row("SELECT 1 FROM connector_logos WHERE slug = ?1", params![slug], |_| Ok(())).ok()).is_some() }

// ── the secrets: <project>/.antiloki/.env, mode 0600, out of git ──────────────────────────────────────────────────────────

pub fn env_key(slug: &str, id: &str) -> String { format!("ANTILOKI_CONNECTOR_{}_{}", slug.to_uppercase().replace('-', "_"), id.to_uppercase()) }
fn env_path(root: &str) -> PathBuf { dir_of(root).join(".env") }
pub fn read_env(root: &str) -> BTreeMap<String, String> {
    std::fs::read_to_string(env_path(root)).unwrap_or_default().lines().filter(|l| !l.trim_start().starts_with('#')).filter_map(|l| l.split_once('=').map(|(k, v)| (k.trim().to_string(), v.to_string()))).collect()
}
fn write_env(root: &str, m: &BTreeMap<String, String>) -> Result<(), String> {
    let d = dir_of(root); std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    if Path::new(root).join(".git").exists() { for l in [".antiloki/.env", ".antiloki/connectors.db"] { antiloki_rs::cockpit_cockpit_pane_context_bake::git_exclude_line(Path::new(root), l); } }
    let body: String = std::iter::once("# antiloki connectors: secrets stay on this machine and out of git\n".to_string()).chain(m.iter().map(|(k, v)| format!("{k}={v}\n"))).collect();
    let tmp = d.join(".env.tmp"); std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)); }
    std::fs::rename(&tmp, env_path(root)).map_err(|e| e.to_string())
}
fn put_secrets(root: &str, slug: &str, vals: &Value) -> Result<usize, String> {
    let mut m = read_env(root); let mut n = 0;
    for (id, v) in vals.as_object().into_iter().flatten() {
        let t = v.as_str().unwrap_or("").trim(); if t.is_empty() { continue; }
        if !ident(id, 30, false) { return Err(format!("`{id}` is not an input")); }
        if t.contains('\n') || t.contains('\r') { return Err("a value is one line".into()); }
        m.insert(env_key(slug, id), t.to_string()); n += 1;
    }
    write_env(root, &m)?; Ok(n)
}
fn drop_secrets(root: &str, slug: &str) { let mut m = read_env(root); let p = format!("ANTILOKI_CONNECTOR_{}_", slug.to_uppercase().replace('-', "_")); m.retain(|k, _| !k.starts_with(&p)); let _ = write_env(root, &m); }

// ── calling out ────────────────────────────────────────────────────────────────────────────────────────────────────────────

fn sign(root: &str, recipe: &Value) -> Result<Vec<(String, String)>, String> {
    let h = &recipe["auth_header"]; if !h.is_object() { return Ok(vec![]); }
    let env = read_env(root); let slug = s(&recipe["slug"]);
    let mut t = s(&h["template"]);
    for p in placeholders(&t.clone()) { let v = env.get(&env_key(&slug, &p)).ok_or_else(|| format!("`{p}` is not filled in yet: the person completes the connector in the Connectors pane"))?; t = t.replace(&format!("{{{p}}}"), v); }
    Ok(vec![(s(&h["name"]), t)])
}
fn client() -> Result<reqwest::Client, String> { reqwest::Client::builder().timeout(std::time::Duration::from_secs(20)).redirect(reqwest::redirect::Policy::none()).build().map_err(|e| e.to_string()) }
fn clip(t: String) -> String { if t.chars().count() > MAX_REPLY { format!("{}\n… (cut at {MAX_REPLY} characters)", t.chars().take(MAX_REPLY).collect::<String>()) } else { t } }

/// A short-lived sign-in (Google's lasts an hour) is renewed with the refresh token the service gave at the first sign-in.
pub async fn refresh(root: &str, recipe: &Value) -> Result<(), String> {
    let slug = s(&recipe["slug"]); let o = &recipe["oauth"]; if s(&recipe["auth"]) != "oauth" { return Err("not an OAuth connector".into()); }
    let env = read_env(root);
    let rt = env.get(&env_key(&slug, "refresh_token")).cloned().ok_or("the sign-in expired and the service gave no refresh token: the person presses Connect again")?;
    let pick = |a: &str, b: &str| env.get(&env_key(&slug, a)).filter(|v| !v.is_empty()).or_else(|| if b.is_empty() { None } else { env.get(&env_key(&slug, b)).filter(|v| !v.is_empty()) }).cloned().unwrap_or_default();
    let (cid, csec) = (pick("oauth_client_id", &s(&o["client_id_input"])), pick("oauth_client_secret", &s(&o["client_secret_input"])));
    let mut form = vec![("grant_type", "refresh_token".to_string()), ("refresh_token", rt), ("client_id", cid)];
    if !csec.is_empty() { form.push(("client_secret", csec)); }
    let resp = client()?.post(s(&o["token_url"])).header("Accept", "application/json").form(&form).send().await.map_err(|e| format!("the service did not answer the renewal: {e}"))?;
    let v: Value = resp.json().await.map_err(|_| "the service did not answer the renewal with JSON".to_string())?;
    let tok = s(&v["access_token"]); if tok.is_empty() { return Err(format!("the sign-in could not be renewed ({}): the person presses Connect again", s(&v["error"]))); }
    let mut vals = json!({ "access_token": tok }); if !s(&v["refresh_token"]).is_empty() { vals["refresh_token"] = v["refresh_token"].clone(); }
    put_secrets(root, &slug, &vals)?;
    if let Ok(c) = open(root) { event(&c, &slug, "antiloki", "renewed-sign-in", ""); }
    Ok(())
}

/// One call of an api connector (renewing an expired OAuth sign-in once): only toward its own base address, GET/HEAD only when it is read-only.
pub async fn api_call(root: &str, recipe: &Value, method: &str, path: &str, query: &Value, body: &Value) -> Result<(u16, String), String> {
    let r = api_call_once(root, recipe, method, path, query, body).await?;
    if r.0 == 401 && s(&recipe["auth"]) == "oauth" && refresh(root, recipe).await.is_ok() { return api_call_once(root, recipe, method, path, query, body).await; }
    Ok(r)
}
async fn api_call_once(root: &str, recipe: &Value, method: &str, path: &str, query: &Value, body: &Value) -> Result<(u16, String), String> {
    let method = if method.is_empty() { "GET".to_string() } else { method.to_uppercase() };
    if !matches!(method.as_str(), "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE") { return Err(format!("`{method}` is not an HTTP method antiloki sends")); }
    if s(&recipe["access"]) != "read-write" && !matches!(method.as_str(), "GET" | "HEAD") { return Err("this connector is read-only: only GET and HEAD are sent. To allow writes, the person removes it in the Connectors pane and approves a new recipe with access `read-write`".into()); }
    if !path_ok(path) { return Err("`path` starts with one `/` and stays inside the service (no `..`, no `@`)".into()); }
    let base = s(&recipe["base_url"]); let base_u = safe_url(&base)?;
    let mut u = url::Url::parse(&format!("{}{}", base.trim_end_matches('/'), path)).map_err(|e| e.to_string())?;
    if u.host_str() != base_u.host_str() || u.port() != base_u.port() || u.scheme() != base_u.scheme() { return Err("that path leaves the service's own address".into()); }
    for (k, v) in query.as_object().into_iter().flatten() { u.query_pairs_mut().append_pair(k, &v.as_str().map(String::from).unwrap_or_else(|| v.to_string())); }
    let mut rq = client()?.request(method.parse().map_err(|_| "bad method".to_string())?, u).header("User-Agent", "antiloki-connector").header("Accept", "application/json");
    for (k, v) in sign(root, recipe)? { rq = rq.header(k, v); }
    if !body.is_null() { rq = rq.json(body); }
    let r = rq.send().await.map_err(|e| format!("could not reach the service: {e}"))?;
    let code = r.status().as_u16(); let text = r.text().await.unwrap_or_default();
    Ok((code, clip(text)))
}

/// A JSON-RPC call to a remote MCP server over HTTP (a plain JSON answer or an event stream with one `data:` line).
async fn mcp_rpc(cl: &reqwest::Client, url: &str, headers: &[(String, String)], session: &mut Option<String>, id: u64, method: &str, params: Value) -> Result<Value, String> {
    let mut rq = cl.post(url).header("Content-Type", "application/json").header("Accept", "application/json, text/event-stream").header("User-Agent", "antiloki-connector");
    for (k, v) in headers { rq = rq.header(k, v); }
    if let Some(sid) = session.as_ref() { rq = rq.header("Mcp-Session-Id", sid); }
    let msg = if id == 0 { json!({ "jsonrpc": "2.0", "method": method, "params": params }) } else { json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }) };
    let r = rq.json(&msg).send().await.map_err(|e| format!("could not reach the MCP server: {e}"))?;
    if let Some(h) = r.headers().get("mcp-session-id").and_then(|h| h.to_str().ok()) { *session = Some(h.to_string()); }
    let status = r.status(); let text = r.text().await.unwrap_or_default();
    if !status.is_success() { return Err(format!("the MCP server answered HTTP {}: {}", status.as_u16(), text.chars().take(200).collect::<String>())); }
    if id == 0 { return Ok(Value::Null); }
    let body = text.lines().filter_map(|l| l.strip_prefix("data:")).map(|l| l.trim()).filter(|l| l.starts_with('{')).last().map(String::from).unwrap_or(text);
    let v: Value = serde_json::from_str(body.trim()).map_err(|_| "the MCP server did not answer with JSON-RPC".to_string())?;
    if v["error"].is_object() { return Err(format!("the MCP server said: {}", s(&v["error"]["message"]))); }
    Ok(v["result"].clone())
}
async fn mcp_session(root: &str, recipe: &Value) -> Result<(reqwest::Client, String, Vec<(String, String)>, Option<String>), String> {
    let (cl, url, headers) = (client()?, s(&recipe["url"]), sign(root, recipe)?); safe_url(&url)?;
    let (mut headers, mut session) = (headers, None);
    let init = json!({ "protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": { "name": "antiloki", "version": "1" } });
    match mcp_rpc(&cl, &url, &headers, &mut session, 1, "initialize", init.clone()).await {
        Err(e) if e.contains("HTTP 401") && s(&recipe["auth"]) == "oauth" => { refresh(root, recipe).await?; headers = sign(root, recipe)?; session = None; mcp_rpc(&cl, &url, &headers, &mut session, 1, "initialize", init).await?; }
        r => { r?; }
    }
    let _ = mcp_rpc(&cl, &url, &headers, &mut session, 0, "notifications/initialized", json!({})).await;
    Ok((cl, url, headers, session))
}
pub async fn mcp_tools(root: &str, recipe: &Value) -> Result<Vec<Value>, String> {
    let (cl, url, h, mut sess) = mcp_session(root, recipe).await?;
    let r = mcp_rpc(&cl, &url, &h, &mut sess, 2, "tools/list", json!({})).await?;
    Ok(r["tools"].as_array().cloned().unwrap_or_default())
}
/// Is this tool one that only reads? A tool says so itself (`readOnlyHint`); the common read verbs count as well.
pub fn tool_reads(t: &Value) -> bool {
    if t["annotations"]["readOnlyHint"] == json!(true) { return true; }
    let n = s(&t["name"]).to_lowercase(); ["get", "list", "read", "search", "find", "fetch", "describe", "show", "query", "lookup"].iter().any(|v| n.starts_with(v) || n.contains(&format!("_{v}")))
}
pub async fn mcp_call(root: &str, recipe: &Value, tool: &str, args: &Value) -> Result<String, String> {
    let (cl, url, h, mut sess) = mcp_session(root, recipe).await?;
    if s(&recipe["access"]) != "read-write" {
        let r = mcp_rpc(&cl, &url, &h, &mut sess, 2, "tools/list", json!({})).await?;
        let t = r["tools"].as_array().into_iter().flatten().find(|t| s(&t["name"]) == tool).cloned().ok_or_else(|| format!("the server has no tool `{tool}`"))?;
        if !tool_reads(&t) { return Err(format!("`{tool}` may change things and this connector is read-only")); }
    }
    let r = mcp_rpc(&cl, &url, &h, &mut sess, 3, "tools/call", json!({ "name": tool, "arguments": if args.is_object() { args.clone() } else { json!({}) } })).await?;
    let text: Vec<String> = r["content"].as_array().into_iter().flatten().filter_map(|c| c["text"].as_str().map(String::from)).collect();
    Ok(clip(if text.is_empty() { serde_json::to_string(&r).unwrap_or_default() } else { text.join("\n") }))
}

/// The test of a connector: one harmless read. Sets the status.
pub async fn test(root: &str, slug: &str) -> Result<String, String> {
    let r = { let c = open(root)?; row(&c, slug)? }; let recipe = r["recipe"].clone();
    let res: Result<String, String> = async {
        if s(&recipe["kind"]) == "mcp" { let t = mcp_tools(root, &recipe).await?; Ok(format!("connected: the server offers {} tool(s)", t.len())) }
        else { let (code, body) = api_call(root, &recipe, "GET", &s(&recipe["test"]["path"]), &Value::Null, &Value::Null).await?; if (200..300).contains(&code) { Ok(format!("connected: HTTP {code}")) } else { Err(format!("the service answered HTTP {code}: {}", body.chars().take(160).collect::<String>())) } }
    }.await;
    let c = open(root)?;
    match &res { Ok(n) => { set_status(&c, slug, "connected", n); event(&c, slug, "person", "tested", n); } Err(e) => { set_status(&c, slug, "error", e); event(&c, slug, "person", "test-failed", e); } }
    res
}

// ── the logo: from the service's own address, small, an image ────────────────────────────────────────────────────────────────

/// A client for the logo only: it follows up to three redirects, each to an address that passes `safe_url` (https, or http on this machine).
fn logo_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder().timeout(std::time::Duration::from_secs(8)).redirect(reqwest::redirect::Policy::custom(|a| { if a.previous().len() >= 3 { return a.stop(); } if safe_url(a.url().as_str()).is_ok() { a.follow() } else { a.stop() } })).build().map_err(|e| e.to_string())
}
pub async fn fetch_logo(root: &str, slug: &str, site: &str) {
    let Ok(base) = safe_url(site) else { return }; let Some(host) = base.host_str() else { return };
    let origin = format!("{}://{}{}", base.scheme(), host, base.port().map(|p| format!(":{p}")).unwrap_or_default());
    let Ok(cl) = logo_client() else { return };
    for p in ["/apple-touch-icon.png", "/favicon.ico"] {
        let Ok(r) = cl.get(format!("{origin}{p}")).send().await else { continue }; if !r.status().is_success() { continue; }
        let mime = r.headers().get("content-type").and_then(|h| h.to_str().ok()).unwrap_or("").split(';').next().unwrap_or("").trim().to_string();
        let ok_mime = mime.starts_with("image/") && mime != "image/svg+xml" || mime == "application/octet-stream" && p.ends_with(".ico");
        if !ok_mime { continue; }
        let Ok(b) = r.bytes().await else { continue }; if b.is_empty() || b.len() > 262_144 { continue; }
        let mime = if mime == "application/octet-stream" { "image/x-icon".to_string() } else { mime };
        if let Ok(c) = open(root) { let _ = c.execute("INSERT INTO connector_logos (slug, mime, bytes) VALUES (?1, ?2, ?3) ON CONFLICT(slug) DO UPDATE SET mime = excluded.mime, bytes = excluded.bytes", params![slug, mime, b.to_vec()]); }
        return;
    }
}

// ── OAuth: the person clicks Connect, the service's own page asks them, the code comes back to this machine ─────────────────────
// WHICH CLIENT SIGNS IN (operator 2026-11-25: "can't it be one click authorize, like other tools?"). A service wants to know WHICH app asks. In order:
//   1. the client the person pasted into the pane (their own Google Cloud client, say);
//   2. a BUILT-IN client antiloki ships for that service (`oauth.builtin`, e.g. google): its id lives in `oauth-clients.json` (or the table below);
//   3. one antiloki REGISTERS ITSELF at the service (`oauth.register_url`, RFC 7591: remote MCP servers such as Notion or Linear allow it).
// With 2 or 3 the connector needs no input at all: the pane shows one Connect button. The sign-in always uses PKCE.

/// Clients antiloki ships (name, client id, client secret or ""). Register antiloki once with the service, then add the id here or in `oauth-clients.json`.
const SHIPPED_CLIENTS: &[(&str, &str, &str)] = &[];
/// `<antiloki home>/oauth-clients.json` (or `$ANTILOKI_OAUTH_CLIENTS`): `{"google": {"client_id": "...", "client_secret": "..."}}`.
pub fn builtin_client(name: &str) -> Option<(String, String)> {
    if name.is_empty() { return None; }
    let path = std::env::var("ANTILOKI_OAUTH_CLIENTS").ok().filter(|p| !p.is_empty()).map(PathBuf::from).unwrap_or_else(|| {
        let home = std::env::var("ANTILOKI_HOME").ok().filter(|h| !h.is_empty()).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".antiloki"));
        home.join("oauth-clients.json")
    });
    if let Some(v) = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) { let id = s(&v[name]["client_id"]); if !id.is_empty() { return Some((id, s(&v[name]["client_secret"]))); } }
    SHIPPED_CLIENTS.iter().find(|(n, id, _)| *n == name && !id.is_empty()).map(|(_, id, sec)| (id.to_string(), sec.to_string()))
}
/// Can Connect work right now without the person pasting anything?
pub fn oauth_ready(env: &BTreeMap<String, String>, slug: &str, recipe: &Value) -> bool {
    let o = &recipe["oauth"]; let have = |id: &str| !id.is_empty() && env.get(&env_key(slug, id)).map_or(false, |v| !v.is_empty());
    have(&s(&o["client_id_input"])) || have("oauth_client_id") || builtin_client(&s(&o["builtin"])).is_some() || !s(&o["register_url"]).is_empty()
}
async fn oauth_client(root: &str, slug: &str, rc: &Value) -> Result<(String, String), String> {
    let env = read_env(root); let o = &rc["oauth"];
    let key = |id: &str| env.get(&env_key(slug, id)).cloned().unwrap_or_default();
    let (cin, sin) = (s(&o["client_id_input"]), s(&o["client_secret_input"]));
    if !cin.is_empty() && !key(&cin).is_empty() { return Ok((key(&cin), if sin.is_empty() { String::new() } else { key(&sin) })); }
    if let Some(b) = builtin_client(&s(&o["builtin"])) { return Ok(b); }
    let reg = s(&o["register_url"]);
    if !reg.is_empty() {
        if !key("oauth_client_id").is_empty() { return Ok((key("oauth_client_id"), key("oauth_client_secret"))); }
        // the service lets an app register itself (RFC 7591): antiloki says who it is and where the answer goes
        let resp = client()?.post(&reg).header("Accept", "application/json").json(&json!({ "client_name": "antiloki", "redirect_uris": [redirect_uri()], "grant_types": ["authorization_code", "refresh_token"], "response_types": ["code"], "token_endpoint_auth_method": "none" }))
            .send().await.map_err(|e| format!("the service did not answer the registration: {e}"))?;
        let v: Value = resp.json().await.map_err(|_| "the service did not answer the registration with JSON".to_string())?;
        let id = s(&v["client_id"]); if id.is_empty() { return Err(format!("the service would not register antiloki ({})", { let e = s(&v["error_description"]); if e.is_empty() { s(&v["error"]) } else { e } })); }
        let sec = s(&v["client_secret"]);
        put_secrets(root, slug, &json!({ "oauth_client_id": id, "oauth_client_secret": sec }))?;
        return Ok((id, sec));
    }
    Err("fill in the client id first (antiloki has no built-in sign-in for this service yet)".into())
}

struct Pending { slug: String, at: i64, verifier: String, cid: String, csec: String }
fn oauth_states() -> &'static std::sync::Mutex<HashMap<String, Pending>> { static M: std::sync::OnceLock<std::sync::Mutex<HashMap<String, Pending>>> = std::sync::OnceLock::new(); M.get_or_init(Default::default) }
fn redirect_uri() -> String { format!("http://127.0.0.1:{}/api/v1/connectors/oauth/callback", std::env::var("ANTILOKI_SERVE_PORT").ok().and_then(|p| p.parse::<u16>().ok()).unwrap_or(41931)) }
fn random_text(n: usize) -> String { use rand::{distributions::Alphanumeric, Rng}; rand::thread_rng().sample_iter(&Alphanumeric).take(n).map(char::from).collect() }

pub async fn oauth_start(root: &str, slug: &str) -> Result<String, String> {
    let r = { let c = open(root)?; row(&c, slug)? }; let rc = &r["recipe"]; if s(&rc["auth"]) != "oauth" { return Err("this connector does not use OAuth".into()); }
    let o = &rc["oauth"];
    let (cid, csec) = oauth_client(root, slug, rc).await?;
    let mut u = url::Url::parse(&s(&o["authorize_url"])).map_err(|e| e.to_string())?;
    let (state, verifier) = (random_text(32), random_text(64));
    let challenge = { use base64::Engine; use sha2::Digest; base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes())) };
    { let mut m = oauth_states().lock().unwrap(); m.retain(|_, p| now() - p.at < 600_000); m.insert(state.clone(), Pending { slug: slug.to_string(), at: now(), verifier, cid: cid.clone(), csec }); }
    u.query_pairs_mut().append_pair("response_type", "code").append_pair("client_id", &cid).append_pair("redirect_uri", &redirect_uri()).append_pair("state", &state)
        .append_pair("code_challenge", &challenge).append_pair("code_challenge_method", "S256")
        .append_pair("scope", &o["scopes"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" ")).unwrap_or_default());
    for (k, v) in o["extra"].as_object().into_iter().flatten() { u.query_pairs_mut().append_pair(k, v.as_str().unwrap_or("")); }
    Ok(u.to_string())
}
async fn oauth_finish(root: &str, state: &str, code: &str) -> Result<String, String> {
    let p = { let mut m = oauth_states().lock().unwrap(); let p = m.remove(state).ok_or("this sign-in link is not one antiloki started (or it expired): press Connect again")?; if now() - p.at > 600_000 { return Err("this sign-in link expired: press Connect again".into()); } p };
    let r = { let c = open(root)?; row(&c, &p.slug)? }; let o = &r["recipe"]["oauth"];
    let mut form = vec![("grant_type", "authorization_code".to_string()), ("code", code.to_string()), ("redirect_uri", redirect_uri()), ("client_id", p.cid.clone()), ("code_verifier", p.verifier.clone())];
    if !p.csec.is_empty() { form.push(("client_secret", p.csec.clone())); }
    let resp = client()?.post(s(&o["token_url"])).header("Accept", "application/json").form(&form).send().await.map_err(|e| format!("the service did not answer: {e}"))?;
    let v: Value = resp.json().await.map_err(|_| "the service did not answer with JSON".to_string())?;
    let tok = s(&v["access_token"]); if tok.is_empty() { return Err(format!("the service refused: {}", s(&v["error_description"]).chars().take(160).collect::<String>().replace('\n', " "))); }
    // keep which client signed in, so a later renewal uses the same one
    let mut vals = json!({ "access_token": tok, "oauth_client_id": p.cid, "oauth_client_secret": p.csec }); if !s(&v["refresh_token"]).is_empty() { vals["refresh_token"] = v["refresh_token"].clone(); }
    put_secrets(root, &p.slug, &vals)?;
    Ok(p.slug)
}

// ── the routes the Connectors pane uses ───────────────────────────────────────────────────────────────────────────────────────

fn reply(v: Value) -> Response { (StatusCode::OK, Json(v)).into_response() }
fn bad(msg: &str) -> Response { (StatusCode::OK, Json(json!({ "ok": false, "error": msg }))).into_response() }
fn body_json(b: &Bytes) -> Value { serde_json::from_slice(b).unwrap_or(Value::Null) }

pub async fn list_get(state: Arc<AppState>) -> Response {
    let root = crate::agents_http::active_root(&state).await;
    let r2 = root.clone();
    let list = tokio::task::spawn_blocking(move || open(&r2).map(|c| rows(&c).iter().map(|r| public(&r2, r)).collect::<Vec<_>>()).unwrap_or_default()).await.unwrap_or_default();
    reply(json!({ "ok": true, "connectors": list }))
}
pub async fn get_get(state: Arc<AppState>, q: Query<HashMap<String, String>>) -> Response {
    let root = crate::agents_http::active_root(&state).await; let slug = q.get("slug").cloned().unwrap_or_default();
    let r = tokio::task::spawn_blocking(move || open(&root).and_then(|c| row(&c, &slug)).map(|r| public(&root, &r))).await.unwrap_or_else(|e| Err(e.to_string()));
    match r { Ok(v) => reply(json!({ "ok": true, "connector": v })), Err(e) => bad(&e) }
}
/// `POST /api/v1/connectors/save {slug, inputs: {id: value}}`: the person's values go to `.antiloki/.env`; then the connector is tested.
pub async fn save_post(state: Arc<AppState>, body: Bytes) -> Response {
    let b = body_json(&body); let slug = s(&b["slug"]); let root = crate::agents_http::active_root(&state).await;
    let (r2, s2, vals) = (root.clone(), slug.clone(), b["inputs"].clone());
    let saved = tokio::task::spawn_blocking(move || { let c = open(&r2)?; let r = row(&c, &s2)?; let n = put_secrets(&r2, &s2, &vals)?; event(&c, &s2, "person", "saved-inputs", &format!("{n} value(s)")); Ok::<_, String>(()) }).await.unwrap_or_else(|e| Err(e.to_string()));
    if let Err(e) = saved { return bad(&e); }
    clear_note(&state, &slug).await;
    let rec = { let (r3, s3) = (root.clone(), slug.clone()); tokio::task::spawn_blocking(move || open(&r3).and_then(|c| row(&c, &s3))).await.unwrap_or_else(|e| Err(e.to_string())).ok() };
    let auth = rec.as_ref().map(|r| s(&r["recipe"]["auth"])).unwrap_or_default();
    // an OAuth connector is tested after the person has signed in, not now
    let res = if auth == "oauth" { Ok("saved: now press Connect to sign in".to_string()) } else { test(&root, &slug).await };
    let (r4, s4) = (root.clone(), slug.clone());
    let v = tokio::task::spawn_blocking(move || open(&r4).and_then(|c| row(&c, &s4)).map(|r| public(&r4, &r))).await.unwrap_or_else(|e| Err(e.to_string()));
    match (res, v) { (Ok(m), Ok(v)) => reply(json!({ "ok": true, "message": m, "connector": v })), (Err(e), Ok(v)) => reply(json!({ "ok": false, "error": e, "connector": v })), (_, Err(e)) => bad(&e) }
}
pub async fn test_post(state: Arc<AppState>, body: Bytes) -> Response {
    let slug = s(&body_json(&body)["slug"]); let root = crate::agents_http::active_root(&state).await;
    let res = test(&root, &slug).await; clear_note(&state, &slug).await;
    let (r4, s4) = (root.clone(), slug.clone());
    let v = tokio::task::spawn_blocking(move || open(&r4).and_then(|c| row(&c, &s4)).map(|r| public(&r4, &r))).await.unwrap_or_else(|e| Err(e.to_string()));
    match (res, v) { (Ok(m), Ok(v)) => reply(json!({ "ok": true, "message": m, "connector": v })), (Err(e), Ok(v)) => reply(json!({ "ok": false, "error": e, "connector": v })), (_, Err(e)) => bad(&e) }
}
pub async fn remove_post(state: Arc<AppState>, body: Bytes) -> Response {
    let slug = s(&body_json(&body)["slug"]); let root = crate::agents_http::active_root(&state).await;
    let r = tokio::task::spawn_blocking(move || { let c = open(&root)?; row(&c, &slug)?; drop_secrets(&root, &slug); c.execute("DELETE FROM connectors WHERE slug = ?1", params![slug]).map_err(|e| e.to_string())?; let _ = c.execute("DELETE FROM connector_logos WHERE slug = ?1", params![slug]); event(&c, &slug, "person", "removed", ""); Ok::<_, String>(()) }).await.unwrap_or_else(|e| Err(e.to_string()));
    match r { Ok(()) => reply(json!({ "ok": true })), Err(e) => bad(&e) }
}
/// What agents may do through this connector. Only the person changes it (the pane's switch); an agent's recipe never can, and a connected one is not overwritten.
pub fn set_access(root: &str, slug: &str, access: &str) -> Result<(), String> {
    if access != "read-only" && access != "read-write" { return Err("access is `read-only` or `read-write`".into()); }
    let c = open(root)?; let r = row(&c, slug)?; let mut recipe = r["recipe"].clone(); recipe["access"] = json!(access);
    c.execute("UPDATE connectors SET recipe_json = ?2, updated_at = ?3 WHERE slug = ?1", params![slug, serde_json::to_string(&recipe).unwrap_or_default(), now()]).map_err(|e| e.to_string())?;
    event(&c, slug, "person", "access-changed", access); Ok(())
}
pub async fn access_post(state: Arc<AppState>, body: Bytes) -> Response {
    let b = body_json(&body); let (slug, access) = (s(&b["slug"]), s(&b["access"])); let root = crate::agents_http::active_root(&state).await;
    let r = tokio::task::spawn_blocking(move || { set_access(&root, &slug, &access)?; let c = open(&root)?; row(&c, &slug).map(|r| public(&root, &r)) }).await.unwrap_or_else(|e| Err(e.to_string()));
    match r { Ok(v) => reply(json!({ "ok": true, "connector": v })), Err(e) => bad(&e) }
}
pub async fn oauth_start_get(state: Arc<AppState>, q: Query<HashMap<String, String>>) -> Response {
    let root = crate::agents_http::active_root(&state).await; let slug = q.get("slug").cloned().unwrap_or_default();
    match oauth_start(&root, &slug).await { Ok(u) => reply(json!({ "ok": true, "url": u })), Err(e) => bad(&e) }
}
pub async fn oauth_callback_get(state: Arc<AppState>, q: Query<HashMap<String, String>>) -> Response {
    let root = crate::agents_http::active_root(&state).await;
    let page = |t: &str, ok: bool| (StatusCode::OK, [("content-type", "text/html; charset=utf-8")], format!("<!doctype html><meta charset=utf-8><title>antiloki</title><body style=\"font:16px system-ui;padding:48px;max-width:34em\"><h2>{}</h2><p>{}</p>", if ok { "Connected" } else { "Not connected" }, t.replace('<', "&lt;"))).into_response();
    if let Some(e) = q.get("error") { return page(&format!("The service said: {e}. Close this tab and try again in antiloki."), false); }
    let (state_p, code) = (q.get("state").cloned().unwrap_or_default(), q.get("code").cloned().unwrap_or_default());
    match oauth_finish(&root, &state_p, &code).await {
        Ok(slug) => { let res = test(&root, &slug).await; clear_note(&state, &slug).await; match res { Ok(m) => page(&format!("{m}. You can close this tab and go back to antiloki."), true), Err(e) => page(&format!("Signed in, but the test failed: {e}"), false) } }
        Err(e) => page(&e, false),
    }
}
pub async fn logo_get(state: Arc<AppState>, q: Query<HashMap<String, String>>) -> Response {
    let root = crate::agents_http::active_root(&state).await; let slug = q.get("slug").cloned().unwrap_or_default();
    let r = tokio::task::spawn_blocking(move || open(&root).ok().and_then(|c| c.query_row("SELECT mime, bytes FROM connector_logos WHERE slug = ?1", params![slug], |x| Ok((x.get::<_, String>(0)?, x.get::<_, Vec<u8>>(1)?))).ok())).await.unwrap_or(None);
    match r { Some((m, b)) => (StatusCode::OK, [("content-type", m), ("cache-control", "max-age=300".to_string())], b).into_response(), None => StatusCode::NOT_FOUND.into_response() }
}

// the "needs you" note on the pane whose agent asked, so the person sees there is something to finish
fn asked_by() -> &'static std::sync::Mutex<HashMap<String, i64>> { static M: std::sync::OnceLock<std::sync::Mutex<HashMap<String, i64>>> = std::sync::OnceLock::new(); M.get_or_init(Default::default) }
async fn clear_note(_state: &Arc<AppState>, slug: &str) { let p = asked_by().lock().ok().and_then(|mut m| m.remove(slug)); if let Some(p) = p { crate::cowork::note_dialog(p, None); } }

// ── the agent's tool ────────────────────────────────────────────────────────────────────────────────────────────────────────────

async fn run_on_screen_ok(slug: &str) -> Result<Value, String> { crate::cowork_mcp::run_on_screen("connector_open", &json!({ "slug": slug }), 8_000).await.map_err(|e| format!("could not open it: {e}. Tell the person to open the new-pane menu and choose Connectors.")) }
/// `antiloki_connector`: ops `propose` · `list` · `status` · `tools` · `call` · `open`.
pub async fn run_tool(state: &Arc<AppState>, you: i64, tool: &str, args: &Value) -> Result<String, String> {
    if tool != "antiloki_connector" { return Err(format!("unknown tool {tool}")); }
    let root = crate::agents_http::active_root(state).await;
    let op = { let o = s(&args["op"]); if o.is_empty() { "list".to_string() } else { o } };
    match op.as_str() {
        "propose" | "ask" => {
            let recipe = if args["recipe"].is_object() { args["recipe"].clone() } else { args.clone() };
            let (r2, rc) = (root.clone(), recipe.clone());
            let (slug, fresh) = tokio::task::spawn_blocking(move || propose(&r2, &rc, "agent")).await.map_err(|e| e.to_string())??;
            let site = s(&recipe["site"]); if !site.is_empty() { fetch_logo(&root, &slug, &site).await; }
            if you > 0 { crate::cowork::note_dialog(you, Some(format!("Connector `{slug}` is ready to set up: open the Connectors pane and finish it"))); if let Ok(mut m) = asked_by().lock() { m.insert(slug.clone(), you); } }
            // OPEN IT FOR THE PERSON (operator 2026-11-25: "it should have already opened for me"): the Connectors pane comes up on this connector's screen. A specialist cannot open panes, so the serve does it; with no screen open the note above is the fallback.
            let shown = crate::cowork_mcp::run_on_screen("connector_open", &json!({ "slug": slug }), 8_000).await.is_ok();
            Ok(format!("{} connector `{slug}` as a draft. {} It is NOT connected: the person follows your steps, enters what you asked for and presses \"Save and test\" (or Connect, for OAuth). Check `status` with slug `{slug}` later. You never see the key.", if fresh { "Proposed" } else { "Rewrote" }, if shown { "Its connection screen is now open on the person's screen: you do not need to tell them where to click." } else { "No antiloki screen answered, so tell the person to open the Connectors pane (new-pane menu, Connectors)." }))
        }
        // OPEN THE PANE (operator 2026-11-25: "open the connectors" was refused because an agent had no way to): the serve opens it on the person's screen, on one connector if `slug` is given, else the list. A specialist cannot open panes, so this is the door.
        "open" => {
            let slug = s(&args["slug"]);
            if !slug.is_empty() { let (r2, g2) = (root.clone(), slug.clone()); tokio::task::spawn_blocking(move || open(&r2).and_then(|c| row(&c, &g2))).await.map_err(|e| e.to_string())??; }
            run_on_screen_ok(&slug).await.map(|_| if slug.is_empty() { "The Connectors pane is open on the person's screen.".to_string() } else { format!("The Connectors pane is open on the person's screen, on `{slug}`.") })
        }
        "list" => {
            let r2 = root.clone(); let list = tokio::task::spawn_blocking(move || open(&r2).map(|c| rows(&c).iter().map(|r| public(&r2, r)).collect::<Vec<_>>())).await.map_err(|e| e.to_string())??;
            if list.is_empty() { return Ok("no connectors yet. When the person wants a service connected (Drive, Slack, a database, any API), use op `propose` with a recipe.".into()); }
            Ok(list.iter().map(|c| format!("- {} ({}) · {} · {} · {}{}", s(&c["slug"]), s(&c["title"]), s(&c["kind"]), s(&c["access"]), s(&c["status"]), if s(&c["note"]).is_empty() { String::new() } else { format!(" · {}", s(&c["note"])) })).collect::<Vec<_>>().join("\n"))
        }
        "status" | "tools" | "call" => {
            let slug = s(&args["slug"]); let (r2, s2) = (root.clone(), slug.clone());
            let r = tokio::task::spawn_blocking(move || open(&r2).and_then(|c| row(&c, &s2))).await.map_err(|e| e.to_string())??;
            if op == "status" { let p = public(&root, &r); return Ok(format!("{} ({}): {}{}\nmissing inputs: {}", s(&p["slug"]), s(&p["title"]), s(&p["status"]), if s(&p["note"]).is_empty() { String::new() } else { format!(" · {}", s(&p["note"])) }, if p["missing"].as_array().map_or(true, |a| a.is_empty()) { "none".to_string() } else { p["missing"].as_array().unwrap().iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ") })); }
            if s(&r["status"]) != "connected" { return Err(format!("`{slug}` is {}: the person finishes it in the Connectors pane first", s(&r["status"]))); }
            let recipe = r["recipe"].clone();
            if op == "tools" {
                if s(&recipe["kind"]) == "api" { return Ok(format!("{} is an HTTP API at {} ({}). Use op `call` with `path` (starting with /), optional `method`, `query`, `body`.", slug, s(&recipe["base_url"]), s(&recipe["access"]))); }
                let t = mcp_tools(&root, &recipe).await?;
                return Ok(t.iter().map(|t| format!("- {} [{}]: {}", s(&t["name"]), if tool_reads(t) { "reads" } else { "may change things" }, s(&t["description"]).chars().take(160).collect::<String>())).collect::<Vec<_>>().join("\n"));
            }
            if s(&recipe["kind"]) == "mcp" { return mcp_call(&root, &recipe, &s(&args["tool"]), &args["arguments"]).await; }
            let (code, text) = api_call(&root, &recipe, &s(&args["method"]), &s(&args["path"]), &args["query"], &args["body"]).await?;
            Ok(format!("HTTP {code}\n{text}"))
        }
        _ => Err("op is `propose`, `list`, `open`, `status`, `tools` or `call`".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn recipe() -> Value { json!({ "title": "Mock API", "site": "http://127.0.0.1:9", "kind": "api", "auth": "key", "access": "read-only", "base_url": "http://127.0.0.1:9/v1", "summary": "reads the mock",
        "inputs": [{ "id": "token", "label": "Token", "secret": true, "help": "in settings", "link": "http://127.0.0.1:9/settings" }], "auth_header": { "name": "Authorization", "template": "Bearer {token}" },
        "steps": ["Open settings", "Copy the token", "Paste it here"], "test": { "path": "/me" } }) }
    fn tmp(tag: &str) -> String { let d = std::env::temp_dir().join(format!("antiloki-conn-{tag}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d.to_string_lossy().to_string() }

    #[test]
    fn a_good_recipe_is_kept_and_a_bad_one_says_what_to_fix() {
        let r = validate(&recipe()).unwrap(); assert_eq!(r["slug"], "mock-api"); assert_eq!(r["inputs"][0]["secret"], true); assert_eq!(r["test"]["path"], "/me");
        for (k, v, why) in [("kind", json!("ftp"), "kind"), ("base_url", json!("http://evil.example.com/x"), "https"), ("base_url", json!("https://u:p@host.example/x"), "user name"), ("steps", json!([]), "steps")] {
            let mut x = recipe(); x[k] = v; let e = validate(&x).unwrap_err(); assert!(e.contains(why), "{k}: {e}");
        }
        let mut x = recipe(); x["auth_header"]["template"] = json!("Bearer {nope}"); assert!(validate(&x).unwrap_err().contains("not an input"));
        let mut x = recipe(); x["test"] = json!({ "path": "/../etc" }); assert!(validate(&x).is_err());
        let mut x = recipe(); x["inputs"] = json!([{ "id": "Bad Id", "label": "x" }]); assert!(validate(&x).unwrap_err().contains("input id"));
    }
    #[test]
    fn paths_cannot_leave_the_service() { for p in ["/ok", "/a/b?x=1"] { assert!(path_ok(p), "{p}"); } for p in ["//evil.com/x", "/../x", "x", "/a@b", "/a\\b", "/a\nb"] { assert!(!path_ok(p), "{p}"); } }
    #[test]
    fn the_yaml_is_a_rendering_of_the_recipe_and_holds_no_secret() {
        let y = to_yaml(&validate(&recipe()).unwrap()); assert!(y.contains("slug: mock-api") && y.contains("auth_header:") && y.contains("Bearer {token}"), "{y}");
    }
    #[test]
    fn a_proposal_is_a_draft_the_secret_goes_to_a_private_env_file_and_the_row_never_holds_it() {
        let root = tmp("store"); let (slug, fresh) = propose(&root, &recipe(), "agent").unwrap(); assert!(fresh && slug == "mock-api");
        let c = open(&root).unwrap(); assert_eq!(s(&row(&c, &slug).unwrap()["status"]), "draft");
        put_secrets(&root, &slug, &json!({ "token": "sk-SECRET-123" })).unwrap();
        let p = public(&root, &row(&c, &slug).unwrap()); assert_eq!(p["missing"], json!([])); assert_eq!(p["recipe"]["inputs"][0]["filled"], true);
        let all = serde_json::to_string(&p).unwrap(); assert!(!all.contains("SECRET"), "the public view must not carry the secret");
        let rowtxt: String = c.query_row("SELECT recipe_json FROM connectors", [], |x| x.get(0)).unwrap(); assert!(!rowtxt.contains("SECRET"));
        let env = std::fs::read_to_string(env_path(&root)).unwrap(); assert!(env.contains("ANTILOKI_CONNECTOR_MOCK_API_TOKEN=sk-SECRET-123"), "{env}");
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; assert_eq!(std::fs::metadata(env_path(&root)).unwrap().permissions().mode() & 0o777, 0o600); }
        assert_eq!(sign(&root, &validate(&recipe()).unwrap()).unwrap(), vec![("Authorization".to_string(), "Bearer sk-SECRET-123".to_string())]);
        set_status(&c, &slug, "connected", "ok"); assert!(propose(&root, &recipe(), "agent").unwrap_err().contains("already connected"));
        drop_secrets(&root, &slug); assert!(read_env(&root).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn a_secret_with_a_newline_is_refused() { let root = tmp("nl"); assert!(put_secrets(&root, "x", &json!({ "token": "a\nB=c" })).is_err()); let _ = std::fs::remove_dir_all(&root); }
    #[test]
    fn a_read_only_connector_refuses_anything_that_writes() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap(); let root = tmp("ro");
        let r = validate(&recipe()).unwrap();
        let e = rt.block_on(api_call(&root, &r, "POST", "/x", &Value::Null, &Value::Null)).unwrap_err(); assert!(e.contains("read-only"), "{e}");
        let e = rt.block_on(api_call(&root, &r, "GET", "//evil.com", &Value::Null, &Value::Null)).unwrap_err(); assert!(e.contains("starts with one"), "{e}");
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn a_tool_that_only_reads_is_told_from_one_that_may_write() {
        assert!(tool_reads(&json!({ "name": "list_files" })) && tool_reads(&json!({ "name": "x", "annotations": { "readOnlyHint": true } })) && tool_reads(&json!({ "name": "drive_search" })));
        assert!(!tool_reads(&json!({ "name": "delete_file" })) && !tool_reads(&json!({ "name": "create_doc" })));
    }
    /// A tiny service on loopback: /v1/me wants `Bearer fresh-token`; /oauth/token renews with refresh token r1. Answers `n` requests.
    fn tiny_service(n: usize) -> u16 {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || { for st in l.incoming().take(n) {
            let mut st = st.unwrap(); let mut buf = vec![0u8; 8192]; let k = st.read(&mut buf).unwrap_or(0); let req = String::from_utf8_lossy(&buf[..k]).to_string();
            let (code, body) = if req.starts_with("GET /v1/me") { if req.contains("Bearer fresh-token") { (200, "{\"user\":\"x\"}") } else { (401, "{\"error\":\"expired\"}") } }
                else if req.starts_with("POST /oauth/token") {
                    if req.contains("grant_type=refresh_token") && req.contains("refresh_token=r1") { (200, "{\"access_token\":\"fresh-token\"}") }
                    else if req.contains("grant_type=authorization_code") && req.contains("code_verifier=") && req.contains("client_id=dcr-1") { (200, "{\"access_token\":\"tok-1\",\"refresh_token\":\"r9\"}") }
                    else if req.contains("grant_type=authorization_code") && req.contains("code_verifier=") && req.contains("client_id=builtin-id") { (200, "{\"access_token\":\"tok-2\"}") }
                    else { (400, "{\"error\":\"invalid_grant\"}") } }
                else if req.starts_with("POST /register") { if req.contains("antiloki") && req.contains("redirect_uris") { (201, "{\"client_id\":\"dcr-1\"}") } else { (400, "{\"error\":\"invalid_client_metadata\"}") } }
                else { (404, "{}") };
            let _ = write!(st, "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        } });
        port
    }
    fn oauth_recipe(port: u16) -> Value {
        let mut r = recipe(); r["auth"] = json!("oauth"); r["base_url"] = json!(format!("http://127.0.0.1:{port}/v1")); r["site"] = json!("");
        r["inputs"] = json!([{ "id": "client_id", "label": "Client id", "secret": false }, { "id": "client_secret", "label": "Client secret" }]);
        r["auth_header"] = json!({ "name": "Authorization", "template": "Bearer {access_token}" });
        r["oauth"] = json!({ "authorize_url": "https://accounts.example.com/auth", "token_url": format!("http://127.0.0.1:{port}/oauth/token"), "scopes": ["read"], "client_id_input": "client_id", "client_secret_input": "client_secret", "extra": { "access_type": "offline", "prompt": "consent" } });
        r
    }
    #[test]
    fn an_expired_oauth_sign_in_is_renewed_once_with_the_refresh_token_and_the_call_goes_through() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap(); let root = tmp("refresh");
        let port = tiny_service(3); let (slug, _) = propose(&root, &oauth_recipe(port), "agent").unwrap();
        put_secrets(&root, &slug, &json!({ "client_id": "abc", "client_secret": "sec", "access_token": "stale", "refresh_token": "r1" })).unwrap();
        let rc = { let c = open(&root).unwrap(); row(&c, &slug).unwrap()["recipe"].clone() };
        let (code, _) = rt.block_on(api_call(&root, &rc, "GET", "/me", &Value::Null, &Value::Null)).unwrap();
        assert_eq!(code, 200, "the stale token got a 401, was renewed, and the retry passed");
        assert_eq!(read_env(&root).get(&env_key(&slug, "access_token")).map(String::as_str), Some("fresh-token"));
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn with_no_refresh_token_a_401_is_returned_as_it_is() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap(); let root = tmp("norefresh");
        let port = tiny_service(1); let (slug, _) = propose(&root, &oauth_recipe(port), "agent").unwrap();
        put_secrets(&root, &slug, &json!({ "client_id": "abc", "client_secret": "sec", "access_token": "stale" })).unwrap();
        let rc = { let c = open(&root).unwrap(); row(&c, &slug).unwrap()["recipe"].clone() };
        assert_eq!(rt.block_on(api_call(&root, &rc, "GET", "/me", &Value::Null, &Value::Null)).unwrap().0, 401);
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn the_extra_sign_in_parameters_are_kept_checked_and_sent() {
        let root = tmp("extra"); let (slug, _) = propose(&root, &oauth_recipe(9), "agent").unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        put_secrets(&root, &slug, &json!({ "client_id": "abc" })).unwrap(); let u = rt.block_on(oauth_start(&root, &slug)).unwrap();
        assert!(u.contains("access_type=offline") && u.contains("prompt=consent"), "{u}");
        let mut bad = oauth_recipe(9); bad["oauth"]["extra"] = json!({ "redirect_uri": "http://evil" }); assert!(validate(&bad).unwrap_err().contains("set by antiloki"));
        let _ = std::fs::remove_dir_all(&root);
    }
    fn query_of(u: &str, k: &str) -> String { url::Url::parse(u).unwrap().query_pairs().find(|(a, _)| a == k).map(|(_, v)| v.to_string()).unwrap_or_default() }
    #[test]
    fn a_service_that_lets_an_app_register_needs_no_input_antiloki_registers_itself_and_the_sign_in_uses_pkce() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap(); let root = tmp("dcr");
        let port = tiny_service(3);
        let mut r = oauth_recipe(port); r["inputs"] = json!([]); r["oauth"]["client_id_input"] = json!(""); r["oauth"]["client_secret_input"] = json!(""); r["oauth"]["register_url"] = json!(format!("http://127.0.0.1:{port}/register"));
        let (slug, _) = propose(&root, &r, "agent").unwrap();
        let rc = { let c = open(&root).unwrap(); row(&c, &slug).unwrap() };
        assert_eq!(public(&root, &rc)["oauth_ready"], json!(true), "one Connect button, nothing to paste"); assert_eq!(public(&root, &rc)["missing"], json!([]));
        let u = rt.block_on(oauth_start(&root, &slug)).unwrap();
        assert_eq!(query_of(&u, "client_id"), "dcr-1", "{u}"); assert_eq!(query_of(&u, "code_challenge_method"), "S256"); assert!(query_of(&u, "code_challenge").len() >= 43, "{u}");
        let st = query_of(&u, "state"); assert_eq!(rt.block_on(oauth_finish(&root, &st, "c1")).unwrap(), slug);
        let env = read_env(&root); assert_eq!(env.get(&env_key(&slug, "access_token")).map(String::as_str), Some("tok-1")); assert_eq!(env.get(&env_key(&slug, "oauth_client_id")).map(String::as_str), Some("dcr-1"));
        assert!(rt.block_on(oauth_finish(&root, &st, "c1")).is_err(), "a sign-in link works once");
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn a_built_in_client_from_the_clients_file_makes_connect_one_click() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap(); let root = tmp("builtin");
        let f = std::path::Path::new(&root).join("oauth-clients.json"); std::fs::write(&f, r#"{"testservice": {"client_id": "builtin-id"}}"#).unwrap(); std::env::set_var("ANTILOKI_OAUTH_CLIENTS", &f);
        let port = tiny_service(1);
        let mut r = oauth_recipe(port); r["oauth"]["builtin"] = json!("testservice");
        let (slug, _) = propose(&root, &r, "agent").unwrap(); let rc = { let c = open(&root).unwrap(); row(&c, &slug).unwrap() };
        assert_eq!(public(&root, &rc)["oauth_ready"], json!(true), "the client file is enough: the pasted inputs are only a fallback");
        let u = rt.block_on(oauth_start(&root, &slug)).unwrap(); assert_eq!(query_of(&u, "client_id"), "builtin-id");
        assert_eq!(rt.block_on(oauth_finish(&root, &query_of(&u, "state"), "c2")).unwrap(), slug);
        assert_eq!(read_env(&root).get(&env_key(&slug, "access_token")).map(String::as_str), Some("tok-2"));
        std::env::remove_var("ANTILOKI_OAUTH_CLIENTS");
        let r2 = { let c = open(&root).unwrap(); row(&c, &slug).unwrap() }; drop_secrets(&root, &slug);
        assert_eq!(public(&root, &r2)["oauth_ready"], json!(false), "without the file and without a pasted client there is nothing to sign in with: the pane falls back to the fields");
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn the_logo_follows_a_redirect_to_the_image_and_keeps_it() {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let port = l.local_addr().unwrap().port();
        let png: Vec<u8> = vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4];
        let png2 = png.clone();
        std::thread::spawn(move || { for st in l.incoming().take(3) {
            let mut st = st.unwrap(); let mut buf = vec![0u8; 4096]; let k = st.read(&mut buf).unwrap_or(0); let req = String::from_utf8_lossy(&buf[..k]).to_string();
            if req.starts_with("GET /favicon.ico") { let _ = write!(st, "HTTP/1.1 302 Found\r\nLocation: /real.png\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"); }
            else if req.starts_with("GET /real.png") { let _ = write!(st, "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", png2.len()); let _ = st.write_all(&png2); }
            else { let _ = write!(st, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"); }
        } });
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap(); let root = tmp("logo");
        rt.block_on(fetch_logo(&root, "x", &format!("http://127.0.0.1:{port}")));
        let (mime, bytes): (String, Vec<u8>) = open(&root).unwrap().query_row("SELECT mime, bytes FROM connector_logos WHERE slug = 'x'", [], |r| Ok((r.get(0)?, r.get(1)?))).expect("the logo was stored");
        assert_eq!(mime, "image/png"); assert_eq!(bytes, png);
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn only_the_person_changes_what_agents_may_do_and_the_change_is_in_the_recipe_and_the_log() {
        let root = tmp("access"); let (slug, _) = propose(&root, &recipe(), "agent").unwrap();
        assert!(set_access(&root, &slug, "sudo").is_err()); assert!(set_access(&root, "nope", "read-write").is_err());
        set_access(&root, &slug, "read-write").unwrap();
        let c = open(&root).unwrap(); let r = row(&c, &slug).unwrap(); assert_eq!(r["recipe"]["access"], json!("read-write")); assert!(to_yaml(&r["recipe"]).contains("access: read-write"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM connector_events WHERE slug = ?1 AND op = 'access-changed' AND actor = 'person'", params![slug], |x| x.get(0)).unwrap(); assert_eq!(n, 1);
        set_access(&root, &slug, "read-only").unwrap(); assert_eq!(row(&c, &slug).unwrap()["recipe"]["access"], json!("read-only"));
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn the_oauth_start_needs_the_client_id_and_builds_the_services_own_address() {
        let root = tmp("oa"); let mut r = recipe(); r["auth"] = json!("oauth"); r["inputs"] = json!([{ "id": "client_id", "label": "Client id", "secret": false }, { "id": "client_secret", "label": "Client secret" }]);
        r["auth_header"] = json!({ "name": "Authorization", "template": "Bearer {access_token}" }); r["oauth"] = json!({ "authorize_url": "https://accounts.example.com/auth", "token_url": "https://accounts.example.com/token", "scopes": ["read"], "client_id_input": "client_id", "client_secret_input": "client_secret" });
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (slug, _) = propose(&root, &r, "agent").unwrap(); assert!(rt.block_on(oauth_start(&root, &slug)).unwrap_err().contains("client id"));
        put_secrets(&root, &slug, &json!({ "client_id": "abc" })).unwrap(); let u = rt.block_on(oauth_start(&root, &slug)).unwrap();
        assert!(u.starts_with("https://accounts.example.com/auth?") && u.contains("client_id=abc") && u.contains("redirect_uri=") && u.contains("state="), "{u}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
