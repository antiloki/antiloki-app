//! THE APPLY STEP (operator 2026-11-16, the light coder): a model that cannot call tools returns the COMPLETE new content of files, and this step writes them into the run's tree. The model never touches the disk.
//! Rules: a relative path inside the tree, never `..`, never `.git`, never a hidden config, a few files, a bounded size, and (when the node lists `paths`) only the files it owns. The write is the step's, so it needs the `write` effect.
use serde_json::Value;
use std::path::Path;

pub const MAX_FILES: usize = 8;
pub const MAX_BYTES: usize = 200_000;

/// The files to write: `files` is a list of `{path, content_from}` (the path may instead come from `path_from`), contents resolved by `get` (the model's answer). Pure over `get`.
pub fn plan(files: &Value, get: &dyn Fn(&str) -> Result<String, String>) -> Result<Vec<(String, String)>, String> {
    let list = files.as_array().filter(|a| !a.is_empty()).ok_or("`args.files` is a list of {path, content_from}")?;
    if list.len() > MAX_FILES { return Err(format!("at most {MAX_FILES} files in one step")); }
    let mut out = vec![];
    for f in list {
        let path = match (f["path"].as_str().filter(|p| !p.is_empty()), f["path_from"].as_str().filter(|p| !p.is_empty())) { (Some(p), _) => p.to_string(), (None, Some(r)) => get(r)?.trim().to_string(), _ => return Err("each file has a `path` or a `path_from`".into()) };
        clean(&path)?; let from = f["content_from"].as_str().filter(|p| !p.is_empty()).ok_or("each file has a `content_from`")?;
        let content = get(from)?; if content.trim().is_empty() { return Err(format!("{path}: the model gave no content")); } if content.len() > MAX_BYTES { return Err(format!("{path}: more than {MAX_BYTES} bytes")); }
        out.push((path, content));
    }
    Ok(out)
}
/// A path that stays inside the tree. Pure.
pub fn clean(p: &str) -> Result<(), String> {
    let comps: Vec<&str> = p.split(['/', '\\']).collect();
    let bad = p.is_empty() || p.starts_with('/') || p.starts_with('\\') || p.contains(':') || comps.iter().any(|c| c.is_empty() || *c == ".." || *c == "." || *c == ".git" || *c == ".antiloki" || c.starts_with(".env"));
    if bad { Err(format!("`{p}` is not a path this step may write: relative, inside the tree, not `..`, `.git`, `.antiloki` or `.env*`")) } else { Ok(()) }
}
/// Write the planned files under `dir`; returns the paths written.
pub fn write(dir: &Path, files: &[(String, String)], owns: &dyn Fn(&str) -> bool) -> Result<Vec<String>, String> {
    for (p, _) in files { if !owns(p) { return Err(format!("{p} is not in the step's allow-list of paths")); } }
    let mut done = vec![];
    for (p, c) in files { let to = dir.join(p); if let Some(d) = to.parent() { std::fs::create_dir_all(d).map_err(|e| format!("{p}: {e}"))?; } std::fs::write(&to, c).map_err(|e| format!("{p}: {e}"))?; done.push(p.clone()); }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test] fn a_model_s_files_are_planned_only_inside_the_tree_and_never_empty() {
        let get = |r: &str| -> Result<String, String> { match r { "w.src" => Ok("export const a = 1;\n".into()), "w.test" => Ok("test\n".into()), "w.path" => Ok("src/b.js".into()), "w.empty" => Ok("  ".into()), _ => Err(format!("{r} has not run")) } };
        let ok = plan(&json!([{ "path": "greet.js", "content_from": "w.src" }, { "path_from": "w.path", "content_from": "w.test" }]), &get).unwrap(); assert_eq!(ok.len(), 2); assert_eq!(ok[1].0, "src/b.js");
        for p in ["../x", "/etc/passwd", ".git/config", "a/../../b", "C:\\x", ".env", ".env.local", ".antiloki/x", "a//b", ""] { assert!(plan(&json!([{ "path": p, "content_from": "w.src" }]), &get).is_err(), "{p}"); }
        assert!(plan(&json!([{ "path": "a.js", "content_from": "w.empty" }]), &get).is_err()); assert!(plan(&json!([{ "path": "a.js", "content_from": "w.nope" }]), &get).is_err()); assert!(plan(&json!([]), &get).is_err());
        assert!(plan(&json!(vec![json!({ "path": "a.js", "content_from": "w.src" }); 9]), &get).is_err());
    }
    #[test] fn files_are_written_under_the_tree_and_only_the_owned_ones() {
        let d = std::env::temp_dir().join(format!("antiloki-apply-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        let w = write(&d, &[("a.js".into(), "1".into()), ("sub/b.js".into(), "2".into())], &|_| true).unwrap(); assert_eq!(w, vec!["a.js", "sub/b.js"]); assert_eq!(std::fs::read_to_string(d.join("sub/b.js")).unwrap(), "2");
        assert!(write(&d, &[("c.js".into(), "3".into())], &|p| p.starts_with("sub/")).is_err()); assert!(!d.join("c.js").exists());
    }
}
