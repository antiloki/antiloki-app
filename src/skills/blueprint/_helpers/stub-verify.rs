//! stub-verify.rs — twin of skills/blueprint/_helpers/stub-verify.mjs, the deterministic
//! AST-skeleton VERIFY. NAMES ARE CONTRACT: `verifyAgainstStub` (named export, async in JS —
//! awaited to its resolved value per the async lane; this twin is an ordinary sync `pub fn`).
//! No-db helper: takes 3 positional args, no `context`/db of its own — but internally composes
//! two already-converted standard-lane twins (`blueprintCompose`, `astParse`) that DO take a
//! `&Connection` (unused by either); we open a throwaway in-memory connection to satisfy that
//! signature, mirroring the JS source's `{}` context arg passed to both calls.
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashMap;

fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
fn is_type(v: &Value, t: &str) -> bool {
    v.get("type").and_then(|x| x.as_str()) == Some(t)
}

#[allow(non_snake_case)]
pub fn verifyAgainstStub(blocks: &Value, authoredMap: &Value, plannedNodes: &Value) -> Value {
    let conn = Connection::open_in_memory().unwrap();

    // byPath: plannedNodes filtered to type==='function', grouped by String(n.path ?? '').
    let mut by_path: HashMap<String, Vec<(String, String)>> = HashMap::new();
    if let Some(arr) = plannedNodes.as_array() {
        for n in arr {
            if !is_type(n, "function") {
                continue;
            }
            let p = str_qq(n.get("path"));
            by_path
                .entry(p)
                .or_default()
                .push((str_qq(n.get("name")), str_qq(n.get("signature"))));
        }
    }

    let mut files: Vec<Value> = Vec::new();
    let empty_blocks: Vec<Value> = vec![];
    let block_arr = blocks.as_array().unwrap_or(&empty_blocks);

    'outer: for fb in block_arr {
        if !is_type(fb, "file") {
            continue;
        }
        let path_val = nz(fb.get("path")).or_else(|| nz(fb.get("id")));
        let path = match path_val {
            Some(v) => str_qq(Some(v)),
            None => String::new(),
        };
        let expected = match by_path.get(&path) {
            Some(v) if !v.is_empty() => v.clone(),
            _ => continue, // nothing planned for this file → nothing to verify
        };

        let compose_input = json!({ "block": fb, "authored": authoredMap });
        let composed = crate::blueprint_blueprint_compose::blueprintCompose(&compose_input, &conn)
            .unwrap_or(json!({}));
        let source = composed.get("source").cloned().unwrap_or(Value::Null);

        let parse_input = json!({ "path": path, "source": source });
        let mut parses = true;
        let mut got: Vec<(String, String)> = Vec::new();
        match crate::codebase_ast_parse::astParse(&parse_input, &conn) {
            Ok(r) => {
                if let Some(nodes) = nz(r.get("nodes")).and_then(|v| v.as_array()) {
                    for n in nodes {
                        if !is_type(n, "function") {
                            continue;
                        }
                        got.push((str_qq(n.get("name")), str_qq(n.get("signature"))));
                    }
                }
            }
            Err(e) => {
                // no language adapter for this extension → can't verify it (skip, not a
                // failure); a real syntax error → the composed file does NOT parse.
                if e.contains("language adapter") {
                    files.push(json!({ "path": path, "status": "skipped" }));
                    continue 'outer;
                }
                parses = false;
            }
        }

        let mut got_by_name: HashMap<String, String> = HashMap::new();
        for (name, sig) in &got {
            got_by_name.insert(name.clone(), sig.clone());
        }
        let mut mismatches: Vec<Value> = Vec::new();
        for (name, signature) in &expected {
            match got_by_name.get(name) {
                None => mismatches.push(json!({ "name": name, "reason": "missing" })),
                Some(got_sig) if got_sig != signature => mismatches.push(json!({
                    "name": name,
                    "reason": "signature_changed",
                    "planned": signature,
                    "got": got_sig,
                })),
                Some(_) => {}
            }
        }

        // HARD fail = does not parse. Signature drift / missing planned fns → advisory 'drift'.
        let status = if !parses {
            "fail"
        } else if !mismatches.is_empty() {
            "drift"
        } else {
            "ok"
        };
        files.push(json!({
            "path": path,
            "status": status,
            "parses": parses,
            "mismatches": mismatches,
        }));
    }

    let ok = files.iter().all(|f| f.get("status").and_then(|s| s.as_str()) != Some("fail"));
    json!({ "ok": ok, "files": files })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_block(id: &str) -> Value {
        json!({ "type": "file", "id": id, "path": id, "children": [] })
    }

    #[test]
    fn ok_when_planned_signature_matches_composed_source() {
        let blocks = json!([file_block("a.js")]);
        let authored = json!({ "a.js": { "code": "function foo(a, b) { return a + b; }", "imports": [] } });
        let planned = json!([{ "type": "function", "path": "a.js", "name": "foo", "signature": "(a, b)" }]);
        let r = verifyAgainstStub(&blocks, &authored, &planned);
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["files"][0]["status"], json!("ok"));
        assert_eq!(r["files"][0]["mismatches"], json!([]));
    }

    #[test]
    fn drift_when_signature_or_name_mismatches() {
        let blocks = json!([file_block("b.js")]);
        let authored = json!({ "b.js": { "code": "function foo(a) { return a; }", "imports": [] } });
        let planned = json!([{ "type": "function", "path": "b.js", "name": "foo", "signature": "(a, b)" }]);
        let r = verifyAgainstStub(&blocks, &authored, &planned);
        assert_eq!(r["ok"], json!(true)); // drift is not a hard fail
        assert_eq!(r["files"][0]["status"], json!("drift"));
        assert_eq!(r["files"][0]["mismatches"][0]["reason"], json!("signature_changed"));
    }

    #[test]
    fn drift_missing_when_planned_fn_absent_from_composed_source() {
        let blocks = json!([file_block("c.js")]);
        let authored = json!({ "c.js": { "code": "const z = 1;", "imports": [] } });
        let planned = json!([{ "type": "function", "path": "c.js", "name": "foo", "signature": "()" }]);
        let r = verifyAgainstStub(&blocks, &authored, &planned);
        assert_eq!(r["files"][0]["status"], json!("drift"));
        assert_eq!(r["files"][0]["mismatches"][0]["reason"], json!("missing"));
    }

    #[test]
    fn skipped_when_no_language_adapter_for_extension() {
        let blocks = json!([file_block("d.xyz")]);
        let authored = json!({ "d.xyz": { "code": "whatever", "imports": [] } });
        let planned = json!([{ "type": "function", "path": "d.xyz", "name": "foo", "signature": "()" }]);
        let r = verifyAgainstStub(&blocks, &authored, &planned);
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["files"][0], json!({ "path": "d.xyz", "status": "skipped" }));
    }

    #[test]
    fn file_skipped_from_output_when_nothing_planned() {
        let blocks = json!([file_block("e.js")]);
        let authored = json!({ "e.js": { "code": "function bar() {}", "imports": [] } });
        let planned = json!([]);
        let r = verifyAgainstStub(&blocks, &authored, &planned);
        assert_eq!(r["files"], json!([]));
        assert_eq!(r["ok"], json!(true));
    }

    #[test]
    fn non_file_blocks_are_ignored() {
        let blocks = json!([{ "type": "function", "id": "not-a-file" }]);
        let planned = json!([{ "type": "function", "path": "not-a-file", "name": "foo", "signature": "()" }]);
        let r = verifyAgainstStub(&blocks, &json!({}), &planned);
        assert_eq!(r["files"], json!([]));
    }
}
