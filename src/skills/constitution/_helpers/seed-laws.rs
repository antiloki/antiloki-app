//! seed-laws.rs — DATA TWIN of skills/constitution/_helpers/seed-laws.mjs.
//! NAMES ARE CONTRACT: the static carries the exact JS export name (SEED_LAWS). The value is
//! the JS value, pressed at conversion time and proven by the conform 'const' lane (epsilon-0
//! full-value equality). Drift watch re-opens the unit if the JS source changes.
//! Six scopes (language, framework, architecture, community, project, business) — project and
//! business laws are never seeded (mined/authored instead), so only the first four appear here.
use serde_json::Value;
use std::sync::LazyLock;

pub static SEED_LAWS: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(r#"[{"scope":"language","key":"no-swallowed-error","title":"No swallowed errors","rule_md":"An error handler that discards the error hides the #1 source of catastrophic failures (OSDI 2014). Handle, rethrow, or at least log — never an empty `catch {}` or `except: pass`.","detector":"ast","config_json":{"queries":{"tsx":{"q":"(catch_clause body: (statement_block) @hit)","filter":"empty"},"typescript":{"q":"(catch_clause body: (statement_block) @hit)","filter":"empty"},"python":"(except_clause (block (pass_statement)) @hit)"}},"weight":8,"severity":"error","confidence":1,"applies_json":{"languages":["js","ts","py"]}},{"scope":"language","key":"no-loose-equality","title":"No loose equality (`==` / `!=`)","rule_md":"Use strict equality (`===`/`!==`); loose equality performs surprising coercions. `== null` / `!= null` is exempt — it is the idiomatic null-or-undefined check (ESLint `eqeqeq: allow-null`). The project's own `eqeqeq` setting wins: `always` restores the strict form, `off` retires this law for the workspace.","detector":"ast","config_json":{"queries":{"tsx":{"q":"(binary_expression operator: [\"==\" \"!=\"]) @hit","filter":"allow-null"},"typescript":{"q":"(binary_expression operator: [\"==\" \"!=\"]) @hit","filter":"allow-null"}},"project_rule":{"eslint":"eqeqeq"}},"weight":4,"severity":"warn","confidence":1,"applies_json":{"languages":["js","ts"]}},{"scope":"language","key":"no-var","title":"No `var`","rule_md":"Use `const`/`let`. `var` hoists across the function and is a well-known bug source. (Structural — only real `var`, never the word in a string.)","detector":"ast","config_json":{"queries":{"tsx":"[(variable_declaration) (for_in_statement kind: \"var\")] @hit","typescript":"[(variable_declaration) (for_in_statement kind: \"var\")] @hit"}},"weight":3,"severity":"warn","confidence":1,"applies_json":{"languages":["js","ts"]}},{"scope":"language","key":"no-debugger","title":"No `debugger`","rule_md":"A committed `debugger` halts execution in dev tools; never intended in shipped code.","detector":"ast","config_json":{"queries":{"tsx":"(debugger_statement) @hit","typescript":"(debugger_statement) @hit"}},"weight":6,"severity":"error","confidence":1,"applies_json":{"languages":["js","ts"]}},{"scope":"language","key":"js-no-console","title":"No stray `console.log`","rule_md":"Left-in `console.log` is debug residue; use the project logger or remove it.","detector":"regex","config_json":{"pattern":"console\\.log\\s*\\("},"weight":2,"severity":"info","confidence":0.7,"applies_json":{"languages":["js"],"paths_exclude":["**/*.test.*","**/*.spec.*","scripts/**"]}},{"scope":"language","key":"ts-no-any","title":"Avoid `: any`","rule_md":"`any` disables the type checker — the exact defense TypeScript provides. Prefer a real type or `unknown`.","detector":"regex","config_json":{"pattern":":\\s*any(\\[\\])?\\b"},"weight":4,"severity":"warn","confidence":0.7,"applies_json":{"languages":["ts"],"paths_exclude":["**/*.d.ts"]}},{"scope":"language","key":"ts-no-ts-ignore","title":"No `@ts-ignore`","rule_md":"`@ts-ignore` silences a real type error blindly; `@ts-expect-error` with a reason is the reviewable alternative.","detector":"regex","config_json":{"pattern":"@ts-ignore"},"weight":5,"severity":"warn","confidence":0.85,"applies_json":{"languages":["ts"]}},{"scope":"language","key":"py-no-bare-except","title":"No bare `except:`","rule_md":"A bare `except:` catches everything including KeyboardInterrupt/SystemExit. Catch the specific exception.","detector":"regex","config_json":{"pattern":"except\\s*:"},"weight":7,"severity":"error","confidence":0.8,"applies_json":{"languages":["py"]}},{"scope":"language","key":"py-no-mutable-default","title":"No mutable default arguments","rule_md":"A `def f(x=[])` shares one list across all calls — a classic Python trap. Use `None` and build inside.","detector":"regex","config_json":{"pattern":"def\\s+\\w+\\([^)]*=\\s*(\\[\\]|\\{\\})"},"weight":6,"severity":"error","confidence":0.8,"applies_json":{"languages":["py"]}},{"scope":"framework","key":"react-no-array-index-key","title":"No array index as React key","rule_md":"Using the array index as `key` breaks reconciliation on reorder/insert. Use a stable id.","detector":"regex","config_json":{"pattern":"key=\\{[^}]*\\bindex\\b[^}]*\\}"},"weight":4,"severity":"warn","confidence":0.75,"applies_json":{"languages":["js"],"frameworks":["react"]}},{"scope":"framework","key":"react-no-dangerously-set-html","title":"dangerouslySetInnerHTML is an XSS surface","rule_md":"Injecting raw HTML is a CWE-79 vector. If unavoidable, sanitize first and record why in an exception.","detector":"regex","config_json":{"pattern":"dangerouslySetInnerHTML"},"weight":6,"severity":"error","confidence":0.9,"applies_json":{"languages":["js"],"frameworks":["react"]}},{"scope":"framework","key":"node-no-sync-fs","title":"No sync fs in request paths","rule_md":"`readFileSync`/`writeFileSync` block the event loop; in a server hot path they stall every concurrent request.","detector":"regex","config_json":{"pattern":"\\b(readFileSync|writeFileSync)\\s*\\("},"weight":3,"severity":"warn","confidence":0.6,"applies_json":{"languages":["js"],"frameworks":["node"],"paths_exclude":["**/*.test.*","scripts/**","**/*.config.*"]}},{"scope":"architecture","key":"arch-no-test-imports","title":"Production code must not import test code","rule_md":"A non-test file importing a test/spec/fixture module inverts the dependency direction — production now depends on test scaffolding. The layer detector reads the import graph (language-agnostic).","detector":"layer","config_json":{"layers":[{"name":"test","paths":["**/*.test.*","**/*.spec.*","**/__tests__/**","**/tests/**"]},{"name":"src","paths":["**"]}],"forbid":[["src","test"]]},"weight":6,"severity":"error","confidence":1,"applies_json":{}},{"scope":"architecture","key":"arch-no-cycles","title":"No circular dependencies","rule_md":"A file-level import cycle couples modules that should be independent and defeats incremental reasoning.","detector":"graph","config_json":{"smell":"cycle"},"weight":5,"severity":"error","confidence":1,"applies_json":{}},{"scope":"architecture","key":"arch-god-file","title":"No god files","rule_md":"A file in the top decile of BOTH fan-in and complexity is a change magnet — every edit risks everything.","detector":"graph","config_json":{"smell":"god-file"},"weight":4,"severity":"warn","confidence":1,"enforcement":"advise","applies_json":{}},{"scope":"community","key":"lint-clean","title":"The project linter must pass","rule_md":"The codebase's own linter (eslint / ruff, from its config) is the deterministic ground truth for style + many bug patterns. Its findings surface here as violations — one per finding, with the rule code and line.","detector":"linter","config_json":{},"weight":2,"severity":"warn","confidence":1,"applies_json":{"paths_exclude":["**/*.min.*","dist/**","build/**"]}},{"scope":"community","key":"sec-no-hardcoded-secret","title":"No hard-coded secrets","rule_md":"API keys, tokens and passwords in source leak permanently through git history (CWE-798). Read from env.","detector":"regex","config_json":{"pattern":"(api[_-]?key|secret|password|token)\\s*[:=]\\s*['\"][A-Za-z0-9_\\-]{16,}['\"]|\\bsk-[A-Za-z0-9_\\-]{16,}|\\bgh[pousr]_[A-Za-z0-9]{30,}|\\bAKIA[0-9A-Z]{16}\\b|\\bxox[abprs]-[A-Za-z0-9\\-]{10,}|\\bAIza[0-9A-Za-z_\\-]{30,}|-----BEGIN (?:[A-Z]+ )?PRIVATE KEY-----|\\bBearer\\s+[A-Za-z0-9_\\-.=]{24,}","flags":"gi"},"weight":9,"severity":"critical","confidence":0.65,"applies_json":{}}]"#).expect("SEED_LAWS: embedded JSON is valid by construction")
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_laws_parses_with_the_pressed_shape() {
        let v: &Value = &SEED_LAWS;
        let a = v.as_array().expect("array");
        assert_eq!(a.len(), 17, "pressed length");
        let mut keys: Vec<&str> = a.iter().map(|x| x["key"].as_str().expect("key")).collect();
        let n = keys.len();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), n, "keys unique");
    }

    /// THE SECRETS LAW KNEW ONLY A KEY-LIKE NAME (found by the law-check A/B, 2026-10-06: three seats handed `sk-test-...` in a task all hard-coded it, and the check saw 2 of 3 -
    /// `Authorization: "Bearer sk-..."` and `const VENDOR_SANDBOX_KEY = "sk-..."` have no `api_key`/`token` beside the `=`). A secret-SHAPED value is a secret whatever it is called.
    #[test]
    fn the_secrets_law_flags_a_secret_shaped_value_under_any_name_and_leaves_ordinary_code_alone() {
        let v: &Value = &SEED_LAWS;
        let law = v.as_array().unwrap().iter().find(|x| x["key"] == "sec-no-hardcoded-secret").expect("the law");
        let re = regex::RegexBuilder::new(law["config_json"]["pattern"].as_str().unwrap()).case_insensitive(true).build().expect("pattern compiles in the regex crate");
        for flagged in [
            r#"const apiKey = "abcdef0123456789abcdef";"#,                                  // the old, name-based case still holds
            r#"Authorization: "Bearer sk-test-9f3a1c7e5b2d4a60b8c1e7d3f5a92b46","#,         // no key-like name
            r#"const VENDOR_SANDBOX_KEY = "sk-test-9f3a1c7e5b2d4a60b8c1e7d3f5a92b46";"#,    // `_KEY` is not `api_key`
            r#"const k = "ghp_0123456789abcdefghijABCDEFGHIJ0123";"#,
            r#"aws = "AKIAIOSFODNN7EXAMPLE""#,
            r#"slack: "xoxb-1234567890-abcdefghij""#,
            r#"gkey: "AIzaSyA-0123456789abcdefghijklmnopqrstu""#,
            "-----BEGIN RSA PRIVATE KEY-----",
            r#"headers = { Authorization: "Bearer eyJhbGciOiJIUzI1NiJ9.abcdefghijklmnop" }"#,
        ] {
            assert!(re.is_match(flagged), "should be flagged: {flagged}");
        }
        for ordinary in [
            r#"Authorization: `Bearer ${key}`"#,                       // a template, no literal token
            r#"const cacheKey = "a-long-descriptive-cache-key-name";"#,  // a long string under a `key` name is not a secret shape
            r#"const id = "task-definition-identifier-long";"#,          // `sk-` inside `task-` is not a word boundary
            r#"const label = "ask-the-user-something-really-long";"#,
            "const s = process.env.API_KEY;",
            r#"header: "Bearer""#,
        ] {
            assert!(!re.is_match(ordinary), "should NOT be flagged: {ordinary}");
        }
    }

    #[test]
    fn seed_laws_scopes_are_the_expected_four() {
        let v: &Value = &SEED_LAWS;
        let a = v.as_array().expect("array");
        let mut scopes: Vec<&str> = a.iter().map(|x| x["scope"].as_str().expect("scope")).collect();
        scopes.sort();
        scopes.dedup();
        let expected: Value = serde_json::from_str("[\"language\",\"framework\",\"architecture\",\"community\"]").unwrap();
        let mut expected: Vec<&str> = expected.as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
        expected.sort();
        assert_eq!(scopes, expected);
    }
}
