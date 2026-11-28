//! author-form.rs — twin of skills/agents/tl/_helpers/author-form.mjs (pure, no-db). NAMES ARE
//! CONTRACT: renderAuthorForm · parseAuthorForm · formatFormErrors. MD-form compiler for the
//! per-file author call — renders a pre-filled skeleton and compiles a completed form back into
//! flat {op, anchor_before, anchor_after, code, file_declared} fields with localizable errors.
// QUALITY: 80/100 — deterministic form compiler: longest-fence rule, trailing-echo tolerance, localizable compile errors
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::OnceLock;

fn str_qq(v: Option<&Value>) -> String {
    // String(v ?? '')
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn js_string(v: &Value) -> String {
    // = JS String(v)
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
fn tmpl(v: Option<&Value>) -> String {
    // = JS `${v}`
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    // JS `??` right operand: nz(a).or(nz(b))
    v.filter(|x| !x.is_null())
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
fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
} // JS .slice(0,n) (BMP-equivalent)

const FENCE3: &str = "```";
const SECTIONS: [&str; 3] = ["ANCHOR_BEFORE", "ANCHOR_AFTER", "CODE"];

fn h1_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)^#\s+FILE\s+(.+?)\s+\u{b7}\s+(CREATE|INSERT_AFTER|REPLACE_SECTION)\s*$").unwrap())
}
fn section_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^##\s+(\S+)\s*$").unwrap())
}
fn placeholder_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^\([^)]*\)$").unwrap())
}
fn fence_open_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^(`{3,}|~{3,})\s*\S*\s*$").unwrap())
}
fn non_az_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"[^A-Z_]").unwrap())
}
fn split_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\r?\n").unwrap())
}

/// Render the blank form the model completes. Deterministic fields (path,
/// op) are PRE-FILLED — the model's freedom is exactly the blanks.
pub fn renderAuthorForm(input: &Value) -> Value {
    let opkind_raw = nz(input.get("op_kind")).cloned().unwrap_or(json!("create"));
    let op_upper = js_string(&opkind_raw).to_uppercase();
    let op = non_az_re().replace_all(&op_upper, "").to_string();
    let op_norm = if op == "CREATE" || op == "INSERT_AFTER" || op == "REPLACE_SECTION" {
        op
    } else {
        "CREATE".to_string()
    };
    let file_s = tmpl(input.get("file"));

    let anchor_before_line = if op_norm == "CREATE" {
        "(none \u{2014} create writes the whole file)".to_string()
    } else {
        "(replace this line with the EXACT anchor text copied verbatim from CURRENT SOURCE)".to_string()
    };
    let anchor_after_line = if op_norm == "REPLACE_SECTION" {
        "(replace this line with the EXACT closing anchor copied verbatim from CURRENT SOURCE)".to_string()
    } else {
        "(none)".to_string()
    };
    let code_desc = if op_norm == "CREATE" { "file content" } else { "splice code" };

    let lines = vec![
        format!("# FILE {} \u{b7} {}", file_s, op_norm),
        "## ANCHOR_BEFORE".to_string(),
        anchor_before_line,
        "## ANCHOR_AFTER".to_string(),
        anchor_after_line,
        "## CODE".to_string(),
        FENCE3.to_string(),
        format!("(the complete {} \u{2014} nothing else)", code_desc),
        FENCE3.to_string(),
    ];
    json!(lines.join("\n"))
}

struct Section {
    start: i64,
    lines: Vec<String>,
}

/// Compile a completed form back into flat fields.
/// Returns { ok: true, fields: {op, anchor_before, anchor_after, code, file_declared} }
/// or { ok: false, errors: [{ line?, heading?, problem }] } — every error
/// is localizable so the corrective re-prompt names exactly what to fix.
pub fn parseAuthorForm(md: &Value) -> Value {
    let mut errors: Vec<Value> = Vec::new();
    let full = str_qq(Some(md));
    let lines: Vec<String> = split_re().split(full.as_str()).map(|s| s.to_string()).collect();

    // H1 — first non-empty line must be the FILE header.
    let mut i: usize = 0;
    while i < lines.len() && lines[i].trim().is_empty() {
        i += 1;
    }
    let h1 = lines.get(i).cloned().unwrap_or_default();
    let caps = h1_re().captures(&h1);
    let (file_declared, op) = match caps {
        None => {
            return json!({
                "ok": false,
                "errors": [ { "line": (i as i64) + 1, "problem": "first line must be the form header `# FILE <path> \u{b7} <OP>` (returned exactly as given)" } ]
            });
        }
        Some(c) => {
            let file_declared = c.get(1).unwrap().as_str().trim().to_string();
            let op = c.get(2).unwrap().as_str().to_lowercase();
            (file_declared, op)
        }
    };
    i += 1;

    // Section split — only the three known headings are legal. Once the
    // CODE section opens, EVERYTHING until EOF belongs to it (CODE is the
    // form's LAST section by construction) — so authored markdown content
    // carrying its own `## ` headings inside the fence is never mistaken
    // for a form section.
    let mut sections: HashMap<String, Section> = HashMap::new();
    let mut current: Option<String> = None;
    while i < lines.len() {
        if current.as_deref() == Some("CODE") {
            sections.get_mut("CODE").unwrap().lines.push(lines[i].clone());
            i += 1;
            continue;
        }
        if let Some(m) = section_re().captures(&lines[i]) {
            let raw = m.get(1).unwrap().as_str().to_string();
            let name = raw.to_uppercase();
            if !SECTIONS.contains(&name.as_str()) {
                errors.push(json!({
                    "line": (i as i64) + 1,
                    "heading": raw,
                    "problem": format!("unknown section ## {} \u{2014} the form has exactly: ## ANCHOR_BEFORE, ## ANCHOR_AFTER, ## CODE", raw),
                }));
                current = None;
                i += 1;
                continue;
            }
            if sections.contains_key(&name) {
                errors.push(json!({
                    "line": (i as i64) + 1,
                    "heading": name,
                    "problem": format!("duplicate section ## {}", name),
                }));
                current = None;
                i += 1;
                continue;
            }
            sections.insert(name.clone(), Section { start: (i as i64) + 1, lines: Vec::new() });
            current = Some(name);
            i += 1;
            continue;
        }
        if let Some(name) = current.clone() {
            sections.get_mut(&name).unwrap().lines.push(lines[i].clone());
        } else if !lines[i].trim().is_empty() {
            let snippet = take_chars(lines[i].trim(), 40);
            errors.push(json!({
                "line": (i as i64) + 1,
                "problem": format!("content outside any section (\"{}\") \u{2014} everything belongs under a ## heading", snippet),
            }));
        }
        i += 1;
    }
    for name in SECTIONS.iter() {
        if !sections.contains_key(*name) {
            errors.push(json!({
                "heading": name,
                "problem": format!("missing section ## {} \u{2014} return the COMPLETE form, all sections present", name),
            }));
        }
    }
    if !errors.is_empty() {
        return json!({ "ok": false, "errors": errors });
    }

    // Anchors: plain text; skeleton placeholders and emptiness compile to ''.
    let anchor_of = |name: &str| -> String {
        let sec = sections.get(name).unwrap();
        let text = sec.lines.join("\n");
        let text = text.trim();
        if text.is_empty() || placeholder_re().is_match(text) {
            String::new()
        } else {
            text.to_string()
        }
    };
    let anchor_before = anchor_of("ANCHOR_BEFORE");
    let anchor_after = anchor_of("ANCHOR_AFTER");

    // CODE: exactly one fenced block; the fence is the protocol. Longest-
    // fence rule (CommonMark): the closing line is the same char repeated
    // >= the opening length — so an outer 4-backtick fence safely carries
    // inner ``` runs (authoring .md files stays possible).
    let sec = sections.get("CODE").unwrap();
    let mut open_idx: i64 = -1;
    let mut fence_char: char = '\0';
    let mut fence_len: usize = 0;
    let mut code: Option<String> = None;
    for j in 0..sec.lines.len() {
        let t = sec.lines[j].trim();
        if open_idx == -1 {
            if let Some(fm) = fence_open_re().captures(t) {
                let run = fm.get(1).unwrap().as_str();
                open_idx = j as i64;
                fence_char = run.chars().next().unwrap();
                fence_len = run.chars().count();
                continue;
            }
            if !t.is_empty() {
                let snippet = take_chars(t, 40);
                errors.push(json!({
                    "heading": "CODE",
                    "line": sec.start + (j as i64) + 1,
                    "problem": format!("content before the opening fence (\"{}\") \u{2014} the CODE section is one fenced block, nothing else", snippet),
                }));
            }
            continue;
        }
        let close_char = if fence_char == '`' { '`' } else { '~' };
        let mut close_pattern = String::from("^");
        close_pattern.push(close_char);
        close_pattern.push('{');
        close_pattern.push_str(&fence_len.to_string());
        close_pattern.push_str(",}\\s*$");
        let close_re = Regex::new(&close_pattern).unwrap();
        if close_re.is_match(t) {
            // FIX-AUTHOR-FORM-TRAILING-ECHO (2026-06-14): the code is fully captured
            // between the open and close fence — anything after it is never code. A
            // low-tier author frequently ECHOES the form's trailing instruction
            // sections after the closing fence; tolerate it: take the fenced block
            // and IGNORE the trailing form-skeleton echo instead of rejecting the
            // whole answer.
            code = Some(sec.lines[(open_idx as usize) + 1..j].join("\n"));
            break;
        }
    }
    if code.is_none() {
        if open_idx == -1 {
            errors.push(json!({
                "heading": "CODE",
                "problem": "the CODE section must contain a fenced code block (open and close with ``` lines)",
            }));
        } else {
            errors.push(json!({
                "heading": "CODE",
                "line": sec.start + open_idx + 1,
                "problem": "unclosed code fence \u{2014} add the closing ``` line",
            }));
        }
    }

    if let Some(c) = &code {
        let trimmed = c.trim();
        if trimmed.is_empty() || placeholder_re().is_match(trimmed) {
            errors.push(json!({
                "heading": "CODE",
                "problem": "the code block still carries the skeleton placeholder \u{2014} fill it with the REAL content",
            }));
        }
    }
    if !errors.is_empty() {
        return json!({ "ok": false, "errors": errors });
    }

    let code = code.unwrap();
    let code_out = if code.ends_with('\n') { code } else { format!("{}\n", code) };

    json!({
        "ok": true,
        "fields": {
            "op": op,
            "anchor_before": anchor_before,
            "anchor_after": anchor_after,
            "code": code_out,
            "file_declared": file_declared,
        }
    })
}

