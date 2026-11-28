//! cognitive.rs — twin of skills/constitution/_helpers/cognitive.mjs (no-db helper module + one
//! TRANSITIVE native-tree-sitter walk). NAMES ARE CONTRACT: `rollupCognitive` (fixtured no-db
//! helper), `analyzeTree` (TRANSITIVE).
//!
//! ## `analyzeTree` is now ported — the prior PARITY-DEFERRED blocker is resolved
//! An earlier pass of this module deferred `analyzeTree` because it walks a LIVE tree-sitter CST
//! and, at the time, this codebase had no Rust tree-sitter counterpart. `ts-query.rs` has since
//! landed the native `tree-sitter` crate (real `Tree`/`Language`, no WASM — see its module doc),
//! and `constitution-check.mjs` (the only real caller of `analyzeTree`, via
//! `ctx.stmt = analyzeTree(grammar, parsed.tree)`) needs a faithful `stmt` fact stream to drive
//! detectors.rs's `stmt` detector tier (`await-in-loop`/`nested-loop`/`bool-chain`/etc.) — so the
//! blocker no longer holds and this twin ports it for real.
//!
//! `analyzeTree` takes `(grammar: &str, tree: &Tree, source: &str) -> Value` — one extra
//! parameter (`source`) versus the JS `analyzeTree(grammar, tree)`, for the same zero-copy reason
//! ts-query.rs's `queryMatches` and parse-treesitter.rs's helpers already carry `source`/`src`:
//! native `Node::text` has no getter, only `Node::utf8_text(source_bytes)`. It is TRANSITIVE (the
//! input `&Tree` is not a `serde_json::Value`, so it cannot ride the `(a: &Value, b: &Value)`
//! no-db-helper fixture lane — same convention as detectors.rs's `runDetector`) — real `pub fn` +
//! native `#[cfg(test)]` coverage exercising every fact kind (branch/loop/await/alloc/concat/bool/
//! assert/the leak-pair vocabulary) for both the `tsx` and `python` families, no JSON fixture.
//!
//! Node-type/field-name strings (`FAMILY`, `CALLS`) are ported VERBATIM from the JS `FAMILY`
//! table — `binary_expression`/`boolean_operator`'s `operator` field and `call_expression`/
//! `call`'s `function` field are the exact names parse-treesitter.rs's `ts_complexity`/
//! `ts_callee_name`/`py_callee_name` already use successfully against the same pinned native
//! grammar crates, so this reuses confirmed-correct field names rather than assuming them fresh.
//! `grammar === 'python' ? 'python' : 'tsx'` fallback (JS) is ported as `grammar == "python"`
//! else `tsx` — `typescript`/`javascript` both fall through to the `tsx` family, matching JS.
use serde_json::{json, Map, Value};
use tree_sitter::{Node, Tree};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

fn as_i64(v: Option<&Value>) -> i64 {
    v.and_then(|x| x.as_i64()).unwrap_or(0)
}

// ── analyzeTree — ONE walk over a live tree-sitter CST, verbatim node-type/field-name port ──

struct Family {
    branch: &'static [&'static str],
    loop_: &'static [&'static str],
    func: &'static [&'static str],
    await_n: &'static [&'static str],
    alloc: &'static [&'static str],
    bool_ops: &'static [&'static str],
    aug_concat: &'static [&'static str],
}

const TSX_FAMILY: Family = Family {
    branch: &["if_statement", "switch_statement", "ternary_expression", "catch_clause"],
    loop_: &["for_statement", "for_in_statement", "while_statement", "do_statement"],
    func: &["function_declaration", "function_expression", "function", "arrow_function", "method_definition", "generator_function", "generator_function_declaration"],
    await_n: &["await_expression"],
    alloc: &["object", "array", "new_expression", "arrow_function", "function_expression"],
    bool_ops: &["&&", "||"],
    aug_concat: &["augmented_assignment_expression"],
};

const PYTHON_FAMILY: Family = Family {
    branch: &["if_statement", "conditional_expression", "except_clause"],
    loop_: &["for_statement", "while_statement"],
    func: &["function_definition", "lambda"],
    await_n: &["await", "await_expression"],
    alloc: &["list", "dictionary", "set", "lambda"],
    bool_ops: &["and", "or"],
    aug_concat: &["augmented_assignment"],
};

