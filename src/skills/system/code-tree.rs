//! system/code-tree.rs — twin of skills/system/code-tree.mjs. BESPOKE-NATIVE, not conform-db
//! fixtured (see the crossing playbook's VET step + `external/file-list.rs` precedent, which
//! excludes an entire module — including its pure helper export — once its DEFAULT export
//! unconditionally shells out / touches the real filesystem). NAMES ARE CONTRACT: `codeTree`
//! (default), `resolveBrowseRoot` (named, db-first helper).
//!
//! `codeTree` UNCONDITIONALLY spawns `git -C <root> ls-files` on every call (there is no input
//! flag that skips it, unlike `constitution-estimate.mjs`'s `input.base`-gated git-diff branch) —
//! real subprocess, no injection seam. When git fails (root isn't a git repo, or `git` is
//! missing) it falls back to a real recursive filesystem walk (`walkFs`). Both branches read
//! REAL disk/process state the conform-db fixture format cannot seed (it only seeds `context.db`
//! rows), so neither lane is comparable JS-vs-Rust byte-for-byte. This twin ports BOTH branches
//! for real (`crate::proc::cmd("git")` / `std::fs::read_dir`) and proves them via
//! `#[cfg(test)]` native tests using real temp dirs (one of which is a real `git init`'d repo —
//! same temp-dir-injection pattern as `file-list.rs`/`god-driver-halt.rs`), NOT a conform-db
//! fixture. `resolveBrowseRoot` itself is pure db logic (no fs/subprocess) but is excluded from
//! the fixture/frontier for the same reason `file-list.rs`'s pure `validatePath` is: it is not
//! independently dispatchable — every real caller reaches it only through `codeTree`, so grading
//! it alone would prove nothing the native tests below don't already cover.
//!
//! Tree-order gotcha: `ensureDir(dir).children.push({...})` — DIRECTORY nodes carry a `children`
//! array key, FILE nodes do NOT (JS never sets it, so `JSON.stringify` omits it). This twin
//! mirrors that: `node_to_value` inserts `"children"` only for `is_dir` nodes.
//!
//! Sort: `node.children.sort((a,b) => a.type!==b.type ? (a.type==='dir'?-1:1) : a.name.localeCompare(b.name))`.
//! `localeCompare` (no options) is a full ICU collation with no Rust-stdlib equivalent (same
//! caveat as `file-list.rs`, which approximates with `sensitivity:'base'`). Here the default
//! (no `sensitivity` option) additionally has a real tie-break for case-only differences —
//! confirmed against real Node: `['Banana','banana'].sort(...)` AND `['banana','Banana'].sort(...)`
//! both resolve to `['banana','Banana']` regardless of input order (lowercase wins ties), so a
//! naive `to_lowercase()`-stable-sort (which would preserve INPUT order on a tie) is not
//! sufficient. `locale_compare_approx` below: primary key = lowercase compare; on a tie, break by
//! reversed raw byte compare (lowercase byte values are numerically larger, so reversing puts
//! lowercase first) — verified against the same real-Node ground truth. Approximation only exact
//! for ASCII-case-only differences; acceptable because this unit has no JS-vs-Rust byte-parity
//! check (bespoke-native, native tests only), same disclosure as `file-list.rs`.
// QUALITY: bespoke-native (real git subprocess + real fs walk, no conform-db fixture — see module doc)

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::Path;

fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

// Noise the non-git fallback walk skips (git ls-files gets this for free from .gitignore).
const SKIP_DIRS: [&str; 8] = ["node_modules", ".git", "dist", "build", "out", "coverage", ".next", ".cache"];

fn skip_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".sqlite") || lower.ends_with(".sqlite-wal") || lower.ends_with(".sqlite-shm")
}

