//! THE COMPLETE FEATURE TEST (operator 2026-09-24: "After you finish make a complete test of the feature").
//! Real git repositories, real branches, real `git format-patch`, real `git am` on the other side. The only
//! thing not exercised against a live server is the HTTPS call itself — that needs somebody's credential, and
//! the decision that leads to it is what this proves.
use antiloki_serve::forge::*;
use serde_json::json;

fn repo(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("antiloki-e2e-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("src")).unwrap();
    let git = |a: &[&str]| { let _ = std::process::Command::new("git").args(a).current_dir(&d).output(); };
    git(&["init", "-q", "-b", "main"]); git(&["config", "user.email", "t@t"]); git(&["config", "user.name", "t"]);
    std::fs::write(d.join("src/cart.ts"), "export const total = (n: number) => n;\n").unwrap();
    git(&["add", "-A"]); git(&["commit", "-qm", "base"]);
    d
}
fn git_in(d: &std::path::Path, a: &[&str]) -> String {
    let o = std::process::Command::new("git").args(a).current_dir(d).output().expect("git runs");
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

#[test]
fn the_whole_delivery_from_an_agents_branch_to_a_reviewer_who_can_apply_it() {
    // ── an agent worked on its own episode branch, the way boxes.rs leaves it ──────────────────
    let d = repo("deliver");
    let root = d.to_string_lossy().to_string();
    let git = |a: &[&str]| { let _ = std::process::Command::new("git").args(a).current_dir(&d).output(); };
    git(&["checkout", "-q", "-b", "antiloki/dani/main"]);
    git(&["checkout", "-q", "-b", "antiloki/dani/ep-2"]);
    std::fs::write(d.join("src/cart.ts"), "export const total = (n: number) => Math.max(0, n);\n").unwrap();
    std::fs::write(d.join("src/discount.ts"), "export const apply = (c: number) => c;\n").unwrap();
    git(&["add", "-A"]); git(&["commit", "-qm", "clamp the total and add discounts"]);

    // ── the story the run produced, rendered as the review a human reads ───────────────────────
    let story = json!({
        "goal": "Clamp the cart total and add discount codes", "status": "done",
        "who": { "solo": false,
            "brain": { "label": "Claude Sonnet", "did": "decided the plan, split the work and judged what came back" },
            "hands": { "label": "OpenCode DeepSeek v4 Flash", "did": "wrote the code, one file at a time" } },
        "router": { "on": true, "line": "Heimdall sent this to the low tier with high confidence" },
        "stages": [
            { "title": "Agreed what to build", "who": "Claude · mid tier", "went_right": true, "red_is_good": false, "ms": 41000.0 },
            { "title": "Proved the tests fail on empty files", "who": "the compiler and the test runner", "went_right": true, "red_is_good": true, "ms": 3100.0 },
        ],
        "gates": [
            { "name": "The tests", "kind": "deterministic", "passed": true, "detail": "npx vitest related 'src/cart.ts' · passed · 700 ms" },
            { "name": "The judge", "kind": "model", "passed": false, "score": 62, "detail": "apply() ignores unknown codes silently" },
        ],
        "summary": { "files": { "written": ["src/cart.ts", "src/discount.ts"], "read": 4 },
            "tokens": { "human": "88k" }, "time": { "human": "2 min 3 s" },
            "money": { "spent_human": "$0.04", "baseline_human": "$0.61", "saved_pct": 93.4 } },
    });
    let body = pr_body(&story, "Clamp the cart total", "antiloki/dani/ep-2");
    assert!(wants_full_body(&story), "two files and two gates is worth the page");
    for must in ["## How it was made", "**Claude Sonnet**", "**OpenCode DeepSeek v4 Flash**",
                 "✓ as intended", "_a machine, not an opinion_", "_a model's judgement_",
                 "`src/discount.ts`", "88k tokens · 2 min 3 s", "93% kept", "a subscription pays $0 metered"] {
        assert!(body.contains(must), "the review must carry {must:?}:\n{body}");
    }

    // ── delivered with no forge, no token, no account ──────────────────────────────────────────
    let req = PrRequest { root: root.clone(), branch: "antiloki/dani/ep-2".into(), base: "antiloki/dani/main".into(),
        title: "Clamp the cart total".into(), body: body.clone() };
    let out = PatchForge::at(&root).open_pr(&req).expect("the patch is written");
    assert_eq!(out.kind, "patch");
    let patch_at = out.local.clone().unwrap();
    assert!(out.note.contains("git am"), "{}", out.note);
    let review = std::fs::read_to_string(d.join(".antiloki/outbox/antiloki-dani-ep-2.REVIEW.md")).unwrap();
    assert_eq!(review, body, "what the reviewer reads IS the run's ledger, not a second telling of it");

    // ── A REVIEWER ON THE OTHER SIDE OF THE WORLD APPLIES IT ───────────────────────────────────
    // a clone that has never heard of antiloki, no worktree, no serve, no token
    let them = repo("reviewer");
    let before = std::fs::read_to_string(them.join("src/cart.ts")).unwrap();
    let am = std::process::Command::new("git").args(["am", &patch_at.to_string_lossy()]).current_dir(&them).output().expect("git am runs");
    assert!(am.status.success(), "the patch must apply on a plain clone: {}", String::from_utf8_lossy(&am.stderr));
    assert_ne!(std::fs::read_to_string(them.join("src/cart.ts")).unwrap(), before);
    assert!(them.join("src/discount.ts").is_file(), "a created file arrives too");
    assert!(git_in(&them, &["log", "-1", "--pretty=%s"]).contains("clamp the total"), "and it lands with its own history");

    let _ = std::fs::remove_dir_all(&d); let _ = std::fs::remove_dir_all(&them);
}

#[test]
fn a_pull_request_is_decided_from_the_remote_and_falls_back_rather_than_failing() {
    let none = json!({});
    // the operator's own site repo, read from the machine it lives on
    let real = std::process::Command::new("git").args(["remote", "get-url", "origin"])
        .current_dir("/home/user/dev/antiloki/antiloki-site").output().ok()
        .filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    if let Some(r) = real {
        let (kind, host, repo) = detect(&r, &none);
        assert_eq!(kind, Kind::GitHub);
        assert_eq!(host, "github.com");
        assert!(repo.contains('/'), "owner/name: {repo}");
    }
    // a workspace with NO remote — which is ministore on this machine — is the patch backend, not an error
    assert_eq!(detect("", &none).0, Kind::Patch);

    // A TOKEN IS NEVER PRINTED, whatever happens. This assertion exists because the first run of this very test
    // called `unwrap_err()` on an `Ok(Token)` and the panic message carried a live GitHub credential into a log.
    // `find_token` legitimately SUCCEEDS on a machine where `gh` is logged in, so the test must never render it.
    match find_token(Kind::GitHub, "example-no-such-host.invalid") {
        Ok(t) => {
            assert!(!format!("{t:?}").contains(&t.value), "a token must not print itself");
            assert!(!t.from.is_empty(), "…but where it came from is always said");
        }
        Err(e) => {
            assert!(e.contains("ANTILOKI_FORGE_TOKEN"), "{e}");
            assert!(e.contains("gh auth login"), "the error says how to sign in: {e}");
            assert!(!e.contains("gho_"), "an error must not carry a credential either");
        }
    }
}

#[test]
fn a_branch_with_nothing_new_is_refused_before_anything_is_written() {
    let d = repo("empty");
    let root = d.to_string_lossy().to_string();
    let req = PrRequest { root: root.clone(), branch: "main".into(), base: "main".into(),
        title: "nothing".into(), body: "b".into() };
    let e = PatchForge::at(&root).open_pr(&req).unwrap_err();
    assert!(e.contains("nothing to send"), "{e}");
    assert!(!d.join(".antiloki/outbox/main.patch").exists(), "and no empty file is left behind");
    let _ = std::fs::remove_dir_all(&d);
}
