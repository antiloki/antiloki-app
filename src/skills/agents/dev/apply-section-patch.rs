//! apply-section-patch.rs — twin of skills/agents/dev/apply-section-patch.mjs (no-db, pure). NAMES ARE
//! CONTRACT: applySectionPatch (default) · applyPatchToSource. Anchor-based deterministic file splicer.
// QUALITY: 82/100 — verbatim-then-whitespace-flexible anchor locate, create/insert_after/replace_section
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

fn anchor_to_flexible_regex(anchor: &str) -> Option<Regex> {
    let parts: Vec<&str> = anchor.split(|c: char| c.is_whitespace()).filter(|s| !s.is_empty()).collect();
    if parts.is_empty() { return None; }
    static SP: OnceLock<Regex> = OnceLock::new();
    let sp = SP.get_or_init(|| Regex::new(r"[.*+?^${}()|\[\]\\]").unwrap());
    let escaped: Vec<String> = parts.iter().map(|s| sp.replace_all(s, |c: &regex::Captures| format!("\\{}", &c[0])).into_owned()).collect();
    Regex::new(&escaped.join(r"\s+")).ok()
}

enum Anchor { Found(usize, usize), Ambiguous, Missing }
fn find_anchor(source: &str, anchor: &str, from_index: usize, unique: bool) -> Anchor {
    let hay = if from_index > 0 { &source[from_index..] } else { source };
    if let Some(i) = hay.find(anchor) {
        if unique && hay.get(i + 1..).map_or(false, |rest| rest.contains(anchor)) { return Anchor::Ambiguous; }
        return Anchor::Found(from_index + i, from_index + i + anchor.len());
    }
    let re = match anchor_to_flexible_regex(anchor) { Some(r) => r, None => return Anchor::Missing };
    let mut it = re.find_iter(hay);
    match it.next() {
        None => Anchor::Missing,
        Some(m0) => {
            let (s, e) = (from_index + m0.start(), from_index + m0.end());
            if !unique { return Anchor::Found(s, e); }
            if it.next().is_some() { Anchor::Ambiguous } else { Anchor::Found(s, e) }
        }
    }
}

// Ok(new content) | Err(cause) — the Err path throws in JS (error-message-fragile, not fixtured).
fn apply_patch_impl(source: &str, patch: &Value) -> Result<String, String> {
    let op = patch.get("op").and_then(|v| v.as_str()).unwrap_or("replace_section");
    let replacement = patch.get("replacement").and_then(|v| v.as_str());

    if op == "create" {
        return match replacement { Some(r) => Ok(r.to_string()), None => Err("op=create requires replacement to be a string (the whole file)".into()) };
    }
    let replacement = match replacement { Some(r) => r, None => return Err(format!("op={} requires replacement to be a string", op)) };

    if op == "insert_after" {
        let anchor_before = match patch.get("anchor_before").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) { Some(a) => a, None => return Err("op=insert_after requires anchor_before (a unique string in the file)".into()) };
        match find_anchor(source, anchor_before, 0, true) {
            Anchor::Missing => Err(format!("anchor_before not found in file: '{}...'", anchor_before.chars().take(60).collect::<String>())),
            Anchor::Ambiguous => Err("anchor_before is ambiguous (matches multiple places). Extend the anchor with more surrounding lines.".into()),
            Anchor::Found(_, end) => Ok(format!("{}{}{}", &source[..end], replacement, &source[end..])),
        }
    } else {
        // replace_section
        let anchor_before = patch.get("anchor_before").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
        let anchor_after = patch.get("anchor_after").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
        let (ab, aa) = match (anchor_before, anchor_after) {
            (Some(b), Some(a)) => (b, a),
            _ => return Err("op=replace_section requires both anchor_before AND anchor_after (unique strings bracketing the change)".into()),
        };
        let section_start = match find_anchor(source, ab, 0, true) {
            Anchor::Missing => return Err(format!("anchor_before not found in file: '{}...'", ab.chars().take(60).collect::<String>())),
            Anchor::Ambiguous => return Err("anchor_before is ambiguous. Make it more specific.".into()),
            Anchor::Found(_, end) => end,
        };
        match find_anchor(source, aa, section_start, false) {
            Anchor::Missing | Anchor::Ambiguous => Err(format!("anchor_after not found after anchor_before: '{}...'", aa.chars().take(60).collect::<String>())),
            Anchor::Found(start, _) => Ok(format!("{}{}{}", &source[..section_start], replacement, &source[start..])),
        }
    }
}

pub fn applyPatchToSource(source: &Value, patch: &Value) -> Value {
    match apply_patch_impl(source.as_str().unwrap_or(""), patch) { Ok(s) => json!(s), Err(_) => Value::Null }
}

pub fn applySectionPatch(input: &Value, _db: &Connection) -> Result<Value, String> {
    let source = match input.get("source").and_then(|v| v.as_str()) { Some(s) => s, None => return Err("apply-section-patch: 'source' is required".into()) };
    let patches = match input.get("patches").and_then(|v| v.as_array()) { Some(p) => p, None => return Err("apply-section-patch: 'patches' is required".into()) };
    let mut current = source.to_string();
    let mut outcomes: Vec<Value> = Vec::new();
    for p in patches {
        let op_out = nz(p.get("op")).cloned().unwrap_or(json!("replace_section"));
        match apply_patch_impl(&current, p) {
            Ok(s) => { current = s; outcomes.push(json!({ "ok": true, "op": op_out })); }
            Err(e) => return Err(e), // fail fast (JS records the error outcome then rethrows — the return never happens)
        }
    }
    Ok(json!({ "content": current, "outcomes": outcomes }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn splices() {
        // replace_section between verbatim anchors
        let src = "line 1\nFOO\nold content\nBAR\nline 5";
        assert_eq!(applyPatchToSource(&json!(src), &json!({ "op": "replace_section", "anchor_before": "FOO\n", "anchor_after": "\nBAR", "replacement": "new stuff" })), json!("line 1\nFOO\nnew stuff\nBAR\nline 5"));
        // insert_after
        assert_eq!(applyPatchToSource(&json!("a\nMARK\nb"), &json!({ "op": "insert_after", "anchor_before": "MARK", "replacement": "-INS" })), json!("a\nMARK-INS\nb"));
        // create — replacement is the whole file
        assert_eq!(applyPatchToSource(&json!(""), &json!({ "op": "create", "replacement": "brand new" })), json!("brand new"));
        // whitespace-flexible: source has different spacing than the anchor
        assert_eq!(applyPatchToSource(&json!("x\nlet   a = 1;\ny"), &json!({ "op": "insert_after", "anchor_before": "let a = 1;", "replacement": "!" })), json!("x\nlet   a = 1;!\ny"));
        // default: apply a sequence
        let r = applySectionPatch(&json!({ "source": "a\nMARK\nb", "patches": [{ "op": "insert_after", "anchor_before": "MARK", "replacement": "1" }] }), &Connection::open_in_memory().unwrap()).unwrap();
        assert_eq!(r["content"], json!("a\nMARK1\nb"));
        assert_eq!(r["outcomes"][0], json!({ "ok": true, "op": "insert_after" }));
    }
}
