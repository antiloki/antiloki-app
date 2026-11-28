//! context-contracts.rs — twin of skills/codebase/context-contracts.mjs. NAMES ARE CONTRACT:
//! `BONE_LEVELS`, `CONTRACTS`, `DEFAULT_CONTRACT` (data) + `resolveContract`. Pure, no DB, no AI.
// QUALITY: 87/100 — pressed data + a two-line lookup faithful to the JS key-building
use serde_json::Value;
use std::sync::LazyLock;

pub static BONE_LEVELS: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(r#"["skeleton","facts","full"]"#).expect("BONE_LEVELS json"));

pub static CONTRACTS: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(r#"{"pl:research":{"bone":"full","sliced":["deep-files","data-stories","rag"]},"pl:clarify":{"bone":"facts","sliced":["prior-lessons"]},"pl:refine":{"bone":"skeleton","sliced":["touched-files","decorators","lint-config"]},"tl:design":{"bone":"full","sliced":["touched-files","blast-radius","entry-points"]},"tl:author":{"bone":"skeleton","sliced":["touched-files"]},"tl:plan":{"bone":"skeleton","sliced":["touched-files","lint-config"]},"qa:review":{"bone":"facts","sliced":["touched-files","blast-radius"]},"dev:author":{"bone":"skeleton","sliced":["touched-files"]}}"#).expect("CONTRACTS json")
});

pub static DEFAULT_CONTRACT: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(r#"{"bone":"facts","sliced":["rag"]}"#).expect("DEFAULT_CONTRACT json"));

/// JS: `resolveContract(agent, phase)` → `CONTRACTS[`${agent}:${phase}`.toLowerCase()] ?? DEFAULT_CONTRACT`.
/// String(x ?? '') coercion: null/absent → "". Only string agent/phase occur in practice.
pub fn resolveContract(agent: &Value, phase: &Value) -> Value {
    let a = agent.as_str().unwrap_or("").to_lowercase();
    let p = phase.as_str().unwrap_or("").to_lowercase();
    let key = format!("{a}:{p}");
    CONTRACTS.get(&key).cloned().unwrap_or_else(|| DEFAULT_CONTRACT.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn known_and_default_contracts() {
        assert_eq!(resolveContract(&json!("pl"), &json!("research"))["bone"], json!("full"));
        assert_eq!(resolveContract(&json!("PL"), &json!("Research"))["bone"], json!("full")); // lowercased
        assert_eq!(resolveContract(&json!("nope"), &json!("x")), *DEFAULT_CONTRACT);
        assert_eq!(BONE_LEVELS.as_array().unwrap().len(), 3);
    }
}
