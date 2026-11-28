//! `antiloki doctor` — WHAT THIS MACHINE ACTUALLY HAS, COMPUTED.
//!
//! PLAN-THE-LEAN-YEARS I.6. The ask was "live documentation, more than we have", and I argued the problem is
//! not volume: `DESKTOP.md`, `WINDOWS-TESTING.md`, `QUALITY.md`, the changelog and unusually thorough commit
//! messages already describe this system, and a second layer of prose describing the same code is how
//! documentation starts lying. What was missing is documentation that **cannot** go stale, because it is
//! computed from the thing it describes.
//!
//! So this answers, live: which engines are on this PATH and where, which models each offers, what this
//! workspace is built in, which gates would run here and the exact command each would use, how many merges the
//! record can offer and how many this machine could actually start, and where every path resolves.
//!
//! It is also the first thing to run when something is wrong on a machine that is not this one — a Windows box
//! answering "claude: not installed" next to a `where.exe` that finds it is a one-line bug report.
use serde_json::{json, Value};

/// The whole report as data, so the same answers serve a terminal and an HTTP route without drifting apart.
/// The only impure function here; `render` below is pure over its output and tested.
pub fn report(root: &str) -> Value {
    let engines = crate::engines::probe();
    let advice = crate::engines::advice(&engines);
    let merges = crate::merges::pairs().unwrap_or_default();
    let offerable = crate::engines::offerable(&merges, &engines);
    let ready = offerable.iter().filter(|m| m["ready"] == json!(true)).count();

    // canonical, because "." tells a reader nothing and a bug report with "." in it is unactionable
    let root_abs = std::fs::canonicalize(root).map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| root.to_string());
    let p = crate::gates::probe(std::path::Path::new(&root_abs));
    let files = vec!["src/example.ts".to_string()];
    let gates: Vec<Value> = crate::gates::Gate::ALL.iter().map(|g| {
        let (cmd, why) = match crate::gates::gate_cmd(*g, &p, &files) {
            Ok(c) => (Some(c), None),
            Err(e) => (None, Some(e)),
        };
        json!({ "gate": g.key(), "cmd": cmd, "skipped": why })
    }).collect();

    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
        "workspace": { "root": root_abs, "stack": crate::gates::stack_of(&p),
                       "manifests": manifests(&p) },
        "engines": engines, "advice": advice,
        "merges": { "measured": merges.len(), "runnable_here": ready },
        // "is the record corrupted?" must be a question with an answer, printed without being asked
        "evidence": crate::merges::integrity().unwrap_or(json!({})),
        "gates": gates,
        // A DIAGNOSTIC MUST NOT CHANGE WHAT IT DIAGNOSES. The first version called `ensure_data_root`, which
        // creates directories and sets ANTILOKI_SUBSTRATE_ROOT as a side effect — so running `doctor` on a
        // machine that was configured wrongly would half-fix it, and the second run would report a state the
        // first one created. It reads the same two sources that function reads, and creates nothing.
        "paths": {
            "data_root": std::env::var("ANTILOKI_SUBSTRATE_ROOT").ok().filter(|v| !v.trim().is_empty())
                .or_else(|| crate::default_data_root(std::env::consts::OS, &|k| std::env::var(k).ok()).map(|p| p.to_string_lossy().to_string()))
                .unwrap_or_else(|| "(not resolved)".into()),
            "merges_db": crate::merges::path().to_string_lossy().to_string(),
        },
    })
}

fn manifests(p: &crate::gates::Project) -> Vec<&'static str> {
    let mut v = vec![];
    if p.pkg.is_some() { v.push("package.json") }
    if p.cargo.is_some() { v.push("Cargo.toml") }
    if p.go { v.push("go.mod") }
    if p.py.is_some() { v.push("pyproject.toml") }
    if p.make.is_some() { v.push("Makefile") }
    if p.dotnet { v.push("*.sln/*.csproj") }
    if p.gradle { v.push("gradle") }
    if p.maven { v.push("pom.xml") }
    v
}

