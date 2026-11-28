//! embed.rs — twin of skills/external/embed.mjs. NAMES ARE CONTRACT.
//!
//! Crossed for real: `DEFAULT_MODEL` (the model id constant) and `embedModelCached` (the fs
//! probe deciding whether the local ONNX cache already holds the model — used by implicit
//! auto-sweep callers to fail closed instead of triggering a ~140MB download mid-pipeline).
//!
//! `embed` is REAL as of 2026-08-09, mirrored via `fastembed` (ONNX Runtime + tokenizers): the SAME
//! bge-base-en-v1.5 weights, the same mean-pooling + L2-normalize, downloaded and cached on first
//! use exactly as transformers.js does — so vectors are COMPARABLE across the two engines, not
//! merely both 768 floats. Previously deferred as: `@huggingface/transformers` feature-extraction
//! pipeline (mean pooling + L2 normalize over a 768-dim ONNX/WASM model) — which was true of
//! `transformers.js` specifically, not of ML inference in Rust.

use serde_json::Value;
use std::path::{Path, PathBuf};

pub const DEFAULT_MODEL: &str = "Xenova/bge-base-en-v1.5";

/// `err.required('embed', field)` message shape: `${skill}: ${field} is required`.
fn err_required(field: &str) -> String {
    format!("embed: {field} is required")
}

#[cfg(windows)]
fn homedir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("USERPROFILE") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let drive = std::env::var("HOMEDRIVE").ok().filter(|s| !s.is_empty());
    let path = std::env::var("HOMEPATH").ok().filter(|s| !s.is_empty());
    match (drive, path) {
        (Some(d), Some(p)) => Some(PathBuf::from(format!("{d}{p}"))),
        _ => None,
    }
}

#[cfg(not(windows))]
fn homedir() -> Option<PathBuf> {
    std::env::var("HOME").ok().filter(|s| !s.is_empty()).map(PathBuf::from)
}

/// JS: `dirname(createRequire(import.meta.url).resolve('@huggingface/transformers/package.json'))`
/// — Node's CommonJS resolver walks UP from the calling module's directory through each
/// ancestor's `node_modules/` looking for the package. Mirrored here by walking up from this
/// crate's manifest directory (the closest stand-in for "this module's location" in a compiled
/// binary) checking `<ancestor>/node_modules/@huggingface/transformers/package.json`.
fn resolve_pkg_root(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let candidate = dir
            .join("node_modules")
            .join("@huggingface")
            .join("transformers")
            .join("package.json");
        if candidate.exists() {
            return candidate.parent().map(Path::to_path_buf);
        }
    }
    None
}

/// Builds the same candidate list as the JS `candidates` array, in the same order, given the
/// (possibly absent) resolved package root and the (possibly overridden) HF_HOME directory.
fn candidate_paths(model: &str, pkg_root: Option<&Path>, hf_home: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(root) = pkg_root {
        candidates.push(root.join(".cache").join(model));
    }
    candidates.push(hf_home.join("hub").join(format!("models--{}", model.replace('/', "--"))));
    candidates.push(hf_home.join(model));
    candidates
}

/// Cheap fs probe: is the model ALREADY cached locally? See module doc + JS doc comment for the
/// fail-closed rationale. `existsSync` swallowing its own error (JS `try { existsSync } catch`)
/// is a no-op port here: `Path::exists()` never throws — it returns `false` on any stat error.
pub fn embedModelCached(model: &str) -> bool {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let pkg_root = resolve_pkg_root(&manifest_dir);
    let hf_home = match std::env::var("HF_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".cache").join("huggingface"),
    };
    candidate_paths(model, pkg_root.as_deref(), &hf_home)
        .iter()
        .any(|p| p.exists())
}

/// Process-wide extractor cache — the Rust counterpart of JS's module-level `extractors` Map.
/// The model load is the expensive part (~2-5s cold); every later call is warm.
static EXTRACTORS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<std::sync::Mutex<fastembed::TextEmbedding>>>>> =
    std::sync::OnceLock::new();