const CALLS: &[(&str, &str)] = &[
    ("addEventListener", "listener_add"), ("removeEventListener", "listener_remove"),
    ("setInterval", "interval_set"), ("clearInterval", "interval_clear"),
    ("subscribe", "sub"), ("unsubscribe", "unsub"),
];

// `pairs[key] ?? (pairs[key] = {n:0, line})` — the FIRST occurrence's line is kept; only `n`
// bumps on later hits, mirroring JS's nullish-assignment-then-increment idiom exactly.
fn bump_pair(pairs: &mut Map<String, Value>, key: &str, line: i64) {
    let entry = pairs.entry(key.to_string()).or_insert_with(|| json!({ "n": 0, "line": line }));
    let n = entry.get("n").and_then(Value::as_i64).unwrap_or(0) + 1;
    entry["n"] = json!(n);
}

#[allow(clippy::too_many_arguments)]
fn visit(
    node: Node,
    depth: i64,
    loop_depth: i64,
    fam: &Family,
    src: &[u8],
    facts: &mut Vec<Value>,
    pairs: &mut Map<String, Value>,
    max_depth: &mut i64,
    max_line: &mut Value,
) {
    let t = node.kind();
    let line = node.start_position().row as i64 + 1;
    let mut d = depth;
    let mut ld = loop_depth;
    if fam.func.contains(&t) { d = 0; ld = 0; } // cognitive resets per function (Sonar)
    if fam.branch.contains(&t) {
        facts.push(json!({ "kind": "branch", "line": line, "depth": d, "loopDepth": ld }));
        d += 1;
    } else if fam.loop_.contains(&t) {
        facts.push(json!({ "kind": "loop", "line": line, "depth": d, "loopDepth": ld }));
        d += 1;
        ld += 1;
    }
    if fam.await_n.contains(&t) {
        facts.push(json!({ "kind": "await", "line": line, "depth": d, "loopDepth": ld }));
    }
    if fam.alloc.contains(&t) {
        facts.push(json!({ "kind": "alloc", "line": line, "depth": d, "loopDepth": ld }));
    }
    if fam.aug_concat.contains(&t) {
        let text = node.utf8_text(src).unwrap_or("");
        let head: String = text.chars().take(40).collect();
        if head.contains("+=") {
            facts.push(json!({ "kind": "concat", "line": line, "depth": d, "loopDepth": ld }));
        }
    }
    if t == "binary_expression" || t == "boolean_operator" {
        let op_text: Option<String> = node
            .child_by_field_name("operator")
            .and_then(|n| n.utf8_text(src).ok())
            .map(|s| s.to_string())
            .or_else(|| {
                let mut cursor = node.walk();
                let found = node.children(&mut cursor).map(|c| c.kind().to_string()).find(|k| fam.bool_ops.contains(&k.as_str()));
                found
            });
        if let Some(op) = op_text {
            if fam.bool_ops.contains(&op.as_str()) {
                facts.push(json!({ "kind": "bool", "line": line, "depth": d, "loopDepth": ld }));
            }
        }
    }
    if t == "call_expression" || t == "call" {
        let callee = node
            .child_by_field_name("function")
            .and_then(|n| n.utf8_text(src).ok())
            .unwrap_or("")
            .to_string();
        for (needle, key) in CALLS {
            if callee == *needle || callee.ends_with(&format!(".{needle}")) {
                bump_pair(pairs, key, line);
            }
        }
        if callee == "it" || callee == "test" {
            bump_pair(pairs, "t_block", line);
        }
        if callee == "expect" || callee == "assert" || callee.starts_with("assert.") {
            bump_pair(pairs, "t_assert", line);
            facts.push(json!({ "kind": "assert", "line": line, "depth": d, "loopDepth": ld }));
        }
    }
    if d > *max_depth {
        *max_depth = d;
        *max_line = json!(line);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        visit(child, d, ld, fam, src, facts, pairs, max_depth, max_line);
    }
}