/// The report as a person reads it. Pure over the report — tested, so the shape of the output is pinned and
/// cannot quietly lose a line.
pub fn render(r: &Value) -> String {
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let mut out = vec![format!("antiloki {} · {} {}", s(r, "version"), s(r, "os"), s(r, "arch")), String::new()];

    let w = &r["workspace"];
    let man = w["manifests"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" · ")).unwrap_or_default();
    out.push("WORKSPACE".into());
    out.push(format!("  {}", if s(w, "root").is_empty() { "(none active)".into() } else { s(w, "root") }));
    out.push(format!("  built in   {}{}", s(w, "stack"), if man.is_empty() { String::new() } else { format!("  ({man})") }));
    out.push(String::new());

    out.push("ENGINES".into());
    for e in r["engines"].as_array().into_iter().flatten() {
        let n = e["models"].as_array().map(|a| a.len()).unwrap_or(0);
        if e["installed"] == json!(true) {
            out.push(format!("  ✓ {:<12} {}  ({} model{})", s(e, "label"), s(e, "path"), n, if n == 1 { "" } else { "s" }));
        } else {
            out.push(format!("  ✗ {:<12} not on the PATH — {}", s(e, "label"), s(e, "install")));
        }
    }
    out.push(String::new());

    out.push("GATES — what would run here, and the command it would use".into());
    for g in r["gates"].as_array().into_iter().flatten() {
        match g["cmd"].as_str() {
            Some(c) => out.push(format!("  ✓ {:<6} {c}", s(g, "gate"))),
            // A SKIPPED GATE IS NEVER A PASS, and the reason is the useful half — "no lint script" and "the
            // lint passed" are different facts, so the report prints why rather than leaving a blank.
            None => out.push(format!("  – {:<6} skipped: {}", s(g, "gate"), s(g, "skipped"))),
        }
    }
    out.push(String::new());

    let m = &r["merges"];
    out.push("MERGES".into());
    out.push(format!("  {} measured in the record · {} runnable on this machine",
        m["measured"].as_i64().unwrap_or(0), m["runnable_here"].as_i64().unwrap_or(0)));
    let ev = &r["evidence"];
    if ev.get("controlled").is_some() {
        let n = |p: &str, k: &str| ev[p][k].as_i64().unwrap_or(0);
        out.push(format!("  evidence: {} controlled runs ({} rows) · {} observed ({} rows)",
            n("controlled", "runs"), n("controlled", "rows"), n("observed", "runs"), n("observed", "rows")));
        out.push("            the two are scored separately — a weight only compares within its kind".into());
        let q = ev["quarantined"].as_array().cloned().unwrap_or_default();
        if q.is_empty() { out.push("            nothing quarantined".into()); }
        else { for x in q { out.push(format!("            QUARANTINED source {:?}: {} rows, not counted", s(&x, "source"), x["rows"].as_i64().unwrap_or(0))); } }
    }
    let a = &r["advice"];
    out.push(format!("  {}", s(a, "headline")));
    for u in a["unlock"].as_array().into_iter().flatten() {
        out.push(format!("  + {} unlocks {}", s(u, "engine"), s(u, "unlocks")));
    }
    out.push(String::new());

    out.push("PATHS".into());
    for k in ["data_root", "merges_db"] { out.push(format!("  {:<11} {}", k, s(&r["paths"], k))); }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        json!({
            "version": "0.1.5", "os": "windows", "arch": "x86_64",
            "workspace": { "root": "C:/dev/thing", "stack": "rust", "manifests": ["Cargo.toml", "Makefile"] },
            "engines": [
                { "label": "Claude Code", "installed": true, "path": "C:/n/claude.cmd", "models": ["haiku", "sonnet", "opus"], "install": "" },
                { "label": "Codex", "installed": false, "path": null, "models": [], "install": "npm i -g @openai/codex" },
            ],
            "advice": { "headline": "You can run agents, but not a merge.", "unlock": [{ "engine": "opencode", "unlocks": "every merge in the catalogue" }] },
            "merges": { "measured": 24, "runnable_here": 9 },
            "gates": [
                { "gate": "test", "cmd": "cargo test --quiet", "skipped": null },
                { "gate": "lint", "cmd": null, "skipped": "this Cargo project declares no lints (no clippy.toml, no [lints])" },
            ],
            "paths": { "data_root": "C:/Users/d/.antiloki", "merges_db": "C:/Users/d/.antiloki/merges.sqlite" },
        })
    }

    /// ARCHITECTURE.md NAMES FILES, AND A DOCUMENT THAT NAMES A FILE THAT MOVED IS WORSE THAN NO DOCUMENT
    /// (2026-09-25). It is the one page on this project that cannot be generated — nothing computed can
    /// explain how a keystroke becomes a worktree commit — so the next best thing is a test that fails when
    /// its references rot. Every `path/like/this.rs` in it must exist.
    #[test]
    fn every_file_architecture_md_points_at_still_exists() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let doc = std::fs::read_to_string(repo.join("ARCHITECTURE.md")).expect("ARCHITECTURE.md is missing");
        let mut checked = 0;
        for chunk in doc.split('`').skip(1).step_by(2) {
            // only things that look like a path INTO this tree: a slash and a known source extension
            let c = chunk.trim();
            if !c.contains('/') || c.contains(' ') { continue }
            if !(c.ends_with(".rs") || c.ends_with(".tsx") || c.ends_with(".toml") || c.ends_with(".sqlite")) { continue }
            if c.ends_with(".sqlite") { continue }   // those are runtime files, not tracked ones
            // a path into a SIBLING repo (`../antiloki-ui/...`) can only be checked where that checkout sits beside this one - a CI runner has only this repo
            if c.starts_with("../") && !repo.join(c.split('/').nth(1).map(|d| format!("../{d}")).unwrap_or_default()).exists() { continue }
            let p = repo.join(c.trim_start_matches("./"));
            assert!(p.exists(), "ARCHITECTURE.md points at {c}, which does not exist");
            checked += 1;
        }
        assert!(checked >= 10, "only {checked} paths checked — the extractor stopped matching the document");
    }

    #[test]
    fn the_report_reads_as_a_bug_report_someone_can_send_back() {
        let t = render(&sample());
        // the first line answers "which build, on what" — the two things every report needs and half omit
        assert!(t.starts_with("antiloki 0.1.5 · windows x86_64"), "{t}");
        // an engine that is here says WHERE, because "not installed" next to a where.exe that finds it is the
        // whole bug report, and the path is what tells those apart
        assert!(t.contains("✓ Claude Code  C:/n/claude.cmd  (3 models)"), "{t}");
        // …and one that is missing says how to get it rather than just being absent
        assert!(t.contains("✗ Codex        not on the PATH — npm i -g @openai/codex"), "{t}");
        assert!(t.contains("built in   rust  (Cargo.toml · Makefile)"));
        assert!(t.contains("24 measured in the record · 9 runnable on this machine"));
        assert!(t.contains("+ opencode unlocks every merge in the catalogue"));
    }

    #[test]
    fn a_skipped_gate_prints_why_and_is_never_shown_as_a_pass() {
        let t = render(&sample());
        assert!(t.contains("✓ test   cargo test --quiet"));
        assert!(t.contains("– lint   skipped: this Cargo project declares no lints"), "{t}");
        assert!(!t.contains("✓ lint"), "a gate with nothing to run must not wear a tick");
    }

    #[test]
    fn no_workspace_says_so_rather_than_printing_an_empty_line() {
        let mut v = sample(); v["workspace"]["root"] = json!("");
        assert!(render(&v).contains("(none active)"));
    }

    // ── PHASE 5 (PLAN-THE-TESTS): the renderer with holes in its input, the manifests, and the report on a real directory ──
    fn scratch(tag: &str) -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!("antiloki-doctor-{tag}-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// RULE: a report with nothing in it still prints - a diagnostic that crashes on a broken machine is no diagnostic.
    #[test]
    fn an_empty_report_renders_every_heading_and_never_panics() {
        let t = render(&json!({}));
        for heading in ["WORKSPACE", "ENGINES", "GATES", "MERGES", "PATHS"] { assert!(t.contains(heading), "{heading} missing:\n{t}"); }
        assert!(t.contains("(none active)") && t.contains("0 measured in the record · 0 runnable on this machine"), "{t}");
        assert!(!t.contains("evidence:"), "no evidence block without evidence");
        // wrong types read as absent, not as a crash
        let weird = json!({ "engines": "x", "gates": 5, "workspace": [], "merges": null, "advice": 7, "evidence": "e", "paths": null });
        let _ = render(&weird);
    }

    #[test]
    fn a_single_model_is_singular_and_a_long_label_is_not_cut() {
        let mut r = sample();
        r["engines"] = json!([
            { "label": "Cursor", "installed": true, "path": "/bin/cursor-agent", "models": ["composer"], "install": "" },
            { "label": "A Very Long Engine Label", "installed": true, "path": "/x", "models": [], "install": "" },
        ]);
        let t = render(&r);
        assert!(t.contains("✓ Cursor       /bin/cursor-agent  (1 model)"), "{t}");
        assert!(t.contains("(0 models)") && t.contains("A Very Long Engine Label"), "{t}");
        assert!(!t.contains("(1 models)"));
    }

    #[test]
    fn the_evidence_block_names_what_is_controlled_observed_and_quarantined() {
        let mut r = sample();
        r["evidence"] = json!({ "controlled": { "runs": 120, "rows": 30 }, "observed": { "runs": 45, "rows": 12 }, "quarantined": [] });
        let t = render(&r);
        assert!(t.contains("evidence: 120 controlled runs (30 rows) · 45 observed (12 rows)"), "{t}");
        assert!(t.contains("nothing quarantined"));
        r["evidence"]["quarantined"] = json!([{ "source": "zen-legacy", "rows": 7 }, { "source": "x", "rows": 1 }]);
        let t = render(&r);
        assert!(t.contains("QUARANTINED source \"zen-legacy\": 7 rows, not counted") && t.contains("QUARANTINED source \"x\": 1 rows, not counted"), "{t}");
        assert!(!t.contains("nothing quarantined"));
        // a half-written evidence object counts what it has as zero
        r["evidence"] = json!({ "controlled": {} });
        assert!(render(&r).contains("evidence: 0 controlled runs (0 rows) · 0 observed (0 rows)"));
    }

    #[test]
    fn every_unlock_is_listed_in_order_and_no_manifest_means_no_parentheses() {
        let mut r = sample();
        r["advice"]["unlock"] = json!([{ "engine": "codex", "unlocks": "the planners" }, { "engine": "cursor", "unlocks": "composer merges" }]);
        r["workspace"]["manifests"] = json!([]);
        let t = render(&r);
        assert!(t.find("+ codex unlocks the planners").unwrap() < t.find("+ cursor unlocks composer merges").unwrap());
        assert!(t.contains("built in   rust\n") || t.contains("built in   rust\r\n") || t.lines().any(|l| l == "  built in   rust"), "{t}");
    }

    #[test]
    fn gate_and_path_columns_line_up() {
        let t = render(&sample());
        assert!(t.contains("  data_root   C:/Users/d/.antiloki") && t.contains("  merges_db   C:/Users/d/.antiloki/merges.sqlite"), "{t}");
        assert!(t.contains("  ✓ test   cargo test --quiet") && t.contains("  – lint   skipped:"), "the gate name is padded to six");
    }

    #[test]
    fn each_manifest_is_named_once_in_a_fixed_order() {
        let d = scratch("manifests");
        let names = |d: &std::path::Path| manifests(&crate::gates::probe(d));
        assert!(names(&d).is_empty(), "an empty directory declares nothing");
        for (file, want) in [("package.json", "package.json"), ("Cargo.toml", "Cargo.toml"), ("go.mod", "go.mod"), ("pyproject.toml", "pyproject.toml"), ("Makefile", "Makefile"),
                             ("app.sln", "*.sln/*.csproj"), ("build.gradle.kts", "gradle"), ("pom.xml", "pom.xml")] {
            let sub = scratch("manifest-one");
            std::fs::write(sub.join(file), "{}").unwrap();
            assert_eq!(names(&sub), vec![want], "{file}");
            let _ = std::fs::remove_dir_all(&sub);
        }
        // together they come out in the order the checks are written, not directory order
        for f in ["pom.xml", "go.mod", "Cargo.toml", "package.json"] { std::fs::write(d.join(f), "{}").unwrap(); }
        assert_eq!(names(&d), vec!["package.json", "Cargo.toml", "go.mod", "pom.xml"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    // `report()` itself is not driven from a unit test on purpose: it probes the engines, and for an installed OpenCode
    // that means running `opencode models` - a model CLI, which no test here may start.
}
