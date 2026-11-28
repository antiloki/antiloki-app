//! THREADS UNDER A SPACE (operator 2026-11-10: "instead of tabs, threads, like below the space, so the space is the project, and threads are inside them, each thread can have the panes and agents").
//!
//! A space is a project; a thread is one line of work inside it with its own screen: its own panes and agents (its own layout). `<root>/.antiloki/threads.json` holds the list and which one is open:
//!   { "active": "main", "threads": [{ "id": "main", "name": "Main", "created_ms": … }, …] }
//! The first thread, `main`, keeps the layout file the space always had (`cowork-layout.json`), so nothing made before threads is lost; every other thread has `threads/<id>.layout.json`.
//! A thread that still has panes cannot be deleted (its agents would be left with nowhere to be seen): close them first. The last thread cannot be deleted either.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const MAIN: &str = "main";
const MAX_THREADS: usize = 24;

pub fn threads_path(root: &str) -> PathBuf { Path::new(root).join(".antiloki").join("threads.json") }
/// The layout file of a thread. Pure.
pub fn layout_file(root: &str, thread: &str) -> PathBuf {
    if thread == MAIN { crate::cowork::layout_path(root) } else { Path::new(root).join(".antiloki").join("threads").join(format!("{thread}.layout.json")) }
}
/// A thread's id from its name: lower case words and dashes; `main` is taken. Pure.
pub fn slug(name: &str) -> String { let t: String = name.trim().to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect(); t.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-").chars().take(40).collect() }

/// The stored list, or the default one (one thread, `main`). A torn file is the default.
pub fn load(root: &str) -> Value {
    let v: Value = std::fs::read_to_string(threads_path(root)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    let ok = v["threads"].as_array().map_or(false, |a| !a.is_empty() && a.iter().all(|t| t["id"].is_string()));
    if ok { v } else { json!({ "active": MAIN, "threads": [{ "id": MAIN, "name": "Main", "created_ms": 0 }] }) }
}
fn save(root: &str, v: &Value) -> Result<(), String> {
    let p = threads_path(root); std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    let tmp = p.with_extension("json.tmp"); std::fs::write(&tmp, v.to_string()).map_err(|e| e.to_string())?; std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
}
/// The open thread's id (always one that exists).
pub fn active(root: &str) -> String { let v = load(root); let a = v["active"].as_str().unwrap_or(MAIN).to_string(); if v["threads"].as_array().map_or(false, |t| t.iter().any(|x| x["id"] == json!(a))) { a } else { MAIN.into() } }
pub fn exists(root: &str, id: &str) -> bool { load(root)["threads"].as_array().map_or(false, |t| t.iter().any(|x| x["id"] == json!(id))) }

/// The panes a layout holds, flat: the screen's pane id, its kind, its title and its cockpit pane number. Pure.
/// The engine panes (their numbers) a thread has open, and the thread's layout cleared: the first step of deleting a thread, whose panes may not be closed by hand on a team thread. Blocking.
pub fn take_pane_ids(root: &str, id: &str) -> Vec<i64> { let ids: Vec<i64> = panes_of(&layout_of(root, id)).iter().filter_map(|p| p["paneId"].as_i64()).collect(); let _ = std::fs::remove_file(layout_file(root, id)); ids }
pub fn panes_of(layout: &Value) -> Vec<Value> {
    fn walk(n: &Value, out: &mut Vec<Value>) { if n["type"] == "leaf" { let p = &n["pane"]; out.push(json!({ "id": p["id"], "kind": p["kind"], "engine": p["engine"], "title": p["title"], "paneId": p["paneId"], "tool": p["tool"] })); } else { for c in n["children"].as_array().into_iter().flatten() { walk(c, out); } } }
    let mut out = vec![]; walk(&layout["root"], &mut out); out
}
fn layout_of(root: &str, id: &str) -> Value { std::fs::read_to_string(layout_file(root, id)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null) }

/// The list with what each thread holds (its panes), and which is open.
pub fn view(root: &str) -> Value {
    let v = load(root); let act = active(root);
    let threads: Vec<Value> = v["threads"].as_array().into_iter().flatten().map(|t| { let id = t["id"].as_str().unwrap_or(""); json!({ "id": id, "name": t["name"], "created_ms": t["created_ms"], "active": id == act, "panes": panes_of(&layout_of(root, id)), "team": t["team"], "leader": t["leader"] }) }).collect();
    json!({ "active": act, "threads": threads })
}

/// The ops: `list`, `create {name}` (and open it), `rename {thread, name}`, `select {thread}`, `delete {thread}`. Returns the new view or the reason. The door only passes the root.
pub fn apply(root: &str, b: &Value) -> Result<Value, String> {
    let mut v = load(root); let op = b["op"].as_str().unwrap_or("list");
    let name_of = |x: &Value| -> String { x.as_str().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ").chars().take(60).collect() };
    let find = |v: &Value, k: &str| v["threads"].as_array().and_then(|a| a.iter().position(|t| t["id"] == json!(k) || t["name"].as_str().map_or(false, |n| n.eq_ignore_ascii_case(k.trim()))));
    match op {
        "list" | "" => {}
        "create" => {
            let name = name_of(&b["name"]); let id = slug(&name); if id.is_empty() { return Err("a thread needs a name".into()); }
            let arr = v["threads"].as_array_mut().unwrap();
            if arr.len() >= MAX_THREADS { return Err(format!("a space keeps at most {MAX_THREADS} threads")); }
            if id == MAIN || arr.iter().any(|t| t["id"] == json!(id) || t["name"].as_str().map_or(false, |n| n.eq_ignore_ascii_case(&name))) { return Err(format!("a thread called `{name}` exists")); }
            arr.push(json!({ "id": id, "name": name, "created_ms": crate::cli::now_ms() })); v["active"] = json!(id);
        }
        // A TEAM THREAD (RFC 0008 §4.15): a thread that is a team's own: same id as the team, no agents dropped into it by hand, its branch the team's. `create_team {name}`.
        "create_team" => {
            let name = name_of(&b["name"]); let id = slug(&name); if id.is_empty() { return Err("a team needs a name".into()); }
            let arr = v["threads"].as_array_mut().unwrap();
            if arr.len() >= MAX_THREADS { return Err(format!("a space keeps at most {MAX_THREADS} threads")); }
            if id == MAIN || arr.iter().any(|t| t["id"] == json!(id) || t["name"].as_str().map_or(false, |n| n.eq_ignore_ascii_case(&name))) { return Err(format!("a thread called `{name}` exists")); }
            arr.push(json!({ "id": id, "name": name, "team": id, "created_ms": crate::cli::now_ms() })); if b["select"] != json!(false) { v["active"] = json!(id); }   // a team the Leader saves on its own must not take the person's screen
        }
        // THE "NEW TEAM" THREAD BECOMES THE TEAM'S (operator 2026-11-28: "based on my answer you change the name of the team itself and the thread"). `adopt_team {thread, name, leader}`: the same thread, renamed; its screen
        // (the layout, with the leader's chat on it) moves with it. `leader` is the pane that asked, so the screen knows which one leads.
        "adopt_team" => {
            let from = b["thread"].as_str().unwrap_or(""); let i = find(&v, from).ok_or("no such thread")?; let old = v["threads"][i]["id"].as_str().unwrap_or("").to_string();
            let name = name_of(&b["name"]); let id = slug(&name); if id.is_empty() { return Err("a team needs a name".into()); }
            if old == MAIN || v["threads"][i]["team"].is_string() { return Err("that thread cannot become this team's".into()); }
            if id == MAIN || v["threads"].as_array().unwrap().iter().enumerate().any(|(j, t)| j != i && (t["id"] == json!(id) || t["name"].as_str().map_or(false, |n| n.eq_ignore_ascii_case(&name)))) { return Err(format!("a thread called `{name}` exists")); }
            if id != old { let (a, z) = (layout_file(root, &old), layout_file(root, &id)); if a.exists() { std::fs::rename(&a, &z).map_err(|e| format!("the thread's screen could not be moved: {e}"))?; } }
            let t = &mut v["threads"][i]; t["id"] = json!(id); t["name"] = json!(name); t["team"] = json!(id); if let Some(l) = b["leader"].as_i64() { t["leader"] = json!(l); }
            if v["active"] == json!(old) { v["active"] = json!(id); }
        }
        "rename" => {
            let i = find(&v, b["thread"].as_str().unwrap_or("")).ok_or("no such thread")?; let name = name_of(&b["name"]); if name.is_empty() { return Err("a thread needs a name".into()); }
            if v["threads"].as_array().unwrap().iter().enumerate().any(|(j, t)| j != i && t["name"].as_str().map_or(false, |n| n.eq_ignore_ascii_case(&name))) { return Err(format!("a thread called `{name}` exists")); }
            v["threads"][i]["name"] = json!(name);
        }
        "select" => { let i = find(&v, b["thread"].as_str().unwrap_or("")).ok_or("no such thread")?; v["active"] = v["threads"][i]["id"].clone(); }
        "delete" => {
            let i = find(&v, b["thread"].as_str().unwrap_or("")).ok_or("no such thread")?; let id = v["threads"][i]["id"].as_str().unwrap_or("").to_string();
            if v["threads"].as_array().unwrap().len() == 1 { return Err("the last thread cannot be deleted".into()); }
            let n = panes_of(&layout_of(root, &id)).len(); if n > 0 && b["force"] != json!(true) { return Err(format!("the thread still has {n} pane{}: close {} first", if n == 1 { "" } else { "s" }, if n == 1 { "it" } else { "them" })); }
            v["threads"].as_array_mut().unwrap().remove(i); let _ = std::fs::remove_file(layout_file(root, &id));
            if v["active"] == json!(id) { v["active"] = v["threads"][0]["id"].clone(); }
        }
        other => return Err(format!("op is one of: list, create, create_team, rename, select, delete (not `{other}`)")),
    }
    if op != "list" && !op.is_empty() { save(root, &v)?; }
    Ok(view(root))
}

#[cfg(test)]
mod tests {
    #[test] fn the_new_team_thread_becomes_the_teams_with_its_screen() {
        let d = std::env::temp_dir().join(format!("al-adopt-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); let r = d.to_string_lossy().to_string();
        super::apply(&r, &serde_json::json!({ "op": "create", "name": "New team" })).unwrap();
        std::fs::create_dir_all(super::layout_file(&r, "new-team").parent().unwrap()).unwrap(); std::fs::write(super::layout_file(&r, "new-team"), r#"{"root":{"type":"leaf","pane":{"id":"a","kind":"engine","paneId":7}}}"#).unwrap();
        let v = super::apply(&r, &serde_json::json!({ "op": "adopt_team", "thread": "new-team", "name": "Price Watch", "leader": 7 })).unwrap();
        assert_eq!(v["active"], "price-watch"); let t = v["threads"].as_array().unwrap().iter().find(|t| t["id"] == "price-watch").unwrap();
        assert_eq!((t["name"].as_str(), t["team"].as_str(), t["leader"].as_i64(), t["panes"][0]["paneId"].as_i64()), (Some("Price Watch"), Some("price-watch"), Some(7), Some(7)));
        assert!(v["threads"].as_array().unwrap().iter().all(|t| t["id"] != "new-team") && !super::layout_file(&r, "new-team").exists());
        assert!(super::apply(&r, &serde_json::json!({ "op": "adopt_team", "thread": "price-watch", "name": "Other" })).is_err(), "a team's thread is not taken over");
        assert!(super::apply(&r, &serde_json::json!({ "op": "adopt_team", "thread": "main", "name": "Other" })).is_err());
        let _ = std::fs::remove_dir_all(d);
    }
    use super::*;
    fn tmp() -> String { static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0); let d = std::env::temp_dir().join(format!("antiloki-threads-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed))); std::fs::create_dir_all(&d).unwrap(); d.to_string_lossy().to_string() }
    fn leaf(id: &str, pid: i64) -> Value { json!({ "type": "leaf", "pane": { "id": id, "kind": "engine", "paneId": pid, "title": "t" } }) }
    #[test]
    fn a_space_starts_with_one_thread_that_keeps_the_layout_file_it_always_had() {
        let r = tmp(); let v = view(&r); assert_eq!((v["active"].clone(), v["threads"].as_array().unwrap().len()), (json!("main"), 1));
        assert_eq!(layout_file(&r, "main"), crate::cowork::layout_path(&r)); assert!(layout_file(&r, "review").ends_with("threads/review.layout.json"));
        // the layout that was there before threads is main's
        std::fs::create_dir_all(Path::new(&r).join(".antiloki")).unwrap(); std::fs::write(crate::cowork::layout_path(&r), json!({ "root": leaf("a-1", 1) }).to_string()).unwrap();
        assert_eq!(view(&r)["threads"][0]["panes"].as_array().unwrap().len(), 1);
    }
    #[test]
    fn threads_are_created_opened_renamed_and_each_holds_its_own_panes() {
        let r = tmp();
        let v = apply(&r, &json!({ "op": "create", "name": "  Fix  the checkout " })).unwrap(); assert_eq!((v["active"].clone(), v["threads"].as_array().unwrap().len()), (json!("fix-the-checkout"), 2));
        assert_eq!(v["threads"][1]["name"], "Fix the checkout"); assert_eq!(active(&r), "fix-the-checkout");
        std::fs::create_dir_all(layout_file(&r, "fix-the-checkout").parent().unwrap()).unwrap(); std::fs::write(layout_file(&r, "fix-the-checkout"), json!({ "root": { "type": "split", "dir": "row", "sizes": [0.5, 0.5], "children": [leaf("c-2", 2), leaf("c-3", 3)] } }).to_string()).unwrap();
        let v = view(&r); assert_eq!(v["threads"][0]["panes"].as_array().unwrap().len(), 0); assert_eq!(v["threads"][1]["panes"].as_array().unwrap().iter().map(|p| p["paneId"].as_i64().unwrap()).collect::<Vec<_>>(), vec![2, 3]);
        let v = apply(&r, &json!({ "op": "select", "thread": "main" })).unwrap(); assert_eq!(v["active"], "main");
        let v = apply(&r, &json!({ "op": "rename", "thread": "Fix the checkout", "name": "Checkout" })).unwrap(); assert_eq!((v["threads"][1]["id"].clone(), v["threads"][1]["name"].clone()), (json!("fix-the-checkout"), json!("Checkout")));
    }
    #[test]
    fn what_cannot_be_done_says_why() {
        let r = tmp(); apply(&r, &json!({ "op": "create", "name": "a" })).unwrap();
        for (b, why) in [(json!({ "op": "create", "name": "A" }), "exists"), (json!({ "op": "create", "name": "Main" }), "exists"), (json!({ "op": "create", "name": " " }), "needs a name"), (json!({ "op": "select", "thread": "zzz" }), "no such thread"), (json!({ "op": "rename", "thread": "a", "name": "main" }), "exists"), (json!({ "op": "nope" }), "op is one of")] {
            assert!(apply(&r, &b).unwrap_err().contains(why), "{b}");
        }
        std::fs::create_dir_all(layout_file(&r, "a").parent().unwrap()).unwrap(); std::fs::write(layout_file(&r, "a"), json!({ "root": leaf("x-1", 9) }).to_string()).unwrap();
        assert!(apply(&r, &json!({ "op": "delete", "thread": "a" })).unwrap_err().contains("still has 1 pane: close it first"));
        std::fs::write(layout_file(&r, "a"), json!({ "root": null }).to_string()).unwrap(); let v = apply(&r, &json!({ "op": "delete", "thread": "a" })).unwrap();
        assert_eq!((v["threads"].as_array().unwrap().len(), v["active"].clone()), (1, json!("main")), "deleting the open thread opens another"); assert!(!layout_file(&r, "a").exists());
        // a discard from the person deletes the thread with its panes (the front end has stopped them first): `force`
        apply(&r, &json!({ "op": "create", "name": "b" })).unwrap(); std::fs::write(layout_file(&r, "b"), json!({ "root": leaf("x-2", 8) }).to_string()).unwrap();
        let v = apply(&r, &json!({ "op": "delete", "thread": "b", "force": true })).unwrap(); assert!(v["threads"].as_array().unwrap().iter().all(|t| t["id"] != json!("b")) && !layout_file(&r, "b").exists());
        assert!(apply(&r, &json!({ "op": "delete", "thread": "main" })).unwrap_err().contains("last thread"));
        for i in 0..MAX_THREADS { let _ = apply(&r, &json!({ "op": "create", "name": format!("t{i}") })); } assert!(apply(&r, &json!({ "op": "create", "name": "one more" })).unwrap_err().contains("at most"));
    }
}
