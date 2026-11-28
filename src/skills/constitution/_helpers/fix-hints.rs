//! fix-hints.rs — twin of skills/constitution/_helpers/fix-hints.mjs (helper module, no-db).
//! NAMES ARE CONTRACT: fixHintFor. FIX_HINTS/FIX_EXAMPLES are data consts (not units) —
//! FIX_HINTS is transcribed here as the lookup table fixHintFor reads from.
use serde_json::{json, Value};

fn str_qq(v: Option<&Value>) -> String {
    // String(v ?? '')
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

fn fix_hints_for_key(key: &str) -> Option<&'static str> {
    Some(match key {
        "no-swallowed-error" => "Handle or at least log the error — an empty catch silently hides failures. Minimum: `catch (e) { console.error(e) }`; better: recover or re-throw.",
        "no-loose-equality" => "Use strict equality: `===` / `!==` (loose `==` coerces types and hides bugs).",
        "no-var" => "Replace `var` with `const` (or `let` only if reassigned).",
        "no-debugger" => "Delete the `debugger` statement — it halts production browsers with devtools open.",
        "js-no-console" => "Remove the console.* call, or route it through the project's logger.",
        "ts-no-any" => "Give it a real type — or `unknown` plus a narrowing check if the shape is truly dynamic.",
        "ts-no-ts-ignore" => "Fix the underlying type error; if suppression is unavoidable, use `@ts-expect-error` with a reason.",
        "no-nested-ternary" => "Extract to an if/else or a small named helper — nested ternaries are unreadable.",
        "no-ts-non-null" => "Guard the null case explicitly (`if (!x) …`) instead of asserting with `!`.",
        "no-as-any" => "Cast to a real type — or fix the type at its source so no cast is needed.",
        "no-throw-literal" => "Throw `new Error('…')` (or a typed error) so the stack trace survives.",
        "no-with" => "Rewrite without `with` — destructure the object instead: `const { a, b } = obj`.",
        "sec-no-hardcoded-secret" => "Move it to an env var / secret store (`process.env.X`), delete it from source, and ROTATE the leaked value — git history keeps it forever.",
        "no-eval" => "Remove eval — parse data instead of executing it (JSON.parse, a lookup map, a real parser).",
        "py-no-exec" => "Remove exec/eval — parse the data instead of executing it (json.loads, a dispatch dict).",
        "no-new-function" => "Replace `new Function(str)` with a real function or a lookup map of allowed operations.",
        "no-inner-html-assign" => "Use `textContent`, or sanitize first (e.g. DOMPurify) if HTML is unavoidable.",
        "no-document-write" => "Build DOM nodes (createElement/append) or set sanitized markup on a container instead.",
        "no-insecure-http" => "Use https:// — plain http is cleartext (CWE-319).",
        "react-no-href-javascript" => "Use an onClick handler or a real URL — `javascript:` hrefs are an XSS vector.",
        "react-no-dangerously-set-html" => "Render text/JSX instead; if raw HTML is unavoidable, sanitize it (DOMPurify) before injecting.",
        "no-only-test" => "Remove `.only` — it silently disables every other test in the suite.",
        "no-skip-test" => "Fix the skipped test or delete it — a skip is a silent coverage hole.",
        "arch-no-test-imports" => "Production code must not import from tests — move the shared helper into src and import it from both sides.",
        "no-test-gap" => "Add a test that imports this file — it is heavily depended on and a regression here has a wide blast radius.",
        "no-alert" => "Replace the blocking dialog with the app's own toast/modal UI.",
        "no-todo-comments" => "File it as a card/ticket (so it is tracked) and remove the comment.",
        "py-no-print" => "Use the logging module instead of print — leveled, filterable, off in production.",
        "py-no-wildcard-import" => "Import the names you actually use: `from x import a, b`.",
        "py-no-bare-except" => "Catch the specific exception type(s) — a bare except swallows KeyboardInterrupt and real bugs alike.",
        "py-no-mutable-default" => "Default to None and create inside: `def f(x=None): x = x if x is not None else []`.",
        "react-no-bind-in-render" => "Hoist the handler — `useCallback` (or a class-bound method) instead of `.bind`/inline arrow in the prop.",
        "react-no-array-index-key" => "Key by a stable id from the data, not the array index — index keys break reordering.",
        "node-no-process-exit" => "Throw instead, and let the entrypoint decide the exit code.",
        "node-no-sync-fs" => "Use the async fs API (fs.promises) — a *Sync call blocks the event loop for every request.",
        "lint-clean" => "Fix the linter finding (its rule code + message are in the evidence), or adjust the project's own linter config if the rule is wrong for this codebase.",
        "arch-no-cycles" => "Break the cycle — extract the piece both sides need into a third module they both import.",
        "arch-god-file" => "Split the file by responsibility — one concern per module.",
        "max-file-loc" => "Split the file by responsibility until it reads in one sitting.",
        "max-complexity" => "Extract branches into small named functions; flatten nesting with early returns.",
        "max-fan-out" => "Split the hub — group related imports behind one facade, or move logic closer to the data it uses.",
        "no-orphan-files" => "Wire it in (import it where it belongs) or delete it — dead weight rots.",
        "no-unreferenced-files" => "Nothing imports this file — wire its exports in or delete it.",
        "no-await-in-loop" => "Collect the promises and await once: `await Promise.all(items.map(save))` — N sequential awaits become one round.",
        "no-nested-loops" => "Index one side first: `const byId = new Map(bs.map(b => [b.id, b]))`, then a single loop does the join in O(n).",
        "no-alloc-in-loop" => "Hoist what does not depend on the iteration out of the loop; reuse one object/array where safe.",
        "no-concat-in-loop" => "Collect the parts in an array and `parts.join(\"\")` once after the loop.",
        "max-nesting-depth" => "Flatten with early returns (`if (!x) return`) and extract the deep branch into a named function.",
        "max-boolean-chain" => "Extract the chain into a named predicate (`const isEligible = …`) — the name documents the rule; the condition reads as intent.",
        "listener-needs-cleanup" => "Keep the handler reference and remove it on teardown: `removeEventListener` in the cleanup (React: return it from useEffect).",
        "interval-needs-clear" => "Store the id and clear it on teardown: `const id = setInterval(…); return () => clearInterval(id)`.",
        "subscription-needs-unsubscribe" => "Keep the subscription and tear it down: `const sub = obs.subscribe(…); return () => sub.unsubscribe()`.",
        "test-must-assert" => "Every it/test must call expect/assert — a test without an assertion can never fail. Add the assertion, or delete the placeholder.",
        "no-conditional-assert" => "Hoist the assertion out of the branch — assert the guard itself first: expect(user).toBeTruthy(), THEN assert its fields.",
        "no-empty-test" => "Write the body or delete the test — an empty callback always passes and fakes coverage.",
        "no-random-in-tests" => "Inject fixed values (a fixture id, a frozen clock / fake timers) — the test must do the same thing every run.",
        "no-sleep-in-tests" => "Await the actual condition (waitFor / until-loop / fake timers) instead of a fixed sleep — faster AND deterministic.",
        "limit-snapshot-tests" => "Assert the specific fields you care about — a snapshot that asserts everything verifies nothing on purpose.",
        "prefer-composition" => "Compose instead of extending: accept the collaborator as a constructor/prop dependency.",
        "discourage-singleton" => "Export a factory (`createX()`) or inject the instance — a module-level instance is a hidden global.",
        "pattern-no-default-export" => "Use a named export — it survives renames and tree-shakes.",
        "pattern-long-parameter-list" => "Pass one options object: `f({ a, b, c })` — named at the call site, extendable without breaking callers.",
        "pattern-god-object" => "Split by responsibility — one concern per module/class.",
        _ => return None,
    })
}