/// Map a JS model id to fastembed's enum. Only the models this substrate actually names are
/// mapped — an unknown id is refused by name rather than silently substituted, because returning
/// a vector from a DIFFERENT model than the caller asked for would poison the embeddings table
/// with mixed-geometry rows that nothing downstream could detect.
fn model_for(id: &str) -> Result<fastembed::EmbeddingModel, String> {
    match id {
        "Xenova/bge-base-en-v1.5" | "BAAI/bge-base-en-v1.5" => Ok(fastembed::EmbeddingModel::BGEBaseENV15),
        "Xenova/bge-small-en-v1.5" | "BAAI/bge-small-en-v1.5" => Ok(fastembed::EmbeddingModel::BGESmallENV15),
        "Xenova/bge-large-en-v1.5" | "BAAI/bge-large-en-v1.5" => Ok(fastembed::EmbeddingModel::BGELargeENV15),
        other => Err(format!(
            "embed: model '{other}' is not mapped in the Rust twin (mapped: bge-{{small,base,large}}-en-v1.5)"
        )),
    }
}

/// Where downloaded weights live. Honors `HF_HOME` like the JS side, so a machine that already
/// has the model cached for transformers.js does not download it twice.
fn weights_dir(model_id: &str) -> PathBuf {
    let hf = match std::env::var("HF_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".cache").join("huggingface"),
    };
    hf.join("antiloki-fastembed").join(model_id.replace('/', "--"))
}

/// Fetch one file, caching it on disk. Uses this crate's own `ureq` (with `tls`) rather than
/// fastembed's bundled `hf-hub` downloader: that downloader failed opaquely here
/// ("Failed to retrieve onnx/model.onnx") on a machine where the very same URL returns 200, and a
/// hidden HTTP stack inside a dependency is a bad place for the model supply chain to live.
/// Fetching it ourselves also makes the source URL explicit and auditable.
fn fetch_cached(model_id: &str, rel: &str) -> Result<Vec<u8>, String> {
    let dest = weights_dir(model_id).join(rel.replace('/', "__"));
    if let Ok(b) = std::fs::read(&dest) {
        if !b.is_empty() {
            return Ok(b);
        }
    }
    let url = format!("https://huggingface.co/{model_id}/resolve/main/{rel}");
    let resp = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(600))
        .call()
        .map_err(|e| format!("embed: fetching {url}: {e}"))?;
    let mut buf: Vec<u8> = Vec::new();
    std::io::Read::read_to_end(&mut resp.into_reader(), &mut buf)
        .map_err(|e| format!("embed: reading {url}: {e}"))?;
    if let Some(parent) = dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&dest, &buf);
    Ok(buf)
}

/// Load a model by fetching its ONNX + tokenizer files ourselves and handing fastembed the bytes.
fn load_model(model_id: &str, _variant: fastembed::EmbeddingModel) -> Result<fastembed::TextEmbedding, String> {
    // HERMETICITY GUARD. `cargo test` must never pull ~140MB off the network: a unit suite that
    // downloads is neither reproducible nor offline-runnable, and several caller tests
    // (interviewer-embed-turn, interviewer-turn) deliberately assert that an embed FAILURE is
    // non-fatal — they would otherwise start doing real inference. Under test the download is
    // refused unless the weights are already on disk, or ANTILOKI_EMBED_ALLOW_DOWNLOAD is set
    // (which the #[ignore] live test does).
    // Deliberately NOT conditioned on the cache: once a developer has run the live test the
    // weights exist, and a cache-conditional guard would make the caller suites pass or fail
    // depending on whether that had happened — the worst kind of flake. Under test, embedding is
    // always refused unless explicitly opted in.
    if cfg!(test) && std::env::var("ANTILOKI_EMBED_ALLOW_DOWNLOAD").is_err() {
        return Err(format!(
            "embed: inference for '{model_id}' is disabled under test \
             (set ANTILOKI_EMBED_ALLOW_DOWNLOAD=1 to permit)"
        ));
    }
    let onnx = fetch_cached(model_id, "onnx/model.onnx")?;
    let tokenizer_files = fastembed::TokenizerFiles {
        tokenizer_file: fetch_cached(model_id, "tokenizer.json")?,
        config_file: fetch_cached(model_id, "config.json")?,
        special_tokens_map_file: fetch_cached(model_id, "special_tokens_map.json")?,
        tokenizer_config_file: fetch_cached(model_id, "tokenizer_config.json")?,
    };
    // Mean pooling — JS passes `{ pooling: 'mean', normalize: true }`; fastembed normalizes by
    // default, so only the pooling has to be stated.
    let mut udm = fastembed::UserDefinedEmbeddingModel::new(onnx, tokenizer_files);
    udm.pooling = Some(fastembed::Pooling::Mean);
    fastembed::TextEmbedding::try_new_from_user_defined(udm, Default::default())
        .map_err(|e| format!("embed: could not load model '{model_id}': {e}"))
}

