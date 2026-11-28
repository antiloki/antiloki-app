//! detect-bypass.rs — twin of skills/audit/detect-bypass.mjs, the canary that scans every skill's
//! source for direct mutations of audited entity tables (writes that skip with-audit).
//! NAMES ARE CONTRACT: `detectBypass` (default) · `KNOWN_BYPASS_DEBT` (named).
//!
//! ## acorn → oxc
//! Like lint-error-class.rs, the acorn parse + hand-rolled `Object.keys` walk becomes
//! `oxc_parser` + `oxc_ast_visit::Visit`. Only THREE node kinds are actually consumed, so the
//! generated visitor is a precise fit:
//!   - `Literal` with a string value       → `visit_string_literal`
//!   - `TemplateLiteral`                    → `visit_template_literal` (quasis joined by ' ')
//!   - `ImportDeclaration` / `ImportExpression` whose source ends `with-audit.mjs`
//!
//! **Deliberate scope note on `Literal`:** JS's walk visits every `Literal` node and keeps the ones
//! whose `.value` is a string. In oxc a JS string literal is a `StringLiteral`, so
//! `visit_string_literal` is the exact counterpart — numeric/boolean/regex/null literals are
//! separate node types there and were already filtered out by the `typeof === 'string'` test.
//!
//! ## The regex is a contract
//! `MUTATION_SQL_RE` is transcribed verbatim except for the JS `/gi` flags → `(?i)` + `find_iter`.
//! It has no lookaround/backrefs, so the `regex` crate accepts it unchanged (playbook gotcha #7).
//! `\s+` inside the alternation matters: `DELETE FROM` and `INSERT OR REPLACE INTO` must both match
//! with arbitrary internal whitespace, and the captured op is then whitespace-collapsed + uppercased
//! exactly as JS does (`m[1].toUpperCase().replace(/\s+/g, ' ')`).
//!
//! ## Sanction order
//! Reproduced exactly, because it is load-bearing: a PROTECTED_TABLES hit is judged ONLY against
//! that table's allow-list and then `continue`s — it never consults KNOWN_WRAPPED, the action-wrap
//! set, or `composesAudit`. The audited-entity branch is the one with the four-way sanction.
//! `composesAudit` is computed lazily and at most once per file, matching the JS ternary.
use oxc_allocator::Allocator;
use rusqlite::Connection;
use oxc_ast::ast::{ImportDeclaration, ImportExpression, StringLiteral, TemplateLiteral};
use oxc_ast_visit::{walk, Visit};
use oxc_parser::Parser;
use oxc_span::SourceType;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::OnceLock;

/// JS `PROTECTED_TABLES` — table → the only skills allowed to write it directly.
fn protected_tables(table: &str) -> Option<&'static [&'static str]> {
    match table {
        "sacred_timeline" => Some(&["with-audit", "timeline-rotate"]),
        _ => None,
    }
}

const AUDITED_ENTITY_TABLES: [&str; 5] = ["cards", "agents", "actions", "memories", "laws"];

const KNOWN_WRAPPED: [&str; 3] = ["open-db", "canonical-actions", "canonical-laws"];

/// JS `KNOWN_BYPASS_DEBT` — FROZEN. Pre-existing unsealed writers, reported under `debt` so the
/// zero-violations canary gates NEW bypasses while these burn down card-by-card.
pub const KNOWN_BYPASS_DEBT: [&str; 20] = [
    "prompt-quality-report",
    "run-session",
    "contract-audit-determinism",
    "qa-sandbox-reset",
    "sync-restore-from-revision",
    "agent-custom-prompt-set",
    "card-accuracy-persist",
    "card-clone-insert",
    "card-restart-from-failure",
    "card-rewind-rearm",
    "card-score-persist",
    "law-set-active",
    "sandbox-phase-set",
    "eval-run-one",
    "onboarding-orchestrate",
    "onboarding-write",
    "work-start",
    "keeper-card-consolidate",
    "keeper-memory-capture-correction",
    "pl-session-start",
];

fn mutation_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(INSERT\s+(?:OR\s+\w+\s+)?INTO|UPDATE|DELETE\s+FROM)\s+([A-Za-z_][\w.]*)")
            .expect("MUTATION_SQL_RE is valid")
    })
}

