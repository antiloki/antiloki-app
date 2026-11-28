//! failure-advice.rs — twin of skills/driver/_helpers/failure-advice.mjs (no-db helper). NAMES ARE
//! CONTRACT: `adviseOnFailure` (named). Deterministic error-class → remedy table over failure text.
// QUALITY: 80/100 — ordered regex rules (first match wins), one capture-group remedy; verbatim strings
use serde_json::{json, Value};
use std::sync::OnceLock;
use regex::Regex;

fn rules() -> &'static Vec<(&'static str, Regex, &'static str)> {
    static C: OnceLock<Vec<(&'static str, Regex, &'static str)>> = OnceLock::new();
    C.get_or_init(|| vec![
        ("missing-sass-compiler", Regex::new(r"(?i)To import Sass files, you first need to install node-sass").unwrap(),
         "A WORKSPACE toolchain gap, not the authored code: old sass-loader (react-scripts <=4) requires a module named node-sass, which the workspace no longer ships. Fix in the WORKSPACE repo: `npm install node-sass@npm:sass` (aliases modern dart-sass under the legacy name) or upgrade react-scripts/sass-loader. Re-authoring code can NEVER fix this — file a workspace-fix card."),
        ("native-build-dep", Regex::new(r"(?i)node-sass|node-gyp|gyp ERR|prebuild-install|Could not find any Visual Studio").unwrap(),
         "A NATIVE module cannot build on this Node version (the r16 killer: node-sass). Fix the WORKSPACE repo, not the snippets: migrate node-sass → sass (drop-in for .scss imports) or pin an older Node; commit to master, `git merge master` + `npm install` inside the worktree, then rewind the card to the build gate. Re-authoring code can NEVER fix this."),
        ("missing-dependency-binary", Regex::new(r"(?i)is not recognized as an internal or external command|command not found|ENOENT.*(npm|npx|node_modules)").unwrap(),
         "node_modules is missing or incomplete in the worktree — the build tool itself is absent. Run `npm install` in the worktree; if it fails, the [gate worktree.deps] warn notification carries the npm error tail (the root cause is usually a broken dependency, not your code). Re-authoring code can NEVER fix this."),
        // missing-module — capture-group remedy, handled specially below (placeholder text unused)
        ("missing-module", Regex::new(r"(?i)Cannot find module '([^']+)'|Can't resolve '([^']+)'").unwrap(), "\u{0}MISSING_MODULE"),
        ("openssl-legacy-webpack", Regex::new(r"(?i)ERR_OSSL_EVP_UNSUPPORTED|digital envelope routines::unsupported|error:0308010C").unwrap(),
         "Node 17+ removed the legacy OpenSSL hashes that OLD webpack (react-scripts ≤4) uses — the CODE is fine; the toolchain era is the defect. Deterministic fixes, in order: (1) the gate auto-retries the build with NODE_OPTIONS=--openssl-legacy-provider (see the recovery note in this evidence); (2) permanent fix in the WORKSPACE repo: upgrade react-scripts to 5 (npm install react-scripts@5) or pin the env in the build script. Re-authoring code can NEVER fix this."),
        ("warnings-as-errors", Regex::new(r"(?i)Treating warnings as errors because process\.env\.CI|Most CI servers set it automatically").unwrap(),
         "The build FAILED ON WARNINGS, not errors — CI=true makes react-scripts treat every eslint warning as fatal. The real list is at the END of the output (tail-biased evidence now keeps it). A re-author can fix the warned lines; alternatively set the gate build_cmd to run without CI strictness if warnings are acceptable policy."),
        ("deprecation-noise", Regex::new(r"(?i)DeprecationWarning|\[DEP\d+\]").unwrap(),
         "DeprecationWarnings alone do NOT fail builds — they are noise from dependencies under a newer Node. If the build is RED, the REAL error is elsewhere (check the tail of the evidence / the live terminal); do not chase the deprecation."),
        ("syntax-error", Regex::new(r"(?i)SyntaxError|Unexpected token|Parsing error").unwrap(),
         "A real code defect — the compiler line above pinpoints it. The bounded re-author receives this exact output; if the loop caps out, compare the snippet against its design stub on the waterfall code tab."),
        ("timeout", Regex::new(r"(?i)ETIMEDOUT|timed? ?out").unwrap(),
         "The command exceeded its budget. Usually infra (cold cache, native compile). Run the same command manually in the worktree to see how long it really needs; nothing about the code is implicated yet."),
    ])
}

pub fn adviseOnFailure(text: &Value) -> Value {
    let t = match text { Value::Null => String::new(), Value::String(s) => s.clone(), o => o.to_string() };
    for (class, re, remedy) in rules() {
        if let Some(caps) = re.captures(&t) {
            let remedy_md = if *class == "missing-module" {
                let name = caps.get(1).or_else(|| caps.get(2)).map(|m| m.as_str()).unwrap_or("undefined");
                format!("The import '{name}' does not resolve. If it is a package: add it to the WORKSPACE package.json (npm install {name}). If it is a local path: the snippet imports a file the blueprint never authored — check the blueprint blocks/bindings; a re-author with this evidence can fix a wrong path, only a human can add a dependency.")
            } else { remedy.to_string() };
            return json!({ "failure_class": class, "remedy_md": remedy_md });
        }
    }
    json!({ "failure_class": "unclassified", "remedy_md": "Unrecognized failure class — read the full evidence above; the escalation notification carries the tail. If this class recurs, teach it to failure-advice.mjs (one regex + one remedy)." })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classifies_failures() {
        assert_eq!(adviseOnFailure(&json!("SyntaxError: Unexpected token"))["failure_class"], json!("syntax-error"));
        assert_eq!(adviseOnFailure(&json!("Error: Cannot find module 'lodash'"))["failure_class"], json!("missing-module"));
        assert!(adviseOnFailure(&json!("Error: Cannot find module 'lodash'"))["remedy_md"].as_str().unwrap().contains("'lodash'"));
        assert_eq!(adviseOnFailure(&json!("ETIMEDOUT after 30s"))["failure_class"], json!("timeout"));
        assert_eq!(adviseOnFailure(&json!("nothing recognizable"))["failure_class"], json!("unclassified"));
        assert_eq!(adviseOnFailure(&json!(null))["failure_class"], json!("unclassified"));
    }
}
