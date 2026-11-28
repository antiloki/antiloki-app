//! DID THE WORK SURVIVE? — the correctness signal for a repo that has no tests.
//!
//! Operator 2026-09-25: *"remember some codebasis dont have tests and are unknown to the system … So how to
//! always be aware of the quality and correctness response?"*
//!
//! There is no oracle for "is this right" in a repository nothing measures, and inventing one is how a number
//! ends up confidently wrong. What every repository can answer, with nothing but its own history, is whether
//! the work **held** — and the strongest of those answers costs nothing at all:
//!
//! > **The operator asked the same thing twice.**
//!
//! If you ask again, the first answer did not do it. No test suite was needed to find that out, it works in
//! any language and any project, and nothing in this product reads it today.
//!
//! DELIBERATELY NOT WIRED TO THE WEIGHT YET. A re-ask is not always a failure — an operator refines, adds a
//! condition, or changes their mind, and a signal that punished that would make the router wrong in a new way.
//! So this detects and reports; whether it should move a row's quality is a question for after the operator has
//! seen how often it fires on their own work. That is the same order the laws gate got, and for the same reason.
use serde::{Deserialize, Serialize};

// ── THE OTHER TWO SIGNALS: REVERTED AND CHURN (2026-09-25) ─────────────────────────────────────────────────────────
//
// A re-ask needs only the operator's two messages. These two need the box's own git history, which every repository
// has whether or not it has a test — which is the point. Neither is an accusation and both are reported, never scored,
// for the same reason the re-ask is: an operator reverts deliberately, and a file that changes often may simply be the
// file the work is in this week.
//
// WHAT THEY ARE NOT. `reverted` is not "someone used git revert"; it is the narrower, harder fact that a line an agent
// ADDED was gone again shortly after. `churn` is not "this file is big"; it is the same file being rewritten across
// several consecutive turns, which is what a plan going in circles looks like from the outside.

/// one file's fate after a turn touched it
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fate {
    pub path: String,
    /// lines this turn added that a later commit removed again
    pub undone: usize,
    /// how many of the last commits touched this same file
    pub touches: usize,
}

/// DID A LATER COMMIT UNDO WHAT THIS ONE ADDED? Given the lines a turn added and the lines later commits removed, the
/// overlap is the part that did not survive. Whitespace-only lines are ignored — a reformat that moves a brace is not
/// a revert of anybody's work. Pure, so the rule can be argued with without a repository. Tested.
pub fn undone_lines(added: &[String], later_removed: &[String]) -> usize {
    // A MULTISET overlap: a line written eight times (`}`) and removed once later was undone once, not eight times.
    let norm = |s: &str| s.trim().to_string();
    let mut removed: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for l in later_removed.iter().map(|l| norm(l)).filter(|l| !l.is_empty()) { *removed.entry(l).or_insert(0) += 1; }
    let mut undone = 0;
    for l in added.iter().map(|l| norm(l)).filter(|l| !l.is_empty()) {
        if let Some(left) = removed.get_mut(&l) { if *left > 0 { *left -= 1; undone += 1; } }
    }
    undone
}

/// The lines a `git show --unified=0` diff added (`+`) or removed (`-`): CONTENT only. The file headers (`--- a/x`,
/// `+++ b/x`) sit before the first `@@`, so a line that merely looks like one (`++counter;` shows as `+++counter;`,
/// a SQL comment `-- note` as `--- note`) is still a line. Pure.
fn diff_lines(diff: &str, sign: char) -> Vec<String> {
    let mut in_hunk = false;
    let mut out = vec![];
    for l in diff.lines() {
        if l.starts_with("diff --git ") { in_hunk = false; continue }
        if l.starts_with("@@") { in_hunk = true; continue }
        if in_hunk { if let Some(rest) = l.strip_prefix(sign) { out.push(rest.to_string()) } }
    }
    out
}