/// `analyzeTree(grammar, tree)` — JS takes only `(grammar, tree)`; this twin adds `source` (see
/// module doc). Walks the whole tree once emitting `{facts, pairs, maxNesting}` — statement-level
/// events with context (branch/loop/bool/await/alloc/concat/assert), leak-family counters
/// (listener/interval/subscription add-vs-remove, first-occurrence line), and the deepest
/// Sonar-style nesting (+ its line). `grammar == "python"` selects the Python family; every other
/// grammar (including `tsx`/`typescript`) uses the JS-family superset — same fallback as the JS
/// ternary.
pub fn analyzeTree(grammar: &str, tree: &Tree, source: &str) -> Value {
    let fam = if grammar == "python" { &PYTHON_FAMILY } else { &TSX_FAMILY };
    let src = source.as_bytes();
    let mut facts: Vec<Value> = Vec::new();
    let mut pairs: Map<String, Value> = Map::new();
    let mut max_depth: i64 = 0;
    let mut max_line: Value = Value::Null;
    visit(tree.root_node(), 0, 0, fam, src, &mut facts, &mut pairs, &mut max_depth, &mut max_line);
    json!({ "facts": facts, "pairs": Value::Object(pairs), "maxNesting": { "depth": max_depth, "line": max_line } })
}