/// Render compile errors as the strings ai-call folds into validation.
pub fn formatFormErrors(errors: &Value) -> Value {
    let arr: Vec<Value> = nz(Some(errors)).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let out: Vec<Value> = arr
        .iter()
        .map(|e| {
            let heading_part = if truthy(e.get("heading")) {
                format!("[## {}] ", tmpl(e.get("heading")))
            } else {
                String::new()
            };
            let line_part = if nz(e.get("line")).is_some() {
                format!("line {}: ", tmpl(e.get("line")))
            } else {
                String::new()
            };
            json!(format!("form compile: {}{}{}", heading_part, line_part, tmpl(e.get("problem"))))
        })
        .collect();
    json!(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_skeleton() {
        let md = renderAuthorForm(&json!({ "file": "src/App.tsx", "op_kind": "create" }));
        let s = md.as_str().unwrap();
        assert!(s.starts_with("# FILE src/App.tsx \u{b7} CREATE"));
        assert!(s.contains("## ANCHOR_BEFORE"));
        assert!(s.contains("(none \u{2014} create writes the whole file)"));
        assert!(s.contains("## CODE"));

        let bogus = renderAuthorForm(&json!({ "file": "x.ts", "op_kind": "bogus" }));
        assert!(bogus.as_str().unwrap().contains("\u{b7} CREATE"));
    }

    #[test]
    fn parses_complete_create_form() {
        let md = format!(
            "# FILE src/a.ts \u{b7} CREATE\n## ANCHOR_BEFORE\n(none \u{2014} create writes the whole file)\n## ANCHOR_AFTER\n(none)\n## CODE\n```ts\nexport const a = 1;\n```"
        );
        let r = parseAuthorForm(&json!(md));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["fields"]["op"], json!("create"));
        assert_eq!(r["fields"]["anchor_before"], json!(""));
        assert_eq!(r["fields"]["code"], json!("export const a = 1;\n"));
        assert_eq!(r["fields"]["file_declared"], json!("src/a.ts"));
    }

    #[test]
    fn longest_fence_rule_carries_inner_fences() {
        let md = "# FILE docs/x.md \u{b7} CREATE\n## ANCHOR_BEFORE\n(none)\n## ANCHOR_AFTER\n(none)\n## CODE\n````\n# Doc\n```\ninner code\n```\n````";
        let r = parseAuthorForm(&json!(md));
        assert_eq!(r["ok"], json!(true));
        assert!(r["fields"]["code"].as_str().unwrap().contains("inner code"));
        assert!(r["fields"]["code"].as_str().unwrap().contains("```"));
    }

    #[test]
    fn trailing_echo_after_close_fence_is_tolerated() {
        let md = "# FILE apps/x/package.json \u{b7} CREATE\n## ANCHOR_BEFORE\n(none)\n## ANCHOR_AFTER\n(none)\n## CODE\n```\n{ \"name\": \"x\" }\n```\n## MANDATORY_LINES_CHECK\n- present\n\u{2550}\u{2550}\u{2550} END OF FORM \u{2550}\u{2550}\u{2550}";
        let r = parseAuthorForm(&json!(md));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["fields"]["code"].as_str().unwrap().trim(), "{ \"name\": \"x\" }");
    }

    #[test]
    fn missing_and_unclosed_fence_errors() {
        let no_fence = parseAuthorForm(&json!("# FILE a.ts \u{b7} CREATE\n## ANCHOR_BEFORE\n(none)\n## ANCHOR_AFTER\n(none)\n## CODE\nexport const a = 1;"));
        assert_eq!(no_fence["ok"], json!(false));
        // The single non-empty, non-fence CODE line pushes TWO errors (matching JS exactly):
        // [0] "content before the opening fence (...)" from the per-line check, THEN [1] "the CODE
        // section must contain a fenced code block..." once the loop ends with open_idx still -1.
        let no_fence_msgs = formatFormErrors(&no_fence["errors"]);
        let no_fence_arr = no_fence_msgs.as_array().unwrap();
        assert_eq!(no_fence_arr.len(), 2);
        assert!(no_fence_arr[1].as_str().unwrap().contains("fenced code block"));

        let unclosed = parseAuthorForm(&json!("# FILE a.ts \u{b7} CREATE\n## ANCHOR_BEFORE\n(none)\n## ANCHOR_AFTER\n(none)\n## CODE\n```\nx = 1"));
        assert_eq!(unclosed["ok"], json!(false));
        assert!(formatFormErrors(&unclosed["errors"]).as_array().unwrap()[0].as_str().unwrap().contains("unclosed code fence"));
    }

    #[test]
    fn format_form_errors_variants() {
        assert_eq!(formatFormErrors(&Value::Null), json!([]));
        let errs = json!([
            { "line": 3, "heading": "CODE", "problem": "boom" },
            { "problem": "no loc" },
        ]);
        let out = formatFormErrors(&errs);
        assert_eq!(out[0], json!("form compile: [## CODE] line 3: boom"));
        assert_eq!(out[1], json!("form compile: no loc"));
    }
}
