//! WHAT A SAGA CHANGED IN THE CODE (operator 2026-11-10: "inside a saga when the code is changed add diff for that saga, saved also on the sqlite saga").
//!
//! A snapshot of the agent's box (everything it changed against its starting commit, new files included, never antiloki's own files) is taken whenever the box's work moves, and kept in the saga's own sqlite beside its
//! tasks and events: `changes(saga_id, task_id, t_ms, actor, files, added, removed, patch, sig)`. A snapshot identical to the last one is not stored twice; a patch is capped (the row says it was cut); a saga keeps its
//! latest 40. The patch is a real `git diff`, so the screen draws it and anything else can apply it. Who writes: an episode's saga (the box it ran in), and a team's run (the worker's delivery, under its work step).
use rusqlite::{params, Connection};
use serde_json::{json, Value};

/// A patch longer than this is cut (and the row says so): the point is to read a change, not to archive a branch.
pub const PATCH_CAP: usize = 1_500_000;
/// How many snapshots a saga keeps.
pub const KEEP: i64 = 40;

pub fn init(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS changes (
        id INTEGER PRIMARY KEY AUTOINCREMENT, saga_id INTEGER NOT NULL REFERENCES sagas(id) ON DELETE CASCADE, task_id INTEGER REFERENCES tasks(id) ON DELETE SET NULL,
        t_ms INTEGER NOT NULL, actor TEXT NOT NULL DEFAULT '', files TEXT NOT NULL DEFAULT '[]', added INTEGER NOT NULL DEFAULT 0, removed INTEGER NOT NULL DEFAULT 0,
        patch TEXT NOT NULL DEFAULT '', truncated INTEGER NOT NULL DEFAULT 0, sig TEXT NOT NULL DEFAULT '');
      CREATE INDEX IF NOT EXISTS changes_saga ON changes(saga_id, t_ms, id);")
}

/// The files of a unified diff, each with its lines added and removed and what happened to it. Pure.
pub fn files_of(patch: &str) -> Vec<Value> {
    let mut out: Vec<Value> = vec![]; let (mut path, mut status, mut add, mut del) = (String::new(), "modified", 0i64, 0i64); let mut open = false; let mut in_hunk = false;
    let flush = |out: &mut Vec<Value>, path: &str, status: &str, add: i64, del: i64, open: bool| { if open && !path.is_empty() { out.push(json!({ "path": path, "status": status, "added": add, "removed": del })); } };
    for l in patch.lines() {
        if let Some(rest) = l.strip_prefix("diff --git ") {
            flush(&mut out, &path, status, add, del, open);
            path = rest.rsplit_once(" b/").map(|(_, b)| b.to_string()).unwrap_or_default(); status = "modified"; add = 0; del = 0; open = true; in_hunk = false;
        } else if l.starts_with("new file mode") { status = "added"; } else if l.starts_with("deleted file mode") { status = "deleted"; }
        else if l.starts_with("rename to ") { status = "renamed"; if let Some(p) = l.strip_prefix("rename to ") { path = p.to_string(); } }
        else if l.starts_with("@@") { in_hunk = true; }
        else if in_hunk && l.starts_with('+') && !l.starts_with("+++") { add += 1; }
        else if in_hunk && l.starts_with('-') && !l.starts_with("---") { del += 1; }
    }
    flush(&mut out, &path, status, add, del, open); out
}

