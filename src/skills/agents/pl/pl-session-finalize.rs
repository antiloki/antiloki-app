//! pl-session-finalize.rs — twin of skills/agents/pl/pl-session-finalize.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `plSessionFinalize` (default). Assembles a pl_proposed_cards row from a
//! session's raw_intent + answers + research findings, supersedes any prior proposed card, derives
//! slug (reuse bound card else synth) + template_kind, and flips the session to awaiting_approval.
// QUALITY: SELECT * + LEFT-JOIN qa + regex classifier + kebab slug + INSERT/UPDATE (frozen clock)
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// = JS String(v ?? '')
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// = JS String(v)
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
// = JS `${v}` interpolation (undefined/null render as words; else String(v ?? '')).
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}

// String(s).toUpperCase().replace(/[^A-Z0-9]+/g,'-').replace(/^-|-$/g,'').slice(0,48).replace(/-$/,'')
fn to_kebab_upper(s: &str) -> String {
    let up = s.to_uppercase();
    let dashed = regex::Regex::new(r"[^A-Z0-9]+").unwrap().replace_all(&up, "-");
    let trimmed = dashed.trim_matches('-'); // collapse already single → trims the lone leading/trailing dash
    let sliced: String = trimmed.chars().take(48).collect();
    sliced.trim_end_matches('-').to_string()
}

// intent.toLowerCase() then first matching keyword family; default 'feature-add'.
fn classify_template(intent: &str) -> &'static str {
    let h = intent.to_lowercase();
    let m = |p: &str| regex::Regex::new(p).unwrap().is_match(&h);
    if m(r"\b(bug|broken|fail|crash|fix)\b") { return "bug-fix"; }
    if m(r"\b(refactor|cleanup|rename)\b") { return "refactor"; }
    if m(r"\b(arch|adr|design|decide)\b") { return "arch"; }
    if m(r"\b(docs?|readme|guide)\b") { return "docs"; }
    if m(r"\b(deploy|env|config|migration)\b") { return "ops"; }
    if m(r"\b(tech.?debt|cruft|legacy)\b") { return "tech-debt"; }
    "feature-add"
}

// TEMPLATE_PREFIX[kind] ?? kind.toUpperCase().replace(/-/g,'')
fn template_prefix(kind: &str) -> String {
    let mapped = match kind {
        "feature-add" => Some("FEAT"),
        "bug-fix" => Some("FIX"),
        "refactor" => Some("REFACTOR"),
        "arch" => Some("ARCH"),
        "docs" => Some("DOCS"),
        "ops" => Some("OPS"),
        "tech-debt" => Some("TECHDEBT"),
        _ => None,
    };
    match mapped {
        Some(p) => p.to_string(),
        None => kind.to_uppercase().replace('-', ""),
    }
}

