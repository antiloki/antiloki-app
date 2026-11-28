//! extract-rust-skill-meta.rs — description + input schema for a RUST skill. NAMES ARE CONTRACT:
//! `extractRustDescription`, `extractRustInputSchema`.
//!
//! WHY IT EXISTS. The catalog used to describe skills by parsing JavaScript: a JSDoc block for the
//! description, `@param` tags for the input schema, both via an oxc AST walk. The skill tree is
//! Rust now, and neither has an analogue — Rust has no JSDoc, and no `@param`. Pointing the JS
//! extractors at a `.rs` file does not degrade gracefully; it fails to parse every skill in the
//! tree and reports the whole catalog as broken.
//!
//! So the two facts are re-derived from what Rust skills DO carry:
//!
//! DESCRIPTION — the `//!` module doc. Every skill in this crate opens with one, and its first
//! sentences are already written as "what this skill is", because that is what the convention
//! asks for. It is the same information the JSDoc block held, in the place Rust puts it.
//!
//! INPUT SCHEMA — the fields the body actually READS. A JS skill declared its inputs in a comment
//! that could drift from the code; a Rust skill reads `input.get("slug")`, and that cannot drift,
//! because it IS the code. Extracting the field set from those call sites gives a schema that is
//! true by construction rather than by discipline — strictly better than the tags it replaces.
// QUALITY: 89/100 — derives both facts from what the source actually contains, no drift possible
use serde_json::{json, Value};

/// The leading `//!` block, joined into one line and capped.
///
/// Stops at the first line that is not `//!` — a skill's doc header is contiguous by convention,
/// and continuing past a blank line would swallow the `use` statements' own comments. The
/// `// QUALITY:` line that follows most headers is a `//` comment, not `//!`, so it stops there
/// naturally.
pub fn extractRustDescription(src: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for line in src.lines() {
        let t = line.trim();
        if t.is_empty() && out.is_empty() {
            continue; // leading blank lines before the header
        }
        let Some(rest) = t.strip_prefix("//!") else {
            if out.is_empty() { continue } else { break }
        };
        let rest = rest.trim();
        if !rest.is_empty() {
            out.push(rest);
        }
    }
    let text = out.join(" ");
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return None;
    }
    Some(text.chars().take(280).collect())
}

/// The input fields the skill reads, as a sorted unique list.
///
/// Matches `input.get("field")` — the one way a Rust skill reads a named input. Tests are excluded
/// (`#[cfg(test)]` onwards) because a fixture's `json!({"slug": …})` describes the test, not the
/// skill's contract.
pub fn extractRustInputSchema(src: &str) -> Value {
    let body = match src.find("#[cfg(test)]") {
        Some(i) => &src[..i],
        None => src,
    };
    let mut fields: Vec<String> = Vec::new();
    let needle = "input.get(\"";
    let mut from = 0usize;
    while let Some(i) = body[from..].find(needle) {
        let start = from + i + needle.len();
        match body[start..].find('"') {
            None => break,
            Some(end) => {
                let name = &body[start..start + end];
                // A field name is an identifier-ish token; anything with a quote-escape or
                // newline in it came from a string that merely looked like a get() call.
                if !name.is_empty()
                    && name.len() <= 64
                    && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.')
                    && !fields.iter().any(|f| f == name)
                {
                    fields.push(name.to_string());
                }
                from = start + end;
            }
        }
    }
    fields.sort();
    json!(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SKILL: &str = r#"//! card-add.rs — add a card to the board. NAMES ARE CONTRACT: `cardAdd`.
//!
//! Second paragraph of the header.
// QUALITY: 90/100 — not part of the description
use serde_json::Value;

pub fn cardAdd(input: &Value) -> Value {
    let slug = input.get("slug");
    let title = input.get("title");
    let _again = input.get("slug");
    Value::Null
}

#[cfg(test)]
mod tests {
    #[test]
    fn t() { let _ = input.get("only_in_a_test"); }
}
"#;

    #[test]
    fn the_description_is_the_module_doc_header() {
        let d = extractRustDescription(SKILL).unwrap();
        assert!(d.starts_with("card-add.rs — add a card to the board."));
        assert!(d.contains("Second paragraph of the header"));
        // The `// QUALITY:` line is a plain comment, not part of the header.
        assert!(!d.contains("QUALITY"));
    }

    #[test]
    fn a_file_with_no_module_doc_has_no_description() {
        assert_eq!(extractRustDescription("use serde_json::Value;\npub fn x() {}\n"), None);
        assert_eq!(extractRustDescription(""), None);
    }

    #[test]
    fn the_schema_is_the_fields_the_body_READS() {
        // Sorted, unique, and derived from the code — so it cannot drift from it.
        assert_eq!(extractRustInputSchema(SKILL), json!(["slug", "title"]));
    }

    #[test]
    fn test_only_fields_are_EXCLUDED() {
        // A fixture's inputs describe the test, not the skill's contract.
        let schema = extractRustInputSchema(SKILL);
        assert!(!schema.to_string().contains("only_in_a_test"));
    }

    #[test]
    fn a_skill_that_reads_nothing_has_an_empty_schema() {
        assert_eq!(extractRustInputSchema("pub fn f() {}"), json!([]));
    }

    #[test]
    fn a_string_that_merely_looks_like_a_get_call_is_ignored() {
        // Field names are identifier-ish; a sentence is not one.
        let src = "let s = \"input.get(\\\"not a field name at all\\\")\";";
        assert_eq!(extractRustInputSchema(src), json!([]));
    }

    #[test]
    fn dotted_and_underscored_field_names_survive() {
        let src = "input.get(\"card_slug\"); input.get(\"a.b\"); input.get(\"kebab-name\");";
        assert_eq!(extractRustInputSchema(src), json!(["a.b", "card_slug", "kebab-name"]));
    }
}
