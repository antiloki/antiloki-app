//! code-authors.rs — twin of skills/codebase/code-authors.mjs (db lane). NAMES ARE CONTRACT:
//! `codeAuthors` (default). The AUTHORS layer: parses `git log --name-only` into the
//! author↔file graph, per-author stats (commits/files/tenure/solo-ownership = bus-factor), and
//! the author×author co-dependency matrix.
//!
//! GIT SUBPROCESS BRANCH — DISCLOSED UNTESTED (playbook lesson 5 / constitution-estimate.rs
//! precedent, `git_diff_paths`): the JS reaches `spawnSync('git', ...)` ONLY when `input.__log`
//! is absent/null — the JS module's OWN documented test seam (`let log = input?.__log; // test
//! seam: feed a raw git-log string to exercise the parse without git`). Every fixture case here
//! supplies `__log`, so `git_log` below is real, correct code (mirrors the JS `git()` helper's
//! exact spawnSync arg list, including the `-C <root>` form) but is never exercised by conform-db.
//! Likewise `getWorkspaceRoot` (already-converted `engine_workspace_root` twin) is only called on
//! that same git-required path, so it too is real but untested by this unit's fixture.
//!
//! DATE-FIELD EDGE CASE — DISCLOSED UNTESTED: `head.split(US)` destructures `[sha,name,email,date]`;
//! `date` (and, in principle, `name`/`email`) can be `undefined` if a log line has fewer than 4
//! `US`-separated fields. `first`/`last` are then `undefined` too and JSON.stringify OMITS the key
//! (gotcha 3) — ported below via `Option<String>` + conditional `Map` inserts. Real git
//! `--format=...%H\x1f%an\x1f%ae\x1f%ad` output always emits exactly 4 fields (possibly empty
//! strings, never fewer), so this shape is not reachable from a real git invocation; it is only
//! reachable from a hand-crafted `__log` seam value, which no fixture case here constructs.
// QUALITY: real git subprocess for the untested branch (disclosed), deterministic log-parse
// proven via `__log` seam fixtures — mirrors code-git-info.rs's bespoke-native git() port style.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::OnceLock;

const US: char = '\u{1f}';
const RS: char = '\u{1e}';

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

// = JS Number.prototype.toString()/`${f}` for a raw f64 template value (playbook helper). Used
// for `String(limit)` inside the disclosed-untested git-log branch (`limit` is never rounded by
// the JS source, so it can be fractional — `Number.prototype.toString`, not integer formatting).
fn js_number_to_string(f: f64) -> String {
    if f.is_nan() { return "NaN".to_string(); }
    if f == 0.0 { return "0".to_string(); }
    let neg = f.is_sign_negative();
    let abs = f.abs();
    if abs.is_infinite() { return if neg { "-Infinity".into() } else { "Infinity".into() }; }
    let sci = format!("{:e}", abs);
    let (mantissa, exp_str) = sci.split_once('e').unwrap();
    let exp: i32 = exp_str.parse().unwrap();
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let body = if (-6..=20).contains(&exp) {
        if exp >= 0 {
            if exp + 1 >= k { format!("{digits}{}", "0".repeat((exp + 1 - k) as usize)) }
            else { let (i, f) = digits.split_at((exp + 1) as usize); format!("{i}.{f}") }
        } else { format!("0.{}{digits}", "0".repeat((-exp - 1) as usize)) }
    } else {
        let m = if k == 1 { digits.clone() } else { format!("{}.{}", &digits[..1], &digits[1..]) };
        format!("{m}e{}{}", if exp >= 0 { "+" } else { "-" }, exp.abs())
    };
    if neg { format!("-{body}") } else { body }
}

// `Math.max`/`Math.min`: NaN-poisoning (any NaN operand → NaN), unlike Rust's f64::max/min which
// return the non-NaN operand. `Math.min(Math.max(x,lo),hi)` — the JS source's clamp idiom.
fn js_math_max(a: f64, b: f64) -> f64 { if a.is_nan() || b.is_nan() { f64::NAN } else if a > b { a } else { b } }
fn js_math_min(a: f64, b: f64) -> f64 { if a.is_nan() || b.is_nan() { f64::NAN } else if a < b { a } else { b } }
fn clamp(v: f64, lo: f64, hi: f64) -> f64 { js_math_min(js_math_max(v, lo), hi) }

// `Array.prototype.slice(0, n)`'s ToIntegerOrInfinity(n) for the always-non-negative-or-NaN `n`
// values this module clamps to (`topN`/`fpa`): NaN → 0, else truncate toward zero (== floor, since
// never negative once clamped).
fn to_len(f: f64) -> usize { if f.is_nan() || f <= 0.0 { 0 } else { f.trunc() as usize } }