/// JS `KNOWN_WRAPPED_DIRS = /[\\/]engine[\\/](open-db|canonical-actions)[\\/]/`
fn known_wrapped_dirs() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[\\/]engine[\\/](open-db|canonical-actions)[\\/]").expect("valid"))
}

fn normalize_table(raw: &str) -> &str {
    raw.rsplit('.').next().unwrap_or(raw)
}

/// 1-based line + 0-based UTF-16 column, matching acorn's `loc.start` (see lint-error-class.rs).
fn line_col_utf16(source: &str, offset: usize) -> (i64, i64) {
    let mut line = 1i64;
    let mut col = 0i64;
    for (idx, ch) in source.char_indices() {
        if idx >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += ch.len_utf16() as i64;
        }
    }
    (line, col)
}

#[derive(Default)]
struct SqlStrings {
    /// (sql text, byte offset of the node)
    found: Vec<(String, usize)>,
}

impl<'a> Visit<'a> for SqlStrings {
    fn visit_string_literal(&mut self, it: &StringLiteral<'a>) {
        self.found.push((it.value.to_string(), it.span.start as usize));
    }
    fn visit_template_literal(&mut self, it: &TemplateLiteral<'a>) {
        // JS: n.quasis.map((q) => q.value.cooked).join(' ')
        let joined = it
            .quasis
            .iter()
            .map(|q| q.value.cooked.as_ref().map(|c| c.to_string()).unwrap_or_default())
            .collect::<Vec<_>>()
            .join(" ");
        self.found.push((joined, it.span.start as usize));
        walk::walk_template_literal(self, it);
    }
}

#[derive(Default)]
struct WithAuditImport {
    found: bool,
}

impl<'a> Visit<'a> for WithAuditImport {
    fn visit_import_declaration(&mut self, it: &ImportDeclaration<'a>) {
        if it.source.value.ends_with("with-audit.mjs") {
            self.found = true;
        }
        walk::walk_import_declaration(self, it);
    }
    fn visit_import_expression(&mut self, it: &ImportExpression<'a>) {
        if let oxc_ast::ast::Expression::StringLiteral(s) = &it.source {
            if s.value.ends_with("with-audit.mjs") {
                self.found = true;
            }
        }
        walk::walk_import_expression(self, it);
    }
}

struct Mutation {
    op: String,
    table: String,
    line: i64,
    column: i64,
}

fn scan_source(source: &str) -> Result<(Vec<Mutation>, bool), String> {
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, source, SourceType::jsx()).parse();
    if ret.diagnostics.has_errors() {
        return Err(ret
            .diagnostics
            .errors()
            .next()
            .map(|d| d.to_string())
            .unwrap_or_else(|| "parse error".to_string()));
    }

    let mut strings = SqlStrings::default();
    strings.visit_program(&ret.program);

    let mut mutations = Vec::new();
    for (sql, offset) in &strings.found {
        for caps in mutation_re().captures_iter(sql) {
            let op_raw = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            // JS: m[1].toUpperCase().replace(/\s+/g, ' ')
            let op = op_raw.to_uppercase().split_whitespace().collect::<Vec<_>>().join(" ");
            let table = normalize_table(caps.get(2).map(|m| m.as_str()).unwrap_or("")).to_string();
            if protected_tables(&table).is_some() || AUDITED_ENTITY_TABLES.contains(&table.as_str()) {
                let (line, column) = line_col_utf16(source, *offset);
                mutations.push(Mutation { op, table, line, column });
            }
        }
    }

    let mut imp = WithAuditImport::default();
    imp.visit_program(&ret.program);
    Ok((mutations, imp.found))
}