pub fn fixHintFor(law: &Value, _violation: &Value) -> Value {
    if let Some(key) = law.get("key").and_then(|k| k.as_str()) {
        if let Some(hint) = fix_hints_for_key(key) {
            return json!(hint);
        }
    }
    let det = law.get("detector").and_then(|d| d.as_str());
    let cfg: Value = law
        .get("config_json")
        .and_then(|c| c.as_str())
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .filter(|v| !v.is_null())
        .unwrap_or_else(|| json!({}));

    match det {
        Some("metric") => {
            let metric = cfg.get("metric").filter(|v| !v.is_null()).map(|v| str_qq(Some(v))).unwrap_or_else(|| "the metric".to_string());
            let max = cfg.get("max").filter(|v| !v.is_null()).map(|v| str_qq(Some(v))).unwrap_or_else(|| "the limit".to_string());
            json!(format!("Bring {} back under {} — split or simplify until it fits.", metric, max))
        }
        Some("absent") => {
            let pattern = str_qq(cfg.get("pattern"));
            let sliced: String = pattern.chars().take(80).collect();
            json!(format!("Add the required pattern to this file: {}.", sliced))
        }
        Some("path") => json!("Rename or move the file to match the convention this law states."),
        Some("layer") => json!("Reverse the dependency — this import crosses a layer boundary in the forbidden direction."),
        Some("import") => json!("Remove or replace this import — the dependency is forbidden here."),
        _ => {
            match law.get("rule_md").filter(|v| !v.is_null()) {
                Some(rule_md) => json!(format!("Follow the law: {}", str_qq(Some(rule_md)))),
                None => json!("Align this line with the law it breaks."),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_law_key_wins() {
        let law = json!({ "key": "no-var" });
        assert_eq!(fixHintFor(&law, &Value::Null), json!("Replace `var` with `const` (or `let` only if reassigned)."));
    }

    #[test]
    fn detector_metric_fallback() {
        let law = json!({ "detector": "metric", "config_json": "{\"metric\":\"loc\",\"max\":200}" });
        assert_eq!(fixHintFor(&law, &Value::Null), json!("Bring loc back under 200 — split or simplify until it fits."));
    }

    #[test]
    fn detector_absent_fallback() {
        let law = json!({ "detector": "absent", "config_json": "{\"pattern\":\"try/catch\"}" });
        assert_eq!(fixHintFor(&law, &Value::Null), json!("Add the required pattern to this file: try/catch."));
    }

    #[test]
    fn rule_md_fallback() {
        let law = json!({ "rule_md": "Do the right thing." });
        assert_eq!(fixHintFor(&law, &Value::Null), json!("Follow the law: Do the right thing."));
    }

    #[test]
    fn generic_fallback() {
        let law = Value::Null;
        assert_eq!(fixHintFor(&law, &Value::Null), json!("Align this line with the law it breaks."));
    }
}
