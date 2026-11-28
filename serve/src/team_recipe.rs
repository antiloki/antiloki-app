//! THE TEAM RECIPE (RFC 0013 §1, operator 2026-11-16): a team is rebuilt from ONE text file, like a Dockerfile. The recipe is the team graph written as TOML, with a first line that carries
//! its format and the SHA-256 of the graph it builds; building it again must give the identical graph (the hash is the proof). The graph file stays what the app runs; the recipe is what is versioned, exported and improved.
//!
//!   to_recipe(&Graph)  -> text            from_recipe(text) -> Graph (refuses a recipe whose hash line does not match what it builds)
//!   hash_of(&Graph)    -> sha256 hex      round_trips(&Graph) -> Ok(hash) or the first difference
//!
//! TOML has no null, so a null field is dropped on the way out and comes back as the field's default; a graph whose meaning depends on a null is reported by `round_trips`, never silently changed.
use crate::team_graph::Graph;
use serde_json::Value;
use sha2::{Digest, Sha256};

const HEAD: &str = "# antiloki team recipe v1";

fn strip_nulls(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(m.iter().filter(|(_, x)| !x.is_null()).map(|(k, x)| (k.clone(), strip_nulls(x))).collect()),
        Value::Array(a) => Value::Array(a.iter().filter(|x| !x.is_null()).map(strip_nulls).collect()),
        o => o.clone(),
    }
}
/// The hash of a graph: SHA-256 over its canonical JSON (keys sorted, nulls dropped). Pure.
pub fn hash_of(g: &Graph) -> String {
    fn canon(v: &Value) -> Value { match v { Value::Object(m) => { let mut k: Vec<_> = m.iter().collect(); k.sort_by(|a, b| a.0.cmp(b.0)); Value::Object(k.into_iter().map(|(a, b)| (a.clone(), canon(b))).collect()) } Value::Array(a) => Value::Array(a.iter().map(canon).collect()), o => o.clone() } }
    let t = serde_json::to_string(&canon(&strip_nulls(&g.to_json()))).unwrap_or_default();
    Sha256::digest(t.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}
pub fn to_recipe(g: &Graph) -> Result<String, String> {
    let body = toml::to_string_pretty(&strip_nulls(&g.to_json())).map_err(|e| format!("the team cannot be written as a recipe: {e}"))?;
    Ok(format!("{HEAD} · sha256:{}\n{body}", hash_of(g)))
}
pub fn from_recipe(text: &str) -> Result<Graph, String> {
    let first = text.lines().next().unwrap_or("");
    let want = first.strip_prefix(HEAD).and_then(|r| r.split("sha256:").nth(1)).map(|h| h.trim().to_string());
    let Some(h) = want else { return Err("this is not an antiloki team recipe (no header line)".into()) };
    let v: Value = toml::from_str(text).map_err(|e| format!("the recipe is torn: {e}"))?;
    let g = Graph::from_json(&v)?;
    if h == hash_of(&g) { Ok(g) } else { Err("the recipe does not build the team it says it builds (its hash does not match)".into()) }
}
/// Write the graph as a recipe, build it back, and say whether it is the same team. Ok carries the hash.
pub fn round_trips(g: &Graph) -> Result<String, String> {
    let back = from_recipe(&to_recipe(g)?)?;
    if hash_of(&back) != hash_of(g) { return Err("the rebuilt team differs from the original".into()); }
    if strip_nulls(&back.to_json()) != strip_nulls(&g.to_json()) { return Err("the rebuilt graph differs in content".into()); }
    Ok(hash_of(g))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team_graph::{template, TEMPLATES};
    #[test] fn every_template_rebuilds_from_its_recipe_with_the_same_hash() {
        for k in TEMPLATES { let g = template(k, "pack", 6.0).unwrap(); let h = round_trips(&g).unwrap_or_else(|e| panic!("{k}: {e}")); assert_eq!(h, hash_of(&g), "{k}"); assert!(to_recipe(&g).unwrap().starts_with(HEAD), "{k}"); }
    }
    #[test] fn a_changed_recipe_is_refused_not_rebuilt() {
        let g = template("blank", "x", 2.0).unwrap(); let r = to_recipe(&g).unwrap();
        let bad = r.replace("budget_usd = 2.0", "budget_usd = 3.0"); assert_ne!(bad, r, "the test edits something");
        assert!(from_recipe(&bad).unwrap_err().contains("hash"), "an edited recipe must fail its hash");
        assert!(from_recipe("team = \"x\"").unwrap_err().contains("not an antiloki"));
    }
    #[test] fn the_hash_ignores_how_the_graph_was_written() { let a = template("blank", "x", 2.0).unwrap(); let b = Graph::from_json(&a.to_json()).unwrap(); assert_eq!(hash_of(&a), hash_of(&b)); let c = template("blank", "y", 2.0).unwrap(); assert_ne!(hash_of(&a), hash_of(&c)); }
}
