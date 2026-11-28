//! extract-jsdoc-description.rs — twin of skills/audit/extract-jsdoc-description.mjs. NAMES ARE
//! CONTRACT: `extractJsdocDescription` kept exactly as the JS source names it.
//!
//! ## Parser choice: `oxc_parser` (already a codebase dependency), not a bespoke reuse of
//! `parse-js.rs`'s `parseJs`
//! The JS source parses with `acorn` (`onComment` callback collecting every block comment) purely
//! to find the leading `/** ... */` above `export default`. `codebase/parse-js.rs`'s `parseJs`
//! twin (also oxc-backed) builds a call-graph/import shape and does not surface comments at all,
//! so it cannot be reused here — this file parses independently with the same `oxc_parser` crate
//! and its own small walk, mirroring the JS source's own narrow acorn usage rather than borrowing
//! `parseJs`'s unrelated output shape.
//!
//! ## acorn `onComment` fields ↔ oxc `Comment` fields
//! acorn's `onComment(block, text, start, end)` gives `text` as the comment's INNER content
//! (delimiters `/*`/`*/`/`//` stripped) and `start`/`end` as the FULL span (delimiters included).
//! oxc's `Comment.span` is the full span (matching acorn's start/end) and `Comment.content_span()`
//! strips the delimiters (matching acorn's `text`) — verified against the oxc_ast 0.143.0 source
//! (`content_span` subtracts 2 bytes off each end for block comments, 2 off the start only for
//! line comments, i.e. `//`/`/*`/`*/` are each 2 bytes). `Comment.is_block()` matches acorn's
//! `block` boolean (oxc splits block comments into `SingleLineBlock`/`MultiLineBlock` internally,
//! `is_block()` covers both — the JS source never distinguishes single- vs multi-line blocks
//! either, it just checks `block`).
//!
//! ## `export default` node lookup
//! The JS source finds the first `ExportDefaultDeclaration` in `ast.body` (`for...of` + `break` on
//! first match). oxc's `Statement::ExportDefaultDeclaration(edd)` is the matching variant; `edd`
//! is a concrete struct (not an enum needing `GetSpan`), so `edd.span` is read directly — the same
//! pattern `parse-js.rs` uses for `ImportDeclaration.span` (`imp.span`).
use oxc_allocator::Allocator;
use oxc_ast::ast::Statement;
use oxc_parser::Parser;
use oxc_span::SourceType;
use serde_json::{json, Value};

fn strip_leading_star(line: &str) -> String {
    // Mirrors JS `line.replace(/^\s*\*\s?/, '')`: consume all leading whitespace (regex `\s*` is
    // greedy and `*` is never whitespace, so this can't backtrack differently than Rust's
    // `trim_start`), then a literal `*`, then AT MOST one following whitespace char.
    let trimmed = line.trim_start();
    match trimmed.strip_prefix('*') {
        Some(rest) => {
            let mut chars = rest.chars();
            match chars.next() {
                Some(c) if c.is_whitespace() => chars.as_str().to_string(),
                _ => rest.to_string(),
            }
        }
        None => line.to_string(),
    }
}

fn clean_jsdoc_text(raw: &str) -> String {
    let lines: Vec<String> =
        raw.split('\n').map(|line| strip_leading_star(line).trim_end().to_string()).collect();
    let n = lines.len();
    let kept: Vec<String> = lines
        .into_iter()
        .enumerate()
        .filter(|(i, line)| !(line.is_empty() && (*i == 0 || *i == n - 1)))
        .map(|(_, line)| line)
        .collect();
    kept.join("\n").trim().to_string()
}

fn truthy_str(v: Option<&Value>) -> Option<&str> {
    match v {
        Some(Value::String(s)) if !s.is_empty() => Some(s.as_str()),
        _ => None,
    }
}

