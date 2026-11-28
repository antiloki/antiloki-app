//! THE FILE TYPE (RFC 0011 §13.2): read and write inside the run's own folder ONLY. Text, JSON, CSV, Markdown. No shell, no path outside the folder, no link out of it.
use std::path::{Component, Path, PathBuf};

/// The most a read returns (bytes).
pub const MAX_READ: usize = 1_000_000;
const KINDS: [&str; 5] = ["txt", "md", "markdown", "json", "csv"];

/// Resolve `rel` under `base`, or say why not: a relative path of plain names, with an allowed extension, that cannot leave `base` by `..`, by an absolute path, or through a link.
pub fn confine(base: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel = rel.trim();
    if rel.is_empty() { return Err("say which file".into()); }
    let p = Path::new(rel);
    if p.is_absolute() || rel.starts_with('/') || rel.starts_with('\\') || rel.contains(':') { return Err(format!("`{rel}` is outside the run's folder: only a relative path inside it is allowed")); }
    for c in p.components() { match c { Component::Normal(_) => {}, Component::CurDir => {}, _ => return Err(format!("`{rel}` goes outside the run's folder")) } }
    let ext = p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
    if !KINDS.contains(&ext.as_str()) { return Err(format!("`{rel}`: a file here is text, markdown, json or csv (.txt .md .json .csv)")); }
    std::fs::create_dir_all(base).map_err(|e| e.to_string())?;
    let base_c = base.canonicalize().map_err(|e| e.to_string())?;
    let target = base_c.join(p);
    // a link inside the folder must not lead out of it: the deepest part that exists is resolved and checked
    let mut probe = target.clone();
    while !probe.exists() { if !probe.pop() { break; } }
    let real = probe.canonicalize().map_err(|e| e.to_string())?;
    if !real.starts_with(&base_c) { return Err(format!("`{rel}` leads outside the run's folder")); }
    Ok(target)
}

pub fn read(base: &Path, rel: &str) -> Result<String, String> {
    let p = confine(base, rel)?;
    let meta = std::fs::metadata(&p).map_err(|_| format!("there is no file `{rel}` in the run's folder"))?;
    if meta.len() as usize > MAX_READ { return Err(format!("`{rel}` is {} bytes: more than the {MAX_READ} a read returns", meta.len())); }
    let text = std::fs::read_to_string(&p).map_err(|e| format!("`{rel}` is not text: {e}"))?;
    Ok(text)
}

/// The shape of a file's content for its kind: JSON parses, CSV rows have one width.
pub fn check_shape(rel: &str, content: &str) -> Result<(), String> {
    match Path::new(rel).extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("json") => serde_json::from_str::<serde_json::Value>(content).map(|_| ()).map_err(|e| format!("`{rel}` is not valid JSON: {e}")),
        Some("csv") => { let rows = crate::team_db::parse_csv(content)?; let w = rows.first().map_or(0, |r| r.len()); if rows.iter().any(|r| r.len() != w) { Err(format!("`{rel}`: the csv rows do not all have {w} columns")) } else { Ok(()) } }
        _ => Ok(()),
    }
}

pub fn write(base: &Path, rel: &str, content: &str) -> Result<usize, String> {
    if content.len() > MAX_READ * 4 { return Err("that is more than a file here may hold".into()); }
    let p = confine(base, rel)?;
    check_shape(rel, content)?;
    if let Some(d) = p.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
    std::fs::write(&p, content).map_err(|e| e.to_string())?;
    Ok(content.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(name: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("antiloki-file-{}-{name}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }

    #[test]
    fn file_read_and_write_are_confined_to_the_run_tree() {
        let base = tmp("run").join("files"); let outside = tmp("outside");
        std::fs::write(outside.join("secret.txt"), "top secret").unwrap();
        // a round trip inside
        assert_eq!(write(&base, "notes/a.md", "# hi").unwrap(), 4); assert_eq!(read(&base, "notes/a.md").unwrap(), "# hi");
        assert_eq!(write(&base, "data.json", "{\"a\":1}").unwrap(), 7); assert_eq!(write(&base, "t.csv", "a,b\n1,2\n").unwrap(), 8);
        // every way out is refused
        for bad in ["../x.txt", "a/../../x.txt", "/etc/passwd.txt", "C:/x.txt", "\\\\host\\share\\x.txt", "..", "", "a/../../../secret.txt"] { assert!(confine(&base, bad).is_err(), "{bad:?} must be refused"); assert!(read(&base, bad).is_err()); assert!(write(&base, bad, "x").is_err()); }
        assert!(read(&base, &format!("{}/secret.txt", outside.display())).is_err(), "an absolute path to a real file is refused");
        // a link inside the folder that leads out is refused (where links exist)
        #[cfg(unix)] { std::os::unix::fs::symlink(&outside, base.join("link")).unwrap();
            assert!(read(&base, "link/secret.txt").unwrap_err().contains("outside"), "a link out of the folder is not a way out");
            assert!(write(&base, "link/new.txt", "x").is_err()); assert!(!outside.join("new.txt").exists()); }
        // only the file kinds the type is for; the shape is checked
        assert!(confine(&base, "run.sh").is_err() && confine(&base, "a.exe").is_err() && confine(&base, "noext").is_err());
        assert!(write(&base, "bad.json", "{nope").unwrap_err().contains("JSON")); assert!(write(&base, "bad.csv", "a,b\n1\n").unwrap_err().contains("columns"));
        assert!(read(&base, "missing.txt").unwrap_err().contains("no file"));
        std::fs::write(base.join("big.txt"), vec![b'x'; MAX_READ + 1]).unwrap(); assert!(read(&base, "big.txt").unwrap_err().contains("more than"));
        assert!(!outside.join("x.txt").exists());
    }
}
