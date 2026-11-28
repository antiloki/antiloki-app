//! THE ONE SOURCE OF TRUTH FOR A PROJECT'S DOCUMENTS (the operator 2026-11-21: "sqlite should be the single source of truth for teams as well, for all YAMLs; YAML is a self-generated rendering by a deterministic pure Rust function").
//! A project's templates, branches and book packs are rows of `.antiloki/antiloki.db` (`docs(kind, id, json, t_ms)`); the YAML anyone reads or shares is `team_yaml::to_yaml` of the row (one pure function, sorted keys, stable bytes).
//! A file that exists from before (`.antiloki/templates/x.yaml`, `.antiloki/book/p.json`) is IMPORTED once, by id, the first time it is looked for; after that the row wins and the file is left alone as a backup.
use rusqlite::{params, Connection};
use serde_json::Value;
use std::path::Path;

fn open(root: &str) -> Result<Connection, String> {
    let d = Path::new(root).join(".antiloki"); std::fs::create_dir_all(&d).map_err(|e| e.to_string())?; let c = Connection::open(d.join("antiloki.db")).map_err(|e| e.to_string())?; c.busy_timeout(std::time::Duration::from_secs(3)).map_err(|e| e.to_string())?;
    c.execute_batch("CREATE TABLE IF NOT EXISTS docs(kind TEXT NOT NULL, id TEXT NOT NULL, json TEXT NOT NULL, t_ms INTEGER NOT NULL, PRIMARY KEY(kind, id))").map_err(|e| e.to_string())?; Ok(c)
}
fn now_ms() -> i64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0) }
fn ok_id(id: &str) -> bool { !id.is_empty() && id.len() <= 80 && !id.starts_with('.') && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') }
/// Store a document given as YAML or JSON text (JSON is YAML). The strict reader refuses anchors, tags and duplicate keys. Blocking.
pub fn put_text(root: &str, kind: &str, id: &str, text: &str) -> Result<(), String> { let v = crate::team_yaml::parse_strict(text)?; put(root, kind, id, &v) }
pub fn put(root: &str, kind: &str, id: &str, v: &Value) -> Result<(), String> {
    if !ok_id(id) { return Err(format!("`{id}` is not a document id")); } let c = open(root)?;
    c.execute("INSERT INTO docs(kind, id, json, t_ms) VALUES(?1,?2,?3,?4) ON CONFLICT(kind, id) DO UPDATE SET json = ?3, t_ms = ?4", params![kind, id, v.to_string(), now_ms()]).map_err(|e| e.to_string())?; Ok(())
}
pub fn get(root: &str, kind: &str, id: &str) -> Option<Value> { let c = open(root).ok()?; let t: String = c.query_row("SELECT json FROM docs WHERE kind=?1 AND id=?2", params![kind, id], |r| r.get(0)).ok()?; serde_json::from_str(&t).ok() }
/// The document as YAML: the one pure rendering. Same row, same bytes.
pub fn render(v: &Value) -> String { crate::team_yaml::to_yaml(v) }
pub fn get_text(root: &str, kind: &str, id: &str) -> Option<String> { get(root, kind, id).map(|v| render(&v)) }
pub fn ids(root: &str, kind: &str) -> Vec<String> { let Ok(c) = open(root) else { return vec![] }; let Ok(mut st) = c.prepare("SELECT id FROM docs WHERE kind=?1 ORDER BY id") else { return vec![] }; st.query_map(params![kind], |r| r.get::<_, String>(0)).map(|it| it.flatten().collect()).unwrap_or_default() }
pub fn delete(root: &str, kind: &str, id: &str) -> bool { open(root).ok().and_then(|c| c.execute("DELETE FROM docs WHERE kind=?1 AND id=?2", params![kind, id]).ok()).map_or(false, |n| n > 0) }
/// Bring in the files of a folder that are not rows yet (by file stem). Files that do not parse are returned as problems and are not stored. Never deletes or changes a file. Blocking.
pub fn import_dir(root: &str, kind: &str, dir: &Path, exts: &[&str]) -> Vec<String> {
    let mut problems = vec![]; let have = ids(root, kind); let Ok(rd) = std::fs::read_dir(dir) else { return problems }; let mut files: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().and_then(|e| e.to_str()).map_or(false, |e| exts.contains(&e))).collect(); files.sort();
    for p in files { let Some(id) = p.file_stem().and_then(|s| s.to_str()).map(String::from) else { continue }; if have.contains(&id) { continue; }
        match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|t| put_text(root, kind, &id, &t)) { Ok(()) => {} Err(e) => problems.push(format!("{}: {e}", p.file_name().and_then(|n| n.to_str()).unwrap_or(""))) } }
    problems
}

#[cfg(test)]
mod tests {
    use super::*; use serde_json::json;
    fn tmp(n: &str) -> String { let d = std::env::temp_dir().join(format!("antiloki-docstore-{n}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d.to_string_lossy().to_string() }
    #[test] fn a_document_is_a_row_and_its_yaml_is_a_pure_rendering_that_reads_back_exactly() {
        let r = tmp("a"); let v = json!({ "name": "launch", "nodes": { "b": { "kind": "check" }, "a": { "kind": "role" } }, "notes": "two\nlines" }); put(&r, "template", "launch", &v).unwrap(); let y = get_text(&r, "template", "launch").unwrap(); assert_eq!(y, render(&v), "same row, same bytes"); assert!(y.contains("nodes:") && y.starts_with("name:"), "sorted keys: {y}");
        assert_eq!(crate::team_yaml::parse_strict(&y).unwrap(), v, "the YAML reads back to the row"); assert_eq!(ids(&r, "template"), vec!["launch"]); assert!(get(&r, "pack", "launch").is_none(), "kinds are separate"); assert!(delete(&r, "template", "launch") && ids(&r, "template").is_empty());
        assert!(put(&r, "template", "../x", &v).is_err() && put_text(&r, "template", "x", "a: &x 1\nb: *x\n").is_err(), "bad ids and dangerous YAML are refused"); let _ = std::fs::remove_dir_all(&r);
    }
    #[test] fn old_files_are_imported_once_and_then_the_row_wins() {
        let r = tmp("i"); let d = Path::new(&r).join(".antiloki").join("templates"); std::fs::create_dir_all(&d).unwrap(); std::fs::write(d.join("one.yaml"), "name: one\nn: 1\n").unwrap(); std::fs::write(d.join("bad.yaml"), "a: [1, 2\n").unwrap(); std::fs::write(d.join("note.txt"), "x").unwrap();
        let problems = import_dir(&r, "template", &d, &["yaml"]); assert_eq!(ids(&r, "template"), vec!["one"]); assert_eq!(problems.len(), 1); assert!(problems[0].starts_with("bad.yaml"));
        put(&r, "template", "one", &json!({ "name": "one", "n": 2 })).unwrap(); std::fs::write(d.join("one.yaml"), "name: one\nn: 99\n").unwrap(); import_dir(&r, "template", &d, &["yaml"]); assert_eq!(get(&r, "template", "one").unwrap()["n"], json!(2), "the row wins over the file"); assert!(d.join("one.yaml").is_file(), "the file is left alone"); let _ = std::fs::remove_dir_all(&r);
    }
}