/// `export default function extractJsdocDescription(input, _context)`.
pub fn extractJsdocDescription(input: &Value, _context: &Value) -> Result<Value, String> {
    let (name, source_arg): (String, Option<String>) = if let Value::String(s) = input {
        (s.clone(), None)
    } else if let Some(src) = truthy_str(input.get("source")) {
        let name = truthy_str(input.get("name")).unwrap_or("<inline>").to_string();
        (name, Some(src.to_string()))
    } else if let Some(nm) = truthy_str(input.get("name")) {
        (nm.to_string(), None)
    } else {
        return Err("extract-jsdoc-description: name or source is required".to_string());
    };

    let mut path: Value = Value::Null;
    let source = match source_arg {
        Some(s) => s,
        None => {
            let resolved = crate::engine_load_skill::findSkillPath(&json!(name))?;
            let path_str = resolved.as_str().unwrap_or_default().to_string();
            let contents = std::fs::read_to_string(&path_str)
                .map_err(|e| format!("extract-jsdoc-description: {e}"))?;
            path = json!(path_str);
            contents
        }
    };

    let allocator = Allocator::default();
    let source_type = SourceType::jsx();
    let ret = Parser::new(&allocator, &source, source_type).parse();
    let program = ret.program;

    let mut export_start: Option<u32> = None;
    for stmt in &program.body {
        if let Statement::ExportDefaultDeclaration(edd) = stmt {
            export_start = Some(edd.span.start);
            break;
        }
    }

    let Some(export_start) = export_start else {
        return Ok(json!({
            "name": name,
            "path": path,
            "description": Value::Null,
            "reason": "no export default found",
        }));
    };

    let mut best: Option<(u32, String)> = None;
    for c in &program.comments {
        if !c.is_block() {
            continue;
        }
        if c.span.end > export_start {
            continue;
        }
        let content = c.content_span();
        let text = &source[content.start as usize..content.end as usize];
        if !text.starts_with('*') {
            continue;
        }
        if best.as_ref().map_or(true, |(end, _)| c.span.end > *end) {
            best = Some((c.span.end, text.to_string()));
        }
    }

    let Some((_, text)) = best else {
        return Ok(json!({
            "name": name,
            "path": path,
            "description": Value::Null,
            "reason": "no leading JSDoc found above export default",
        }));
    };

    Ok(json!({
        "name": name,
        "path": path,
        "description": clean_jsdoc_text(&text),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_jsdoc_above_export_default() {
        let source = "/**\n * Does the thing.\n * Second line.\n */\nexport default function foo(input, context) {\n  return input;\n}\n";
        let out = extractJsdocDescription(&json!({"name": "foo", "source": source}), &json!({}))
            .unwrap();
        assert_eq!(out["name"], json!("foo"));
        assert_eq!(out["path"], Value::Null);
        assert_eq!(out["description"], json!("Does the thing.\nSecond line."));
    }

    #[test]
    fn returns_reason_when_no_export_default() {
        let source = "/**\n * Orphan doc.\n */\nexport function foo() {}\n";
        let out = extractJsdocDescription(&json!({"name": "foo", "source": source}), &json!({}))
            .unwrap();
        assert_eq!(out["description"], Value::Null);
        assert_eq!(out["reason"], json!("no export default found"));
    }

    #[test]
    fn returns_reason_when_no_leading_jsdoc() {
        let source = "// plain line comment, not jsdoc\nexport default function foo() {}\n";
        let out = extractJsdocDescription(&json!({"name": "foo", "source": source}), &json!({}))
            .unwrap();
        assert_eq!(out["description"], Value::Null);
        assert_eq!(out["reason"], json!("no leading JSDoc found above export default"));
    }

    #[test]
    fn ignores_non_jsdoc_block_comment_and_picks_closest_jsdoc() {
        let source = "/* not jsdoc */\n/**\n * Real doc.\n */\nexport default function foo() {}\n";
        let out = extractJsdocDescription(&json!({"name": "foo", "source": source}), &json!({}))
            .unwrap();
        assert_eq!(out["description"], json!("Real doc."));
    }

    #[test]
    fn plain_string_input_uses_find_skill_path_and_errors_when_not_found() {
        let err = extractJsdocDescription(&json!("definitely-not-a-real-skill-name"), &json!({}))
            .unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn missing_name_and_source_errors() {
        let err = extractJsdocDescription(&json!({}), &json!({})).unwrap_err();
        assert_eq!(err, "extract-jsdoc-description: name or source is required");
    }

    #[test]
    fn inline_name_defaults_when_source_present_without_name() {
        let source = "/**\n * Inline doc.\n */\nexport default function () {}\n";
        let out = extractJsdocDescription(&json!({"source": source}), &json!({})).unwrap();
        assert_eq!(out["name"], json!("<inline>"));
        assert_eq!(out["description"], json!("Inline doc."));
    }
}
