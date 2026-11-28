//! artefact-store.rs — twin of skills/entities/artefact-store.mjs (db lane, default export
//! `artefactStore`; `db`/`context` are entirely unused, mirroring the JS `_context` param — the
//! module has ZERO SQL statements). NAMES ARE CONTRACT.
//!
//! THE PERMANENT SHELF: `list` scans `ARTEFACTS_DIR` for `.json` sidecars, pairs each with its
//! `.wav`, and returns the decorated metadata newest-first (by `created_at`), capped at 200.
//! `get` resolves one `.wav` by basename (traversal-proof: the name IS the key).
//!
//! BESPOKE NATIVE TEST (not conform-db), same class as knowledge-document-add.rs (lesson 5/6):
//! unlike a skill where the fs touch is a best-effort side branch invisible to `db_after`
//! (source-remove.rs precedent), this module has NO db table backing ANY of its logic — `list`'s
//! entire purpose is a real `readdirSync`/`readFileSync`/`statSync` scan, and `get`'s core branch
//! is an unconditional `existsSync`. There is no db-only subset the conform-db JSON-fixture
//! harness (SQL seed/read only, no fs-injection lane) could exercise, so this is proven instead
//! via `#[cfg(test)]` Rust-native tests that write real temp files and inject the shelf root via
//! `ANTILOKI_DATA_DIR` (env-var + tempdir, `with_data_dir` — same locking pattern as
//! knowledge-document-add.rs / sandbox-resolve-path.rs). No frontier/fixture entries for this
//! file (matches the god-driver-halt.rs / god-driver-resume.rs / knowledge-document-add.rs
//! precedent: present in lib.rs only, absent from frontier.json).
//!
//! roots.mjs: `ARTEFACTS_DIR = join(SOURCES_DIR, 'artefacts')`, `SOURCES_DIR = join(DATA_DIR,
//! 'sources')`, `DATA_DIR = ANTILOKI_DATA_DIR` else `SUBSTRATE_ROOT/.antiloki`, `SUBSTRATE_ROOT =
//! ANTILOKI_SUBSTRATE_ROOT` else self-derived from roots.mjs's own file location. roots.mjs
//! itself is not imported/converted — inlined + env-read at call time below (`artefacts_dir()`),
//! anchored at CARGO_MANIFEST_DIR when no override is set (source-list.rs's `sources_dir()` /
//! knowledge-document-add.rs's `vault_root()` precedent: antiloki-core-rust and antiloki-core are
//! siblings under the same SUBSTRATE_ROOT on this checkout layout). `ARTEFACTS_DIR` itself is not
//! a unit (only functions count per the census rule) — modeled as the `artefacts_dir()` fn, same
//! convention as `halt_file()` in god-driver-halt.rs.
//!
//! `err.required`/`err.notFound` message text reproduced verbatim (`${skill}: ${cause}` assembly
//! in engine/error.mjs) for audit fidelity, per the playbook's throw-fidelity lesson:
//! `"artefact-store: a .wav artifact name is required"`, `"artefact-store: artefact '<file>' not
//! found"`, `"artefact-store: op 'list'|'get' is required"`.
//!
//! `basename()` ports node:path's basename for the traversal-proofing this call exists for:
//! trims trailing separators, then takes the tail after the last `/` or `\` (both separators
//! stripped regardless of host OS — the JS import is the platform-default `node:path`, and this
//! call's entire purpose is defensive, so stripping both is the safe superset).
//!
//! DISCLOSED gap: `{ ...meta, file, bytes }` (list op) is modeled as object-spread-then-override
//! (gotcha #1: clone the source Map, `.insert` file/bytes) — exact for the realistic case (a
//! finalize-written sidecar is always a JSON object, per the module doc "sidecar carrying its
//! receipts"). A malformed sidecar whose top-level JSON is an array/string/number/bool would spread
//! to `{}` here (object-only), diverging from JS's index-keyed array/string spread — an
//! unrealistic shape never written by the one real producer (narration-render's `finalize` op),
//! so left unexercised rather than fully ported (source-add.rs's `resolve_path` precedent for
//! disclosed-but-out-of-scope edge cases).
//!
//! DISCLOSED gap: sort ties (equal `created_at`) resolve by native filesystem enumeration order
//! (`std::fs::read_dir`), which is OS/filesystem-dependent — same class as source-list.rs's
//! unexercised ordering assumptions. Not JS-parity-critical since this is bespoke-native (no
//! cross-lane byte comparison); this module's own tests use distinct `created_at` values per case
//! specifically to keep sort order deterministic regardless of readdir enumeration order.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

