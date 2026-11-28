//! chat-tool-belt.rs — twin of skills/agents/in/chat-tool-belt.mjs (no-db helpers, pure).
//! NAMES ARE CONTRACT: toolBelt · toolTier · toToolSchemas. The curated map of system.* actions
//! the cockpit exposes to a model as TOOLS; each entry carries a tier (read/propose). BELT is the
//! belt, pressed verbatim from the JS module value — the exported fns are thin readers over it.
// QUALITY: 80/100 — data-driven belt: embedded BELT + tier lookup + provider-shaped schema map
use serde_json::{json, Map, Value};
use std::sync::LazyLock;

// The curated belt — the exact JS BELT value, pressed at conversion time. READ tools are all-
// grounding (no side effects); PROPOSE tools create a backlog card.
static BELT: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(r#"[{"action":"system.codebase.find","tier":"read","description":"Find a function / class / file by name or path fragment.","parameters":{"type":"object","properties":{"query":{"type":"string","description":"name or path fragment"},"limit":{"type":"number","description":"max results"}},"required":["query"],"additionalProperties":false}},{"action":"system.codebase.get","tier":"read","description":"Read the atlas graph (nodes + edges), optionally scoped to one file path.","parameters":{"type":"object","properties":{"path":{"type":"string","description":"restrict to one file (optional)"}},"required":[],"additionalProperties":false}},{"action":"system.codebase.blast","tier":"read","description":"Reverse-dependency closure: what breaks if these nodes change (+ affected tests).","parameters":{"type":"object","properties":{"node_ids":{"type":"array","items":{"type":"string"},"description":"atlas node ids"}},"required":["node_ids"],"additionalProperties":false}},{"action":"system.codebase.subgraph","tier":"read","description":"The local neighborhood subgraph around one node.","parameters":{"type":"object","properties":{"node_id":{"type":"string","description":"the center node id"}},"required":["node_id"],"additionalProperties":false}},{"action":"system.codebase.metrics","tier":"read","description":"Per-file fan-in/out, complexity, coverage, churn — what is central / risky.","parameters":{"type":"object","properties":{"top":{"type":"number","description":"top-N (default 10)"}},"required":[],"additionalProperties":false}},{"action":"system.codebase.smells","tier":"read","description":"Structural smells: cycles, god-files, test gaps, orphans.","parameters":{"type":"object","properties":{},"required":[],"additionalProperties":false}},{"action":"system.codebase.entrypoints.get","tier":"read","description":"The external surface (routes / db / ui), optionally for one file (path) or folder (prefix).","parameters":{"type":"object","properties":{"path":{"type":"string","description":"one file"},"prefix":{"type":"string","description":"one folder"}},"required":[],"additionalProperties":false}},{"action":"system.codebase.story.trace","tier":"read","description":"Deterministically trace a data-flow story from an entry point to a data/state sink.","parameters":{"type":"object","properties":{"goal":{"type":"string","description":"a goal term to anchor on"},"limit":{"type":"number","description":"max stories"}},"required":[],"additionalProperties":false}},{"action":"system.codebase.report.latest","tier":"read","description":"The latest atlas-insights snapshot (health/risk score + smells) — no recompute.","parameters":{"type":"object","properties":{},"required":[],"additionalProperties":false}},{"action":"system.codebase.smell.card","tier":"propose","description":"Turn a structural smell into a backlog card the driver can act on.","parameters":{"type":"object","properties":{"smell":{"type":"object","description":"a smell {kind,severity,nodes,detail}"}},"required":["smell"],"additionalProperties":false}},{"action":"system.codebase.story.card","tier":"propose","description":"Draft a fix card for a BROKEN data story.","parameters":{"type":"object","properties":{"id":{"type":"number","description":"the story id"}},"required":["id"],"additionalProperties":false}}]"#).expect("BELT: embedded JSON is valid by construction")
});

// toolBelt() → BELT.map(t => ({ ...t })): a shallow copy of every entry. Spreading preserves the
// keys and order, so the result is JSON-identical to BELT itself.
pub fn toolBelt() -> Value {
    BELT.clone()
}

// toolTier(action) → BELT.find(t => t.action === action)?.tier ?? null. A non-matching or absent
// action yields null (no entry's string action equals a missing/non-string value).
pub fn toolTier(action: &Value) -> Value {
    BELT.as_array()
        .and_then(|belt| belt.iter().find(|t| t.get("action") == Some(action)))
        .and_then(|t| t.get("tier").cloned())
        .unwrap_or(Value::Null)
}

// toToolSchemas() → provider-shaped [{ type:'function', function:{ name, description, parameters } }].
// The model calls by name; the loop maps name → action 1:1.
pub fn toToolSchemas() -> Value {
    let schemas: Vec<Value> = BELT
        .as_array()
        .map(|belt| {
            belt.iter()
                .map(|t| {
                    let mut func = Map::new();
                    func.insert("name".into(), t.get("action").cloned().unwrap_or(Value::Null));
                    func.insert("description".into(), t.get("description").cloned().unwrap_or(Value::Null));
                    func.insert("parameters".into(), t.get("parameters").cloned().unwrap_or(Value::Null));
                    let mut o = Map::new();
                    o.insert("type".into(), json!("function"));
                    o.insert("function".into(), Value::Object(func));
                    Value::Object(o)
                })
                .collect()
        })
        .unwrap_or_default();
    Value::Array(schemas)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn belt_and_tiers() {
        let belt = toolBelt();
        let arr = belt.as_array().expect("array");
        assert_eq!(arr.len(), 11, "11 curated tools");
        assert_eq!(arr[0]["action"], json!("system.codebase.find"));
        assert_eq!(arr[0]["tier"], json!("read"));
        assert_eq!(arr[0]["parameters"]["type"], json!("object"));

        assert_eq!(toolTier(&json!("system.codebase.find")), json!("read"));
        assert_eq!(toolTier(&json!("system.codebase.smell.card")), json!("propose"));
        assert_eq!(toolTier(&json!("system.codebase.story.card")), json!("propose"));
        assert_eq!(toolTier(&json!("system.does.not.exist")), Value::Null);
        assert_eq!(toolTier(&Value::Null), Value::Null);

        let schemas = toToolSchemas();
        let s = schemas.as_array().expect("array");
        assert_eq!(s.len(), arr.len());
        assert_eq!(s[0]["type"], json!("function"));
        assert_eq!(s[0]["function"]["name"], json!("system.codebase.find"));
        assert_eq!(s[0]["function"]["parameters"]["type"], json!("object"));
        assert!(s[0]["function"]["parameters"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("query")));
    }
}