/// HOW MUCH OF A TURN DID NOT SURVIVE, 0..1 — and `None` when the turn added nothing, because a turn that wrote no
/// lines cannot have had lines undone and scoring it 0 would flatter it. Pure.
pub fn reverted_share(added: usize, undone: usize) -> Option<f64> {
    if added == 0 { return None }
    Some((undone as f64 / added as f64).clamp(0.0, 1.0))
}

/// THE CHURN OF A FILE — how many of the last `window` commits touched it. One is normal, two is work, and "every
/// commit in the window" is the shape of a plan going in circles. Reported as a count, never as a verdict. Pure.
pub fn churn_of(touches_per_commit: &[bool]) -> usize { touches_per_commit.iter().filter(|t| **t).count() }

/// THE BOX'S OWN HISTORY, READ (2026-09-25). The pure rules above judge; this is the only part that touches git, and
/// it asks for nothing a repository might not have: the last `window` commits, their changed files, and one diff.
///
/// `reverted` is measured on the OLDEST commit in the window — the only one that has had time to be undone — by
/// intersecting the lines it added with the lines every later commit removed. `churn` counts, for each file the window
/// touched, how many of those commits touched it.
///
/// A box with no git, or with fewer commits than the window, returns nothing rather than a zero: "we cannot tell" and
/// "nothing was undone" are different answers and only one of them is honest here.
pub fn look_back(cwd: &str, window: usize) -> Option<(Option<f64>, Vec<Fate>)> {
    if window < 2 { return None }
    let git = |args: &[&str]| -> Option<String> {
        let o = crate::proc::cmd("git").args(args).current_dir(cwd).output().ok()?;
        if !o.status.success() { return None }
        Some(String::from_utf8_lossy(&o.stdout).to_string())
    };
    let shas: Vec<String> = git(&["log", "--format=%H", &format!("-{window}")])?.lines().map(str::to_string).collect();
    if shas.len() < 2 { return None }                       // a single commit cannot have been undone by a later one
    let oldest = shas.last()?.clone();

    // what the oldest commit in the window ADDED, and what every commit after it REMOVED
    let added: Vec<String> = diff_lines(&git(&["show", "--format=", "--unified=0", &oldest])?, '+');
    let mut removed: Vec<String> = vec![];
    for sha in shas.iter().take(shas.len() - 1) {
        if let Some(d) = git(&["show", "--format=", "--unified=0", sha]) {
            removed.extend(diff_lines(&d, '-'));
        }
    }
    let undone = undone_lines(&added, &removed);
    let share = reverted_share(added.iter().filter(|l| !l.trim().is_empty()).count(), undone);

    // and how often each file was touched across the window
    let mut by: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for sha in &shas {
        if let Some(files) = git(&["show", "--format=", "--name-only", sha]) {
            for f in files.lines().map(str::trim).filter(|f| !f.is_empty()) { *by.entry(f.to_string()).or_insert(0) += 1; }
        }
    }
    let mut fates: Vec<Fate> = by.into_iter().map(|(path, touches)| Fate { path, undone: 0, touches }).collect();
    fates.sort_by(|a, b| b.touches.cmp(&a.touches).then(a.path.cmp(&b.path)));
    fates.truncate(5);
    Some((share, fates))
}

/// THE LINE A PERSON READS. Says the fact and stops — no advice, no blame, and nothing at all when there is nothing
/// worth saying, so the trace stays quiet on a healthy turn. Pure.
pub fn survival_note(share: Option<f64>, churn: usize, window: usize) -> Option<String> {
    let mut parts: Vec<String> = vec![];
    // a third is the same line the re-ask penalty draws: below it, noise; above it, worth a sentence
    if let Some(s) = share { if s >= 0.34 { parts.push(format!("{:.0}% of the lines this turn added were gone again within a few commits", s * 100.0)); } }
    if window > 0 && churn >= window.max(3) { parts.push(format!("the same file was touched by all {window} of the last commits")); }
    if parts.is_empty() { None } else { Some(format!("↺ {}", parts.join(" · "))) }
}


