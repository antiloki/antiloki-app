//! agents-md.rs — twin of skills/render/agents-md.mjs (db READ lane, cross-db laws via ATTACH).
//! NAMES ARE CONTRACT: `agentsMd` (default). Renders laws.laws (+ law_clauses) as AGENTS.md —
//! cross-tool AI-agent instructions: quickstart, attached-db census, then laws grouped by kind.
use rusqlite::Connection;
use serde_json::Value;
use std::collections::HashMap;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn js_string(v:&Value)->String{ match v{Value::Null=>"null".into(),Value::String(s)=>s.clone(),Value::Number(n)=>n.to_string(),Value::Bool(b)=>b.to_string(),Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","),Value::Object(_)=>"[object Object]".into()} } // = JS String(v)
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>js_string(x) } } // = JS `${v}`
fn field<'a>(law: &'a Value, k: &str) -> Option<&'a Value> { law.get(k) }
fn esc_md(s: &str) -> String { s.replace('|', "\\|") }

const KIND_ORDER: [&str; 6] = ["constitutional", "constraint", "procedural", "permission", "decision", "principle"];

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "constitutional" => "Constitutional (HARD — never violated)",
        "constraint" => "Constraint (HARD — enforced at compile/runtime/DB layer)",
        "procedural" => "Procedural (workflow rules — soft, RAG-surfaced)",
        "permission" => "Permission (who-can-do-what)",
        "decision" => "Decision (binding architectural choices)",
        "principle" => "Principle (soft guidelines)",
        _ => unreachable!(),
    }
}

// name -> note, in DB_NOTES object-literal insertion order (the try/catch fallback iterates this order).
const DB_NOTES: [(&str, &str); 9] = [
    ("main", "`state.sqlite` — what the system IS (cards, agents, actions)"),
    ("memory", "`memory.sqlite` — what the system EXPERIENCED (memories, embeddings, narratives)"),
    ("brain", "`brain.sqlite` — what the system DOES/THINKS (driver fires, decisions, prompts, bench runs)"),
    ("laws", "`laws.sqlite` — what the system MUST/MUST NOT DO (the rules below)"),
    ("knowledge", "`knowledge.sqlite` — what the system has LEARNED FROM EXTERNAL SOURCES (vault extracts)"),
    ("timeline", "`timeline.sqlite` — the sacred_timeline audit chain (append-only, hash-linked)"),
    ("judgments", "`judgments.sqlite` — swarm audit verdicts"),
    ("code", "`code.sqlite` — the code atlas (files, symbols, imports)"),
    ("nb", "`notebooks.sqlite` — notebook sources + generated study artifacts"),
];
fn db_note(name: &str) -> String {
    DB_NOTES.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string()).unwrap_or_else(|| format!("`{name}`"))
}

fn render_law(law: &Value, clauses: &[Value]) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("### `{}`", tmpl(field(law, "slug"))));
    lines.push(String::new());
    lines.push(format!("**{}**", tmpl(field(law, "title"))));
    lines.push(String::new());
    lines.push(format!("> {}", tmpl(field(law, "statement"))));
    lines.push(String::new());
    lines.push(format!("**Why:** {}", tmpl(field(law, "rationale"))));
    lines.push(String::new());

    let mut meta: Vec<String> = vec![
        format!("enforcement `{}`", tmpl(field(law, "enforcement"))),
        format!("domain `{}`", tmpl(field(law, "domain"))),
        format!("by {}", tmpl(field(law, "established_by"))),
    ];
    if truthy(field(law, "enforcement_hook")) { meta.push(format!("hook `{}`", tmpl(field(law, "enforcement_hook")))); }
    if truthy(field(law, "trigger_sql")) { meta.push("+DB-trigger".to_string()); }
    if truthy(field(law, "source_card")) { meta.push(format!("card `{}`", tmpl(field(law, "source_card")))); }
    lines.push(format!("\u{2014} {}", meta.join(" \u{b7} ")));
    lines.push(String::new());

    if !clauses.is_empty() {
        let mut order: Vec<String> = Vec::new();
        let mut by_type: HashMap<String, Vec<String>> = HashMap::new();
        for c in clauses {
            let ct = js_string(c.get("clause_type").unwrap_or(&Value::Null));
            if !by_type.contains_key(&ct) { order.push(ct.clone()); by_type.insert(ct.clone(), Vec::new()); }
            by_type.get_mut(&ct).unwrap().push(js_string(c.get("text").unwrap_or(&Value::Null)));
        }
        for ct in &order {
            lines.push(format!("*{ct}:*"));
            for t in &by_type[ct] { lines.push(format!("- {}", esc_md(t))); }
            lines.push(String::new());
        }
    }

    lines.join("\n")
}