/// `export function resolveBrowseRoot(db, inputRoot, skill)` — a registered session worktree, the
/// active workspace root, or a thrown NOT_FOUND. See module doc for why this is real-but-untested
/// (no conform-db fixture).
pub fn resolveBrowseRoot(db: &Connection, input_root: &Value, skill: &Value) -> Result<Value, String> {
    let req = str_qq(Some(input_root)).trim().to_string();
    if req.is_empty() {
        return Ok(crate::engine_workspace_root::getWorkspaceRoot(db));
    }
    let rows = crate::db_json::query_rows(
        db,
        "SELECT 1 AS ok FROM cockpit_panes WHERE worktree_path = ? LIMIT 1",
        &[json!(req)],
    )?;
    let hit = rows.as_array().map_or(false, |a| !a.is_empty());
    if !hit {
        let skill_s = str_qq(Some(skill));
        return Err(format!("{skill_s}: registered session worktree '{req}' not found"));
    }
    Ok(json!(req))
}

/// `spawnSync('git', ['-C', root, 'ls-files'], {...})` — real subprocess, no injection seam. `None`
/// mirrors JS's `res.status !== 0 || res.error` (git missing, root not a repo, non-zero exit).
fn git_ls_files(root: &str) -> Option<Vec<String>> {
    let output = crate::proc::cmd("git").args(["-C", root, "ls-files"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Some(stdout.split('\n').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
}

/// `function walkFs(root)` — real recursive fs walk, POSIX-style rel paths (`/`-joined
/// regardless of host separator, matching JS's literal `${rel}/${e.name}` template). A dir that
/// fails to read (permission, race) is silently skipped, matching JS's `try {...} catch { return; }`.
fn walk_fs(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, rel: &str, out: &mut Vec<String>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                if !SKIP_DIRS.contains(&name.as_str()) {
                    walk(&dir.join(&name), &r, out);
                }
            } else if !skip_file(&name) {
                out.push(r);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, "", &mut out);
    out
}

struct TreeNode {
    name: String,
    path: String,
    is_dir: bool,
    children: Vec<usize>,
}

fn ensure_dir(rel_dir: &str, arena: &mut Vec<TreeNode>, dir_index: &mut HashMap<String, usize>) -> usize {
    if let Some(&idx) = dir_index.get(rel_dir) {
        return idx;
    }
    let (parent_path, name) = match rel_dir.rfind('/') {
        None => (String::new(), rel_dir.to_string()),
        Some(pos) => (rel_dir[..pos].to_string(), rel_dir[pos + 1..].to_string()),
    };
    let parent_idx = ensure_dir(&parent_path, arena, dir_index);
    let node_idx = arena.len();
    arena.push(TreeNode { name, path: rel_dir.to_string(), is_dir: true, children: Vec::new() });
    arena[parent_idx].children.push(node_idx);
    dir_index.insert(rel_dir.to_string(), node_idx);
    node_idx
}

/// Builds the `{name,path,type,children}` arena from a flat SORTED path list — mirrors
/// `ensureDir` + the file-push loop. Index 0 is always the synthetic root (`{name:'',path:''}`).
fn build_tree(paths: &[String]) -> Vec<TreeNode> {
    let mut arena: Vec<TreeNode> = vec![TreeNode { name: String::new(), path: String::new(), is_dir: true, children: Vec::new() }];
    let mut dir_index: HashMap<String, usize> = HashMap::new();
    dir_index.insert(String::new(), 0);
    for p in paths {
        let (dir_path, name) = match p.rfind('/') {
            None => (String::new(), p.clone()),
            Some(pos) => (p[..pos].to_string(), p[pos + 1..].to_string()),
        };
        let dir_idx = ensure_dir(&dir_path, &mut arena, &mut dir_index);
        let file_idx = arena.len();
        arena.push(TreeNode { name, path: p.clone(), is_dir: false, children: Vec::new() });
        arena[dir_idx].children.push(file_idx);
    }
    arena
}

/// Approximates JS default `String.prototype.localeCompare` (no options) — see module doc.
fn locale_compare_approx(a: &str, b: &str) -> std::cmp::Ordering {
    let la = a.to_lowercase();
    let lb = b.to_lowercase();
    match la.cmp(&lb) {
        std::cmp::Ordering::Equal => a.cmp(b).reverse(),
        other => other,
    }
}

// `node.children.sort((a,b) => a.type!==b.type ? (a.type==='dir'?-1:1) : a.name.localeCompare(b.name))`
// applied recursively (`for (const c of node.children) if (c.type === 'dir') sortNode(c)`).
fn sort_all(idx: usize, arena: &mut Vec<TreeNode>) {
    let mut items: Vec<(usize, String, bool)> =
        arena[idx].children.iter().map(|&c| (c, arena[c].name.clone(), arena[c].is_dir)).collect();
    items.sort_by(|a, b| {
        if a.2 != b.2 {
            if a.2 { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater }
        } else {
            locale_compare_approx(&a.1, &b.1)
        }
    });
    arena[idx].children = items.iter().map(|x| x.0).collect();
    let child_idxs: Vec<usize> = arena[idx].children.clone();
    for c in child_idxs {
        if arena[c].is_dir {
            sort_all(c, arena);
        }
    }
}

// File nodes never carry a `children` key (JS never sets it on `{ name, path, type: 'file' }`).
fn node_to_value(idx: usize, arena: &[TreeNode]) -> Value {
    let n = &arena[idx];
    let mut m = Map::new();
    m.insert("name".into(), json!(n.name));
    m.insert("path".into(), json!(n.path));
    m.insert("type".into(), json!(if n.is_dir { "dir" } else { "file" }));
    if n.is_dir {
        let children: Vec<Value> = n.children.iter().map(|&c| node_to_value(c, arena)).collect();
        m.insert("children".into(), Value::Array(children));
    }
    Value::Object(m)
}

/// `export default function codeTree(input, context)` — real-but-untested-by-fixture (see module
/// doc): unconditional `git ls-files` subprocess with a real-fs-walk fallback.
pub fn codeTree(input: &Value, db: &Connection) -> Result<Value, String> {
    let empty = json!({});
    let input_obj = if input.is_object() { input } else { &empty };
    let input_root = input_obj.get("root").cloned().unwrap_or(Value::Null);

    let root_v = resolveBrowseRoot(db, &input_root, &json!("code-tree"))?;
    let root = root_v.as_str().unwrap_or("").to_string();

    let mut paths: Vec<String> = git_ls_files(&root).unwrap_or_else(|| walk_fs(Path::new(&root)));
    paths.sort();

    let mut arena = build_tree(&paths);
    sort_all(0, &mut arena);
    let tree = node_to_value(0, &arena);

    Ok(json!({ "root": root, "count": paths.len() as i64, "tree": tree }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-code-tree-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db_with_cockpit_panes() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cockpit_panes (id INTEGER PRIMARY KEY AUTOINCREMENT, worktree_path TEXT); \
             CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')",
        )
        .unwrap();
        c
    }

    #[test]
    fn resolve_browse_root_empty_falls_back_to_workspace_root() {
        let c = db_with_cockpit_panes();
        let r = resolveBrowseRoot(&c, &Value::Null, &json!("code-tree")).unwrap();
        // no active workspace row → falls to SUBSTRATE_ROOT, whatever that resolves to here.
        assert_eq!(r, crate::engine_workspace_root::getWorkspaceRoot(&c));
    }

    #[test]
    fn resolve_browse_root_returns_registered_worktree_path() {
        let c = db_with_cockpit_panes();
        c.execute("INSERT INTO cockpit_panes (worktree_path) VALUES ('C:/tmp/some-worktree')", []).unwrap();
        let r = resolveBrowseRoot(&c, &json!("C:/tmp/some-worktree"), &json!("code-tree")).unwrap();
        assert_eq!(r, json!("C:/tmp/some-worktree"));
    }

    #[test]
    fn resolve_browse_root_unregistered_path_is_not_found() {
        let c = db_with_cockpit_panes();
        let e = resolveBrowseRoot(&c, &json!("C:/not/registered"), &json!("code-tree")).unwrap_err();
        assert_eq!(e, "code-tree: registered session worktree 'C:/not/registered' not found");
    }

    #[test]
    fn git_ls_files_real_subprocess_lists_tracked_files() {
        let dir = temp_dir("git");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main(){}").unwrap();
        fs::write(dir.join("README.md"), "# hi").unwrap();
        // git tracks whatever is added regardless of extension — the SKIP_FILE regex only
        // applies on the walkFs fallback, matching the JS module doc's own claim.
        fs::write(dir.join("app.sqlite"), "binary").unwrap();
        assert!(crate::proc::cmd("git").args(["init", "-q"]).current_dir(&dir).status().unwrap().success());
        assert!(crate::proc::cmd("git").args(["add", "-A"]).current_dir(&dir).status().unwrap().success());

        let root = dir.to_string_lossy().into_owned();
        let mut paths = git_ls_files(&root).expect("git ls-files should succeed inside a real repo");
        paths.sort();
        assert_eq!(paths, vec!["README.md".to_string(), "app.sqlite".to_string(), "src/main.rs".to_string()]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_ls_files_returns_none_for_non_git_directory() {
        let dir = temp_dir("nogit");
        let root = dir.to_string_lossy().into_owned();
        assert!(git_ls_files(&root).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_fs_skips_noise_dirs_and_sqlite_files_recursively() {
        let dir = temp_dir("walkfs");
        fs::create_dir_all(dir.join("src/nested")).unwrap();
        fs::create_dir_all(dir.join("node_modules/pkg")).unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join("src/nested/a.ts"), "x").unwrap();
        fs::write(dir.join("src/b.ts"), "x").unwrap();
        fs::write(dir.join("node_modules/pkg/index.js"), "x").unwrap();
        fs::write(dir.join("state.sqlite"), "x").unwrap();
        fs::write(dir.join("state.sqlite-wal"), "x").unwrap();
        fs::write(dir.join("README.md"), "x").unwrap();

        let mut out = walk_fs(&dir);
        out.sort();
        assert_eq!(out, vec!["README.md".to_string(), "src/b.ts".to_string(), "src/nested/a.ts".to_string()]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_tree_nests_dirs_before_files_case_insensitive_sorted() {
        let mut paths = vec![
            "b.txt".to_string(),
            "A.txt".to_string(),
            "src/index.ts".to_string(),
            "src/Apple.ts".to_string(),
            "src/apple2.ts".to_string(),
            "docs/readme.md".to_string(),
        ];
        paths.sort();
        let mut arena = build_tree(&paths);
        sort_all(0, &mut arena);
        let tree = node_to_value(0, &arena);

        let top_names: Vec<String> =
            tree["children"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(top_names, vec!["docs", "src", "A.txt", "b.txt"]);

        let src = tree["children"].as_array().unwrap().iter().find(|c| c["name"] == "src").unwrap();
        let src_names: Vec<String> =
            src["children"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(src_names, vec!["Apple.ts", "apple2.ts", "index.ts"]);

        // file nodes never carry a "children" key.
        let a_file = tree["children"].as_array().unwrap().iter().find(|c| c["name"] == "A.txt").unwrap();
        assert!(a_file.as_object().unwrap().get("children").is_none());
        let src_index = src["children"].as_array().unwrap().iter().find(|c| c["name"] == "index.ts").unwrap();
        assert_eq!(src_index["type"], json!("file"));
        assert_eq!(src_index["path"], json!("src/index.ts"));
        assert!(src_index.as_object().unwrap().get("children").is_none());
    }

    #[test]
    fn code_tree_end_to_end_via_registered_worktree_walkfs_fallback() {
        let dir = temp_dir("e2e");
        fs::create_dir_all(dir.join("lib")).unwrap();
        fs::write(dir.join("lib/util.js"), "x").unwrap();
        fs::write(dir.join("main.js"), "x").unwrap();
        // not a git repo → exercises the walkFs fallback branch end-to-end.

        let c = db_with_cockpit_panes();
        let root = dir.to_string_lossy().into_owned();
        c.execute("INSERT INTO cockpit_panes (worktree_path) VALUES (?1)", rusqlite::params![root]).unwrap();

        let r = codeTree(&json!({ "root": root }), &c).unwrap();
        assert_eq!(r["count"], json!(2));
        let top_names: Vec<String> =
            r["tree"]["children"].as_array().unwrap().iter().map(|n| n["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(top_names, vec!["lib", "main.js"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn code_tree_unregistered_root_propagates_not_found_error() {
        let c = db_with_cockpit_panes();
        let e = codeTree(&json!({ "root": "C:/nope" }), &c).unwrap_err();
        assert_eq!(e, "code-tree: registered session worktree 'C:/nope' not found");
    }
}