fn is_code_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"(?i)\.(m?[jt]sx?|py|go|rs|java|rb|c|h|cc|cpp|cs|php|swift|kt|scala|sql|sh|vue|svelte)$").unwrap())
}
fn is_code(p: &str) -> bool { is_code_re().is_match(p) }

/// `function git(root, args)` — real `git` subprocess (spawnSync mirrored via `std::process::Command`).
/// See module doc: reachable only when `input.__log` is absent; disclosed untested by fixture.
fn git_log(root: &str, n_arg: &str) -> Option<String> {
    let fmt = format!("--format={RS}%H{US}%an{US}%ae{US}%ad");
    let out = crate::proc::cmd("git")
        .arg("-C").arg(root)
        .arg("log").arg("--no-merges").arg("-n").arg(n_arg)
        .arg(&fmt)
        .arg("--date=short").arg("--name-only")
        .output();
    match out {
        Ok(o) if o.status.success() => Some(String::from_utf8_lossy(&o.stdout).into_owned()),
        _ => None,
    }
}

// Insertion-order-preserving `Map<string, number>` (mirrors JS `Map` iteration order, which
// Rust's `HashMap` does not preserve — gotcha 1's "object key order" concern applies equally to
// Map-derived arrays like `aFiles.get(id).entries()`).
#[derive(Default)]
struct OrderedCounts {
    order: Vec<String>,
    counts: HashMap<String, i64>,
}
impl OrderedCounts {
    fn bump(&mut self, key: &str) {
        match self.counts.get_mut(key) {
            Some(c) => { *c += 1; }
            None => { self.counts.insert(key.to_string(), 1); self.order.push(key.to_string()); }
        }
    }
    fn entries(&self) -> Vec<(String, i64)> {
        self.order.iter().map(|k| (k.clone(), self.counts[k])).collect()
    }
}

struct Author {
    id: String,
    name: String,
    email: String,
    commits: i64,
    first: Option<String>,
    last: Option<String>,
}

struct AuthorOut {
    id: String,
    name: String,
    email: String,
    commits: i64,
    files: i64,
    first: Option<String>,
    last: Option<String>,
    solo: i64,
    files_list: Vec<String>,
}
fn author_out_to_json(a: &AuthorOut) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), json!(a.id));
    m.insert("name".into(), json!(a.name));
    m.insert("email".into(), json!(a.email));
    m.insert("commits".into(), json!(a.commits));
    m.insert("files".into(), json!(a.files));
    if let Some(s) = &a.first { m.insert("first".into(), json!(s)); } // undefined date → key omitted
    if let Some(s) = &a.last { m.insert("last".into(), json!(s)); }
    m.insert("solo".into(), json!(a.solo));
    m.insert("files_list".into(), json!(a.files_list));
    Value::Object(m)
}

fn early_return(root: Value, error: &str) -> Value {
    json!({
        "root": root, "commit_count": 0, "author_count": 0, "authors": [], "files": [], "edges": [],
        "codep": [], "bus_factor": Value::Null, "error": error,
    })
}

