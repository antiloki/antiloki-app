//! EVERY OTHER CODING CLI, AS A PANE (operator's queue, 2026-09-22: "engine breadth as a launcher — Cursor Agent,
//! Gemini CLI, Aider, Amp, Kimi Code, Copilot, Grok Build, Droid as raw PTY panes … Heimdall routes only the three it
//! can price; the rest are raw panes, said so").
//!
//! antiloki routes, prices and ranks three engines because it knows their flags, their tiers and their token
//! accounting. Every other coding CLI a person might have installed is still a program that edits this repository in a
//! terminal — and a pane is a terminal. So they are offered as LAUNCHERS: antiloki opens them in a pane, in the right
//! box, and then gets out of the way.
//!
//! WHEN ONE OF THESE IS PROMOTED TO A ROUTED ENGINE, IT IS PROMOTED ON ITS OWN CLI (operator, 2026-09-26: "always
//! prefer the provider solution"). A Cursor seat goes through `cursor-agent`, not through a bridge that happens to
//! reach the same model — a seat is a relationship with a vendor, and the vendor's own client authenticates against
//! it natively and keeps working when someone else's adapter does not. OpenCode is welcome where it is already
//! installed and is never a prerequisite: telling somebody who already pays for Cursor to go and install an
//! unrelated third-party tool first is a barrier at the worst possible moment. See PLAN-THE-SUBSCRIPTIONS.md.
//!
//! THE LINE THIS FILE WILL NOT CROSS. A launcher is never presented as a routed engine. It has no tier, no price, no
//! merge row and no place in the jobs table, and `routed: false` says so on every one of them — because a cost figure
//! for a CLI whose accounting we cannot read would be the most confident lie in the product. The table below records
//! the `--model` flag SHAPE where it is public, which is documentation for the operator, not a promise that antiloki
//! will fill it in.
use serde_json::{json, Value};

/// name · binary · the model flag's shape · what it is
pub struct Launcher {
    pub key: &'static str,
    pub label: &'static str,
    pub bin: &'static str,
    /// how that CLI takes a model on the command line, "" when it takes none or it is not public
    pub model_flag: &'static str,
    pub note: &'static str,
}

/// THE TABLE. Ordered as a person would scan it: the ones most likely to be on a working machine first. A CLI missing
/// from here is not a judgement — it is a table someone has to keep, and it says where to add one.
pub const LAUNCHERS: &[Launcher] = &[
    Launcher { key: "cursor",  label: "Cursor Agent", bin: "cursor-agent", model_flag: "--model <id>",
               note: "Cursor's terminal agent; runs on your Cursor subscription" },
    Launcher { key: "gemini",  label: "Gemini CLI",   bin: "gemini",       model_flag: "-m <model>",
               note: "Google's CLI; free tier with a personal account" },
    Launcher { key: "aider",   label: "Aider",        bin: "aider",        model_flag: "--model <id>",
               note: "the polyglot benchmark's own tool; pairs with any API key you hold" },
    Launcher { key: "amp",     label: "Amp",          bin: "amp",          model_flag: "",
               note: "Sourcegraph's agent; picks its own model" },
    Launcher { key: "kimi",    label: "Kimi Code",    bin: "kimi",         model_flag: "--model <id>",
               note: "Moonshot's CLI; antiloki can also reach Kimi through OpenCode, where it IS priced" },
    Launcher { key: "copilot", label: "Copilot CLI",  bin: "copilot",      model_flag: "--model <id>",
               note: "GitHub's CLI agent; runs on your Copilot seat" },
    Launcher { key: "grok",    label: "Grok Build",   bin: "grok",         model_flag: "--model <id>",
               note: "xAI's CLI" },
    Launcher { key: "droid",   label: "Droid",        bin: "droid",        model_flag: "",
               note: "Factory's agent" },
];

/// ONE LAUNCHER, PROBED. `installed` is the only fact this module produces about a machine, and it is a `which`
/// through `crate::proc` — never a shell — so it answers the same way on Windows.
pub fn probe_one(l: &Launcher) -> Value {
    let path = crate::proc::which(l.bin);
    json!({
        "key": l.key, "label": l.label, "bin": l.bin, "model_flag": l.model_flag, "note": l.note,
        "installed": path.is_some(),
        "path": path.map(|p| p.to_string_lossy().to_string()),
        // THE HONEST FLAG. Heimdall routes what it can price; these it cannot, and a launcher that pretended
        // otherwise would put an invented cost in the one place the operator trusts.
        "routed": false,
    })
}

pub fn probe() -> Vec<Value> { LAUNCHERS.iter().map(probe_one).collect() }

/// The sentence the screen puts above the list. Names what is installed, and states the limit rather than leaving the
/// operator to discover it from an empty cost column. Pure.
pub fn summary(rows: &[Value]) -> String {
    let have: Vec<&str> = rows.iter().filter(|r| r["installed"] == json!(true))
        .filter_map(|r| r["label"].as_str()).collect();
    if have.is_empty() {
        "none of these are on this machine — antiloki can open any of them in a pane once one is installed".into()
    } else {
        format!("{} on this machine: {} — antiloki opens them in a pane, but does not route, price or rank them: their token accounting is not ours to read",
                have.len(), have.join(" · "))
    }
}

/// `GET /api/v1/launchers` — every other coding CLI, and whether it is here.
pub async fn http() -> axum::response::Response {
    use axum::response::IntoResponse;
    let rows = tokio::task::spawn_blocking(probe).await.unwrap_or_default();
    let summary = summary(&rows);
    axum::Json(json!({ "ok": true, "launchers": rows, "summary": summary })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_launcher_is_named_binary_and_all_and_none_claims_to_be_routed() {
        assert!(LAUNCHERS.len() >= 8, "the operator listed eight");
        for l in LAUNCHERS {
            assert!(!l.key.is_empty() && !l.label.is_empty() && !l.bin.is_empty(), "{}", l.key);
            assert!(!l.note.is_empty(), "{} has no sentence saying what it is", l.key);
            assert_eq!(probe_one(l)["routed"], json!(false), "a launcher must never claim to be routed");
        }
        // the keys are unique, or a pane cannot tell two of them apart
        let mut keys: Vec<&str> = LAUNCHERS.iter().map(|l| l.key).collect();
        keys.sort(); let n = keys.len(); keys.dedup();
        assert_eq!(keys.len(), n, "duplicate launcher key");
    }

    #[test]
    fn the_summary_states_the_limit_rather_than_leaving_an_empty_cost_column_to_imply_it() {
        let none = summary(&[json!({ "label": "Aider", "installed": false })]);
        assert!(none.contains("none of these are on this machine"), "{none}");

        let some = summary(&[json!({ "label": "Aider", "installed": true }), json!({ "label": "Gemini CLI", "installed": true })]);
        assert!(some.contains("Aider · Gemini CLI"), "{some}");
        assert!(some.contains("does not route, price or rank them"), "the limit is said, not implied: {some}");
    }

    #[test]
    fn a_launcher_that_takes_no_model_flag_says_so_with_an_empty_string_not_a_guess() {
        let amp = LAUNCHERS.iter().find(|l| l.key == "amp").unwrap();
        assert_eq!(amp.model_flag, "", "inventing a flag for a CLI that has none would be a documented lie");
    }
}
