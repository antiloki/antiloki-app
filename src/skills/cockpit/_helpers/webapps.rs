//! WEB APPS (operator 2026-11-10: "install web apps inside antiloki and generate MCPs to them, and open them in a pane ... a store with all the official open MCPs of big apps, and a plus to add one more, two modalities:
//! official MCP or generate MCP ... the button is install, this will install the pane on the new pane list with the web app flag").
//!
//! The machine's list of installed web apps (`<antiloki home>/webapps.json`: an app is the person's, not a project's), the catalog of apps whose makers publish an official MCP server, and the rule for which of them an agent's
//! box is told about. Pure over the files; the serve is the door and the screen is the store.
use serde_json::{json, Map, Value};
use std::path::PathBuf;

/// One app in the store. `mcp_url` is the maker's own remote MCP server; `app_url` the page the pane opens. The addresses are the makers' documented ones, and an install checks that the address answers.
pub struct StoreApp { pub id: &'static str, pub name: &'static str, pub vendor: &'static str, pub category: &'static str, pub blurb: &'static str, pub app_url: &'static str, pub mcp_url: &'static str, pub transport: &'static str, pub auth: &'static str, pub color: &'static str, pub docs: &'static str }

pub const CATALOG: &[StoreApp] = &[
    StoreApp { id: "atlassian", name: "Jira & Confluence", vendor: "Atlassian", category: "Projects", blurb: "Issues, boards, sprints and pages through Atlassian's official Rovo MCP server.", app_url: "https://www.atlassian.com/software/jira", mcp_url: "https://mcp.atlassian.com/v1/mcp", transport: "http", auth: "oauth", color: "#2f6fe4", docs: "https://www.atlassian.com/platform/remote-mcp-server" },
    StoreApp { id: "linear", name: "Linear", vendor: "Linear", category: "Projects", blurb: "Issues, projects and cycles through Linear's official MCP server.", app_url: "https://linear.app", mcp_url: "https://mcp.linear.app/mcp", transport: "http", auth: "oauth", color: "#6e56cf", docs: "https://linear.app/docs/mcp" },
    StoreApp { id: "notion", name: "Notion", vendor: "Notion", category: "Docs", blurb: "Pages and databases through Notion's official MCP server.", app_url: "https://www.notion.so", mcp_url: "https://mcp.notion.com/mcp", transport: "http", auth: "oauth", color: "#d8d8d8", docs: "https://developers.notion.com/docs/mcp" },
    StoreApp { id: "asana", name: "Asana", vendor: "Asana", category: "Projects", blurb: "Tasks and projects through Asana's official MCP server.", app_url: "https://app.asana.com", mcp_url: "https://mcp.asana.com/v2/mcp", transport: "http", auth: "oauth", color: "#f06a6a", docs: "https://developers.asana.com/docs/using-asanas-mcp-server" },
    StoreApp { id: "github", name: "GitHub", vendor: "GitHub", category: "Code", blurb: "Repositories, issues and pull requests through GitHub's official MCP server.", app_url: "https://github.com", mcp_url: "https://api.githubcopilot.com/mcp/", transport: "http", auth: "token", color: "#8b949e", docs: "https://github.com/github/github-mcp-server" },
    StoreApp { id: "sentry", name: "Sentry", vendor: "Sentry", category: "Code", blurb: "Errors and traces through Sentry's official MCP server.", app_url: "https://sentry.io", mcp_url: "https://mcp.sentry.dev/mcp", transport: "http", auth: "oauth", color: "#7553ff", docs: "https://docs.sentry.io/product/sentry-mcp/" },
    StoreApp { id: "stripe", name: "Stripe", vendor: "Stripe", category: "Payments", blurb: "Customers, payments and docs through Stripe's official MCP server.", app_url: "https://dashboard.stripe.com", mcp_url: "https://mcp.stripe.com", transport: "http", auth: "oauth", color: "#635bff", docs: "https://docs.stripe.com/mcp" },
    StoreApp { id: "paypal", name: "PayPal", vendor: "PayPal", category: "Payments", blurb: "Orders, invoices and payments through PayPal's official MCP server.", app_url: "https://www.paypal.com", mcp_url: "https://mcp.paypal.com/mcp", transport: "http", auth: "oauth", color: "#2b7fd4", docs: "https://developer.paypal.com/tools/mcp-server/" },
    StoreApp { id: "square", name: "Square", vendor: "Square", category: "Payments", blurb: "Catalog, orders and payments through Square's official MCP server.", app_url: "https://squareup.com", mcp_url: "https://mcp.squareup.com/mcp", transport: "http", auth: "oauth", color: "#c8c8c8", docs: "https://developer.squareup.com/docs/mcp" },
    StoreApp { id: "intercom", name: "Intercom", vendor: "Intercom", category: "Support", blurb: "Conversations and contacts through Intercom's official MCP server.", app_url: "https://app.intercom.com", mcp_url: "https://mcp.intercom.com/mcp", transport: "http", auth: "oauth", color: "#3d85f0", docs: "https://developers.intercom.com/docs/guides/mcp" },
    StoreApp { id: "webflow", name: "Webflow", vendor: "Webflow", category: "Design", blurb: "Sites and CMS content through Webflow's official MCP server.", app_url: "https://webflow.com", mcp_url: "https://mcp.webflow.com/mcp", transport: "http", auth: "oauth", color: "#4353ff", docs: "https://developers.webflow.com/data/docs/ai-tools" },
    StoreApp { id: "figma", name: "Figma", vendor: "Figma", category: "Design", blurb: "Designs through Figma's Dev Mode MCP server (served by the Figma desktop app on this machine).", app_url: "https://www.figma.com", mcp_url: "http://127.0.0.1:3845/mcp", transport: "http", auth: "local", color: "#a259ff", docs: "https://help.figma.com/hc/en-us/articles/32132100833559" },
];

/// The catalog as JSON, for the store.
pub fn catalog_json() -> Value {
    Value::Array(CATALOG.iter().map(|a| json!({ "id": a.id, "name": a.name, "vendor": a.vendor, "category": a.category, "blurb": a.blurb, "app_url": a.app_url, "mcp_url": a.mcp_url, "transport": a.transport, "auth": a.auth, "color": a.color, "docs": a.docs, "kind": "official" })).collect())
}

/// Where the list lives: the machine's antiloki home, beside the project registry.
pub fn store_path() -> PathBuf { crate::engine_registry::registry_path().with_file_name("webapps.json") }
pub fn load_from(p: &std::path::Path) -> Vec<Value> { std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str::<Vec<Value>>(&t).ok()).unwrap_or_default() }
/// An installed store app keeps the maker's CURRENT address: a catalog that moved an address (Linear's /sse answers 404 since it moved to /mcp) must not leave an old install pointing at the dead one. Pure.
pub fn refresh_official(apps: &mut [Value]) { for a in apps.iter_mut() { if a["kind"] != json!("official") { continue; } let Some(c) = CATALOG.iter().find(|c| a["id"] == json!(c.id)) else { continue; }; if a["mcp_url"] != json!(c.mcp_url) { a["mcp_url"] = json!(c.mcp_url); a["transport"] = json!(c.transport); } } }
pub fn load() -> Vec<Value> { let mut v = load_from(&store_path()); refresh_official(&mut v); v }
pub fn save_to(p: &std::path::Path, apps: &[Value]) -> Result<(), String> {
    if let Some(d) = p.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
    std::fs::write(p, serde_json::to_string_pretty(apps).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// A slug for a custom app's id: lower case words and dashes. Pure.
pub fn slug(name: &str) -> String { let t: String = name.trim().to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect(); t.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-") }

/// An address the pane may open or the MCP may be reached at: http(s) only, no credentials in it. Pure.
pub fn clean_url(u: &str) -> Result<String, String> {
    let u = u.trim(); let low = u.to_ascii_lowercase();
    if !(low.starts_with("https://") || low.starts_with("http://")) { return Err("the address starts with https:// (or http:// for an app on this machine)".into()); }
    let rest = &u[low.find("://").map(|i| i + 3).unwrap_or(0)..]; let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.contains('@') { return Err("the address must not hold a user name or password".into()); }
    if host.is_empty() || host.contains(char::is_whitespace) { return Err("that is not an address".into()); }
    if low.starts_with("http://") { let h = host.split(':').next().unwrap_or(""); if !(h == "localhost" || h == "127.0.0.1" || h == "[::1]" || h.ends_with(".localhost")) { return Err("only an app on this machine may use http://: the rest use https://".into()); } }
    Ok(u.to_string())
}

/// Install an app: from the store by id, or a custom one. Returns the updated list. Installing again replaces the entry (so a changed address takes). Pure over the list.
pub fn install(apps: &mut Vec<Value>, body: &Value, now_ms: i64) -> Result<Value, String> {
    let s = |k: &str| body[k].as_str().unwrap_or("").trim().to_string();
    let entry = if !s("store_id").is_empty() {
        let a = CATALOG.iter().find(|a| a.id == s("store_id")).ok_or_else(|| format!("no app `{}` in the store", s("store_id")))?;
        json!({ "id": a.id, "name": a.name, "kind": "official", "app_url": a.app_url, "mcp_url": a.mcp_url, "transport": a.transport, "auth": a.auth, "color": a.color, "vendor": a.vendor, "docs": a.docs, "tools": [], "installed_ms": now_ms })
    } else {
        let name = s("name"); let id = slug(&name); if id.is_empty() { return Err("an app needs a name".into()); }
        if CATALOG.iter().any(|a| a.id == id) { return Err(format!("`{id}` is in the store: install it from there")); }
        let app_url = clean_url(&s("app_url"))?;
        let kind = match s("kind").as_str() { "official" => "official", _ => "generated" };
        let mcp_url = if s("mcp_url").is_empty() { String::new() } else { clean_url(&s("mcp_url"))? };
        if kind == "official" && mcp_url.is_empty() { return Err("an official MCP needs its address (the MCP server's URL)".into()); }
        let transport = if mcp_url.ends_with("/sse") { "sse" } else { "http" };
        json!({ "id": id, "name": name, "kind": kind, "app_url": app_url, "mcp_url": mcp_url, "transport": transport, "auth": "oauth", "color": s("color"), "openapi_url": s("openapi_url"), "tools": body["tools"].clone(), "installed_ms": now_ms })
    };
    apps.retain(|a| a["id"] != entry["id"]); apps.push(entry.clone()); apps.sort_by(|a, b| a["name"].as_str().unwrap_or("").to_lowercase().cmp(&b["name"].as_str().unwrap_or("").to_lowercase()));
    Ok(entry)
}
pub fn uninstall(apps: &mut Vec<Value>, id: &str) -> bool { let n = apps.len(); apps.retain(|a| a["id"] != json!(id)); apps.len() != n }

/// The MCP servers an agent's box is told about besides antiloki's: every installed app with an MCP address, as the JSON engine configs spell a remote server (`{type, url}`). Pure over the list.
pub fn servers_of(apps: &[Value]) -> Map<String, Value> {
    let mut m = Map::new();
    for a in apps {
        let (id, url) = (a["id"].as_str().unwrap_or(""), a["mcp_url"].as_str().unwrap_or(""));
        if id.is_empty() || url.is_empty() { continue; }
        m.insert(format!("app-{id}"), json!({ "type": if a["transport"] == "sse" { "sse" } else { "http" }, "url": url }));
    }
    m
}

/// What a machine's OpenAPI document offers an agent: one tool per operation. Pure over the document.
pub fn operations_of(doc: &Value) -> Vec<Value> {
    let mut out = vec![];
    for (path, item) in doc["paths"].as_object().into_iter().flatten() {
        for (method, op) in item.as_object().into_iter().flatten() {
            if !["get", "post", "put", "patch", "delete"].contains(&method.as_str()) || !op.is_object() { continue; }
            let id = op["operationId"].as_str().map(String::from).unwrap_or_else(|| format!("{}_{}", method, path.trim_matches('/').replace(['/', '{', '}', '-'], "_")));
            let risk = match method.as_str() { "get" => "read", "delete" => "destructive", _ => "write" };
            out.push(json!({ "name": id, "method": method.to_uppercase(), "path": path, "summary": op["summary"].as_str().or_else(|| op["description"].as_str()).unwrap_or("").chars().take(140).collect::<String>(), "risk": risk }));
        }
    }
    out.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()).then(a["method"].as_str().cmp(&b["method"].as_str()))); out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_store_lists_apps_with_distinct_ids_and_real_addresses() {
        let mut ids: Vec<&str> = CATALOG.iter().map(|a| a.id).collect(); let n = ids.len(); ids.sort(); ids.dedup(); assert_eq!(ids.len(), n, "ids are unique");
        for a in CATALOG { assert!(clean_url(a.mcp_url).is_ok() && clean_url(a.app_url).is_ok(), "{}: {} {}", a.id, a.mcp_url, a.app_url); assert!(["sse", "http"].contains(&a.transport) && ["oauth", "token", "local"].contains(&a.auth), "{}", a.id); }
        assert!(catalog_json().as_array().unwrap().iter().all(|a| a["kind"] == "official"));
    }

    #[test]
    fn an_address_is_https_without_credentials_and_http_only_on_this_machine() {
        assert!(clean_url("https://acme.atlassian.net/jira").is_ok() && clean_url("http://127.0.0.1:3845/mcp").is_ok() && clean_url("http://localhost:3000").is_ok());
        assert!(clean_url("http://example.com").unwrap_err().contains("only an app on this machine"));
        assert!(clean_url("https://me:pw@example.com").unwrap_err().contains("user name or password")); assert!(clean_url("ftp://x").unwrap_err().contains("starts with https")); assert!(clean_url("https://").is_err());
    }

    #[test]
    fn installing_from_the_store_or_by_hand_and_removing() {
        let mut apps = vec![];
        let e = install(&mut apps, &json!({ "store_id": "linear" }), 5).unwrap(); assert_eq!((e["kind"].clone(), e["transport"].clone()), (json!("official"), json!("http")));
        assert!(install(&mut apps, &json!({ "store_id": "nope" }), 5).unwrap_err().contains("no app"));
        let g = install(&mut apps, &json!({ "name": "Acme Billing", "kind": "generated", "app_url": "https://billing.acme.io", "openapi_url": "https://billing.acme.io/openapi.json", "tools": [{ "name": "list" }] }), 6).unwrap(); assert_eq!((g["id"].clone(), g["kind"].clone(), g["mcp_url"].clone()), (json!("acme-billing"), json!("generated"), json!("")));
        let o = install(&mut apps, &json!({ "name": "Acme CRM", "kind": "official", "app_url": "https://crm.acme.io", "mcp_url": "https://mcp.acme.io/sse" }), 7).unwrap(); assert_eq!(o["transport"], json!("sse"));
        assert!(install(&mut apps, &json!({ "name": "Acme CRM", "kind": "official", "app_url": "https://crm.acme.io" }), 7).unwrap_err().contains("needs its address"));
        assert!(install(&mut apps, &json!({ "name": "Linear", "app_url": "https://linear.app" }), 7).unwrap_err().contains("in the store"));
        assert!(install(&mut apps, &json!({ "name": "  ", "app_url": "https://x.io" }), 7).unwrap_err().contains("needs a name")); assert!(install(&mut apps, &json!({ "name": "x", "app_url": "javascript:alert(1)" }), 7).is_err());
        assert_eq!(apps.iter().map(|a| a["id"].as_str().unwrap()).collect::<Vec<_>>(), vec!["acme-billing", "acme-crm", "linear"], "by name");
        install(&mut apps, &json!({ "store_id": "linear" }), 9).unwrap(); assert_eq!(apps.len(), 3, "again replaces"); assert_eq!(apps[2]["installed_ms"], json!(9));
        assert!(uninstall(&mut apps, "acme-crm") && !uninstall(&mut apps, "acme-crm") && apps.len() == 2);
    }

    #[test]
    fn an_agents_box_hears_of_the_apps_that_have_an_mcp_and_only_those() {
        let mut apps = vec![]; install(&mut apps, &json!({ "store_id": "linear" }), 1).unwrap(); install(&mut apps, &json!({ "store_id": "notion" }), 1).unwrap();
        install(&mut apps, &json!({ "name": "Acme Billing", "kind": "generated", "app_url": "https://billing.acme.io" }), 1).unwrap();
        let m = servers_of(&apps); assert_eq!(m.keys().cloned().collect::<Vec<_>>(), vec!["app-linear", "app-notion"]);
        assert_eq!(m["app-linear"], json!({ "type": "http", "url": "https://mcp.linear.app/mcp" })); assert_eq!(m["app-notion"], json!({ "type": "http", "url": "https://mcp.notion.com/mcp" }));
    }

    #[test]
    fn an_openapi_document_becomes_one_tool_per_operation_with_its_risk() {
        let doc = json!({ "paths": { "/invoices": { "get": { "operationId": "listInvoices", "summary": "List invoices" }, "post": { "summary": "Create one" } }, "/invoices/{id}": { "delete": { "operationId": "deleteInvoice" }, "parameters": [] } } });
        let ops = operations_of(&doc);
        assert_eq!(ops.iter().map(|o| (o["name"].as_str().unwrap(), o["risk"].as_str().unwrap())).collect::<Vec<_>>(), vec![("listInvoices", "read"), ("post_invoices", "write"), ("deleteInvoice", "destructive")]);
        assert!(operations_of(&json!({})).is_empty());
    }

    #[test]
    fn the_list_survives_in_a_file() {
        let d = std::env::temp_dir().join(format!("antiloki-webapps-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); let p = d.join("webapps.json");
        assert!(load_from(&p).is_empty()); let mut a = vec![]; install(&mut a, &json!({ "store_id": "asana" }), 3).unwrap(); save_to(&p, &a).unwrap(); assert_eq!(load_from(&p), a); let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;
    #[test]
    fn an_installed_store_app_is_moved_to_the_makers_current_address_and_an_own_app_is_left_alone() {
        let mut v = vec![json!({ "id": "linear", "kind": "official", "mcp_url": "https://mcp.linear.app/sse", "transport": "sse" }), json!({ "id": "linear", "kind": "generated", "mcp_url": "https://mine/x" })];
        refresh_official(&mut v); assert_eq!(v[0]["mcp_url"], json!("https://mcp.linear.app/mcp")); assert_eq!(v[0]["transport"], json!("http")); assert_eq!(v[1]["mcp_url"], json!("https://mine/x"));
        assert!(CATALOG.iter().all(|a| !a.mcp_url.ends_with("/sse")), "no catalog address is the old SSE kind");
    }
}