// node:path `basename(p)` — trailing separators trimmed first, then the tail after the last
// separator (both `/` and `\` accepted regardless of host OS — see module doc).
fn basename(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rfind(['/', '\\']) {
        Some(i) => trimmed[i + 1..].to_string(),
        None => trimmed.to_string(),
    }
}

// roots.mjs: ARTEFACTS_DIR = join(SOURCES_DIR, 'artefacts'); SOURCES_DIR = join(DATA_DIR,
// 'sources'); DATA_DIR = ANTILOKI_DATA_DIR else SUBSTRATE_ROOT/.antiloki; SUBSTRATE_ROOT =
// ANTILOKI_SUBSTRATE_ROOT else self-derived. See module doc.
fn artefacts_dir() -> PathBuf {
    if let Ok(v) = std::env::var("ANTILOKI_DATA_DIR") {
        if !v.is_empty() {
            return PathBuf::from(v).join("sources").join("artefacts");
        }
    }
    if let Ok(v) = std::env::var("ANTILOKI_SUBSTRATE_ROOT") {
        if !v.is_empty() {
            return PathBuf::from(v).join(".antiloki").join("sources").join("artefacts");
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .join(".antiloki")
        .join("sources")
        .join("artefacts")
}

pub fn artefactStore(input: &Value, _db: &Connection) -> Result<Value, String> {
    // JS: String(input?.op ?? 'list')
    let op = match input.get("op").filter(|v| !v.is_null()) {
        Some(v) => js_string(v),
        None => "list".to_string(),
    };

    if op == "list" {
        let dir = artefacts_dir();
        let mut entries: Vec<Value> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.filter_map(|e| e.ok()) {
                let fname = entry.file_name().to_string_lossy().into_owned();
                if !fname.ends_with(".json") {
                    continue;
                }
                let meta = match std::fs::read_to_string(dir.join(&fname))
                    .ok()
                    .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                {
                    Some(m) => m,
                    None => continue, // readFileSync/JSON.parse throw -> caught -> skip
                };

                // String(meta.file ?? f.replace(/\.json$/, '.wav'))
                let default_wav = format!("{}.wav", &fname[..fname.len() - 5]);
                let wav = match meta.get("file").filter(|v| !v.is_null()) {
                    Some(v) => js_string(v),
                    None => default_wav,
                };
                let wav_path = dir.join(&wav);
                if !wav_path.exists() {
                    continue; // sidecar without audio = not an artifact
                }

                // meta.bytes ?? statSync(wav_path).size — statSync failure -> caught -> skip entry
                let bytes_val = match meta.get("bytes").filter(|v| !v.is_null()) {
                    Some(v) => v.clone(),
                    None => match std::fs::metadata(&wav_path) {
                        Ok(md) => json!(md.len() as i64),
                        Err(_) => continue,
                    },
                };

                let mut o: Map<String, Value> = meta.as_object().cloned().unwrap_or_default();
                o.insert("file".into(), json!(wav));
                o.insert("bytes".into(), bytes_val);
                entries.push(Value::Object(o));
            }
        }
        // String(b.created_at ?? '').localeCompare(String(a.created_at ?? '')) desc;
        // sort_by is stable like JS's sort (see module doc for the tie-break disclosure).
        entries.sort_by(|a, b| str_qq(b.get("created_at")).cmp(&str_qq(a.get("created_at"))));
        entries.truncate(200);

        let dir_str = dir.to_string_lossy().into_owned();
        return Ok(json!({ "ok": true, "dir": dir_str, "artefacts": entries }));
    }

    if op == "get" {
        // basename(String(input?.file ?? ''))
        let file = basename(&str_qq(input.get("file")));
        if !file.ends_with(".wav") {
            return Err("artefact-store: a .wav artifact name is required".to_string());
        }
        let abs = artefacts_dir().join(&file);
        if !abs.exists() {
            return Err(format!("artefact-store: artefact '{file}' not found"));
        }
        let abs_str = abs.to_string_lossy().into_owned();
        return Ok(json!({ "ok": true, "abs_path": abs_str, "file": file }));
    }

    Err("artefact-store: op 'list'|'get' is required".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // shared crate-wide lock (test_env_lock.rs) — a local mutex here would not prevent a race
    // with OTHER files' tests over the same real env var.
    fn with_data_dir<T>(data_dir: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_DATA_DIR", data_dir);
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_DATA_DIR");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-artefact-store-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db0() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    fn write_artefact(dir: &Path, base: &str, created_at: &str, extra_bytes: Option<i64>) {
        let shelf = dir.join("sources").join("artefacts");
        fs::create_dir_all(&shelf).unwrap();
        fs::write(shelf.join(format!("{base}.wav")), b"RIFF....WAVEfmt ").unwrap();
        let mut sidecar = json!({
            "kind": "narration",
            "source": "press-1",
            "created_at": created_at,
        });
        if let Some(b) = extra_bytes {
            sidecar["bytes"] = json!(b);
        }
        fs::write(
            shelf.join(format!("{base}.json")),
            serde_json::to_string(&sidecar).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn list_on_missing_shelf_returns_empty() {
        let dir = temp_dir("list-missing");
        let r = with_data_dir(dir.to_str().unwrap(), || artefactStore(&json!({ "op": "list" }), &db0())).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["artefacts"], json!([]));
        assert!(r["dir"].as_str().unwrap().ends_with(&format!("sources{}artefacts", std::path::MAIN_SEPARATOR)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_op_is_list() {
        let dir = temp_dir("default-op");
        let r = with_data_dir(dir.to_str().unwrap(), || artefactStore(&json!({}), &db0())).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["artefacts"], json!([]));
        let r2 = with_data_dir(dir.to_str().unwrap(), || artefactStore(&Value::Null, &db0())).unwrap();
        assert_eq!(r2["artefacts"], json!([]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_sorts_newest_first_by_created_at_and_falls_back_to_real_file_size() {
        let dir = temp_dir("list-sort");
        write_artefact(&dir, "older", "2026-01-01T00:00:00.000Z", None);
        write_artefact(&dir, "newer", "2026-01-05T00:00:00.000Z", Some(999));
        let r = with_data_dir(dir.to_str().unwrap(), || artefactStore(&json!({ "op": "list" }), &db0())).unwrap();
        let arts = r["artefacts"].as_array().unwrap();
        assert_eq!(arts.len(), 2);
        assert_eq!(arts[0]["file"], json!("newer.wav"));
        assert_eq!(arts[0]["bytes"], json!(999)); // explicit meta.bytes wins over real file size
        assert_eq!(arts[1]["file"], json!("older.wav"));
        assert_eq!(arts[1]["bytes"], json!(16)); // b"RIFF....WAVEfmt " is 16 bytes -> statSync fallback
        assert_eq!(arts[1]["kind"], json!("narration"));
        assert_eq!(arts[1]["source"], json!("press-1"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sidecar_without_matching_wav_is_skipped() {
        let dir = temp_dir("no-wav");
        let shelf = dir.join("sources").join("artefacts");
        fs::create_dir_all(&shelf).unwrap();
        fs::write(shelf.join("ghost.json"), r#"{"created_at":"2026-01-01T00:00:00.000Z"}"#).unwrap();
        let r = with_data_dir(dir.to_str().unwrap(), || artefactStore(&json!({ "op": "list" }), &db0())).unwrap();
        assert_eq!(r["artefacts"], json!([]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_json_sidecar_is_skipped_but_does_not_hide_the_shelf() {
        let dir = temp_dir("malformed");
        let shelf = dir.join("sources").join("artefacts");
        fs::create_dir_all(&shelf).unwrap();
        fs::write(shelf.join("bad.json"), b"not valid json {{{").unwrap();
        fs::write(shelf.join("bad.wav"), b"x").unwrap();
        write_artefact(&dir, "good", "2026-01-01T00:00:00.000Z", None);
        let r = with_data_dir(dir.to_str().unwrap(), || artefactStore(&json!({ "op": "list" }), &db0())).unwrap();
        let arts = r["artefacts"].as_array().unwrap();
        assert_eq!(arts.len(), 1);
        assert_eq!(arts[0]["file"], json!("good.wav"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_json_files_in_the_shelf_are_ignored() {
        let dir = temp_dir("non-json");
        let shelf = dir.join("sources").join("artefacts");
        fs::create_dir_all(&shelf).unwrap();
        fs::write(shelf.join("stray.wav"), b"x").unwrap();
        fs::write(shelf.join("notes.txt"), b"hello").unwrap();
        let r = with_data_dir(dir.to_str().unwrap(), || artefactStore(&json!({ "op": "list" }), &db0())).unwrap();
        assert_eq!(r["artefacts"], json!([]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_caps_at_200_newest() {
        let dir = temp_dir("cap-200");
        // 205 distinct instants via the zero-padded millisecond field (000..204) — fixed-width
        // so lexicographic ("localeCompare"-equivalent) order matches numeric/chronological order.
        for i in 0..205 {
            write_artefact(&dir, &format!("a{i:03}"), &format!("2026-01-01T00:00:00.{i:03}Z"), None);
        }
        let r = with_data_dir(dir.to_str().unwrap(), || artefactStore(&json!({ "op": "list" }), &db0())).unwrap();
        let arts = r["artefacts"].as_array().unwrap();
        assert_eq!(arts.len(), 200);
        // newest first: ms=204 ("a204") sorts ahead of ms=203, ..., down through the cap.
        assert_eq!(arts[0]["file"], json!("a204.wav"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn get_returns_abs_path_for_an_existing_wav() {
        let dir = temp_dir("get-ok");
        write_artefact(&dir, "press", "2026-01-01T00:00:00.000Z", None);
        let r = with_data_dir(dir.to_str().unwrap(), || artefactStore(&json!({ "op": "get", "file": "press.wav" }), &db0())).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["file"], json!("press.wav"));
        assert!(r["abs_path"].as_str().unwrap().ends_with(&format!("artefacts{}press.wav", std::path::MAIN_SEPARATOR)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn get_strips_directory_traversal_via_basename() {
        let dir = temp_dir("get-traversal");
        write_artefact(&dir, "safe", "2026-01-01T00:00:00.000Z", None);
        let r = with_data_dir(dir.to_str().unwrap(), || {
            artefactStore(&json!({ "op": "get", "file": "../../etc/safe.wav" }), &db0())
        })
        .unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["file"], json!("safe.wav"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn get_rejects_non_wav_name() {
        let dir = temp_dir("get-non-wav");
        let r = with_data_dir(dir.to_str().unwrap(), || artefactStore(&json!({ "op": "get", "file": "press.mp3" }), &db0()));
        assert_eq!(r, Err("artefact-store: a .wav artifact name is required".to_string()));
        let r2 = with_data_dir(dir.to_str().unwrap(), || artefactStore(&json!({ "op": "get" }), &db0()));
        assert_eq!(r2, Err("artefact-store: a .wav artifact name is required".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn get_reports_not_found_for_a_missing_wav() {
        let dir = temp_dir("get-missing");
        fs::create_dir_all(dir.join("sources").join("artefacts")).unwrap();
        let r = with_data_dir(dir.to_str().unwrap(), || artefactStore(&json!({ "op": "get", "file": "ghost.wav" }), &db0()));
        assert_eq!(r, Err("artefact-store: artefact 'ghost.wav' not found".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_op_is_rejected() {
        let dir = temp_dir("bad-op");
        let r = with_data_dir(dir.to_str().unwrap(), || artefactStore(&json!({ "op": "delete" }), &db0()));
        assert_eq!(r, Err("artefact-store: op 'list'|'get' is required".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }
}