/// Skills sanctioned because a canonical action wraps them in with-audit:
/// steps_json entries shaped `{skill:'with-audit', input:{skill: X, ...}}`.
fn action_wrapped_skills() -> &'static Vec<String> {
    static SET: OnceLock<Vec<String>> = OnceLock::new();
    SET.get_or_init(|| {
        let mut out: Vec<String> = Vec::new();
        for a in crate::engine_canonical_actions::CANONICAL_ACTIONS.as_array().cloned().unwrap_or_default() {
            let steps_json = a.get("steps_json").and_then(Value::as_str).unwrap_or("");
            let steps: Value = match serde_json::from_str(steps_json) {
                Ok(v) => v,
                Err(_) => continue, // JS: try/catch → continue
            };
            let Some(arr) = steps.as_array() else { continue };
            // JS: steps.flat(Infinity) — a step list may nest arrays
            let mut stack: Vec<Value> = arr.clone();
            while let Some(step) = stack.pop() {
                if let Some(nested) = step.as_array() {
                    stack.extend(nested.clone());
                    continue;
                }
                if step.get("skill").and_then(Value::as_str) == Some("with-audit") {
                    if let Some(inner) = step.get("input").and_then(|i| i.get("skill")).and_then(Value::as_str) {
                        if !out.iter().any(|x| x == inner) {
                            out.push(inner.to_string());
                        }
                    }
                }
            }
        }
        out
    })
}