/// How alike two asks are, 0..1, by the words they share. Deliberately not a model and not an embedding: this
/// runs on every ask, must cost nothing, and must give the same answer twice. Pure — tested.
pub fn similarity(a: &str, b: &str) -> f64 {
    let (x, y) = (words_of(a), words_of(b));
    if x.is_empty() || y.is_empty() { return 0.0 }
    let inter = x.intersection(&y).count() as f64;
    // Szymkiewicz–Simpson: measured against the SHORTER ask, so "fix the login" counts as a repeat of a long
    // first attempt rather than being diluted by everything the long one also said.
    inter / (x.len().min(y.len()) as f64)
}

/// The words an ask is made of, for `similarity`.
fn words_of(s: &str) -> std::collections::BTreeSet<String> {
    // A LENGTH FILTER IS THE WRONG TOOL. A first version dropped words of three letters or fewer, which keeps
    // "the" (three) and drops "fix" (three) — exactly backwards. Two unrelated asks then scored 0.25 on the
    // strength of sharing the word "the". A named list says what it means and can be argued with.
    const STOP: &[&str] = &["the", "and", "for", "please", "can", "you", "with", "that", "this", "from",
                            "into", "make", "should", "would", "could", "also", "but", "not", "are", "was"];
    s.to_lowercase().split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 1 && !STOP.contains(w))
        .map(str::to_string).collect()
}

