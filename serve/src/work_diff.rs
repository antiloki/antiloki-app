//! THE DIFF BESIDE THE OLD VERSION (operator 2026-09-24: "at the beautiful box i just mentioned didnt appeared
//! the diff also side by side to the old version of all touched files").
//!
//! For every file a run wrote: the committed text and the text on disk now. The side-by-side is drawn in the
//! browser from those two; what matters here is that both are real and that the payload is honest about WHAT the
//! old version is — `git show HEAD:<file>`, the last commit, not "the file as it was before this turn". Git
//! cannot give us the latter for an uncommitted working tree, and pretending it can would put a diff in front of
//! the operator that quietly folds in every other turn since the last commit. The note says so; the UI repeats it.
use serde_json::{json, Value};

/// 400 KB a side. Past that a browser is not going to render a useful side-by-side anyway, and the point is to
/// read a change, not to ship a file.
pub const CAP: usize = 400_000;

fn git(args: &[&str], cwd: &str) -> Option<String> {
    let out = crate::proc::cmd("git").args(args).current_dir(cwd).output().ok()?;
    if !out.status.success() { return None }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Trimmed to CAP, with a marker so the UI never shows a silent truncation as the whole file. Pure.
pub fn cap(text: String) -> (String, bool) {
    if text.len() <= CAP { return (text, false) }
    let mut cut = CAP;
    while cut > 0 && !text.is_char_boundary(cut) { cut -= 1; }
    (text[..cut].to_string(), true)
}

/// One file, both sides. `old` is empty and `added` true for a file that is not in HEAD at all — a created file
/// has no old version, which is different from having an empty one.
pub fn file_pair(cwd: &str, path: &str) -> Value {
    let head = git(&["show", &format!("HEAD:{path}")], cwd);
    let added = head.is_none();
    let (old, old_cut) = cap(head.unwrap_or_default());
    let disk = std::fs::read_to_string(std::path::Path::new(cwd).join(path)).ok();
    let deleted = disk.is_none() && !added;
    let (new, new_cut) = cap(disk.unwrap_or_default());
    json!({ "path": path, "old": old, "new": new, "added": added, "deleted": deleted,
            "same": !added && !deleted && old == new, "truncated": old_cut || new_cut })
}

/// The files a story says were written, paired. Order kept, duplicates dropped, path traversal refused — the
/// list arrives from a run's own record, but a handler that reads any path it is handed is a handler that reads
/// any path SOMETHING is handed, and the box is not the only thing on this disk.
pub fn pairs(cwd: &str, paths: &[String]) -> Value {
    let mut seen: Vec<&str> = vec![];
    let mut out = vec![];
    for p in paths {
        let p = p.trim();
        if p.is_empty() || p.starts_with('/') || p.split('/').any(|seg| seg == "..") { continue }
        if seen.contains(&p) { continue }
        seen.push(p);
        out.push(file_pair(cwd, p));
        if out.len() >= 40 { break }   // a run that touched more than forty files is not read side by side
    }
    json!({ "files": out, "note": "the old side is the last commit (`git show HEAD:<file>`) — not a snapshot from before this turn, which git cannot give for an uncommitted tree. Anything else changed since that commit is in here too." })
}

#[cfg(test)]
mod tests {
    use super::*;

    // a name per CALL, not per millisecond: these tests run in parallel threads and two of them landing in the
    // same millisecond had them sharing a directory — one removed the other's box mid-assertion
    static BOXES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn box_with(files: &[(&str, &str)], commit: bool) -> std::path::PathBuf {
        let nth = BOXES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("antiloki-diff-{}-{nth}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src")).unwrap();
        let run = |a: &[&str]| { let _ = crate::proc::cmd("git").args(a).current_dir(&d).output(); };
        run(&["init", "-q"]); run(&["config", "user.email", "t@t"]); run(&["config", "user.name", "t"]);
        for (p, c) in files { std::fs::write(d.join(p), c).unwrap(); }
        if commit { run(&["add", "-A"]); run(&["commit", "-qm", "base"]); }
        d
    }

    #[test]
    fn both_sides_are_real_and_a_created_file_has_no_old_side() {
        let d = box_with(&[("src/a.ts", "export const a = 1;\n")], true);
        let root = d.to_string_lossy().to_string();
        std::fs::write(d.join("src/a.ts"), "export const a = 2;\n").unwrap();     // edited
        std::fs::write(d.join("src/new.ts"), "export const b = 3;\n").unwrap();   // created, untracked

        let v = pairs(&root, &["src/a.ts".into(), "src/new.ts".into(), "src/a.ts".into()]);
        let f = v["files"].as_array().unwrap();
        assert_eq!(f.len(), 2, "the same file twice is one file");
        assert_eq!(f[0]["old"], json!("export const a = 1;\n"));
        assert_eq!(f[0]["new"], json!("export const a = 2;\n"));
        assert_eq!(f[0]["added"], json!(false));
        assert_eq!(f[0]["same"], json!(false));
        // a created file has NO old version — which is not the same as an empty one, and the UI shows it differently
        assert_eq!(f[1]["added"], json!(true));
        assert_eq!(f[1]["old"], json!(""));
        assert_eq!(f[1]["new"], json!("export const b = 3;\n"));
        // and the payload says what "old" means, so the UI never implies a per-turn snapshot git cannot give
        assert!(v["note"].as_str().unwrap().contains("not a snapshot from before this turn"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_deleted_file_and_an_unchanged_one_are_told_apart() {
        let d = box_with(&[("src/gone.ts", "bye\n"), ("src/same.ts", "same\n")], true);
        let root = d.to_string_lossy().to_string();
        std::fs::remove_file(d.join("src/gone.ts")).unwrap();
        let f = pairs(&root, &["src/gone.ts".into(), "src/same.ts".into()])["files"].clone();
        assert_eq!(f[0]["deleted"], json!(true));
        assert_eq!(f[0]["old"], json!("bye\n"));
        assert_eq!(f[0]["new"], json!(""));
        assert_eq!(f[1]["same"], json!(true), "a file the run listed but did not really change says so");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_path_that_tries_to_leave_the_box_is_not_read() {
        let d = box_with(&[("src/a.ts", "a\n")], true);
        let root = d.to_string_lossy().to_string();
        let f = pairs(&root, &["../../../etc/passwd".into(), "/etc/passwd".into(), "  ".into(), "src/a.ts".into()])["files"].clone();
        assert_eq!(f.as_array().unwrap().len(), 1);
        assert_eq!(f[0]["path"], json!("src/a.ts"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_huge_file_is_cut_and_says_it_was() {
        let big = "x".repeat(CAP + 10);
        let (t, cut) = cap(big);
        assert!(cut);
        assert_eq!(t.len(), CAP);
        assert_eq!(cap("small".into()), ("small".to_string(), false));
    }
}
