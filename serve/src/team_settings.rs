//! THE TEAM SETTINGS (operator 2026-11-17: "all settings inside a node and the config of a team should be there" — in the team's own git, versioned).
//! The recipe holds the GRAPH (its hash covers only the graph, so older recipes still rebuild). What else configures a team lives in a sibling file of the same repository and the same commits: `team.settings.toml`.
//!   [improver]  mode, seat, budget, cooldown, frozen nodes ... (the person's: an agent can read them, never change them)
//!   [packages]  the exact packages the person approved (name, commit, hash)
//! It has its own header line with the SHA-256 of its content, so an edit by hand is caught the way an edited recipe is. Run history, the ledger, the replay bank and the leader's inbox are not settings: they stay outside.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const HEAD: &str = "# antiloki team settings v1";

fn canon(v: &Value) -> Value { match v { Value::Object(m) => { let mut k: Vec<_> = m.iter().collect(); k.sort_by(|a, b| a.0.cmp(b.0)); Value::Object(k.into_iter().map(|(a, b)| (a.clone(), canon(b))).collect()) } Value::Array(a) => Value::Array(a.iter().map(canon).collect()), o => o.clone() } }
fn hash_of(v: &Value) -> String { Sha256::digest(serde_json::to_string(&canon(v)).unwrap_or_default().as_bytes()).iter().map(|b| format!("{b:02x}")).collect() }

/// The settings as one value: the improver's config and the approved packages (sorted, so the same settings always give the same text). Pure.
pub fn value_of(improver: &Value, approved: &Value) -> Value {
    let mut a: Vec<String> = approved.as_array().map(|x| x.iter().filter_map(|k| k.as_str().map(String::from)).collect()).unwrap_or_default(); a.sort(); a.dedup();
    json!({ "improver": improver, "packages": { "approved": a } })
}
/// The file's text: a header line with the hash, then the settings as TOML. Pure.
pub fn to_text(v: &Value) -> Result<String, String> {
    let body = toml::to_string_pretty(v).map_err(|e| format!("the settings cannot be written: {e}"))?;
    Ok(format!("{HEAD} · sha256:{}\n{body}", hash_of(v)))
}
/// Read a settings file back; refuse one whose content does not match its hash. Pure.
pub fn from_text(text: &str) -> Result<Value, String> {
    let first = text.lines().next().unwrap_or("");
    let Some(h) = first.strip_prefix(HEAD).and_then(|r| r.split("sha256:").nth(1)).map(|h| h.trim().to_string()) else { return Err("this is not an antiloki team settings file (no header line)".into()) };
    let v: Value = toml::from_str(text).map_err(|e| format!("the settings are torn: {e}"))?;
    if hash_of(&v) == h { Ok(v) } else { Err("the settings do not match their hash: they were edited by hand".into()) }
}
/// The current settings of a team, as the file's text, read from the places the app keeps them.
pub fn render(root: &str, team: &str) -> String {
    let cfg = serde_json::to_value(crate::team_improve::load_config(root, team)).unwrap_or(json!({}));
    let approved: Value = std::fs::read_to_string(crate::team_graph_run::team_files(root, team).join("package-approvals.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!([]));
    to_text(&value_of(&cfg, &approved)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_are_written_sorted_hashed_and_refused_when_edited() {
        let cfg = json!({ "mode": "auto_low_risk", "seat": "claude-code:haiku", "budget_usd": 0.3, "frozen_nodes": [] });
        let a = to_text(&value_of(&cfg, &json!(["git:b@2:", "git:a@1:", "git:a@1:"]))).unwrap();
        let b = to_text(&value_of(&cfg, &json!(["git:a@1:", "git:b@2:"]))).unwrap();
        assert_eq!(a, b, "the same settings give the same text, whatever the order");
        assert!(a.starts_with("# antiloki team settings v1 · sha256:") && a.contains("[improver]") && a.contains("mode = \"auto_low_risk\"") && a.contains("[packages]"));
        assert_eq!(from_text(&a).unwrap()["packages"]["approved"], json!(["git:a@1:", "git:b@2:"]));
        let edited = a.replace("auto_low_risk", "auto_low_risk\"\n#"); let edited2 = a.replace("0.3", "9.0");
        assert!(from_text(&edited2).unwrap_err().contains("hash") && edited != a, "a hand edit is caught");
        assert!(from_text("mode = 1").unwrap_err().contains("not an antiloki"));
    }
}
