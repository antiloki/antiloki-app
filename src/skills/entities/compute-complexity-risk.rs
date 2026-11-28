//! compute-complexity-risk.rs — twin of skills/entities/compute-complexity-risk.mjs (db READ
//! lane + REAL filesystem reads). NAMES ARE CONTRACT: `computeComplexityRisk` (default),
//! `bucketComplexity` (named, no-db helper). Deterministic counterpart to
//! board-keeper.card.score (which uses AI): log-bucketed LoC + max file-sensitivity weight.
//!
//! REAL FS SIDE EFFECT — NOT CONFORM-DB FIXTURABLE: the JS reads `existsSync`/`statSync`/
//! `readFileSync` for every `files_touched` entry, resolved against `REPO_ROOT` (three levels up
//! from the module's own file — the workspace root housing antiloki-core / antiloki-crossing /
//! antiloki-core-rust as siblings). The conform-db harness only seeds/compares db state + the
//! JSON return value; it has no way to seed real files on disk, so this unit is verified via a
//! bespoke #[cfg(test)] Rust-native test that writes real temp files under the resolved repo
//! root and cleans them up (mirrors the god-driver-resume.rs / source-list.rs precedents for
//! real-fs-anchored twins). See the manifest: testing_lane = "bespoke-native".
//!
//! `fileSensitivityMatch` reuse: calls the already-converted twin
//! `crate::entities_file_sensitivity::fileSensitivityMatch` directly, matching the JS import.
// QUALITY: 78/100 — log-bucketed complexity (file count + LoC/200), MAX sensitivity weight (JS
// `sens.max_weight || 1` floors a real 0-weight to 1 for the running max only — `per_file`
// entries keep the raw weight), priority rounded half-up + clamped to 1..5, score = risk *
// complexity * priority (null when priority is unset/non-finite).
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// JS: REPO_ROOT = resolve(HERE, '..', '..', '..'), HERE = dirname(this module) =
// antiloki-core/skills/entities. Three '..' lands on antiloki-core's PARENT (the workspace
// root). CARGO_MANIFEST_DIR here is antiloki-core-rust; its parent is that same root on this
// checkout layout (source-list.rs precedent for the identical sibling-crate anchor).
fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .to_path_buf()
}

fn bucket_complexity_core(file_count: f64, loc_total: f64) -> i64 {
    let score = file_count + loc_total / 200.0;
    if score < 2.0 {
        1
    } else if score < 8.0 {
        2
    } else if score < 25.0 {
        3
    } else if score < 75.0 {
        4
    } else {
        5
    }
}

// Named export, no-db helper lane: fn(a, b) -> Value.
pub fn bucketComplexity(file_count: &Value, loc_total: &Value) -> Value {
    let fc = file_count.as_f64().unwrap_or(0.0);
    let lt = loc_total.as_f64().unwrap_or(0.0);
    json!(bucket_complexity_core(fc, lt))
}