// JS: attribute facts (branch/loop, each weighted 1+depth) to block spans → per-block cognitive
// + the file-level rollup (worst block, loop/nested-loop counts, deepest nesting).
pub fn rollupCognitive(stmt: &Value, spans: &Value) -> Value {
    if !truthy(Some(stmt)) {
        return Value::Null;
    }
    let empty: Vec<Value> = Vec::new();
    let facts = stmt.get("facts").and_then(|v| v.as_array()).unwrap_or(&empty);

    let mut total: i64 = 0;
    let mut loops: i64 = 0;
    let mut nested_loops: i64 = 0;
    for f in facts {
        let kind = f.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        if kind == "loop" {
            loops += 1;
            if as_i64(f.get("loopDepth")) >= 1 {
                nested_loops += 1;
            }
        }
        if kind == "branch" || kind == "loop" {
            total += 1 + as_i64(f.get("depth"));
        }
    }

    let spans_arr: &Vec<Value> = spans.as_array().unwrap_or(&empty); // spans ?? []
    let mut worst: i64 = 0;
    let mut worst_block: Value = Value::Null;
    let mut worst_line: Value = Value::Null;
    let mut per_block: Vec<Value> = Vec::new();
    for s in spans_arr {
        let start = as_i64(s.get("start"));
        let end = as_i64(s.get("end"));
        let mut cog: i64 = 0;
        for f in facts {
            let kind = f.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            if kind != "branch" && kind != "loop" {
                continue;
            }
            let line = as_i64(f.get("line"));
            if line >= start && line <= end {
                cog += 1 + as_i64(f.get("depth"));
            }
        }
        let id = s.get("id").filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null); // id ?? null
        let name = s.get("name").cloned().unwrap_or(Value::Null);
        let start_v = s.get("start").cloned().unwrap_or(Value::Null);
        per_block.push(json!({ "id": id, "name": name.clone(), "start": start_v.clone(), "cog": cog }));
        if cog > worst {
            worst = cog;
            worst_block = name;
            worst_line = start_v;
        }
    }

    json!({
        "total": total,
        "worst": worst,
        "worst_block": worst_block,
        "worst_line": worst_line,
        "loops": loops,
        "nested_loops": nested_loops,
        "max_nesting": stmt.get("maxNesting").and_then(|m| m.get("depth")).cloned().unwrap_or(Value::Null),
        "max_nesting_line": stmt.get("maxNesting").and_then(|m| m.get("line")).cloned().unwrap_or(Value::Null),
        "per_block": per_block,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_stmt_short_circuits() {
        assert_eq!(rollupCognitive(&Value::Null, &json!([])), Value::Null);
    }

    #[test]
    fn empty_spans_yield_null_worst_and_empty_per_block() {
        let stmt = json!({
            "facts": [
                { "kind": "branch", "line": 3, "depth": 0, "loopDepth": 0 },
                { "kind": "loop", "line": 4, "depth": 1, "loopDepth": 0 },
                { "kind": "bool", "line": 5, "depth": 2, "loopDepth": 1 },
            ],
            "pairs": {},
            "maxNesting": { "depth": 2, "line": 4 },
        });
        let out = rollupCognitive(&stmt, &json!([]));
        assert_eq!(out["total"], json!(3)); // branch(1+0) + loop(1+1) = 1 + 2
        assert_eq!(out["worst"], json!(0));
        assert_eq!(out["worst_block"], Value::Null);
        assert_eq!(out["worst_line"], Value::Null);
        assert_eq!(out["loops"], json!(1));
        assert_eq!(out["nested_loops"], json!(0)); // the loop fact's own loopDepth is 0
        assert_eq!(out["max_nesting"], json!(2));
        assert_eq!(out["max_nesting_line"], json!(4));
        assert_eq!(out["per_block"], json!([]));
    }

    #[test]
    fn null_spans_behaves_like_empty_spans() {
        let stmt = json!({
            "facts": [{ "kind": "branch", "line": 3, "depth": 0, "loopDepth": 0 }],
            "pairs": {},
            "maxNesting": { "depth": 0, "line": null },
        });
        assert_eq!(rollupCognitive(&stmt, &Value::Null)["per_block"], json!([]));
    }

    #[test]
    fn attributes_nested_loop_cognitive_to_the_winning_span() {
        // mirrors a real analyzeTree() output for a doubly-nested for-loop with an inner if(a&&b) x+=1
        let stmt = json!({
            "facts": [
                { "kind": "loop", "line": 3, "depth": 0, "loopDepth": 0 },
                { "kind": "loop", "line": 4, "depth": 1, "loopDepth": 1 },
                { "kind": "branch", "line": 5, "depth": 2, "loopDepth": 2 },
                { "kind": "bool", "line": 5, "depth": 3, "loopDepth": 2 },
                { "kind": "concat", "line": 6, "depth": 3, "loopDepth": 2 },
            ],
            "pairs": {},
            "maxNesting": { "depth": 3, "line": 5 },
        });
        let spans = json!([
            { "id": "foo", "name": "foo", "start": 2, "end": 9 },
            { "id": null, "name": "bar", "start": 20, "end": 25 },
        ]);
        let out = rollupCognitive(&stmt, &spans);
        assert_eq!(out["total"], json!(6)); // loop(1)+loop(2)+branch(3)
        assert_eq!(out["worst"], json!(6));
        assert_eq!(out["worst_block"], json!("foo"));
        assert_eq!(out["worst_line"], json!(2));
        assert_eq!(out["loops"], json!(2));
        assert_eq!(out["nested_loops"], json!(1)); // the line-4 loop has loopDepth 1
        assert_eq!(out["max_nesting"], json!(3));
        assert_eq!(out["max_nesting_line"], json!(5));
        assert_eq!(
            out["per_block"],
            json!([
                { "id": "foo", "name": "foo", "start": 2, "cog": 6 },
                { "id": null, "name": "bar", "start": 20, "cog": 0 },
            ])
        );
    }

    // ── analyzeTree (real native tree-sitter parses, no fixture) ────────────────────────────

    fn tsx(src: &str) -> Tree {
        crate::constitution__helpers_ts_query::getTree("tsx", src).expect("tsx parses").0
    }
    fn py(src: &str) -> Tree {
        crate::constitution__helpers_ts_query::getTree("python", src).expect("python parses").0
    }
    fn kinds(out: &Value, kind: &str) -> Vec<Value> {
        out["facts"].as_array().unwrap().iter().filter(|f| f["kind"] == json!(kind)).cloned().collect()
    }

    #[test]
    fn nested_for_loop_with_bool_branch_and_concat() {
        let src = "function f() {\n  for (let i=0;i<n;i++) {\n    for (let j=0;j<n;j++) {\n      if (a && b) { s += 1; }\n    }\n  }\n}\n";
        let tree = tsx(src);
        let out = analyzeTree("tsx", &tree, src);
        let loops = kinds(&out, "loop");
        assert_eq!(loops.len(), 2);
        assert_eq!(loops[0]["line"], json!(2));
        assert_eq!(loops[0]["depth"], json!(0));
        assert_eq!(loops[1]["line"], json!(3));
        assert_eq!(loops[1]["depth"], json!(1));
        assert_eq!(loops[1]["loopDepth"], json!(1));
        let branches = kinds(&out, "branch");
        assert_eq!(branches.len(), 1);
        assert_eq!(branches[0]["line"], json!(4));
        assert_eq!(branches[0]["depth"], json!(2));
        assert_eq!(branches[0]["loopDepth"], json!(2));
        let bools = kinds(&out, "bool");
        assert_eq!(bools.len(), 1);
        assert_eq!(bools[0]["line"], json!(4));
        let concats = kinds(&out, "concat");
        assert_eq!(concats.len(), 1);
        assert_eq!(concats[0]["line"], json!(4));
        assert_eq!(out["maxNesting"]["depth"], json!(3));
        assert_eq!(out["maxNesting"]["line"], json!(4));
    }

    #[test]
    fn function_boundary_resets_nesting_sonar_style() {
        let src = "if (a) {\n  function inner() {\n    if (b) { x(); }\n  }\n}\n";
        let tree = tsx(src);
        let out = analyzeTree("tsx", &tree, src);
        let branches = kinds(&out, "branch");
        assert_eq!(branches.len(), 2);
        assert_eq!(branches[0]["depth"], json!(0)); // outer if, top-level depth 0
        assert_eq!(branches[1]["depth"], json!(0)); // inner if resets to 0 at the function boundary
    }

    #[test]
    fn await_and_alloc_facts() {
        let src = "async function f() {\n  const o = { a: 1 };\n  const arr = [1,2];\n  await g();\n}\n";
        let tree = tsx(src);
        let out = analyzeTree("tsx", &tree, src);
        assert_eq!(kinds(&out, "await").len(), 1);
        assert_eq!(kinds(&out, "alloc").len(), 2); // object + array
    }

    #[test]
    fn listener_and_test_call_pairs_first_line_kept() {
        let src = "el.addEventListener('x', f);\nel.addEventListener('y', g);\nel.removeEventListener('x', f);\nit('works', () => { expect(1).toBe(1); });\n";
        let tree = tsx(src);
        let out = analyzeTree("tsx", &tree, src);
        assert_eq!(out["pairs"]["listener_add"], json!({ "n": 2, "line": 1 })); // first occurrence line kept
        assert_eq!(out["pairs"]["listener_remove"], json!({ "n": 1, "line": 3 }));
        assert_eq!(out["pairs"]["t_block"], json!({ "n": 1, "line": 4 }));
        assert_eq!(out["pairs"]["t_assert"], json!({ "n": 1, "line": 4 }));
        assert_eq!(kinds(&out, "assert").len(), 1);
    }

    #[test]
    fn switch_ternary_catch_do_while_and_generator_are_branch_or_loop() {
        let src = "function* g() {\n  switch (x) { case 1: break; }\n  const y = a ? 1 : 2;\n  try { f(); } catch (e) {}\n  let i = 0;\n  do { i++; } while (i < 3);\n}\n";
        let tree = tsx(src);
        let out = analyzeTree("tsx", &tree, src);
        assert_eq!(kinds(&out, "branch").len(), 3); // switch + ternary + catch
        assert_eq!(kinds(&out, "loop").len(), 1); // do-while
    }

    #[test]
    fn python_family_if_for_while_except_and_bool() {
        let src = "def f():\n    for i in range(3):\n        while i:\n            if a and b:\n                pass\n    try:\n        g()\n    except Exception:\n        pass\n";
        let tree = py(src);
        let out = analyzeTree("python", &tree, src);
        assert_eq!(kinds(&out, "loop").len(), 2); // for + while
        assert_eq!(kinds(&out, "branch").len(), 2); // if + except
        assert_eq!(kinds(&out, "bool").len(), 1);
    }

    #[test]
    fn python_alloc_and_call_facts() {
        // NOTE: Python's `assert x` keyword statement is an `assert_statement` node, not a
        // `call` — the t_assert/CALLS vocabulary only fires for an actual function-call node
        // named `assert`/`assert.*` (e.g. a JS test file's `assert(x)`), which native Python
        // source never produces via the `assert` keyword. So this fixture exercises alloc only.
        let src = "def f():\n    lst = [1, 2]\n    d = {}\n    s = {1}\n";
        let tree = py(src);
        let out = analyzeTree("python", &tree, src);
        assert_eq!(kinds(&out, "alloc").len(), 3); // list + dict + set
    }

    #[test]
    fn typescript_grammar_falls_back_to_tsx_family() {
        let src = "function f(): void {\n  for (let i = 0; i < 1; i++) {}\n}\n";
        let (tree, _lang) = crate::constitution__helpers_ts_query::getTree("typescript", src).unwrap();
        let out = analyzeTree("typescript", &tree, src);
        assert_eq!(kinds(&out, "loop").len(), 1);
    }
}