pub fn agentsMd(_input: &Value, db: &Connection) -> Result<Value, String> {
    let all_laws = crate::db_json::query_rows(
        db,
        "SELECT * FROM laws.laws WHERE superseded_by IS NULL AND enforcement != 'archived' ORDER BY kind, slug",
        &[],
    )?.as_array().cloned().unwrap_or_default();

    let mut out: Vec<String> = Vec::new();
    out.push("# AGENTS.md \u{2014} antiloki".to_string());
    out.push(String::new());
    out.push("> **AUTO-GENERATED from `.antiloki/db/laws.sqlite`.** Do not edit.".to_string());
    out.push("> To change a rule: `system.law.add` or `system.law.supersede`.".to_string());
    out.push("> To re-render: `node bin/antiloki.mjs system.laws.render.all '{}'`".to_string());
    out.push(String::new());
    out.push("Read this if you are an AI agent (Claude Code, Cursor, Aider, etc.) opening this repository.".to_string());
    out.push(String::new());

    out.push("## Quickstart for AI agents".to_string());
    out.push(String::new());
    out.push("1. **The substrate is queryable, not narrative.** Don't guess \u{2014} query.".to_string());
    out.push("   ```bash".to_string());
    out.push("   node bin/antiloki.mjs system.law.find '{\"query\":\"audit\"}'        # find rules about audit".to_string());
    out.push("   node bin/antiloki.mjs system.skill.find '{\"query\":\"file\"}'    # find tools for file ops".to_string());
    out.push("   node bin/antiloki.mjs system.action.find '{\"query\":\"memory\"}'     # find actions by topic".to_string());
    out.push("   node bin/antiloki.mjs system.consistency.report '{}'             # verify substrate health".to_string());
    out.push("   node bin/antiloki.mjs board-keeper.log.board '{}'                # current work state".to_string());
    out.push("   ```".to_string());
    out.push(String::new());
    out.push("2. **Hard laws CANNOT be violated** \u{2014} the substrate refuses (SQLite triggers, action-run hooks, lint, AST).".to_string());
    out.push("   Soft laws are surfaced via RAG when relevant.".to_string());
    out.push(String::new());
    out.push("3. **Every claim needs verification.** When you say \"I built X\", give a command the user can run.".to_string());
    out.push(String::new());

    let attached: Vec<String> = match crate::db_json::query_rows(db, "PRAGMA database_list", &[]) {
        Ok(Value::Array(rows)) => rows.iter().map(|r| js_string(r.get("name").unwrap_or(&Value::Null))).collect(),
        _ => DB_NOTES.iter().map(|(k, _)| k.to_string()).collect(),
    };
    out.push(format!("4. **{} attached SQLite databases:**", attached.len()));
    for name in &attached {
        out.push(format!("   - {}", db_note(name)));
    }
    out.push(String::new());
    out.push(format!("## Laws ({} active, by kind)", all_laws.len()));
    out.push(String::new());

    for kind in KIND_ORDER {
        let laws_of_kind: Vec<&Value> = all_laws.iter().filter(|l| l.get("kind").and_then(|v| v.as_str()) == Some(kind)).collect();
        if laws_of_kind.is_empty() { continue; }
        out.push(format!("## {}", kind_label(kind)));
        out.push(String::new());
        for law in laws_of_kind {
            let law_id = law.get("id").cloned().unwrap_or(Value::Null);
            let clauses = crate::db_json::query_rows(db, "SELECT clause_type, text, order_index FROM laws.law_clauses WHERE law_id = ? ORDER BY order_index", &[law_id])?
                .as_array().cloned().unwrap_or_default();
            out.push(render_law(law, &clauses));
        }
    }

    Ok(Value::String(out.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn db_with(laws_sql: &str, clauses_sql: &str) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; \
          CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT, kind TEXT, domain TEXT, title TEXT, statement TEXT, rationale TEXT, enforcement TEXT, enforcement_hook TEXT, trigger_sql TEXT, established_by TEXT, source_card TEXT, superseded_by INTEGER); \
          CREATE TABLE laws.law_clauses (id INTEGER PRIMARY KEY, law_id INTEGER, clause_type TEXT, text TEXT, order_index INTEGER);").unwrap();
        if !laws_sql.is_empty() { c.execute_batch(laws_sql).unwrap(); }
        if !clauses_sql.is_empty() { c.execute_batch(clauses_sql).unwrap(); }
        c
    }

    #[test]
    fn renders_quickstart_db_census_and_kind_sections() {
        let c = db_with(
            "INSERT INTO laws.laws (id,slug,kind,domain,title,statement,rationale,enforcement,enforcement_hook,trigger_sql,established_by,source_card,superseded_by) VALUES \
              (1,'AAA_LAW','constitutional','audit','T1','S1','R1','hard','hook-1','CREATE TRIGGER x','human:dani','CARD-1',NULL), \
              (2,'BBB_LAW','constraint','arch','T2','S2','R2','hard',NULL,NULL,'system:bootstrap',NULL,NULL), \
              (8,'OLD_LAW','decision','audit','Old','Old statement','Old rationale','soft',NULL,NULL,'human:dani',NULL,1), \
              (9,'ARCHIVED_LAW','principle','dev','Archived','Archived statement','Archived rationale','archived',NULL,NULL,'human:dani',NULL,NULL);",
            "INSERT INTO laws.law_clauses (id,law_id,clause_type,text,order_index) VALUES \
              (1,1,'antipattern','bad thing one',1), \
              (2,1,'antipattern','a | pipe here',2), \
              (3,1,'example','good thing',1);",
        );
        let r = agentsMd(&json!({}), &c).unwrap();
        let s = r.as_str().unwrap();
        assert!(s.starts_with("# AGENTS.md \u{2014} antiloki"));
        assert!(s.contains("> To re-render: `node bin/antiloki.mjs system.laws.render.all '{}'`"));
        assert!(s.contains("Don't guess \u{2014} query."));
        // 2 attached (main + laws)
        assert!(s.contains("4. **2 attached SQLite databases:**"));
        assert!(s.contains("- `state.sqlite` \u{2014} what the system IS (cards, agents, actions)"));
        assert!(s.contains("- `laws.sqlite` \u{2014} what the system MUST/MUST NOT DO (the rules below)"));
        // superseded + archived excluded
        assert!(s.contains("## Laws (2 active, by kind)"));
        assert!(!s.contains("OLD_LAW"));
        assert!(!s.contains("ARCHIVED_LAW"));
        // kind headers present in KIND_ORDER, constitutional before constraint
        let ci = s.find("## Constitutional (HARD").unwrap();
        let ki = s.find("## Constraint (HARD").unwrap();
        assert!(ci < ki);
        assert!(s.contains("### `AAA_LAW`"));
        assert!(s.contains("**T1**"));
        assert!(s.contains("> S1"));
        assert!(s.contains("**Why:** R1"));
        // full meta line: base 3 + hook + trigger + card
        assert!(s.contains("\u{2014} enforcement `hard` \u{b7} domain `audit` \u{b7} by human:dani \u{b7} hook `hook-1` \u{b7} +DB-trigger \u{b7} card `CARD-1`"));
        // clause grouping preserves first-seen order + escapes pipe
        assert!(s.contains("*antipattern:*\n- bad thing one\n- a \\| pipe here\n\n*example:*\n- good thing"));
        // law with none of hook/trigger/source: only base 3 meta fields
        assert!(s.contains("\u{2014} enforcement `hard` \u{b7} domain `arch` \u{b7} by system:bootstrap\n"));
    }

    #[test]
    fn empty_laws_table_has_zero_active_and_no_kind_sections() {
        let c = db_with("", "");
        let r = agentsMd(&json!({}), &c).unwrap();
        let s = r.as_str().unwrap();
        assert!(s.contains("## Laws (0 active, by kind)"));
        assert!(!s.contains("## Constitutional"));
        assert!(!s.contains("## Principle"));
    }

    #[test]
    fn law_with_no_clauses_omits_clause_sections() {
        let c = db_with(
            "INSERT INTO laws.laws (id,slug,kind,domain,title,statement,rationale,enforcement,enforcement_hook,trigger_sql,established_by,source_card,superseded_by) VALUES \
              (1,'SOLO_LAW','constraint','d','T','S','R','hard',NULL,NULL,'human:dani',NULL,NULL);",
            "",
        );
        let r = agentsMd(&json!({}), &c).unwrap();
        let s = r.as_str().unwrap();
        assert!(s.contains("### `SOLO_LAW`"));
        let idx = s.find("### `SOLO_LAW`").unwrap();
        assert!(!s[idx..].contains("*antipattern:*"));
        assert!(!s[idx..].contains("*example:*"));
    }

    #[test]
    fn clause_grouping_preserves_first_seen_type_order_across_multiple_types() {
        let c = db_with(
            "INSERT INTO laws.laws (id,slug,kind,domain,title,statement,rationale,enforcement,enforcement_hook,trigger_sql,established_by,source_card,superseded_by) VALUES \
              (1,'ALLMETA_LAW','constitutional','audit','All meta present','This law has everything','Because completeness','hard','hook-x','CREATE TRIGGER foo','human:dani+claude','CARD-X',NULL);",
            "INSERT INTO laws.law_clauses (id,law_id,clause_type,text,order_index) VALUES \
              (1,1,'consequence','c1',1), \
              (2,1,'antipattern','a1',2), \
              (3,1,'antipattern','a2 | with pipe',3), \
              (4,1,'example','e1',4);",
        );
        let r = agentsMd(&json!({}), &c).unwrap();
        let s = r.as_str().unwrap();
        assert!(s.contains("*consequence:*\n- c1\n\n*antipattern:*\n- a1\n- a2 \\| with pipe\n\n*example:*\n- e1"));
        assert!(s.contains("\u{2014} enforcement `hard` \u{b7} domain `audit` \u{b7} by human:dani+claude \u{b7} hook `hook-x` \u{b7} +DB-trigger \u{b7} card `CARD-X`"));
    }
}
