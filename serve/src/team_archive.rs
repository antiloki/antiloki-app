//! THE ARCHIVE OF A RUN'S FILES (the operator 2026-11-15: "it is always deleting, make it impossible to delete, even when disapproved it should be archived on a sqlite db"): nothing a team makes is ever lost. A file is saved here BEFORE anything can remove it (a put-back of a step that went outside its scope,
//! the removal of a working folder once its work is merged, a merge that is not approved) and the media a step makes are saved as it makes them. One SQLite file in the workspace (`.antiloki/team/archive.sqlite`), the bytes in it, never deleted by antiloki.
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

const MAX_FILE: u64 = 250 * 1024 * 1024;
const MAX_PER_CALL: u64 = 600 * 1024 * 1024;
/// Who the files belong to.
#[derive(Clone, Debug)] pub struct Ctx { pub root: String, pub run: String, pub team: String, pub node: String }
fn db_path(root: &str) -> std::path::PathBuf { Path::new(root).join(".antiloki").join("team").join("archive.sqlite") }
fn open(root: &str) -> Result<Connection, String> {
    let p = db_path(root); if let Some(d) = p.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
    let c = Connection::open(p).map_err(|e| e.to_string())?; let _ = c.busy_timeout(std::time::Duration::from_secs(10)); let _ = c.pragma_update(None, "journal_mode", "WAL");
    c.execute_batch("CREATE TABLE IF NOT EXISTS files (id INTEGER PRIMARY KEY, run TEXT NOT NULL, team TEXT NOT NULL, node TEXT NOT NULL, path TEXT NOT NULL, kind TEXT NOT NULL, bytes INTEGER NOT NULL, sha TEXT NOT NULL, reason TEXT NOT NULL, t_ms INTEGER NOT NULL, data BLOB NOT NULL); CREATE INDEX IF NOT EXISTS files_run ON files (run);").map_err(|e| e.to_string())?; Ok(c)
}
fn now() -> i64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0) }
/// Save one file under its path in the folder. A file already saved with the same bytes for this run is not saved twice. Returns whether a new copy was made. Never fails the caller: a failure is `false`, and the caller goes on (but see `store_before_removal`). Blocking.
pub fn store_file(ctx: &Ctx, rel: &str, abs: &Path, reason: &str) -> bool {
    let Ok(meta) = std::fs::metadata(abs) else { return false }; if !meta.is_file() || meta.len() > MAX_FILE { return false; }
    let Ok(data) = std::fs::read(abs) else { return false }; let sha = hex::encode(Sha256::digest(&data)); let Ok(c) = open(&ctx.root) else { return false };
    if c.query_row("SELECT 1 FROM files WHERE run = ?1 AND path = ?2 AND sha = ?3", params![ctx.run, rel, sha], |_| Ok(())).is_ok() { return false; }
    let kind = crate::team_media::kind_of(rel).map(|k| k.0).unwrap_or("file");
    c.execute("INSERT INTO files (run, team, node, path, kind, bytes, sha, reason, t_ms, data) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![ctx.run, ctx.team, ctx.node, rel, kind, data.len() as i64, sha, reason, now(), data]).is_ok()
}
/// Save bytes that are not a file on disk (a patch of commits about to be lost with a branch). Blocking.
pub fn store_bytes(ctx: &Ctx, rel: &str, data: &[u8], reason: &str) -> bool {
    let sha = hex::encode(Sha256::digest(data)); let Ok(c) = open(&ctx.root) else { return false };
    if c.query_row("SELECT 1 FROM files WHERE run = ?1 AND path = ?2 AND sha = ?3", params![ctx.run, rel, sha], |_| Ok(())).is_ok() { return false; }
    c.execute("INSERT INTO files (run, team, node, path, kind, bytes, sha, reason, t_ms, data) VALUES (?1,?2,?3,?4,'file',?5,?6,?7,?8,?9)", params![ctx.run, ctx.team, ctx.node, rel, data.len() as i64, sha, reason, now(), data]).is_ok()
}
/// Is this file safe to remove? Only when its bytes are in the archive. The one gate every removal goes through. Blocking.
pub fn store_before_removal(ctx: &Ctx, rel: &str, abs: &Path, reason: &str) -> bool { if !abs.exists() { return true; } store_file(ctx, rel, abs, reason); let Ok(data) = std::fs::read(abs) else { return false }; let sha = hex::encode(Sha256::digest(&data)); open(&ctx.root).ok().map_or(false, |c| c.query_row("SELECT 1 FROM files WHERE run = ?1 AND path = ?2 AND sha = ?3", params![ctx.run, rel, sha], |_| Ok(())).is_ok()) }
/// Save everything a working folder holds that is not in git yet or has changed, and every video, picture or sound in it (git ignores output folders). At most 600 MB a call. Returns how many files were saved. Blocking.
pub fn store_tree(ctx: &Ctx, dir: &Path, reason: &str) -> usize {
    let mut names: Vec<String> = crate::team_run::names_of(dir).unwrap_or_default(); let mut found = vec![]; crate::team_media::walk(dir, dir, 0, &mut found); for (rel, _) in found { let s = rel.to_string_lossy().replace('\\', "/"); if !names.contains(&s) { names.push(s); } }
    let (mut n, mut total) = (0, 0u64); for rel in names.into_iter().take(400) { let abs = dir.join(&rel); let len = std::fs::metadata(&abs).map(|m| m.len()).unwrap_or(0); if total + len > MAX_PER_CALL { continue; } if store_file(ctx, &rel, &abs, reason) { n += 1; total += len; } } n
}
/// Save the videos, pictures and sounds of a folder as a step ends. Blocking.
pub fn store_media(ctx: &Ctx, dir: &Path) -> usize { let mut found = vec![]; crate::team_media::walk(dir, dir, 0, &mut found); let mut n = 0; for (rel, _) in found { let s = rel.to_string_lossy().replace('\\', "/"); if store_file(ctx, &s, &dir.join(&rel), "made by a step") { n += 1; } } n }
/// What the archive holds for a run, newest last: no bytes. Blocking.
pub fn list(root: &str, run: &str) -> Vec<Value> {
    let Ok(c) = open(root) else { return vec![] }; let Ok(mut st) = c.prepare("SELECT id, node, path, kind, bytes, reason, t_ms, sha FROM files WHERE run = ?1 ORDER BY id") else { return vec![] };
    st.query_map(params![run], |r| Ok(json!({ "id": r.get::<_, i64>(0)?, "node": r.get::<_, String>(1)?, "path": r.get::<_, String>(2)?, "kind": r.get::<_, String>(3)?, "bytes": r.get::<_, i64>(4)?, "reason": r.get::<_, String>(5)?, "t_ms": r.get::<_, i64>(6)?, "sha": r.get::<_, String>(7)? }))).map(|it| it.flatten().collect()).unwrap_or_default()
}
/// One archived file's bytes and name. Blocking.
pub fn get(root: &str, run: &str, id: i64) -> Option<(String, Vec<u8>)> { let c = open(root).ok()?; c.query_row("SELECT path, data FROM files WHERE id = ?1 AND run = ?2", params![id, run], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))).ok() }
/// The newest copy of each media file a run has in the archive, for the player: `file` is `archive/<id>/<name>`. Blocking.
pub fn media_list(root: &str, run: &str) -> Vec<Value> {
    let mut seen = std::collections::HashSet::new(); let mut out = vec![];
    for r in list(root, run).into_iter().rev() { let kind = r["kind"].as_str().unwrap_or(""); if !matches!(kind, "video" | "image" | "audio") { continue; } let path = r["path"].as_str().unwrap_or(""); let name = path.rsplit('/').next().unwrap_or(path).to_string(); if !seen.insert((name.clone(), r["sha"].as_str().unwrap_or("").to_string())) { continue; }
        out.push(json!({ "file": format!("archive/{}/{}", r["id"], name), "name": name, "path": path, "kind": kind, "bytes": r["bytes"], "archived": true })); }
    out.reverse(); out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_file_is_saved_before_it_can_go_once_per_content_and_comes_back_whole() {
        let tmp = std::env::temp_dir().join(format!("antiloki-arch-{}", std::process::id())); let _ = std::fs::remove_dir_all(&tmp); let tree = tmp.join("tree"); std::fs::create_dir_all(tree.join("video/out")).unwrap(); let root = tmp.join("ws").to_string_lossy().to_string();
        let ctx = Ctx { root: root.clone(), run: "r-1".into(), team: "t".into(), node: "builder".into() }; std::fs::write(tree.join("video/out/test.mp4"), b"0123456789").unwrap(); std::fs::write(tree.join("notes.md"), b"hello").unwrap();
        assert_eq!(store_media(&ctx, &tree), 1); assert_eq!(store_media(&ctx, &tree), 0, "the same bytes are saved once"); assert!(store_before_removal(&ctx, "notes.md", &tree.join("notes.md"), "put back"), "a file is saved before it may go");
        assert!(store_before_removal(&ctx, "gone.txt", &tree.join("gone.txt"), "x"), "nothing to lose when the file is not there"); std::fs::write(tree.join("video/out/test.mp4"), b"changed!!!").unwrap(); assert_eq!(store_media(&ctx, &tree), 1, "a changed file is a new copy, the old one stays");
        let l = list(&root, "r-1"); assert_eq!(l.len(), 3, "{l:?}"); let first = l.iter().find(|x| x["path"] == json!("video/out/test.mp4")).unwrap()["id"].as_i64().unwrap(); assert_eq!(get(&root, "r-1", first).unwrap().1, b"0123456789"); assert!(get(&root, "r-2", first).is_none(), "another run cannot read it");
        let m = media_list(&root, "r-1"); assert_eq!(m.len(), 2); assert!(m.iter().all(|x| x["file"].as_str().unwrap().starts_with("archive/") && x["kind"] == json!("video")));
        std::fs::remove_dir_all(&tree).unwrap(); assert_eq!(get(&root, "r-1", first).unwrap().1, b"0123456789", "the folder is gone and the file is not"); let _ = std::fs::remove_dir_all(&tmp);
    }
}
