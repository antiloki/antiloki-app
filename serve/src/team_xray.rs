//! THE X-RAY NODE (operator 2026-11-16): a team step that hands an agent anything antiloki's X-ray knows (ASTs, file and folder reviews, graphs, importers, the tests that reach a file, a budgeted context bundle for a question).
//! It is the MCP's own tools, reused: a tool node of type `mcp` whose name is one of the X-ray tools runs the same code an agent's MCP call runs (`antiloki_rs::cockpit_cockpit_pane_context_bake`), in this process, against the project. No second implementation.
//! A small model cannot use MCP tools itself; the team calls them for it and puts the answer in its prompt.
use serde_json::{json, Value};

pub const TOOLS: [&str; 8] = ["antiloki_query", "antiloki_sources", "antiloki_file_review", "antiloki_folder_review", "antiloki_blast_radius", "antiloki_tests_for", "antiloki_check", "antiloki_laws"];
pub const MAX_CHARS: usize = 16_000;
pub fn is_xray(tool: &str) -> bool { TOOLS.contains(&tool) }
/// `{ask}` and `{node.field}` in every string of the arguments. Pure over the values.
pub fn fill_args(args: &Value, ask: &str, values: &serde_json::Map<String, Value>) -> Value {
    match args { Value::String(s) => Value::String(crate::team_web::fill(s, ask, values)), Value::Array(a) => Value::Array(a.iter().map(|x| fill_args(x, ask, values)).collect()), Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), fill_args(v, ask, values))).collect()), x => x.clone() }
}
/// The source a call names when it costs a model call (`describe`, `logic_spec`...), else None. Pure over the catalog.
pub fn ai_source(tool: &str, args: &Value) -> Option<String> {
    if tool != "antiloki_query" { return None; }
    let key = args["source"].as_str().unwrap_or(""); antiloki_rs::cockpit__helpers_context_catalog::source(key).filter(|s| s.ai).map(|s| s.key.to_string())
}
/// One call to the serve's own X-ray door (the route the MCP uses; pane 0 = the workspace itself). The serve answers with its full database connection, which only it holds.
async fn call(port: u16, tool: &str, args: &Value) -> Result<String, String> {
    let r = reqwest::Client::new().post(format!("http://127.0.0.1:{port}/api/v1/cockpit/pane/context/tool")).json(&json!({ "pane_id": 0, "tool": tool, "args": args })).timeout(std::time::Duration::from_secs(120)).send().await.map_err(|e| format!("the serve did not answer: {e}"))?;
    let v: Value = r.json().await.map_err(|e| e.to_string())?;
    if v["ok"] == json!(false) { return Err(v["error"].as_str().or(v["message"].as_str()).unwrap_or("the X-ray tool failed").to_string()); }
    Ok(v["markdown"].as_str().unwrap_or("").to_string())
}
async fn post(port: u16, path: &str, body: Value) -> Result<(), String> {
    reqwest::Client::new().post(format!("http://127.0.0.1:{port}{path}")).json(&body).timeout(std::time::Duration::from_secs(300)).send().await.map_err(|e| format!("{path}: {e}"))?.error_for_status().map(|_| ()).map_err(|e| format!("{path}: {e}"))
}
/// The answer says the project has not been read yet. Pure.
pub fn unprepared(md: &str) -> bool { md.contains("No codebase documentation yet") || md.contains("rebuild the atlas") }
/// Run one X-ray tool and return its markdown, cut to `MAX_CHARS` with the cut said. When the project has not been read yet (no atlas, no documentation), read it ONCE (refresh and decorate: deterministic, no model) and ask again:
/// a fresh workspace used to answer "rebuild the atlas" to a team that had no way to do it.
pub async fn run(port: u16, tool: &str, args: &Value) -> Result<String, String> {
    if !is_xray(tool) { return Err(format!("{tool} is not an X-ray tool")); }
    let mut md = call(port, tool, args).await?;
    if unprepared(&md) {
        post(port, "/api/v1/codebase/refresh", json!({})).await?; post(port, "/api/v1/codebase/decorate", json!({ "all": true })).await?;
        md = call(port, tool, args).await?;
    }
    Ok(if md.chars().count() > MAX_CHARS { format!("{}\n… (cut at {MAX_CHARS} characters; ask a narrower question)", md.chars().take(MAX_CHARS).collect::<String>()) } else { md })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn only_the_xray_tools_are_xray_and_the_ai_backed_sources_are_named() {
        assert!(is_xray("antiloki_query") && is_xray("antiloki_file_review") && !is_xray("antiloki_pane_act") && !is_xray("antiloki_delegate"));
        assert_eq!(ai_source("antiloki_query", &json!({ "source": "describe" })), Some("describe".into())); assert_eq!(ai_source("antiloki_query", &json!({ "source": "ast" })), None); assert_eq!(ai_source("antiloki_file_review", &json!({})), None);
        assert!(unprepared("_No codebase documentation yet \u{2014} rebuild the atlas (\u{21bb})._") && !unprepared("# Context bundle for: x\n\nscope files"));
    }
    #[test] fn the_ask_and_values_fill_every_string_of_the_arguments() {
        let mut v = serde_json::Map::new(); v.insert("where".into(), json!({ "file": "src/a.rs" }));
        let a = fill_args(&json!({ "source": "bundle", "question": "{ask}", "nested": ["{where.file}", 3] }), "add a farewell", &v);
        assert_eq!(a["question"], json!("add a farewell")); assert_eq!(a["nested"][0], json!("src/a.rs")); assert_eq!(a["nested"][1], json!(3));
    }
}