/// `export default function detectBypass(input, _context)`.
pub fn detectBypass(input: &Value, _db: &Connection) -> Result<Value, String> {
    let only: Option<Vec<String>> = input
        .get("only")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect());

    let all = crate::engine_load_skill::listSkills();
    let targets: Vec<Value> = all
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| match &only {
            None => true,
            Some(names) => s
                .get("name")
                .and_then(Value::as_str)
                .map(|n| names.iter().any(|x| x == n))
                .unwrap_or(false),
        })
        .collect();

    let mut issues: Vec<Value> = Vec::new();
    let mut debt: Vec<Value> = Vec::new();
    let mut checked_count = 0usize;

    for t in &targets {
        let name = t.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        let path = t.get("path").and_then(Value::as_str).unwrap_or("").to_string();

        let source = std::fs::read_to_string(&path)
            .map_err(|e| format!("detect-bypass: cannot read '{path}': {e}"))?;

        let (mutations, composes_audit_raw) = match scan_source(&source) {
            Err(msg) => {
                issues.push(json!({ "skill": name, "path": path, "code": "PARSE_ERROR", "message": msg }));
                continue;
            }
            Ok(v) => v,
        };
        checked_count += 1;

        // JS computes composesAudit only when at least one AUDITED (non-protected) hit exists.
        let needs_sanction = mutations
            .iter()
            .any(|m| AUDITED_ENTITY_TABLES.contains(&m.table.as_str()) && protected_tables(&m.table).is_none());
        let composes_audit = if needs_sanction { composes_audit_raw } else { false };

        for m in &mutations {
            if let Some(allowed) = protected_tables(&m.table) {
                if !allowed.contains(&name.as_str()) {
                    issues.push(json!({
                        "skill": name, "path": path, "code": "BYPASS",
                        "message": format!("{}: direct {} {} at line {} — only allowed in: {}",
                            name, m.op, m.table, m.line, allowed.join(", ")),
                        "op": m.op, "table": m.table, "line": m.line, "column": m.column,
                    }));
                }
                continue; // protected hits never consult the other sanctions
            }

            let sanctioned = KNOWN_WRAPPED.contains(&name.as_str())
                || known_wrapped_dirs().is_match(&path)
                || action_wrapped_skills().iter().any(|x| x == &name)
                || composes_audit;
            if sanctioned {
                continue;
            }

            let is_debt = KNOWN_BYPASS_DEBT.contains(&name.as_str());
            let finding = json!({
                "skill": name, "path": path,
                "code": if is_debt { "DEBT" } else { "BYPASS" },
                "message": format!("{}: direct {} {} at line {} — '{}' is an audited entity table; route the write through a with-audit-wrapped skill (or compose with-audit) per NEVER_BYPASS_WITH_AUDIT",
                    name, m.op, m.table, m.line, m.table),
                "op": m.op, "table": m.table, "line": m.line, "column": m.column,
            });
            if is_debt { debt.push(finding) } else { issues.push(finding) }
        }
    }

    Ok(json!({
        "checked": checked_count,
        "valid": issues.is_empty(),
        "violations": issues.len(),
        "issues": issues,
        "debt": debt,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(src: &str) -> Vec<Mutation> {
        scan_source(src).unwrap().0
    }

    #[test]
    fn detects_insert_update_and_delete_from_in_a_plain_string() {
        let m = scan("db.prepare('INSERT INTO cards (a) VALUES (1)');\ndb.prepare('UPDATE agents SET x=1');\ndb.prepare('DELETE FROM laws WHERE id=1');\n");
        let ops: Vec<&str> = m.iter().map(|x| x.op.as_str()).collect();
        assert_eq!(ops, vec!["INSERT INTO", "UPDATE", "DELETE FROM"]);
        let tables: Vec<&str> = m.iter().map(|x| x.table.as_str()).collect();
        assert_eq!(tables, vec!["cards", "agents", "laws"]);
    }

    #[test]
    fn op_is_uppercased_and_whitespace_collapsed() {
        let m = scan("db.prepare('delete   from   cards');");
        assert_eq!(m[0].op, "DELETE FROM");
        let m2 = scan("db.prepare('insert  or  replace  into  cards');");
        assert_eq!(m2[0].op, "INSERT OR REPLACE INTO");
    }

    #[test]
    fn schema_qualified_tables_normalize_to_the_last_segment() {
        let m = scan("db.prepare('UPDATE memory.memories SET x=1');\ndb.prepare('INSERT INTO laws.laws (a) VALUES (1)');");
        let tables: Vec<&str> = m.iter().map(|x| x.table.as_str()).collect();
        assert_eq!(tables, vec!["memories", "laws"]);
    }

    #[test]
    fn template_literals_are_scanned_with_quasis_joined_by_space() {
        let m = scan("db.prepare(`UPDATE ${'x'} cards SET a=1`);");
        // quasis "UPDATE " + " cards SET a=1" joined by ' ' still yields UPDATE ... cards
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].table, "cards");
    }

    #[test]
    fn unprotected_tables_are_ignored() {
        assert_eq!(scan("db.prepare('INSERT INTO runtime_events (a) VALUES (1)');").len(), 0);
        assert_eq!(scan("db.prepare('SELECT * FROM cards');").len(), 0);
    }

    #[test]
    fn sacred_timeline_is_protected_and_carries_a_position() {
        let m = scan("const x = 1;\ndb.prepare('INSERT INTO sacred_timeline (a) VALUES (1)');");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].table, "sacred_timeline");
        assert_eq!(m[0].line, 2);
        assert!(protected_tables("sacred_timeline").unwrap().contains(&"with-audit"));
    }

    #[test]
    fn static_and_dynamic_with_audit_imports_both_sanction() {
        assert!(scan_source("import withAudit from '../engine/with-audit.mjs';\ndb.prepare('UPDATE cards SET a=1');").unwrap().1);
        assert!(scan_source("const m = await import('../engine/with-audit.mjs');\ndb.prepare('UPDATE cards SET a=1');").unwrap().1);
        assert!(!scan_source("import x from '../engine/other.mjs';\ndb.prepare('UPDATE cards SET a=1');").unwrap().1);
    }

    #[test]
    fn known_wrapped_dirs_matches_the_post_split_homes() {
        assert!(known_wrapped_dirs().is_match("C:/x/skills/engine/open-db/seed.mjs"));
        assert!(known_wrapped_dirs().is_match("/x/skills/engine/canonical-actions/card.mjs"));
        assert!(!known_wrapped_dirs().is_match("/x/skills/entities/card-add.mjs"));
    }

    #[test]
    fn action_wrapped_set_is_derived_from_the_real_canonical_actions() {
        // the substrate's canonical actions really do wrap skills in with-audit; if this ever
        // becomes empty the sanction check silently stops working.
        assert!(!action_wrapped_skills().is_empty());
    }

    #[test]
    fn debt_list_is_frozen_and_separates_from_issues() {
        assert_eq!(KNOWN_BYPASS_DEBT.len(), 20);
        assert!(KNOWN_BYPASS_DEBT.contains(&"work-start"));
        assert!(!KNOWN_BYPASS_DEBT.contains(&"card-add"));
    }

    #[test]
    fn only_filter_yields_a_stable_empty_report() {
        let c = Connection::open_in_memory().unwrap();
        let r = detectBypass(&json!({ "only": ["__no_such_skill__"] }), &c).unwrap();
        assert_eq!(r["checked"], json!(0));
        assert_eq!(r["valid"], json!(true));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["checked", "valid", "violations", "issues", "debt"]);
    }
}
