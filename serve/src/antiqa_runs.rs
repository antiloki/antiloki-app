//! antiQA's runs in a pane's box, for the pane's rail (RFC 0001 §7: "a visual cue when a task gets antiQA'd").
//!
//! The agent starts runs through `antiqa mcp`, which writes each one to `<worktree>/.antiloki/qa-reports/<run id>/` (the bake points
//! `--out` there). That folder IS the record: this module only reads it, so the rail shows what the agent really did, never a guess,
//! and a run started by hand into the same folder shows too. Nothing here starts, stops or judges a run.
use serde_json::{json, Value};
use std::path::Path;

pub const REPORTS_DIR: &str = ".antiloki/qa-reports";

fn read_json(p: &Path) -> Option<Value> { serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok() }

/// `running` in `run.json` whose process is gone is `stale`: the run died without writing its ending. Linux can ask; elsewhere the
/// file is believed (a stale mark must never be invented on a platform that cannot check).
fn pid_alive(pid: i64) -> bool {
    if pid <= 0 { return false; }
    #[cfg(target_os = "linux")]
    { Path::new(&format!("/proc/{pid}")).exists() }
    #[cfg(not(target_os = "linux"))]
    { true }
}

/// What triage concluded (antiQA's exit code is built from it, and `failures.json` alone misses accessibility findings):
/// (verdict, defects, drifted, findings). `none` while no triage exists yet.
fn verdict_of(dir: &Path) -> (String, u64, u64, u64) {
    let Some(t) = read_json(&dir.join("triage.json")) else { return ("none".into(), 0, 0, 0) };
    let items = t["items"].as_array().cloned().unwrap_or_default();
    let n = |class: &str| items.iter().filter(|i| i["classification"] == class).count() as u64;
    let (defects, drifted) = (n("defect"), n("drifted"));
    let v = if defects > 0 { "fail" } else if drifted > 0 { "drifted" } else { "pass" };
    (v.into(), defects, drifted, items.len() as u64)
}

/// The newest `limit` runs, newest first. A run folder without a readable `run.json` is skipped.
pub fn runs(worktree: &Path, limit: usize) -> Vec<Value> {
    let root = worktree.join(REPORTS_DIR);
    let mut names: Vec<String> = std::fs::read_dir(&root).map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
    // run ids start with a UTC timestamp, so the name order is the time order
    names.sort_by(|a, b| b.cmp(a));
    let mut out = vec![];
    for name in names {
        let dir = root.join(&name);
        let Some(r) = read_json(&dir.join("run.json")) else { continue };
        let mut state = r["state"].as_str().unwrap_or("unknown").to_string();
        if state == "running" && !pid_alive(r["pid"].as_i64().unwrap_or(0)) { state = "stale".into(); }
        let (verdict, defects, drifted, findings) = verdict_of(&dir);
        let stories: Vec<String> = r["config"]["stories"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        out.push(json!({
            "run_id": name, "product": r["product"], "mode": r["mode"], "state": state, "stage": r["stage"],
            "exit": r["exit"], "started_at": r["startedAt"], "finished_at": r["finishedAt"],
            "verdict": verdict, "defects": defects, "drifted": drifted, "findings": findings,
            // the stories the agent chose (a feature run), or none = the whole pack
            "stories": stories, "origin": r["origin"],
            "report": dir.join("BUG-REPORT.md").is_file().then(|| dir.join("BUG-REPORT.md").to_string_lossy().into_owned()),
        }));
        if out.len() >= limit { break; }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("antiloki-qa-runs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(REPORTS_DIR)).unwrap();
        d
    }
    fn run(wt: &Path, id: &str, run_json: Value, triage: Option<Value>) {
        let d = wt.join(REPORTS_DIR).join(id);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("run.json"), run_json.to_string()).unwrap();
        if let Some(t) = triage { std::fs::write(d.join("triage.json"), t.to_string()).unwrap(); }
    }

    #[test]
    fn runs_are_listed_newest_first_with_the_verdict_built_from_triage() {
        let wt = tmp("order");
        run(&wt, "2026-10-26T10-00-00-000Z-shop-aaaa", json!({ "state": "passed", "product": "shop", "mode": "url", "exit": 0, "config": { "stories": ["cart"] } }), Some(json!({ "items": [{ "classification": "false-positive" }] })));
        run(&wt, "2026-10-26T11-00-00-000Z-shop-bbbb", json!({ "state": "failed", "product": "shop", "mode": "url", "exit": 1 }), Some(json!({ "items": [{ "classification": "defect" }, { "classification": "defect" }, { "classification": "drifted" }] })));
        let r = runs(&wt, 10);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0]["run_id"].as_str().unwrap().contains("bbbb"), r[0]["verdict"].as_str(), r[0]["defects"].as_u64(), r[0]["drifted"].as_u64()), (true, Some("fail"), Some(2), Some(1)));
        assert_eq!((r[1]["verdict"].as_str(), r[1]["stories"].clone()), (Some("pass"), json!(["cart"])), "a feature run names the stories the agent chose");
        assert_eq!(runs(&wt, 1).len(), 1, "the limit applies after sorting");
        let _ = std::fs::remove_dir_all(&wt);
    }

    #[test]
    fn a_running_run_with_no_triage_yet_has_no_verdict_and_a_dead_one_reads_as_stale() {
        let wt = tmp("live");
        run(&wt, "2026-10-26T12-00-00-000Z-shop-cccc", json!({ "state": "running", "stage": "run", "pid": std::process::id() }), None);
        run(&wt, "2026-10-26T11-00-00-000Z-shop-dddd", json!({ "state": "running", "stage": "run", "pid": 2_000_000_000i64 }), None);
        let r = runs(&wt, 10);
        assert_eq!((r[0]["state"].as_str(), r[0]["verdict"].as_str(), r[0]["stage"].as_str()), (Some("running"), Some("none"), Some("run")));
        #[cfg(target_os = "linux")]
        assert_eq!(r[1]["state"], "stale", "the process is gone, so the run did not finish");
        let _ = std::fs::remove_dir_all(&wt);
    }

    #[test]
    fn a_pane_with_no_runs_or_a_broken_folder_lists_nothing_instead_of_failing() {
        let wt = tmp("none");
        assert!(runs(&wt, 10).is_empty());
        std::fs::create_dir_all(wt.join(REPORTS_DIR).join("2026-10-26T13-00-00-000Z-x-eeee")).unwrap();
        std::fs::write(wt.join(REPORTS_DIR).join("2026-10-26T13-00-00-000Z-x-eeee/run.json"), "{ not json").unwrap();
        assert!(runs(&wt, 10).is_empty(), "an unreadable run.json is skipped");
        assert!(runs(&wt.join("missing"), 10).is_empty());
        let _ = std::fs::remove_dir_all(&wt);
    }
}
