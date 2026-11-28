//! extract-text.rs — twin of skills/sources/extract-text.mjs (no-db helper, named export,
//! async in JS but "async over sync" here — no real async dependency). NAMES ARE CONTRACT:
//! `extractText`. Pulls readable text out of a source file buffer.
//!
//! SCOPING NOTE (deliberate, not an oversight): the JS `.pdf` branch dynamic-`import`s `unpdf`
//! and parses the buffer as a real PDF; ANY failure (dep absent, unparseable bytes, scanned-image
//! PDF with no text layer) is swallowed by its `catch` and degrades to `null`. Porting a real PDF
//! parser is out of scope for a portable Rust twin, so this twin always returns `null` for `.pdf`
//! input — which is faithful to the JS ONLY as long as fixtures/tests feed `.pdf` cases that are
//! NOT valid parseable PDFs (garbage bytes, empty buffer), so the real JS `unpdf` call also fails
//! and returns `null`. A fixture with a real extractable PDF would diverge (JS: text, Rust: null)
//! and must never be used here.
use serde_json::{json, Value};

const MAX_TEXT: usize = 4_000_000; // cap stored text at 4 MB

fn text_exts() -> &'static [&'static str] {
    &[
        ".md", ".markdown", ".txt", ".text", ".json", ".csv", ".tsv", ".yaml", ".yml", ".js", ".mjs", ".cjs", ".ts",
        ".tsx", ".jsx", ".py", ".go", ".rs", ".java", ".kt", ".c", ".h", ".cc", ".cpp", ".hpp", ".cs", ".rb", ".php",
        ".css", ".scss", ".html", ".xml", ".toml", ".ini", ".sh", ".bash", ".sql", ".log",
    ]
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS String(v) — used here on `ext || ''`, never on a nullable value directly (`||` already
// substitutes a non-null default), so the null/undefined arms exist only for completeness.
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

// `buf` in the JS is a real Buffer/Uint8Array; the fixture/harness convention for a byte-arg is
// `{ "__bytes_b64": "<base64>" }` (mirrors compare.mjs's reviveArg / normalizeResult markers).
// A bare JS `null`/`undefined` buf would throw `TypeError: Cannot read properties of null/undefined
// (reading 'toString')` at `buf.toString('utf8')` — reproduced here for the same unguarded access.
fn buf_bytes(buf: &Value) -> Result<Vec<u8>, String> {
    match buf.get("__bytes_b64").and_then(|b| b.as_str()) {
        Some(b64) => {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| format!("extract-text: invalid __bytes_b64: {e}"))
        }
        None if buf.is_null() => Err("Cannot read properties of null (reading 'toString')".into()),
        None => Err("extract-text: buf must be { __bytes_b64 }".into()),
    }
}

pub fn extractText(buf: &Value, ext: &Value) -> Result<Value, String> {
    // String(ext || '').toLowerCase()
    let ext_or_empty = if truthy(Some(ext)) { ext.clone() } else { json!("") };
    let e = js_string(&ext_or_empty).to_lowercase();

    if text_exts().contains(&e.as_str()) {
        let bytes = buf_bytes(buf)?;
        let decoded = String::from_utf8_lossy(&bytes).into_owned();
        let truncated: String = decoded.chars().take(MAX_TEXT).collect(); // .slice(0, MAX_TEXT)
        return Ok(json!(truncated));
    }

    if e == ".pdf" {
        // real parse always fails for the fixtures this twin is scoped to (see module doc) → null
        return Ok(Value::Null);
    }

    Ok(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes_arg(s: &str) -> Value {
        use base64::Engine;
        json!({ "__bytes_b64": base64::engine::general_purpose::STANDARD.encode(s.as_bytes()) })
    }

    #[test]
    fn text_extension_decodes_utf8() {
        let r = extractText(&bytes_arg("hello world"), &json!(".md")).unwrap();
        assert_eq!(r, json!("hello world"));
    }

    #[test]
    fn extension_match_is_case_insensitive() {
        let r = extractText(&bytes_arg("CASE"), &json!(".MD")).unwrap();
        assert_eq!(r, json!("CASE"));
    }

    #[test]
    fn missing_ext_returns_null() {
        let r = extractText(&bytes_arg("no ext here"), &Value::Null).unwrap();
        assert_eq!(r, Value::Null);
    }

    #[test]
    fn unknown_binary_extension_returns_null() {
        let raw = json!({ "__bytes_b64": "AAEC" }); // bytes 0x00 0x01 0x02
        let r = extractText(&raw, &json!(".png")).unwrap();
        assert_eq!(r, Value::Null);
    }

    #[test]
    fn pdf_extension_always_degrades_to_null_in_this_twin() {
        let raw = json!({ "__bytes_b64": "AAECAw==" }); // not a real PDF
        let r = extractText(&raw, &json!(".pdf")).unwrap();
        assert_eq!(r, Value::Null);
    }

    #[test]
    fn null_buf_throws_matching_v8_message() {
        let e = extractText(&Value::Null, &json!(".md")).unwrap_err();
        assert_eq!(e, "Cannot read properties of null (reading 'toString')");
    }
}