/// `export default async function embed(input, _context)` — MIRRORED, not translated.
///
/// JS runs `@huggingface/transformers`' feature-extraction pipeline with
/// `{ pooling: 'mean', normalize: true }`. `fastembed` is the Rust-native equivalent: the SAME
/// ONNX weights (bge-base-en-v1.5), the same mean-pooling + L2-normalize, downloaded and cached on
/// first use exactly as transformers.js does. So the vectors are comparable across the two engines
/// rather than merely both being "768 floats".
///
/// The first call pays the model load; `EXTRACTORS` caches it per model for the process, mirroring
/// JS's module-level Map.
pub fn embed(input: &Value, _context: &Value) -> Result<Value, String> {
    let text = match input.get("text").and_then(Value::as_str) {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => return Err(err_required("text")),
    };
    let model_id = input.get("model").and_then(Value::as_str).unwrap_or(DEFAULT_MODEL).to_string();
    let variant = model_for(&model_id)?;

    let cache = EXTRACTORS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let extractor = {
        let mut guard = cache.lock().map_err(|_| "embed: extractor cache poisoned".to_string())?;
        match guard.get(&model_id) {
            Some(e) => e.clone(),
            None => {
                let te = load_model(&model_id, variant)?;
                let arc = std::sync::Arc::new(std::sync::Mutex::new(te));
                guard.insert(model_id.clone(), arc.clone());
                arc
            }
        }
    };

    let mut te = extractor.lock().map_err(|_| "embed: extractor poisoned".to_string())?;
    let vectors = te
        .embed(vec![text], None)
        .map_err(|e| format!("embed: inference failed: {e}"))?;
    let v = vectors
        .into_iter()
        .next()
        .ok_or_else(|| "embed: model returned no vector".to_string())?;

    Ok(serde_json::json!({
        "model": model_id,
        "dims": v.len(),
        "vector": v,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    // SAFETY: HF_HOME is process-global; serialize with the shared crate-wide lock, same
    // convention as auth-init.rs / card-runtime-state.rs.
    fn with_hf_home<T>(hf_home: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("HF_HOME", hf_home);
        }
        let result = f();
        unsafe {
            std::env::remove_var("HF_HOME");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-embed-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn default_model_matches_js() {
        assert_eq!(DEFAULT_MODEL, "Xenova/bge-base-en-v1.5");
    }

    #[test]
    fn candidate_paths_orders_pkg_then_hub_then_flat() {
        let pkg_root = PathBuf::from("/pkg");
        let hf_home = PathBuf::from("/hf");
        let got = candidate_paths("Xenova/bge-base-en-v1.5", Some(&pkg_root), &hf_home);
        assert_eq!(
            got,
            vec![
                PathBuf::from("/pkg/.cache/Xenova/bge-base-en-v1.5"),
                PathBuf::from("/hf/hub/models--Xenova--bge-base-en-v1.5"),
                PathBuf::from("/hf/Xenova/bge-base-en-v1.5"),
            ]
        );
    }

    #[test]
    fn candidate_paths_replaces_all_slashes_in_hub_layout() {
        let hf_home = PathBuf::from("/hf");
        let got = candidate_paths("org/sub/model", None, &hf_home);
        assert_eq!(got[0], PathBuf::from("/hf/hub/models--org--sub--model"));
    }

    #[test]
    fn cached_true_when_hub_layout_dir_exists() {
        let dir = temp_dir("hub-hit");
        let model = "Xenova/bge-base-en-v1.5";
        let hub_dir = dir.join("hub").join(format!("models--{}", model.replace('/', "--")));
        fs::create_dir_all(&hub_dir).unwrap();

        let got = with_hf_home(dir.to_str().unwrap(), || embedModelCached(model));
        assert!(got);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cached_true_when_flat_layout_dir_exists() {
        let dir = temp_dir("flat-hit");
        let model = "Xenova/bge-base-en-v1.5";
        let flat_dir = dir.join(model);
        fs::create_dir_all(&flat_dir).unwrap();

        let got = with_hf_home(dir.to_str().unwrap(), || embedModelCached(model));
        assert!(got);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cached_false_when_nothing_exists() {
        let dir = temp_dir("miss");
        let got = with_hf_home(dir.to_str().unwrap(), || embedModelCached("no/such-model"));
        assert!(!got);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_pkg_root_none_when_not_present() {
        let dir = temp_dir("no-pkg");
        assert_eq!(resolve_pkg_root(&dir), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_pkg_root_finds_ancestor_node_modules() {
        let dir = temp_dir("has-pkg");
        let nested = dir.join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        let pkg_dir = dir.join("node_modules").join("@huggingface").join("transformers");
        fs::create_dir_all(&pkg_dir).unwrap();
        fs::write(pkg_dir.join("package.json"), "{}").unwrap();

        let got = resolve_pkg_root(&nested);
        assert_eq!(got, Some(pkg_dir));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn embed_errs_missing_text() {
        let err = embed(&json!({}), &Value::Null).unwrap_err();
        assert_eq!(err, "embed: text is required");
    }

    #[test]
    fn embed_errs_empty_text() {
        let err = embed(&json!({ "text": "" }), &Value::Null).unwrap_err();
        assert_eq!(err, "embed: text is required");
    }

    #[test]
    fn embed_errs_non_string_text() {
        let err = embed(&json!({ "text": 5 }), &Value::Null).unwrap_err();
        assert_eq!(err, "embed: text is required");
    }

    #[test]
    fn an_unmapped_model_is_refused_by_name_not_silently_substituted() {
        // Returning a vector from a DIFFERENT model would poison the embeddings table with
        // mixed-geometry rows nothing downstream could detect — so this must be an error.
        let e = embed(&json!({ "text": "hi", "model": "some/other-model" }), &Value::Null).unwrap_err();
        assert!(e.contains("is not mapped"), "{e}");
        assert!(e.contains("some/other-model"), "the offending id is echoed: {e}");
    }

    #[test]
    fn the_three_bge_sizes_map_under_both_the_xenova_and_baai_ids() {
        for id in ["Xenova/bge-base-en-v1.5", "BAAI/bge-base-en-v1.5",
                   "Xenova/bge-small-en-v1.5", "BAAI/bge-small-en-v1.5",
                   "Xenova/bge-large-en-v1.5", "BAAI/bge-large-en-v1.5"] {
            assert!(model_for(id).is_ok(), "{id} must map");
        }
        assert_eq!(DEFAULT_MODEL, "Xenova/bge-base-en-v1.5");
        assert!(model_for(DEFAULT_MODEL).is_ok(), "the default must always be mapped");
    }

    /// LIVE inference. `#[ignore]` because the first run downloads ~140MB of ONNX weights — the
    /// suite must stay hermetic and offline. Run deliberately:
    ///   cargo test --lib external_embed -- --ignored --nocapture
    #[test]
    #[ignore = "downloads ~140MB of model weights on first run"]
    fn live_embedding_is_768_dims_and_l2_normalized() {
        let _g = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("ANTILOKI_EMBED_ALLOW_DOWNLOAD", "1") };
        let r = embed(&json!({ "text": "the capital of France is Paris" }), &Value::Null)
            .expect("live embed");
        assert_eq!(r["model"], json!(DEFAULT_MODEL));
        assert_eq!(r["dims"], json!(768), "bge-base is 768-dim");
        let v: Vec<f64> = r["vector"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        assert_eq!(v.len(), 768);
        // normalize:true in JS means an L2 norm of 1 — the property downstream cosine similarity
        // depends on. If this drifts, every stored vector silently changes meaning.
        let norm: f64 = v.iter().map(|x| x * x).sum::<f64>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3, "expected an L2-normalized vector, got norm {norm}");

        // and it must be deterministic for the same text
        let again = embed(&json!({ "text": "the capital of France is Paris" }), &Value::Null).unwrap();
        assert_eq!(again["vector"], r["vector"], "same text must yield the same vector");
    }
}