/// Keep a snapshot of `patch` for the saga, unless it is empty or the same as the last one. Returns whether a row was written.
pub fn record(c: &Connection, saga: i64, task: Option<i64>, actor: &str, patch: &str) -> Result<bool, String> {
    if patch.trim().is_empty() { return Ok(false); }
    let sig = crate::team_run::fingerprint(patch);
    let last: Option<String> = c.query_row("SELECT sig FROM changes WHERE saga_id = ?1 ORDER BY id DESC LIMIT 1", [saga], |r| r.get(0)).ok();
    if last.as_deref() == Some(sig.as_str()) { return Ok(false); }
    let files = files_of(patch); let (added, removed) = files.iter().fold((0i64, 0i64), |(a, d), f| (a + f["added"].as_i64().unwrap_or(0), d + f["removed"].as_i64().unwrap_or(0)));
    let (text, cut) = if patch.len() > PATCH_CAP { let mut n = PATCH_CAP; while !patch.is_char_boundary(n) { n -= 1; } (&patch[..n], true) } else { (patch, false) };
    c.execute("INSERT INTO changes (saga_id, task_id, t_ms, actor, files, added, removed, patch, truncated, sig) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![saga, task, crate::cli::now_ms(), actor, Value::Array(files).to_string(), added, removed, text, cut as i64, sig]).map_err(|e| e.to_string())?;
    c.execute("DELETE FROM changes WHERE saga_id = ?1 AND id NOT IN (SELECT id FROM changes WHERE saga_id = ?1 ORDER BY id DESC LIMIT ?2)", params![saga, KEEP]).map_err(|e| e.to_string())?;
    let _ = c.execute("UPDATE sagas SET updated_ms = ?1 WHERE id = ?2", params![crate::cli::now_ms(), saga]);
    Ok(true)
}

/// A saga's snapshots, newest first, WITHOUT the patches (the screen asks for one when it opens it).
pub fn list(c: &Connection, saga: i64) -> Result<Vec<Value>, String> {
    let rows = antiloki_rs::db_json::query_rows(c, "SELECT id, saga_id, task_id, t_ms, actor, files, added, removed, truncated FROM changes WHERE saga_id = ?1 ORDER BY id DESC", &[json!(saga)])?;
    Ok(rows.as_array().cloned().unwrap_or_default().into_iter().map(|mut r| { r["files"] = r["files"].as_str().and_then(|t| serde_json::from_str::<Value>(t).ok()).unwrap_or(json!([])); r["truncated"] = json!(r["truncated"].as_i64() == Some(1)); r }).collect())
}
/// One snapshot with its patch.
pub fn get(c: &Connection, id: i64) -> Result<Value, String> {
    let mut r = antiloki_rs::db_json::query_rows(c, "SELECT id, saga_id, task_id, t_ms, actor, files, added, removed, patch, truncated FROM changes WHERE id = ?1", &[json!(id)])?.as_array().and_then(|a| a.first().cloned()).ok_or("no such change")?;
    r["files"] = r["files"].as_str().and_then(|t| serde_json::from_str::<Value>(t).ok()).unwrap_or(json!([])); r["truncated"] = json!(r["truncated"].as_i64() == Some(1)); Ok(r)
}

/// The boxes' open episodes, each saga given what its box has changed so far. Never an error the screen sees.
pub fn record_boxes(c: &Connection, boxes: &[crate::work_boxes::BoxRef]) -> usize {
    let mut n = 0;
    for b in boxes {
        for e in crate::work_boxes::box_episodes(b) {
            if e.closed.is_some() || e.turns.is_empty() { continue; }
            let key = format!("ep:{}:{}", e.pane_id, e.n);
            let Some(saga) = c.query_row("SELECT id FROM sagas WHERE plan_ref = ?1", [&key], |r| r.get::<_, i64>(0)).ok() else { continue };
            let Ok(patch) = crate::team_run::patch_of(&b.dir) else { continue };
            let last = e.turns.last().map(|t| format!("{key}:turn:{}", t.start_ms));
            let task = last.and_then(|x| c.query_row("SELECT id FROM tasks WHERE ext = ?1", [&x], |r| r.get::<_, i64>(0)).ok());
            if record(c, saga, task, &if e.agent.is_empty() { e.engine.clone() } else { e.agent.clone() }, &patch).unwrap_or(false) { n += 1; }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    const P: &str = "diff --git a/src/a.ts b/src/a.ts\nindex 111..222 100644\n--- a/src/a.ts\n+++ b/src/a.ts\n@@ -1,2 +1,3 @@\n one\n-two\n+TWO\n+three\ndiff --git a/new.txt b/new.txt\nnew file mode 100644\nindex 0000000..333\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+hello\ndiff --git a/gone.txt b/gone.txt\ndeleted file mode 100644\nindex 444..0000000\n--- a/gone.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n";
    fn db() -> Connection { let c = Connection::open_in_memory().unwrap(); crate::work::init_for_tests(&c).unwrap(); c }
    #[test]
    fn a_patch_is_read_into_files_with_what_happened_to_each() {
        let f = files_of(P);
        assert_eq!(f.iter().map(|x| (x["path"].as_str().unwrap(), x["status"].as_str().unwrap(), x["added"].as_i64().unwrap(), x["removed"].as_i64().unwrap())).collect::<Vec<_>>(), vec![("src/a.ts", "modified", 2, 1), ("new.txt", "added", 1, 0), ("gone.txt", "deleted", 0, 1)]);
        assert!(files_of("").is_empty());
    }
    #[test]
    fn a_snapshot_is_kept_once_with_its_files_and_its_patch_and_the_same_one_again_is_not() {
        let c = db(); let saga = crate::work::create_saga(&c, "s", "", "you").unwrap();
        assert!(!record(&c, saga, None, "claude", "  \n").unwrap(), "no change, no row");
        assert!(record(&c, saga, None, "claude", P).unwrap()); assert!(!record(&c, saga, None, "claude", P).unwrap(), "the same change twice is one row");
        let l = list(&c, saga).unwrap(); assert_eq!(l.len(), 1); assert_eq!((l[0]["added"].as_i64(), l[0]["removed"].as_i64()), (Some(3), Some(2))); assert!(l[0].get("patch").is_none(), "the list carries no patches");
        let one = get(&c, l[0]["id"].as_i64().unwrap()).unwrap(); assert_eq!(one["patch"].as_str().unwrap(), P); assert_eq!(one["files"].as_array().unwrap().len(), 3); assert_eq!(one["truncated"], json!(false));
        // another change is another snapshot, and going back to the first is stored again (the history is what happened in order)
        assert!(record(&c, saga, None, "claude", &format!("{P}diff --git a/z b/z\n@@ -1 +1 @@\n-a\n+b\n")).unwrap()); assert!(record(&c, saga, None, "claude", P).unwrap()); assert_eq!(list(&c, saga).unwrap().len(), 3);
    }
    #[test]
    fn a_huge_patch_is_cut_and_says_so_and_a_saga_keeps_only_its_latest_snapshots_and_deleting_the_saga_deletes_them() {
        let c = db(); let saga = crate::work::create_saga(&c, "s", "", "you").unwrap();
        let big = format!("diff --git a/big b/big\n@@ -0,0 +1 @@\n+{}\n", "x".repeat(PATCH_CAP + 10)); record(&c, saga, None, "a", &big).unwrap();
        let l = list(&c, saga).unwrap(); assert_eq!(l[0]["truncated"], json!(true)); assert!(get(&c, l[0]["id"].as_i64().unwrap()).unwrap()["patch"].as_str().unwrap().len() <= PATCH_CAP);
        for i in 0..(KEEP + 5) { record(&c, saga, None, "a", &format!("diff --git a/f{i} b/f{i}\n@@ -1 +1 @@\n-a\n+b\n")).unwrap(); }
        assert_eq!(list(&c, saga).unwrap().len() as i64, KEEP);
        crate::work::delete_saga(&c, saga).unwrap(); assert_eq!(c.query_row("SELECT COUNT(*) FROM changes", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }
}