pub fn plSessionFinalize(input: &Value, db: &Connection) -> Result<Value, String> {
    // context.db is always present in the Rust lane (the harness passes the Connection directly).
    let session_id = input.get("session_id").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&session_id)) {
        return Err("pl-session-finalize: session_id is required".into());
    }

    let sess_rows = crate::db_json::query_rows(db, "SELECT * FROM pl_sessions WHERE id = ?", &[session_id.clone()])?;
    let session = match sess_rows.as_array().and_then(|a| a.first()) {
        Some(s) => s.clone(),
        None => return Err(format!("pl-session-finalize: session {} not found", tmpl(Some(&session_id)))),
    };
    let status = session.get("status").cloned().unwrap_or(Value::Null);
    let status_ok = matches!(
        status.as_str(),
        Some("tuning") | Some("awaiting_approval") | Some("researching") | Some("deciding_split") | Some("clarifying")
    );
    if !status_ok {
        return Err(format!("pl-session-finalize: session status='{}' — finalize requires active session", tmpl(Some(&status))));
    }

    // Mark any prior proposed card for this session as superseded.
    crate::db_json::run_write(
        db,
        "UPDATE pl_proposed_cards SET status='superseded' WHERE session_id = ? AND status='awaiting_approval'",
        &[session_id.clone()],
    )?;

    // Gather all answers (multi-step) for body_md.
    let qa = crate::db_json::query_rows(
        db,
        "SELECT q.step, q.order_index, q.question_md, o.option_md AS chosen_option_md, a.writein_md \
         FROM pl_questions q \
         LEFT JOIN pl_answers a ON a.question_id = q.id \
         LEFT JOIN pl_question_options o ON o.id = a.selected_option_id \
         WHERE q.session_id = ? \
         ORDER BY q.step, q.order_index",
        &[session_id.clone()],
    )?;
    let qa_arr = qa.as_array().cloned().unwrap_or_default();

    let intent = session.get("raw_intent").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let template_kind = classify_template(&intent);

    let mut lines: Vec<String> = Vec::new();
    lines.push("## Intent".to_string());
    lines.push(intent.clone());
    lines.push(String::new());
    if !qa_arr.is_empty() {
        lines.push("## Clarifications".to_string());
        for r in &qa_arr {
            let ans = if truthy(r.get("chosen_option_md")) {
                str_qq(r.get("chosen_option_md"))
            } else if truthy(r.get("writein_md")) {
                str_qq(r.get("writein_md"))
            } else {
                "_(unanswered)_".to_string()
            };
            lines.push(format!("- **Q ({}):** {}", tmpl(r.get("step")), tmpl(r.get("question_md"))));
            lines.push(format!("  - **A:** {}", ans));
        }
        lines.push(String::new());
    }

    // Research findings (Block 5b populates these via pl.session.advance.research).
    let findings = crate::db_json::query_rows(
        db,
        "SELECT finding_type, target_ref, why_relevant_md FROM pl_research_findings WHERE session_id = ? ORDER BY order_index",
        &[session_id.clone()],
    )?;
    let findings_arr = findings.as_array().cloned().unwrap_or_default();
    if !findings_arr.is_empty() {
        lines.push("## Research".to_string());
        for f in &findings_arr {
            let suffix = if truthy(f.get("why_relevant_md")) {
                format!(" \u{2014} {}", str_qq(f.get("why_relevant_md")))
            } else {
                String::new()
            };
            lines.push(format!("- **{}**: `{}`{}", tmpl(f.get("finding_type")), tmpl(f.get("target_ref")), suffix));
        }
        lines.push(String::new());
    }

    lines.push("## Acceptance".to_string());
    lines.push("_(to be filled by TL when drafting the contract)_".to_string());

    let body_md = lines.join("\n");

    // FIX-PL-FINALIZE-REUSES-BOUND-CARD-SLUG: reuse an existing bound card's slug; else synthesize.
    let prefix = template_prefix(template_kind);
    let mut slug_suggestion = Value::Null; // JS `let slug_suggestion;` (undefined) — Null stands in for falsy
    let approved = session.get("approved_card_slug").cloned().unwrap_or(Value::Null);
    if truthy(Some(&approved)) {
        let bc = crate::db_json::query_rows(db, "SELECT slug FROM cards WHERE slug = ?", &[approved.clone()])?;
        if let Some(row) = bc.as_array().and_then(|a| a.first()) {
            slug_suggestion = row.get("slug").cloned().unwrap_or(Value::Null);
        }
    }
    if !truthy(Some(&slug_suggestion)) {
        slug_suggestion = json!(format!("{}-{}-S{}", prefix, to_kebab_upper(&intent), tmpl(Some(&session_id))));
    }

    let title: String = intent.chars().take(80).collect();
    let (_changes, rowid) = crate::db_json::run_write(
        db,
        "INSERT INTO pl_proposed_cards \
           (session_id, slug_suggestion, title, body_md, template_kind, priority, size_estimate, status) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 'awaiting_approval')",
        &[
            session_id.clone(),
            slug_suggestion.clone(),
            json!(title),
            json!(body_md),
            json!(template_kind),
            json!(3),
            json!("S"),
        ],
    )?;

    crate::db_json::run_write(
        db,
        &format!("UPDATE pl_sessions SET status='awaiting_approval', updated_at={} WHERE id = ?", crate::clock::now_sql()),
        &[session_id.clone()],
    )?;

    let answers_included = qa_arr
        .iter()
        .filter(|r| truthy(r.get("chosen_option_md")) || truthy(r.get("writein_md")))
        .count();

    let mut out = Map::new();
    out.insert("proposed_card_id".into(), json!(rowid));
    out.insert("session_id".into(), session_id);
    out.insert("slug_suggestion".into(), slug_suggestion);
    out.insert("template_kind".into(), json!(template_kind));
    out.insert("new_status".into(), json!("awaiting_approval"));
    out.insert("answers_included".into(), json!(answers_included as i64));
    out.insert("research_findings_included".into(), json!(findings_arr.len() as i64));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE pl_sessions (id INTEGER PRIMARY KEY, status TEXT, raw_intent TEXT, approved_card_slug TEXT, updated_at TEXT); \
             CREATE TABLE pl_proposed_cards (id INTEGER PRIMARY KEY, session_id INTEGER, slug_suggestion TEXT, title TEXT, body_md TEXT, template_kind TEXT, priority INTEGER, size_estimate TEXT, status TEXT); \
             CREATE TABLE pl_questions (id INTEGER PRIMARY KEY, session_id INTEGER, step TEXT, order_index INTEGER, question_md TEXT); \
             CREATE TABLE pl_answers (id INTEGER PRIMARY KEY, question_id INTEGER, selected_option_id INTEGER, writein_md TEXT); \
             CREATE TABLE pl_question_options (id INTEGER PRIMARY KEY, option_md TEXT); \
             CREATE TABLE pl_research_findings (id INTEGER PRIMARY KEY, session_id INTEGER, finding_type TEXT, target_ref TEXT, why_relevant_md TEXT, order_index INTEGER); \
             CREATE TABLE cards (slug TEXT PRIMARY KEY);",
        )
        .unwrap();
        c
    }

    #[test]
    fn full_path_reuses_slug_and_supersedes() {
        let c = db0();
        c.execute_batch(
            "INSERT INTO pl_sessions (id, status, raw_intent, approved_card_slug, updated_at) VALUES (1, 'tuning', 'Fix the broken login crash', 'FEAT-LOGIN', '2025-12-01 00:00:00'); \
             INSERT INTO cards (slug) VALUES ('FEAT-LOGIN'); \
             INSERT INTO pl_proposed_cards (id, session_id, slug_suggestion, title, body_md, template_kind, priority, size_estimate, status) VALUES (5, 1, 'OLD-SLUG', 'old', 'old body', 'feature-add', 3, 'S', 'awaiting_approval'); \
             INSERT INTO pl_questions (id, session_id, step, order_index, question_md) VALUES (10, 1, 'clarify', 0, 'Which auth?'), (11, 1, 'clarify', 1, 'OAuth provider?'), (12, 1, 'research', 0, 'Any prior art?'); \
             INSERT INTO pl_answers (id, question_id, selected_option_id, writein_md) VALUES (100, 10, 1000, NULL), (101, 11, NULL, 'Google'); \
             INSERT INTO pl_question_options (id, option_md) VALUES (1000, 'Password'); \
             INSERT INTO pl_research_findings (id, session_id, finding_type, target_ref, why_relevant_md, order_index) VALUES (1, 1, 'file', 'src/auth.js', 'core login path', 0), (2, 1, 'symbol', 'loginHandler', NULL, 1);",
        ).unwrap();

        let r = plSessionFinalize(&json!({ "session_id": 1 }), &c).unwrap();
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["proposed_card_id", "session_id", "slug_suggestion", "template_kind", "new_status", "answers_included", "research_findings_included"]);
        assert_eq!(r["proposed_card_id"], json!(6));
        assert_eq!(r["session_id"], json!(1));
        assert_eq!(r["slug_suggestion"], json!("FEAT-LOGIN"));
        assert_eq!(r["template_kind"], json!("bug-fix"));
        assert_eq!(r["new_status"], json!("awaiting_approval"));
        assert_eq!(r["answers_included"], json!(2));
        assert_eq!(r["research_findings_included"], json!(2));

        let expected_body = [
            "## Intent",
            "Fix the broken login crash",
            "",
            "## Clarifications",
            "- **Q (clarify):** Which auth?",
            "  - **A:** Password",
            "- **Q (clarify):** OAuth provider?",
            "  - **A:** Google",
            "- **Q (research):** Any prior art?",
            "  - **A:** _(unanswered)_",
            "",
            "## Research",
            "- **file**: `src/auth.js` \u{2014} core login path",
            "- **symbol**: `loginHandler`",
            "",
            "## Acceptance",
            "_(to be filled by TL when drafting the contract)_",
        ].join("\n");
        let body: String = c.query_row("SELECT body_md FROM pl_proposed_cards WHERE id = 6", [], |row| row.get(0)).unwrap();
        assert_eq!(body, expected_body);
        let sup: String = c.query_row("SELECT status FROM pl_proposed_cards WHERE id = 5", [], |row| row.get(0)).unwrap();
        assert_eq!(sup, "superseded");
        let (sstatus, supd): (String, String) = c.query_row("SELECT status, updated_at FROM pl_sessions WHERE id = 1", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(sstatus, "awaiting_approval");
        assert_eq!(supd, "2026-01-01 00:00:00"); // frozen clock literal
    }

    #[test]
    fn synthesizes_slug_no_qa_no_findings() {
        let c = db0();
        c.execute("INSERT INTO pl_sessions (id, status, raw_intent, approved_card_slug, updated_at) VALUES (2, 'clarifying', 'Add a refactor cleanup pass', NULL, '2025-11-01 00:00:00')", []).unwrap();
        let r = plSessionFinalize(&json!({ "session_id": 2 }), &c).unwrap();
        assert_eq!(r["template_kind"], json!("refactor"));
        assert_eq!(r["slug_suggestion"], json!("REFACTOR-ADD-A-REFACTOR-CLEANUP-PASS-S2"));
        assert_eq!(r["answers_included"], json!(0));
        assert_eq!(r["research_findings_included"], json!(0));
        assert_eq!(r["proposed_card_id"], json!(1));
        let body: String = c.query_row("SELECT body_md FROM pl_proposed_cards WHERE id = 1", [], |row| row.get(0)).unwrap();
        assert_eq!(body, "## Intent\nAdd a refactor cleanup pass\n\n## Acceptance\n_(to be filled by TL when drafting the contract)_");
    }

    #[test]
    fn missing_bound_card_synthesizes_and_findings_only() {
        let c = db0();
        c.execute_batch(
            "INSERT INTO pl_sessions (id, status, raw_intent, approved_card_slug, updated_at) VALUES (3, 'researching', 'Write docs for the deploy guide', 'MISSING-CARD', '2025-10-01 00:00:00'); \
             INSERT INTO cards (slug) VALUES ('OTHER-CARD'); \
             INSERT INTO pl_research_findings (id, session_id, finding_type, target_ref, why_relevant_md, order_index) VALUES (1, 3, 'pattern', 'deploy.yml', 'CI config', 0);",
        ).unwrap();
        let r = plSessionFinalize(&json!({ "session_id": 3 }), &c).unwrap();
        assert_eq!(r["template_kind"], json!("docs"));
        assert_eq!(r["slug_suggestion"], json!("DOCS-WRITE-DOCS-FOR-THE-DEPLOY-GUIDE-S3"));
        assert_eq!(r["research_findings_included"], json!(1));
        let body: String = c.query_row("SELECT body_md FROM pl_proposed_cards WHERE id = 1", [], |row| row.get(0)).unwrap();
        assert_eq!(body, "## Intent\nWrite docs for the deploy guide\n\n## Research\n- **pattern**: `deploy.yml` \u{2014} CI config\n\n## Acceptance\n_(to be filled by TL when drafting the contract)_");
    }

    #[test]
    fn classify_branches() {
        for (id, status, intent, kind, prefix) in [
            (4, "researching", "Update deploy config migration", "ops", "OPS"),
            (5, "deciding_split", "Remove legacy cruft", "tech-debt", "TECHDEBT"),
            (6, "clarifying", "Decide the design approach", "arch", "ARCH"),
            (7, "awaiting_approval", "Build a new widget", "feature-add", "FEAT"),
        ] {
            let c = db0();
            c.execute("INSERT INTO pl_sessions (id, status, raw_intent, approved_card_slug, updated_at) VALUES (?, ?, ?, NULL, '2025-09-01 00:00:00')",
                rusqlite::params![id, status, intent]).unwrap();
            let r = plSessionFinalize(&json!({ "session_id": id }), &c).unwrap();
            assert_eq!(r["template_kind"], json!(kind), "intent={intent}");
            let slug = r["slug_suggestion"].as_str().unwrap();
            assert!(slug.starts_with(&format!("{prefix}-")), "slug {slug} should start {prefix}-");
            assert!(slug.ends_with(&format!("-S{id}")));
        }
    }

    #[test]
    fn guards_reject() {
        let c = db0();
        c.execute("INSERT INTO pl_sessions (id, status, raw_intent, approved_card_slug, updated_at) VALUES (8, 'approved', 'anything here', NULL, '2025-01-01 00:00:00')", []).unwrap();
        assert!(plSessionFinalize(&json!({ "session_id": 999 }), &c).is_err());
        assert!(plSessionFinalize(&json!({ "session_id": 8 }), &c).is_err());
        assert!(plSessionFinalize(&json!({}), &c).is_err());
    }
}
