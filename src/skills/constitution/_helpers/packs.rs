//! packs.rs — twin of skills/constitution/_helpers/packs.mjs (no-db helper module). NAMES ARE
//! CONTRACT: `packById` (named). A pack is a NAMED, auto-activating VIEW over the law set; each
//! carries a `match` PREDICATE (an arrow fn). The conform JS lane runs the return through
//! normalizeResult, which stringifies a function via `String(fn)` (NOT JSON.stringify, which would
//! drop it) — so `match` survives as its exact source text and the twin must materialize that string.
use serde_json::{json, Value};

// The PACKS view. Insertion order id·name·kind·desc·activate·match mirrors the JS object literal;
// `match` is the exact `String(fn)` source of each arrow predicate (what normalizeResult emits).
fn packs() -> Vec<Value> {
    vec![
        json!({ "id": "security", "name": "Security", "kind": "security", "desc": "OWASP / CWE \u{2014} secrets \u{b7} injection \u{b7} XSS \u{b7} unsafe APIs", "activate": "always", "match": "(l) => categoryOf(l) === 'security'" }),
        json!({ "id": "architecture", "name": "Architecture", "kind": "architecture", "desc": "cycles \u{b7} layering \u{b7} god files \u{b7} dead code \u{b7} size limits", "activate": "always", "match": "(l) => categoryOf(l) === 'architecture'" }),
        json!({ "id": "testing", "name": "Testing hygiene", "kind": "testing", "desc": "no focused / skipped tests \u{b7} central files must be tested", "activate": "always", "match": "(l) => categoryOf(l) === 'testing'" }),
        json!({ "id": "correctness", "name": "Correctness", "kind": "language", "desc": "swallowed errors \u{b7} loose equality \u{b7} var \u{b7} debugger", "activate": "always", "match": "(l) => categoryOf(l) === 'correctness'" }),
        json!({ "id": "patterns", "name": "Design Patterns (GoF)", "kind": "patterns", "desc": "pattern usage + anti-patterns \u{2014} singleton \u{b7} god object \u{b7} composition", "activate": "manual", "match": "(l) => categoryOf(l) === 'patterns'" }),
        json!({ "id": "performance", "name": "Performance & memory", "kind": "performance", "desc": "the statement tier \u{2014} await/alloc/concat in loops \u{b7} nested loops \u{b7} the leak family (listeners \u{b7} intervals \u{b7} subscriptions)", "activate": "manual", "match": "(l) => categoryOf(l) === 'performance'" }),
        json!({ "id": "react", "name": "React", "kind": "framework", "desc": "React rules \u{2014} keys \u{b7} dangerouslySetInnerHTML \u{b7} bind-in-render", "activate": { "framework": "react" }, "match": "(l) => hasFw(l, 'react') || String(l.key).startsWith('react-')" }),
        json!({ "id": "node", "name": "Node.js", "kind": "framework", "desc": "Node rules \u{2014} process.exit \u{b7} sync fs in request paths", "activate": { "framework": "node" }, "match": "(l) => hasFw(l, 'node') || String(l.key).startsWith('node-')" }),
        json!({ "id": "python", "name": "Python", "kind": "language", "desc": "Python rules \u{2014} exec/eval \u{b7} wildcard imports \u{b7} bare except \u{b7} mutable defaults", "activate": { "language": "py" }, "match": "(l) => hasLang(l, 'py')" }),
    ]
}

// packById(id) = PACKS.find(p => p.id === id) ?? null. JS `===` between a string p.id and the arg:
// a non-string id can never match (different type), so anything but a matching string yields null.
pub fn packById(id: &Value) -> Value {
    if let Some(target) = id.as_str() {
        for p in packs() {
            if p.get("id").and_then(|v| v.as_str()) == Some(target) {
                return p;
            }
        }
    }
    Value::Null
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn looks_up_by_id() {
        // string-activation pack (recommended-on) — match survives as its stringified source
        assert_eq!(
            packById(&json!("security")),
            json!({ "id": "security", "name": "Security", "kind": "security", "desc": "OWASP / CWE \u{2014} secrets \u{b7} injection \u{b7} XSS \u{b7} unsafe APIs", "activate": "always", "match": "(l) => categoryOf(l) === 'security'" })
        );
        // object-activation packs (framework / language gated)
        assert_eq!(packById(&json!("react"))["activate"], json!({ "framework": "react" }));
        assert_eq!(packById(&json!("python"))["activate"], json!({ "language": "py" }));
        assert_eq!(packById(&json!("performance"))["activate"], json!("manual"));
        // key order is part of the output: id·name·kind·desc·activate·match
        let ser = serde_json::to_string(&packById(&json!("node"))).unwrap();
        assert!(ser.starts_with("{\"id\":\"node\",\"name\":\"Node.js\",\"kind\":\"framework\","));
        assert!(ser.contains("\"match\":\"(l) => hasFw(l, 'node')"));
        // unknown id and non-string ids → null (JS `find(...) ?? null`, strict `===`)
        assert_eq!(packById(&json!("nope")), Value::Null);
        assert_eq!(packById(&json!("")), Value::Null);
        assert_eq!(packById(&json!(5)), Value::Null);
        assert_eq!(packById(&Value::Null), Value::Null);
    }
}
