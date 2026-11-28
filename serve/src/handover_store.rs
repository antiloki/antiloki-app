//! HANDOVER STATE THAT SURVIVES A SERVE RESTART (issue 16, 2026-11-24). A provider switch keeps its reconciliation pack, and a new-team leader its queued first message, per pane. They lived in process memory only, so a restart
//! between the switch and the new terminal's start left the new agent with nothing telling it to read the pack. Each map is kept as one small JSON file beside the database and written on every change. Not written in tests.
use serde::{de::DeserializeOwned, Serialize};
use std::collections::HashMap;
fn file(name: &str) -> std::path::PathBuf {
    let db = std::path::PathBuf::from(crate::db_path());
    db.parent().map(|d| d.to_path_buf()).unwrap_or_default().join(format!("handover-{name}.json"))
}
/// The kept map, or an empty one (no file, unreadable, or in a test).
pub fn load<T: DeserializeOwned>(name: &str) -> HashMap<i64, T> { if cfg!(test) { return HashMap::new(); } load_from(&file(name)) }
fn load_from<T: DeserializeOwned>(f: &std::path::Path) -> HashMap<i64, T> {
    std::fs::read_to_string(f).ok().and_then(|t| serde_json::from_str::<HashMap<String, T>>(&t).ok())
        .map(|m| m.into_iter().filter_map(|(k, v)| Some((k.parse().ok()?, v))).collect()).unwrap_or_default()
}
/// Keep the map. Best effort: a failed write loses nothing that was not already memory-only.
pub fn save<T: Serialize>(name: &str, m: &HashMap<i64, T>) { if cfg!(test) { return; } save_to(&file(name), m) }
/// At most this many panes are kept (the newest ids): a pane that was closed leaves its entry, and pane ids only grow, so the oldest are the ones that cannot matter any more (audit 2026-11-25).
const KEEP: usize = 64;
fn save_to<T: Serialize>(f: &std::path::Path, m: &HashMap<i64, T>) {
    let mut ids: Vec<&i64> = m.keys().collect(); ids.sort(); let from = ids.len().saturating_sub(KEEP); let keep: std::collections::HashSet<i64> = ids[from..].iter().map(|k| **k).collect();
    let as_text: HashMap<String, &T> = m.iter().filter(|(k, _)| keep.contains(k)).map(|(k, v)| (k.to_string(), v)).collect();
    if let Some(d) = f.parent() { let _ = std::fs::create_dir_all(d); }
    if let Ok(t) = serde_json::to_string(&as_text) { let tmp = f.with_extension("json.tmp"); if std::fs::write(&tmp, t).is_ok() { let _ = std::fs::rename(&tmp, f); } }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_map_written_is_the_map_read_back_after_a_restart_and_a_missing_or_broken_file_is_empty() {
        let d = std::env::temp_dir().join(format!("antiloki-handover-{}", std::process::id())); let f = d.join("x.json");
        let mut m: HashMap<i64, String> = HashMap::new(); m.insert(7, "the pack".into()); m.insert(464900004, "brief".into());
        save_to(&f, &m); assert_eq!(load_from::<String>(&f), m);
        std::fs::write(&f, "not json").unwrap(); assert!(load_from::<String>(&f).is_empty());
        assert!(load_from::<String>(&d.join("missing.json")).is_empty()); let _ = std::fs::remove_dir_all(&d);
    }
}