/// `export default function codeAuthors(input, context)`
pub fn codeAuthors(input: &Value, db: &Connection) -> Result<Value, String> {
    let limit_raw = nz(input.get("limit")).cloned().unwrap_or(json!(4000));
    let limit = clamp(js_number(Some(&limit_raw)), 50.0, 20000.0);
    let top_n_raw = nz(input.get("top_authors")).cloned().unwrap_or(json!(24));
    let top_n = clamp(js_number(Some(&top_n_raw)), 3.0, 60.0);
    let fpa_raw = nz(input.get("files_per_author")).cloned().unwrap_or(json!(8));
    let fpa = clamp(js_number(Some(&fpa_raw)), 2.0, 20.0);

    let log_seam = nz(input.get("__log"));
    let (root_val, log_str): (Value, String) = match log_seam {
        Some(v) => (Value::Null, v.as_str().unwrap_or("").to_string()),
        None => {
            let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
            let root_s = root_v.as_str().unwrap_or("").to_string();
            if root_s.is_empty() {
                return Ok(early_return(Value::Null, "no active workspace root"));
            }
            let n_arg = js_number_to_string(limit);
            match git_log(&root_s, &n_arg) {
                Some(text) => (json!(root_s), text),
                None => return Ok(early_return(json!(root_s), "not a git repo / git unavailable")),
            }
        }
    };

    let mut authors: Vec<Author> = Vec::new();
    let mut author_idx: HashMap<String, usize> = HashMap::new();
    let mut a_files: HashMap<String, OrderedCounts> = HashMap::new();
    let mut f_authors: HashMap<String, HashMap<String, i64>> = HashMap::new();
    let mut commit_count: i64 = 0;

    for block in log_str.split(RS) {
        let t = block.trim_start_matches('\n');
        if t.trim().is_empty() { continue; }
        let nl = t.find('\n');
        let head = match nl { Some(i) => &t[..i], None => t };
        let parts: Vec<&str> = head.split(US).collect();
        let sha = parts.first().copied();
        let name = parts.get(1).copied();
        let email = parts.get(2).copied();
        let date = parts.get(3).copied();
        if sha.map(|s| s.is_empty()).unwrap_or(true) { continue; }
        commit_count += 1;

        let id_source: &str = match (email, name) {
            (Some(e), _) if !e.is_empty() => e,
            (_, Some(n)) if !n.is_empty() => n,
            _ => "unknown",
        };
        let id = id_source.to_lowercase().trim().to_string();

        let idx = match author_idx.get(&id) {
            Some(&i) => i,
            None => {
                let name_val = name.filter(|s| !s.is_empty()).unwrap_or(id.as_str()).to_string();
                let email_val = email.filter(|s| !s.is_empty()).unwrap_or("").to_string();
                let date_val = date.map(|s| s.to_string());
                authors.push(Author { id: id.clone(), name: name_val, email: email_val, commits: 0, first: date_val.clone(), last: date_val });
                let i = authors.len() - 1;
                author_idx.insert(id.clone(), i);
                i
            }
        };
        {
            let a = &mut authors[idx];
            a.commits += 1;
            if let Some(n) = name.filter(|s| !s.is_empty()) { a.name = n.to_string(); }
            if let Some(d) = date.filter(|s| !s.is_empty()) {
                // computed as bools first (not `if let Some(x) = &a.first { ... a.first = ... }`)
                // so the immutable borrow of a.first/a.last doesn't overlap the mutable assignment.
                let update_first = a.first.as_deref().map(|f| d < f).unwrap_or(false);
                if update_first { a.first = Some(d.to_string()); }
                let update_last = a.last.as_deref().map(|l| d > l).unwrap_or(false);
                if update_last { a.last = Some(d.to_string()); }
            }
        }

        let nl = match nl { Some(i) => i, None => continue };
        let entry = a_files.entry(id.clone()).or_default();
        for f in t[nl + 1..].split('\n').map(|l| l.trim()).filter(|l| !l.is_empty()) {
            if !is_code(f) { continue; }
            entry.bump(f);
            *f_authors.entry(f.to_string()).or_default().entry(id.clone()).or_insert(0) += 1;
        }
    }

    // solo ownership (bus-factor): files whose only author is one person
    let mut solo: HashMap<String, i64> = HashMap::new();
    let mut solo_files: i64 = 0;
    for fa in f_authors.values() {
        if fa.len() == 1 {
            solo_files += 1;
            let only = fa.keys().next().unwrap().clone();
            *solo.entry(only).or_insert(0) += 1;
        }
    }

    let mut author_list: Vec<AuthorOut> = authors.iter().map(|a| {
        let oc = a_files.get(&a.id);
        let files_count = oc.map(|o| o.order.len()).unwrap_or(0) as i64;
        let mut entries = oc.map(|o| o.entries()).unwrap_or_default();
        entries.sort_by(|p, q| q.1.cmp(&p.1)); // stable desc by touches
        let files_list: Vec<String> = entries.into_iter().take(40).map(|(p, _)| p).collect();
        let solo_v = *solo.get(&a.id).unwrap_or(&0);
        AuthorOut {
            id: a.id.clone(), name: a.name.clone(), email: a.email.clone(), commits: a.commits,
            files: files_count, first: a.first.clone(), last: a.last.clone(), solo: solo_v, files_list,
        }
    }).collect();
    author_list.sort_by(|x, y| y.commits.cmp(&x.commits)); // stable desc by commits

    let top: Vec<&AuthorOut> = author_list.iter().take(to_len(top_n)).collect();

    // graph edges: each top author → their most-touched files; the union becomes the file nodes
    let mut edges: Vec<Value> = Vec::new();
    let mut file_node_order: Vec<String> = Vec::new();
    let mut file_nodes: HashMap<String, Value> = HashMap::new();
    for a in &top {
        let af = match a_files.get(&a.id) { Some(x) => x, None => continue };
        let mut entries = af.entries();
        entries.sort_by(|p, q| q.1.cmp(&p.1)); // stable desc by touches
        for (path, touches) in entries.into_iter().take(to_len(fpa)) {
            edges.push(json!({ "author": a.id, "path": path, "touches": touches }));
            if !file_nodes.contains_key(&path) {
                let fa = f_authors.get(&path);
                let authors_n = fa.map(|m| m.len()).unwrap_or(1) as i64;
                let touches_sum: i64 = fa.map(|m| m.values().sum()).unwrap_or(0);
                file_nodes.insert(path.clone(), json!({ "path": path, "authors": authors_n, "touches": touches_sum }));
                file_node_order.push(path);
            }
        }
    }

    // co-dependency: shared-file counts pairwise across the top authors
    let mut codep: Vec<Value> = Vec::new();
    for i in 0..top.len() {
        for j in (i + 1)..top.len() {
            let ai = a_files.get(&top[i].id);
            let aj = a_files.get(&top[j].id);
            let (ai, aj) = match (ai, aj) { (Some(x), Some(y)) => (x, y), _ => continue };
            let (small, big) = if ai.order.len() <= aj.order.len() { (ai, aj) } else { (aj, ai) };
            let shared: i64 = small.order.iter().filter(|f| big.counts.contains_key(*f)).count() as i64;
            if shared > 0 { codep.push(json!({ "a": top[i].id, "b": top[j].id, "shared": shared })); }
        }
    }
    codep.sort_by(|x, y| y["shared"].as_i64().unwrap().cmp(&x["shared"].as_i64().unwrap())); // stable desc

    let mut top_solo: Vec<Value> = top.iter()
        .filter(|a| *solo.get(&a.id).unwrap_or(&0) > 0)
        .map(|a| json!({ "id": a.id, "name": a.name, "solo": *solo.get(&a.id).unwrap_or(&0) }))
        .collect();
    top_solo.sort_by(|p, q| q["solo"].as_i64().unwrap().cmp(&p["solo"].as_i64().unwrap())); // stable desc
    top_solo.truncate(10);

    let mut out = Map::new();
    out.insert("root".into(), root_val);
    out.insert("commit_count".into(), json!(commit_count));
    out.insert("author_count".into(), json!(author_list.len() as i64));
    out.insert("authors".into(), json!(author_list.iter().take(200).map(author_out_to_json).collect::<Vec<_>>()));
    out.insert("files".into(), json!(file_node_order.iter().map(|p| file_nodes[p].clone()).collect::<Vec<_>>()));
    out.insert("edges".into(), json!(edges));
    out.insert("codep".into(), json!(codep.into_iter().take(80).collect::<Vec<_>>()));
    out.insert("bus_factor".into(), json!({
        "total_files": f_authors.len() as i64,
        "solo_files": solo_files,
        "top_solo": top_solo,
    }));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection { Connection::open_in_memory().unwrap() }

    fn commit(sha: &str, name: &str, email: &str, date: &str, files: &[&str]) -> String {
        format!("{RS}{sha}{US}{name}{US}{email}{US}{date}\n{}\n", files.join("\n"))
    }

    #[test]
    fn author_stats_edges_codep_and_bus_factor() {
        let log = commit("s1", "Alice", "alice@x.com", "2026-06-01", &["src/a.ts", "src/b.ts"])
            + &commit("s2", "Bob", "bob@x.com", "2026-06-02", &["src/b.ts", "src/c.ts"])
            + &commit("s3", "Alice", "alice@x.com", "2026-06-03", &["src/a.ts", "README.md"]);
        let r = codeAuthors(&json!({ "__log": log }), &db()).unwrap();
        assert_eq!(r["commit_count"], json!(3));
        assert_eq!(r["author_count"], json!(2));
        assert_eq!(r["authors"][0]["name"], json!("Alice"));
        assert_eq!(r["authors"][0]["commits"], json!(2));
        assert_eq!(r["authors"][0]["files"], json!(2)); // a.ts + b.ts (README not code)
        assert_eq!(r["authors"][0]["solo"], json!(1)); // a.ts is Alice-only
        assert_eq!(r["authors"][0]["first"], json!("2026-06-01"));
        assert_eq!(r["authors"][0]["last"], json!("2026-06-03"));
        let bob = r["authors"].as_array().unwrap().iter().find(|a| a["name"] == json!("Bob")).unwrap();
        assert_eq!(bob["solo"], json!(1)); // c.ts is Bob-only
        assert_eq!(r["codep"].as_array().unwrap().len(), 1);
        assert_eq!(r["codep"][0]["shared"], json!(1));
        assert_eq!(r["bus_factor"]["total_files"], json!(3));
        assert_eq!(r["bus_factor"]["solo_files"], json!(2));
        assert!(r["edges"].as_array().unwrap().iter().any(|e| e["author"] == json!("alice@x.com") && e["path"] == json!("src/a.ts")));
        assert_eq!(r["root"], Value::Null);
    }

    #[test]
    fn empty_log_is_safe() {
        let r = codeAuthors(&json!({ "__log": "" }), &db()).unwrap();
        assert_eq!(r["author_count"], json!(0));
        assert_eq!(r["authors"], json!([]));
        assert_eq!(r["codep"], json!([]));
        assert_eq!(r["files"], json!([]));
        assert_eq!(r["bus_factor"]["total_files"], json!(0));
    }

    #[test]
    fn top_authors_and_files_per_author_truncate_edges_and_files_list_does_not() {
        // Alice(3 commits, 3 files) > Bob(2, 1 file) > Carol(1) > Dave(1, excluded by top_authors:3).
        let log = commit("a1", "Alice", "alice@x.com", "2026-06-01", &["f1.ts", "f2.ts", "f3.ts"])
            + &commit("a2", "Alice", "alice@x.com", "2026-06-02", &["f1.ts", "f2.ts"])
            + &commit("a3", "Alice", "alice@x.com", "2026-06-03", &["f1.ts"])
            + &commit("b1", "Bob", "bob@x.com", "2026-06-04", &["g1.ts"])
            + &commit("b2", "Bob", "bob@x.com", "2026-06-05", &["g1.ts"])
            + &commit("c1", "Carol", "carol@x.com", "2026-06-06", &["h1.ts"])
            + &commit("d1", "Dave", "dave@x.com", "2026-06-07", &["i1.ts"]);
        let r = codeAuthors(&json!({ "__log": log, "top_authors": 3, "files_per_author": 2 }), &db()).unwrap();
        assert_eq!(r["author_count"], json!(4)); // authors[] itself is unaffected by top_authors
        // files_list (cap 40, independent of files_per_author) keeps all 3 of Alice's files
        assert_eq!(r["authors"][0]["files_list"], json!(["f1.ts", "f2.ts", "f3.ts"]));
        // edges (files_per_author:2) truncate Alice to her top-2 by touches: f1.ts(3), f2.ts(2) — f3.ts dropped
        let edges = r["edges"].as_array().unwrap();
        assert_eq!(edges.len(), 4); // Alice x2 + Bob x1 + Carol x1; Dave excluded (top_authors:3)
        assert_eq!(edges[0], json!({ "author": "alice@x.com", "path": "f1.ts", "touches": 3 }));
        assert_eq!(edges[1], json!({ "author": "alice@x.com", "path": "f2.ts", "touches": 2 }));
        assert!(!edges.iter().any(|e| e["path"] == json!("i1.ts"))); // Dave's file never reached
        assert_eq!(r["codep"], json!([])); // no shared files among the top-3
    }

    #[test]
    fn earlier_date_seen_later_in_the_log_updates_first_not_last() {
        // git log lists newest-first: t1 (2026-07-10) is processed before t2 (2026-07-01).
        let log = commit("t1", "Carol", "carol@x.com", "2026-07-10", &["src/x.ts"])
            + &commit("t2", "Carol", "carol@x.com", "2026-07-01", &["src/x.ts"]);
        let r = codeAuthors(&json!({ "__log": log }), &db()).unwrap();
        assert_eq!(r["authors"][0]["first"], json!("2026-07-01"));
        assert_eq!(r["authors"][0]["last"], json!("2026-07-10"));
    }

    #[test]
    fn block_with_empty_sha_is_skipped() {
        let log = format!("{RS}{US}Ghost{US}ghost@x.com{US}2026-06-01\nsrc/g.ts\n");
        let r = codeAuthors(&json!({ "__log": log }), &db()).unwrap();
        assert_eq!(r["commit_count"], json!(0));
        assert_eq!(r["author_count"], json!(0));
    }

    #[test]
    fn non_code_files_are_excluded_and_id_falls_back_to_name_when_email_is_absent() {
        let log = commit("s1", "NoMail", "", "2026-06-01", &["src/a.py", "notes.txt"]);
        let r = codeAuthors(&json!({ "__log": log }), &db()).unwrap();
        assert_eq!(r["authors"][0]["id"], json!("nomail")); // falls back to name, lowercased
        assert_eq!(r["authors"][0]["files"], json!(1)); // notes.txt is not code
    }
}