pub fn computeComplexityRisk(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: if (!db) throw err.contextMissing('compute-complexity-risk', 'db') — unreachable here,
    // the db lane always injects a live Connection.
    let files_touched: &[Value] = match input.get("files_touched") {
        None => &[], // JS destructure default `files_touched = []` fires only when the key is absent/undefined
        Some(Value::Array(a)) => a.as_slice(),
        _ => return Err("compute-complexity-risk: files_touched (array) is required".into()),
    };
    let priority_val = input.get("priority").cloned().unwrap_or(Value::Null);

    let root = repo_root();
    let mut loc_total: i64 = 0;
    let mut max_risk: i64 = 1;
    let mut file_signals: Vec<Value> = Vec::with_capacity(files_touched.len());
    for rel_v in files_touched {
        let rel = rel_v.as_str().unwrap_or(""); // contract: files_touched entries are strings
        let abs = root.join(rel);
        let mut loc: i64 = 0;
        let mut exists = false;
        // JS: existsSync(abs) && statSync(abs).isFile(), then readFileSync inside a try/catch
        // that silently skips on any error (permissions, race). `Path::is_file` already returns
        // false for a missing path or a directory, and `read_to_string`'s Err is dropped below —
        // together they reproduce the same "skip on any failure" behavior.
        if abs.is_file() {
            if let Ok(content) = std::fs::read_to_string(&abs) {
                loc = content.split('\n').count() as i64;
                exists = true;
                loc_total += loc;
            }
        }
        let sens = crate::entities_file_sensitivity::fileSensitivityMatch(&json!({ "path": rel }), db)?;
        let raw_weight = sens.get("max_weight").cloned().unwrap_or(json!(0));
        let raw_weight_i64 = raw_weight.as_i64().unwrap_or(0);
        let effective = if raw_weight_i64 != 0 { raw_weight_i64 } else { 1 }; // JS `sens.max_weight || 1`
        if effective > max_risk {
            max_risk = effective;
        }

        let mut sig = Map::new();
        sig.insert("file".into(), rel_v.clone());
        sig.insert("exists".into(), json!(exists));
        sig.insert("loc".into(), json!(loc));
        sig.insert("sensitivity_weight".into(), raw_weight); // raw weight, NOT the ||1-floored value above
        file_signals.push(Value::Object(sig));
    }

    let complexity = bucket_complexity_core(files_touched.len() as f64, loc_total as f64);
    let risk = max_risk;

    let final_priority: Option<i64> = match &priority_val {
        // JS: (priority != null && Number.isFinite(priority)) — Number.isFinite does NOT coerce,
        // so any non-number JSON type (string/bool/array/object) is treated as not-finite → null.
        Value::Number(n) => n.as_f64().filter(|f| f.is_finite()).map(|p| {
            let rounded = crate::js_math::js_round(p);
            rounded.max(1.0).min(5.0) as i64
        }),
        _ => None,
    };

    let score: Value = match final_priority {
        Some(fp) => json!(risk * complexity * fp),
        None => Value::Null,
    };

    let mut signals = Map::new();
    signals.insert("file_count".into(), json!(files_touched.len() as i64));
    signals.insert("loc_total".into(), json!(loc_total));
    signals.insert("max_sensitivity".into(), json!(max_risk));
    signals.insert("per_file".into(), Value::Array(file_signals));

    let mut out = Map::new();
    out.insert("risk".into(), json!(risk));
    out.insert("complexity".into(), json!(complexity));
    out.insert(
        "priority".into(),
        final_priority.map(|p| json!(p)).unwrap_or(Value::Null),
    );
    out.insert("score".into(), score);
    out.insert("signals".into(), Value::Object(signals));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE file_sensitivity (path_glob TEXT, risk_weight INTEGER, description TEXT); \
             INSERT INTO file_sensitivity VALUES ('src/**', 2, 'source'), ('src/auth/*.ts', 4, 'auth');",
        )
        .unwrap();
        c
    }

    fn temp_subdir(tag: &str) -> std::path::PathBuf {
        let dir = repo_root().join(format!(".tmp-ccr-rs-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn bucket_complexity_boundaries() {
        assert_eq!(bucket_complexity_core(0.0, 0.0), 1); // score 0 < 2
        assert_eq!(bucket_complexity_core(1.0, 199.0), 1); // score 1.995 < 2
        assert_eq!(bucket_complexity_core(2.0, 0.0), 2); // score 2 not< 2, < 8
        assert_eq!(bucket_complexity_core(0.0, 1400.0), 2); // score 7 < 8
        assert_eq!(bucket_complexity_core(8.0, 0.0), 3); // score 8 not< 8, < 25
        assert_eq!(bucket_complexity_core(25.0, 0.0), 4); // score 25 not< 25, < 75
        assert_eq!(bucket_complexity_core(75.0, 0.0), 5); // score 75 not< 75
        assert_eq!(bucket_complexity_core(1000.0, 1000.0), 5);
        assert_eq!(bucketComplexity(&json!(2), &json!(0)), json!(2));
    }

    #[test]
    fn non_array_files_touched_errs() {
        let c = db0();
        assert!(computeComplexityRisk(&json!({ "files_touched": "nope" }), &c).is_err());
    }

    #[test]
    fn empty_files_touched_baseline() {
        let c = db0();
        let r = computeComplexityRisk(&json!({ "files_touched": [] }), &c).unwrap();
        assert_eq!(
            r,
            json!({
                "risk": 1, "complexity": 1, "priority": null, "score": null,
                "signals": { "file_count": 0, "loc_total": 0, "max_sensitivity": 1, "per_file": [] }
            })
        );
    }

    #[test]
    fn missing_files_touched_key_defaults_to_empty_array() {
        let c = db0();
        let r = computeComplexityRisk(&json!({}), &c).unwrap();
        assert_eq!(r["signals"]["file_count"], json!(0));
    }

    #[test]
    fn reads_real_files_for_loc_and_existence() {
        let c = db0();
        let dir = temp_subdir("read");
        fs::write(dir.join("three.txt"), "line1\nline2\nline3").unwrap(); // 3 lines, no trailing newline
        fs::write(dir.join("two.txt"), "a\nb\n").unwrap(); // trailing newline -> split gives 3 elems
        let rel_dir = dir.file_name().unwrap().to_str().unwrap().to_string();
        let r = computeComplexityRisk(
            &json!({ "files_touched": [
                format!("{rel_dir}/three.txt"), format!("{rel_dir}/two.txt"), format!("{rel_dir}/missing.txt")
            ] }),
            &c,
        )
        .unwrap();
        assert_eq!(r["signals"]["file_count"], json!(3));
        assert_eq!(r["signals"]["loc_total"], json!(6));
        let per_file = r["signals"]["per_file"].as_array().unwrap();
        assert_eq!(per_file[0]["exists"], json!(true));
        assert_eq!(per_file[0]["loc"], json!(3));
        assert_eq!(per_file[1]["exists"], json!(true));
        assert_eq!(per_file[1]["loc"], json!(3));
        assert_eq!(per_file[2]["exists"], json!(false));
        assert_eq!(per_file[2]["loc"], json!(0));
        assert_eq!(r["complexity"], json!(2)); // 3 files + 6/200 = 3.03 -> not< 2, < 8
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn risk_is_max_sensitivity_zero_weight_floors_to_one_for_running_max_only() {
        let c = db0();
        let r = computeComplexityRisk(
            &json!({ "files_touched": ["README.md", "src/auth/login.ts", "src/x.ts"] }),
            &c,
        )
        .unwrap();
        assert_eq!(r["risk"], json!(4)); // src/auth/login.ts matches src/** (2) and src/auth/*.ts (4)
        let per_file = r["signals"]["per_file"].as_array().unwrap();
        assert_eq!(per_file[0]["file"], json!("README.md"));
        assert_eq!(per_file[0]["sensitivity_weight"], json!(0)); // raw weight kept as 0
        assert_eq!(r["signals"]["max_sensitivity"], json!(4)); // running max floors 0 to 1 internally
    }

    #[test]
    fn priority_rounded_half_up_and_clamped() {
        let c = db0();
        let p = |v: Value| computeComplexityRisk(&json!({ "files_touched": [], "priority": v }), &c).unwrap()["priority"].clone();
        assert_eq!(p(json!(3)), json!(3));
        assert_eq!(p(json!(3.5)), json!(4));
        assert_eq!(p(json!(0)), json!(1));
        assert_eq!(p(json!(-5)), json!(1));
        assert_eq!(p(json!(10)), json!(5));
        assert_eq!(p(json!("3")), Value::Null); // no coercion — Number.isFinite("3") is false
        assert_eq!(
            computeComplexityRisk(&json!({ "files_touched": [] }), &c).unwrap()["priority"],
            Value::Null
        ); // key absent
    }

    #[test]
    fn score_is_risk_times_complexity_times_priority_only_when_priority_set() {
        let c = db0();
        let no_priority = computeComplexityRisk(&json!({ "files_touched": ["src/auth/login.ts"] }), &c).unwrap();
        assert_eq!(no_priority["score"], Value::Null);
        let with_priority =
            computeComplexityRisk(&json!({ "files_touched": ["src/auth/login.ts"], "priority": 2 }), &c).unwrap();
        assert_eq!(with_priority["risk"], json!(4));
        assert_eq!(with_priority["complexity"], json!(1));
        assert_eq!(with_priority["priority"], json!(2));
        assert_eq!(with_priority["score"], json!(4 * 1 * 2));
    }
}