/// Shared words over ALL words (Jaccard) - the tie-break between two earlier asks that `similarity` scores alike:
/// the one that says the same thing, not merely a part of it. Pure.
fn jaccard(a: &str, b: &str) -> f64 {
    let (x, y) = (words_of(a), words_of(b));
    let union = x.union(&y).count();
    if union == 0 { 0.0 } else { x.intersection(&y).count() as f64 / union as f64 }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Reask { pub of: String, pub similarity: f64, pub turns_ago: usize }

/// Is this ask a repeat of a recent one? `earlier` is newest-first.
///
/// `within` bounds how far back a repeat counts: an operator returning to a subject next week is not
/// re-asking, they are working. Pure — tested.
pub fn reask_of(ask: &str, earlier: &[String], within: usize, threshold: f64) -> Option<Reask> {
    if ask.trim().len() < 12 { return None }   // "yes", "go on", "thanks" repeat constantly and mean nothing
    earlier.iter().take(within).enumerate()
        .map(|(i, e)| (i, e, similarity(ask, e)))
        .filter(|(_, _, s)| *s >= threshold)
        .map(|(i, e, s)| (i, e, s, jaccard(ask, e)))
        // the closest wins; alike in `similarity`, the one saying the same thing wins; still alike, the NEWER one (the list is
        // newest-first, and `Iterator::max_by` would have handed back the last of a tie - the OLDEST)
        .reduce(|best, cur| if cur.2 > best.2 || (cur.2 == best.2 && cur.3 > best.3) { cur } else { best })
        .map(|(i, e, s, _)| Reask { of: e.chars().take(120).collect(), similarity: (s * 100.0).round() / 100.0, turns_ago: i + 1 })
}

/// The line the trace shows. Says what it means and what it does NOT mean, because a signal that reads as an
/// accusation gets switched off. Pure — tested.
pub fn note(r: &Reask) -> String {
    format!("asked again ({}% like the ask {} turn{} ago) — the first answer may not have held",
        (r.similarity * 100.0).round() as i64, r.turns_ago, if r.turns_ago == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_that_came_back_out_is_counted_and_whitespace_is_not() {
        let added = vec!["let x = 1;".to_string(), "    let y = 2;".to_string(), "".to_string()];
        let removed = vec!["let x = 1;".to_string(), "let y = 2;".to_string()];
        assert_eq!(undone_lines(&added, &removed), 2, "indentation is not identity");
        assert_eq!(undone_lines(&added, &["something else".into()]), 0);
        assert_eq!(undone_lines(&[], &removed), 0);
        // a blank line removed later is not somebody's work being undone
        assert_eq!(undone_lines(&["  ".into()], &["".into()]), 0);
    }

    #[test]
    fn a_turn_that_wrote_nothing_has_no_share_rather_than_a_perfect_one() {
        assert_eq!(reverted_share(0, 0), None, "scoring an empty turn 0% undone would flatter it");
        assert_eq!(reverted_share(10, 5), Some(0.5));
        assert_eq!(reverted_share(10, 99), Some(1.0), "and it cannot exceed everything it wrote");
    }

    #[test]
    fn churn_counts_commits_not_lines() {
        assert_eq!(churn_of(&[true, true, false, true]), 3);
        assert_eq!(churn_of(&[]), 0);
    }

    #[test]
    fn the_note_stays_quiet_unless_there_is_something_to_say() {
        assert_eq!(survival_note(Some(0.1), 1, 5), None, "a tenth undone is noise, and one touch is a file being worked on");
        assert_eq!(survival_note(None, 0, 0), None);
        let hot = survival_note(Some(0.6), 5, 5).unwrap();
        assert!(hot.contains("60%") && hot.contains("all 5"), "{hot}");
        // one signal alone still speaks
        assert!(survival_note(Some(0.9), 0, 5).unwrap().contains("90%"));
        assert!(survival_note(None, 5, 5).unwrap().contains("all 5"));
    }

    use super::*;

    #[test]
    fn alikeness_is_measured_against_the_shorter_ask() {
        assert!(similarity("fix the login page", "fix the login page") > 0.99);
        // the short repeat of a long ask IS a repeat — diluting it by the long ask's other words would hide
        // exactly the case this exists to catch
        assert!(similarity("fix the login", "fix the login page and also the header and the footer styling") >= 0.9);
        assert!(similarity("fix the login page", "write the release notes") < 0.2);
        // function words carry no signal, and sharing them is not likeness
        assert_eq!(similarity("the and for", "the and for"), 0.0);
        assert!(similarity("fix the button", "write the notes") < 0.2, "sharing \"the\" is not sharing a subject");
        assert_eq!(similarity("", "anything"), 0.0);
    }

    #[test]
    fn a_repeat_is_found_and_named_with_how_far_back_it_was() {
        let earlier = vec![
            "add a spinner to the button".to_string(),
            "fix the login page redirect".to_string(),
        ];
        let r = reask_of("fix the login page redirect please", &earlier, 5, 0.7).unwrap();
        assert_eq!(r.turns_ago, 2, "newest-first, so the second entry is two turns ago");
        assert!(r.similarity >= 0.9);
        assert!(note(&r).contains("may not have held"), "the note suggests, it does not accuse");
    }

    #[test]
    fn the_things_that_would_make_this_noise_are_excluded() {
        let earlier = vec!["fix the login page".to_string()];
        // an acknowledgement is not a re-ask, and they repeat constantly
        assert!(reask_of("ok", &earlier, 5, 0.7).is_none());
        assert!(reask_of("go on please", &earlier, 5, 0.7).is_none());
        // a different subject is not a re-ask
        assert!(reask_of("now write the release notes for v2", &earlier, 5, 0.7).is_none());
        // and a repeat from long ago is work, not a re-ask
        let old = vec!["something else".to_string(); 9];
        let mut hist = old.clone(); hist.push("fix the login page".into());
        assert!(reask_of("fix the login page", &hist, 5, 0.7).is_none(), "beyond the window it is not a repeat");
        assert!(reask_of("fix the login page", &hist, 20, 0.7).is_some(), "…and inside a wider window it is");
    }

    #[test]
    fn the_closest_repeat_wins_when_several_are_close() {
        let earlier = vec!["fix login".to_string(), "fix the login page redirect".to_string()];
        let r = reask_of("fix the login page redirect", &earlier, 5, 0.6).unwrap();
        assert_eq!(r.turns_ago, 2, "the nearer match in words, not the nearer in time");
    }

    // ── PHASE 5 (PLAN-THE-TESTS): the thresholds at their edges, the re-ask ties, and the git reader on a real repository ──
    #[test]
    fn the_note_speaks_at_a_third_and_at_a_full_window_and_not_a_hair_below() {
        assert!(survival_note(Some(0.34), 0, 5).is_some(), "0.34 is the line: worth a sentence");
        assert_eq!(survival_note(Some(0.339), 0, 5), None, "a hair under it is noise");
        assert_eq!(survival_note(None, 4, 5), None, "4 of the last 5 commits is work, not a plan going in circles");
        assert!(survival_note(None, 5, 5).unwrap().contains("all 5 of the last commits"));
        // a tiny window still needs THREE touches before it means anything (window.max(3))
        assert_eq!(survival_note(None, 2, 2), None, "two commits, both touching a file, is normal");
        assert!(survival_note(None, 3, 3).is_some());
        assert_eq!(survival_note(None, 9, 0), None, "no window, no claim");
        // both signals speak in one line, undone first, and the line starts with the marker the trace styles on
        let both = survival_note(Some(0.5), 4, 4).unwrap();
        assert!(both.starts_with("↺ ") && both.contains("50% of the lines this turn added") && both.contains(" · the same file was touched by all 4"), "{both}");
        assert!(both.find("50%").unwrap() < both.find("the same file").unwrap());
        assert_eq!(survival_note(Some(1.0), 0, 5).unwrap(), "↺ 100% of the lines this turn added were gone again within a few commits");
    }

    #[test]
    fn alikeness_is_symmetric_case_and_punctuation_blind_and_bounded() {
        let pairs = [("Fix the LOGIN page!", "fix login page"), ("add a spinner to the button", "spinner: button, please"), ("v2 release notes", "write release notes for v2")];
        for (a, b) in pairs {
            let (x, y) = (similarity(a, b), similarity(b, a));
            assert!((x - y).abs() < 1e-12, "{a:?} vs {b:?}: {x} != {y}");
            assert!((0.0..=1.0).contains(&x));
        }
        assert_eq!(similarity("Fix the LOGIN page!", "fix login page"), 1.0, "case, punctuation and function words do not count");
        assert_eq!(similarity("a b c", "a b c"), 0.0, "single letters carry no signal");
        assert_eq!(similarity("fix", ""), 0.0);
        assert_eq!(similarity("", ""), 0.0, "two empty asks are not a repeat of each other");
        assert!(similarity("fix login", "fix-login") > 0.99, "a hyphen is a word boundary");
        assert_eq!(similarity("fix login bug", "write release notes"), 0.0);
    }

    #[test]
    fn a_repeat_needs_a_real_ask_a_window_and_a_threshold_met_exactly() {
        let hist = vec!["fix the login page redirect".to_string()];
        // the length guard is on the trimmed ask: 11 characters is an acknowledgement, 12 is a request
        assert!(reask_of("fix the logi", &hist, 5, 0.1).is_some(), "12 characters is long enough");
        assert!(reask_of("  fix the lo  ", &hist, 5, 0.1).is_none(), "11 after trimming is not");
        // nothing to compare with, or a window of zero, is never a repeat
        assert!(reask_of("fix the login page redirect", &[], 5, 0.7).is_none());
        assert!(reask_of("fix the login page redirect", &hist, 0, 0.7).is_none());
        // the threshold is inclusive: a similarity of exactly 0.5 matches at 0.5 and not at 0.51
        let earlier = vec!["alpha beta gamma delta".to_string()];
        assert_eq!(similarity("alpha beta epsilon zeta eta", "alpha beta gamma delta"), 0.5);
        assert!(reask_of("alpha beta epsilon zeta eta", &earlier, 5, 0.5).is_some());
        assert!(reask_of("alpha beta epsilon zeta eta", &earlier, 5, 0.51).is_none());
        // what it reports is shortened and rounded for the trace
        let long = format!("fix the login page redirect {}", "x".repeat(300));
        let r = reask_of("fix the login page redirect", &[long], 5, 0.5).unwrap();
        assert_eq!(r.of.chars().count(), 120);
        assert_eq!(r.similarity, 1.0);
        let r = reask_of("alpha beta gamma delta epsilon zeta", &["alpha beta gamma".to_string()], 5, 0.5).unwrap();
        assert_eq!(r.similarity, 1.0, "measured against the shorter ask");
        let r = reask_of("alpha beta gamma delta", &["alpha beta xxx".to_string()], 5, 0.5).unwrap();
        assert_eq!(r.similarity, 0.67, "two decimals, not 0.6666…");
    }

    #[test]
    fn when_two_earlier_asks_are_equally_close_the_nearest_in_time_is_the_one_named() {
        // the operator said the same thing 1 turn ago and 2 turns ago: "asked again ... 1 turn ago" is the truth
        let earlier = vec!["fix the login page redirect".to_string(), "fix the login page redirect".to_string()];
        let r = reask_of("fix the login page redirect", &earlier, 5, 0.7).unwrap();
        assert_eq!(r.turns_ago, 1, "a tie goes to the newer ask (the list is newest-first)");
        assert!(note(&r).contains("1 turn ago") && !note(&r).contains("1 turns"));
    }

    #[test]
    fn the_note_pluralises_and_rounds_the_percentage() {
        let r = |s, n| Reask { of: "x".into(), similarity: s, turns_ago: n };
        assert_eq!(note(&r(1.0, 1)), "asked again (100% like the ask 1 turn ago) — the first answer may not have held");
        assert!(note(&r(0.666, 3)).contains("(67% like the ask 3 turns ago)"));
        assert!(note(&r(0.7, 2)).contains("70%"));
    }

    #[test]
    fn a_line_added_five_times_is_not_undone_five_times_by_one_removal() {
        // undone = the OVERLAP of what was added and what was later removed - a multiset overlap, so the closing
        // brace a turn wrote eight times is not counted eight times because a later commit removed one of them
        let added: Vec<String> = ["}", "}", "}", "x"].iter().map(|s| s.to_string()).collect();
        assert_eq!(undone_lines(&added, &["}".to_string()]), 1);
        assert_eq!(undone_lines(&added, &["}".to_string(), "}".to_string()]), 2);
        assert_eq!(undone_lines(&added, &vec!["}".to_string(); 9]), 3, "and never more than were added");
        assert_eq!(undone_lines(&added, &["  x  ".to_string(), "}".to_string()]), 2, "whitespace still does not matter");
    }

    // ── look_back on a real (temporary) repository ──
    fn git_in(dir: &std::path::Path, args: &[&str]) -> String {
        let o = crate::proc::cmd("git").args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]).args(args).current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", if cfg!(windows) { "NUL" } else { "/dev/null" }).env("GIT_CONFIG_SYSTEM", if cfg!(windows) { "NUL" } else { "/dev/null" }).output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).to_string()
    }
    fn scratch_repo(tag: &str) -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!("antiloki-survival-{tag}-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        git_in(&d, &["init", "-q"]);
        d
    }
    fn commit(d: &std::path::Path, files: &[(&str, &str)], msg: &str) {
        for (f, body) in files { std::fs::write(d.join(f), body).unwrap(); }
        git_in(d, &["add", "-A"]);
        git_in(d, &["commit", "-qm", msg]);
    }

    #[test]
    fn the_history_reader_says_nothing_when_it_cannot_tell() {
        let d = scratch_repo("cannot-tell");
        let cwd = d.to_string_lossy().to_string();
        assert_eq!(look_back(&cwd, 0), None);
        assert_eq!(look_back(&cwd, 1), None, "a window of one commit cannot show anything undone");
        assert_eq!(look_back(&cwd, 5), None, "a repository with no commits at all");
        commit(&d, &[("a.txt", "one\n")], "first");
        assert_eq!(look_back(&cwd, 5), None, "one commit cannot have been undone by a later one");
        let plain = std::env::temp_dir().join(format!("antiloki-survival-nogit-{}", std::process::id()));
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(look_back(&plain.to_string_lossy(), 5), None, "no git at all is 'we cannot tell', never 0%");
        assert_eq!(look_back("/definitely/not/a/directory/antiloki", 5), None);
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&plain);
    }

    #[test]
    fn the_history_reader_finds_what_the_oldest_commit_added_and_a_later_one_took_out_again() {
        let d = scratch_repo("undone");
        let cwd = d.to_string_lossy().to_string();
        commit(&d, &[("a.rs", "fn a() {\n    let x = 1;\n    let z = 3;\n}\n"), ("b.txt", "hello\n")], "the turn");
        commit(&d, &[("a.rs", "fn a() {\n    let z = 3;\n}\n")], "took a line out");
        commit(&d, &[("a.rs", "fn a() {\n    let z = 3;\n    let w = 4;\n}\n")], "touched again");
        let (share, fates) = look_back(&cwd, 3).expect("three commits");
        // the oldest commit added 5 non-blank lines; exactly one of them (`let x = 1;`) came out again
        assert_eq!(share, Some(0.2), "1 of 5");
        assert_eq!(fates.iter().map(|f| (f.path.as_str(), f.touches, f.undone)).collect::<Vec<_>>(), vec![("a.rs", 3, 0), ("b.txt", 1, 0)], "most-touched first, then by path");
        // a window that reaches back only two commits judges the middle commit instead
        let (share2, fates2) = look_back(&cwd, 2).unwrap();
        assert_eq!(share2, None, "the second commit only removed lines: a turn that added nothing has no share, not a perfect one");
        assert_eq!(fates2.len(), 1);
        assert_eq!((fates2[0].path.as_str(), fates2[0].touches), ("a.rs", 2));
        // asking for more commits than exist judges the ones that do
        let (share_all, _) = look_back(&cwd, 50).unwrap();
        assert_eq!(share_all, Some(0.2));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn lines_that_look_like_diff_headers_are_still_lines() {
        // `++counter;` shows up in a diff as `+++counter;` and a SQL/Lua/Haskell comment `-- note` as `--- note`;
        // neither is a file header, and dropping them would hide a turn that was undone
        let d = scratch_repo("headers");
        let cwd = d.to_string_lossy().to_string();
        commit(&d, &[("a.c", "++counter;\nkeep();\n"), ("q.sql", "-- note\nselect 1;\n")], "the turn");
        commit(&d, &[("a.c", "keep();\n"), ("q.sql", "select 1;\n")], "took both out");
        let (share, _) = look_back(&cwd, 2).unwrap();
        assert_eq!(share, Some(0.5), "2 of the 4 added lines came out again");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn only_the_five_most_touched_files_are_reported() {
        let d = scratch_repo("five");
        let cwd = d.to_string_lossy().to_string();
        for i in 0..3 { commit(&d, &[("f1.txt", &format!("{i}\n")), ("f2.txt", &format!("{i}\n")), ("f3.txt", &format!("{i}\n")), ("f4.txt", &format!("{i}\n")), ("f5.txt", &format!("{i}\n")), ("f6.txt", &format!("{i}\n"))], &format!("c{i}")); }
        commit(&d, &[("g.txt", "once\n")], "one-off");
        let (_, fates) = look_back(&cwd, 4).unwrap();
        assert_eq!(fates.len(), 5);
        assert!(fates.iter().all(|f| f.touches == 3), "{fates:?}");
        assert_eq!(fates.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["f1.txt", "f2.txt", "f3.txt", "f4.txt", "f5.txt"], "ties break by path, and the one-off is cut");
        let _ = std::fs::remove_dir_all(&d);
    }
}
