//! canonical-heimdall.rs — twin of skills/engine/canonical-heimdall.mjs: HEIMDALL H0 seed data
//! (4 weight profiles, the category taxonomy, the template-base→category map) + the read
//! accessor + the idempotent db seeder. NAMES ARE CONTRACT: HEIMDALL_WEIGHT_PROFILES,
//! HEIMDALL_CATEGORIES, HEIMDALL_TEMPLATE_CATEGORIES, canonicalHeimdall (default), seedHeimdall.
// QUALITY: n/a — pressed statics + a trivial accessor + straight-line INSERT OR IGNORE seeding
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::LazyLock;

pub static HEIMDALL_WEIGHT_PROFILES: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(r#"[{"slug":"BUILDER","title":"code artifacts — clean output above all","weights":{"clean":0.45,"burn_inv":0.35,"ship":0.15,"cost_fit":0.05}},{"slug":"DESIGNER","title":"structure decisions — judged by what ships","weights":{"clean":0.25,"burn_inv":0.35,"ship":0.35,"cost_fit":0.05}},{"slug":"JUDGE","title":"evaluations — consistency beats brilliance","weights":{"consistency":0.5,"grounding":0.3,"reversal_inv":0.2}},{"slug":"TALKER","title":"language + reasoning — rubric-led, cost-aware","weights":{"rubric":0.55,"clean":0.25,"cost_fit":0.2}}]"#)
        .expect("HEIMDALL_WEIGHT_PROFILES: embedded JSON is valid by construction")
});

// tuples: [slug, family, graded_by, title] — matches the JS array-of-tuples exactly.
pub static HEIMDALL_CATEGORIES: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(r#"[["code-snippet","BUILD","deterministic","fill a body/block to a signature"],["code-stub","BUILD","deterministic","declarations + signatures, TODO bodies"],["code-refactor","BUILD","deterministic","transform existing code preserving behavior"],["code-debug","BUILD","deterministic","locate/explain a defect from evidence"],["code-test","BUILD","deterministic","author test cases / assertions"],["sql-authoring","BUILD","deterministic","SQL queries and migrations"],["shell-ops","BUILD","deterministic","command sequences, env bootstrap"],["format-strict","BUILD","deterministic","unavoidable JSON/schema emission"],["diff-patch","BUILD","deterministic","literal patch emission"],["architecture","DESIGN","both","decompose into files/blocks/systems"],["api-contract","DESIGN","both","interface / schema shape decisions"],["naming","DESIGN","both","identifier derivation"],["planning","DESIGN","both","scope, decomposition, task sequencing"],["estimation","DESIGN","both","size / risk tiering"],["critique-adversarial","JUDGMENT","both","prosecutor/defender claims"],["verdict-binary","JUDGMENT","both","true/false rulings per claim"],["rubric-scoring","JUDGMENT","both","grade work against criteria"],["risk-assessment","JUDGMENT","both","screening risk / blast analysis"],["prose-technical","LANGUAGE","rubric","docs, reports, explanations"],["prose-conversational","LANGUAGE","rubric","chat / interviewer turns"],["summarize-compress","LANGUAGE","rubric","digest, consolidate, commit messages"],["extract-structure","LANGUAGE","rubric","unstructured → fields/diagrams/cards"],["translate-localize","LANGUAGE","rubric","pt-BR ⇄ en"],["general","LANGUAGE","rubric","fallback for unmapped / low-confidence free-form"],["logic-deduction","REASONING","both","constraint / rule application"],["math-quant","REASONING","both","budgets, arithmetic, estimates"],["classification","REASONING","both","single/multi-label decisions"],["retrieval-grounded","REASONING","both","answer strictly from provided evidence"],["long-context","REASONING","both","synthesis over big bundles"]]"#)
        .expect("HEIMDALL_CATEGORIES: embedded JSON is valid by construction")
});

pub static HEIMDALL_TEMPLATE_CATEGORIES: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(r#"{"blueprint.snippet.author":"code-snippet","swarm.author.block":"code-snippet","tl.author.file":"code-snippet","dev.exec.microtask":"code-snippet","dev.implement.code":"code-snippet","atlas.code.sample":"code-snippet","blueprint.stub":"code-stub","code.regenerate":"code-refactor","man.diagnose.failure":"code-debug","qa.feedback.remediate":"code-debug","fixture.values":"format-strict","swarm.manifest.reformat":"format-strict","system.card.body.reshape":"format-strict","blueprint.skeleton":"architecture","blueprint.block":"architecture","swarm.manifest.design":"architecture","tl.contract.redraft":"api-contract","pl.screening.clarify":"planning","pl.screening.refine":"planning","pl.clarify.questions":"planning","pl.decompose.decision":"planning","system.card.decompose":"planning","tl.refine.microtasks":"planning","swarm.prosecutor":"critique-adversarial","swarm.prosecutor.design":"critique-adversarial","swarm.defender":"critique-adversarial","swarm.defender.design":"critique-adversarial","swarm.judge":"verdict-binary","swarm.judge.design":"verdict-binary","pl.ratify.contract":"verdict-binary","board-keeper.card.evaluate":"rubric-scoring","board-keeper.card.score":"rubric-scoring","board-keeper.epic.evaluate":"rubric-scoring","card.pl.accuracy":"rubric-scoring","qa.contract.review":"rubric-scoring","qa.contract.plan.review":"rubric-scoring","qa.senior.review":"rubric-scoring","pl.contract.review":"rubric-scoring","atlas.review":"rubric-scoring","atlas.insights.review":"rubric-scoring","cockpit.pr.analyze":"risk-assessment","interviewer.chat":"prose-conversational","interviewer.clarify.questionnaire":"prose-conversational","interviewer.fallback.sdr":"prose-conversational","interviewer.ideation.step":"prose-conversational","interviewer.present":"prose-conversational","system.card.discuss":"prose-conversational","system.card.discuss.field":"prose-conversational","system.turn.rewrite":"prose-conversational","system.repl.direct":"prose-conversational","notebook.followups":"prose-conversational","notebook.tutor":"prose-conversational","blueprint.describe":"prose-technical","code.describe":"prose-technical","code.docs.overview":"prose-technical","atlas.snippet.explain":"prose-technical","atlas.symbol.explain":"prose-technical","design.styleguide":"prose-technical","keeper.postmortem.narrative":"prose-technical","interviewer.investigate.present":"prose-technical","notebook.generate":"prose-technical","atlas.story.propose":"prose-technical","swarm.subagent.tune":"prose-technical","cockpit.promote":"prose-technical","atlas.architecture.digest":"summarize-compress","atlas.file.snapshot":"summarize-compress","atlas.file.summarize":"summarize-compress","atlas.symbol.summarize":"summarize-compress","git-manager.commit.msg":"summarize-compress","keeper.card.consolidate":"summarize-compress","man.review.lessons":"summarize-compress","pl.research.synthesize":"summarize-compress","pl.research.summary":"summarize-compress","board-keeper.card.digest":"summarize-compress","interviewer.investigate.synthesize":"summarize-compress","system.deepsearch.synthesize":"summarize-compress","notebook.flashcards":"extract-structure","notebook.mindmap":"extract-structure","notebook.quiz":"extract-structure","system.card.body.enrich":"extract-structure","atlas.logic.spec":"extract-structure","atlas.architecture.diagram":"extract-structure","code.file.metadata":"extract-structure","notebook.ask":"retrieval-grounded","notebook.compare":"retrieval-grounded","pl.screening.research":"retrieval-grounded","ai.reason.generic":"general"}"#)
        .expect("HEIMDALL_TEMPLATE_CATEGORIES: embedded JSON is valid by construction")
});

// JS `FAMILY_PROFILE` (module-private object) — only the 5 families HEIMDALL_CATEGORIES uses.
fn family_profile(family: &str) -> &'static str {
    match family {
        "BUILD" => "BUILDER",
        "DESIGN" => "DESIGNER",
        "JUDGMENT" => "JUDGE",
        "LANGUAGE" | "REASONING" => "TALKER",
        _ => "TALKER",
    }
}

/// JS: `canonicalHeimdall(_input, _context)` — the skill contract: read-only seed catalog.
pub fn canonicalHeimdall(_input: &Value, _db: &Connection) -> Result<Value, String> {
    let categories: Vec<Value> = HEIMDALL_CATEGORIES
        .as_array()
        .expect("array")
        .iter()
        .map(|t| {
            let a = t.as_array().expect("tuple");
            let mut o = Map::new();
            o.insert("slug".into(), a[0].clone());
            o.insert("family".into(), a[1].clone());
            o.insert("graded_by".into(), a[2].clone());
            o.insert("title".into(), a[3].clone());
            Value::Object(o)
        })
        .collect();
    let mut out = Map::new();
    out.insert("profiles".into(), HEIMDALL_WEIGHT_PROFILES.clone());
    out.insert("categories".into(), Value::Array(categories));
    out.insert("template_categories".into(), HEIMDALL_TEMPLATE_CATEGORIES.clone());
    Ok(Value::Object(out))
}

/// JS: `seedHeimdall(db)` — idempotent (INSERT OR IGNORE) seed of the 3 heimdall tables, then
/// activates any 'dormant' category that a template_categories row now maps to. Returns
/// undefined (→ null); the trailing UPDATE is what lets a later release's new template mapping
/// flip a previously-dormant category live on the next open.
pub fn seedHeimdall(db: &Connection) -> Value {
    for p in HEIMDALL_WEIGHT_PROFILES.as_array().expect("array") {
        let weights_json = json!(serde_json::to_string(&p["weights"]).unwrap_or_default());
        let _ = crate::db_json::run_write(
            db,
            "INSERT OR IGNORE INTO brain.heimdall_weight_profiles (slug, title, weights_json) VALUES (?, ?, ?)",
            &[p["slug"].clone(), p["title"].clone(), weights_json],
        );
    }

    let mapped: HashSet<&str> = HEIMDALL_TEMPLATE_CATEGORIES
        .as_object()
        .expect("object")
        .values()
        .filter_map(|v| v.as_str())
        .collect();
    for t in HEIMDALL_CATEGORIES.as_array().expect("array") {
        let a = t.as_array().expect("tuple");
        let slug = a[0].as_str().unwrap_or("");
        let family = a[1].as_str().unwrap_or("");
        let status = if mapped.contains(slug) { "active" } else { "dormant" };
        let _ = crate::db_json::run_write(
            db,
            "INSERT OR IGNORE INTO brain.heimdall_categories (slug, family, title, description_md, weight_profile, status, graded_by) VALUES (?, ?, ?, ?, ?, ?, ?)",
            &[a[0].clone(), a[1].clone(), a[3].clone(), json!(""), json!(family_profile(family)), json!(status), a[2].clone()],
        );
    }

    for (base, category) in HEIMDALL_TEMPLATE_CATEGORIES.as_object().expect("object") {
        let _ = crate::db_json::run_write(
            db,
            "INSERT OR IGNORE INTO brain.heimdall_template_categories (template_base, category) VALUES (?, ?)",
            &[json!(base), category.clone()],
        );
    }

    let _ = crate::db_json::run_write(
        db,
        "UPDATE brain.heimdall_categories SET status = 'active' WHERE status = 'dormant' AND slug IN (SELECT DISTINCT category FROM brain.heimdall_template_categories)",
        &[],
    );

    Value::Null
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        db.execute_batch(
            "CREATE TABLE brain.heimdall_weight_profiles (slug TEXT PRIMARY KEY, title TEXT NOT NULL, weights_json TEXT NOT NULL);
             CREATE TABLE brain.heimdall_categories (slug TEXT PRIMARY KEY, family TEXT NOT NULL, title TEXT NOT NULL, description_md TEXT NOT NULL DEFAULT '', weight_profile TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'dormant', graded_by TEXT NOT NULL DEFAULT 'both');
             CREATE TABLE brain.heimdall_template_categories (template_base TEXT PRIMARY KEY, category TEXT NOT NULL);",
        ).unwrap();
        db
    }

    #[test]
    fn canonical_heimdall_shapes_categories_from_the_pressed_tuples() {
        let db = Connection::open_in_memory().unwrap();
        let out = canonicalHeimdall(&Value::Null, &db).unwrap();
        assert_eq!(out["profiles"], *HEIMDALL_WEIGHT_PROFILES);
        assert_eq!(out["template_categories"], *HEIMDALL_TEMPLATE_CATEGORIES);
        let cats = out["categories"].as_array().unwrap();
        assert_eq!(cats.len(), 29);
        assert_eq!(cats[0], json!({ "slug": "code-snippet", "family": "BUILD", "graded_by": "deterministic", "title": "fill a body/block to a signature" }));
        assert_eq!(cats[9], json!({ "slug": "architecture", "family": "DESIGN", "graded_by": "both", "title": "decompose into files/blocks/systems" }));
    }

    #[test]
    fn seed_heimdall_populates_all_three_tables_on_a_fresh_db() {
        let db = seeded_db();
        assert_eq!(seedHeimdall(&db), Value::Null);
        let profiles: i64 = db.query_row("SELECT COUNT(*) FROM brain.heimdall_weight_profiles", [], |r| r.get(0)).unwrap();
        assert_eq!(profiles, 4);
        let cats: i64 = db.query_row("SELECT COUNT(*) FROM brain.heimdall_categories", [], |r| r.get(0)).unwrap();
        assert_eq!(cats, 29);
        let maps: i64 = db.query_row("SELECT COUNT(*) FROM brain.heimdall_template_categories", [], |r| r.get(0)).unwrap();
        assert_eq!(maps, 87);
        let mapped_status: String = db.query_row("SELECT status FROM brain.heimdall_categories WHERE slug = 'code-snippet'", [], |r| r.get(0)).unwrap();
        assert_eq!(mapped_status, "active");
        let unmapped_status: String = db.query_row("SELECT status FROM brain.heimdall_categories WHERE slug = 'code-test'", [], |r| r.get(0)).unwrap();
        assert_eq!(unmapped_status, "dormant");
        let weights: String = db.query_row("SELECT weights_json FROM brain.heimdall_weight_profiles WHERE slug = 'BUILDER'", [], |r| r.get(0)).unwrap();
        assert_eq!(weights, r#"{"clean":0.45,"burn_inv":0.35,"ship":0.15,"cost_fit":0.05}"#);
    }

    #[test]
    fn seed_heimdall_preserves_pre_existing_rows_and_activates_a_dormant_category() {
        let db = seeded_db();
        db.execute("INSERT INTO brain.heimdall_weight_profiles (slug, title, weights_json) VALUES ('BUILDER', 'OLD TITLE', '{}')", []).unwrap();
        db.execute("INSERT INTO brain.heimdall_categories (slug, family, title, description_md, weight_profile, status, graded_by) VALUES ('code-test', 'BUILD', 'OLD TITLE', 'old', 'BUILDER', 'dormant', 'deterministic')", []).unwrap();
        db.execute("INSERT INTO brain.heimdall_template_categories (template_base, category) VALUES ('blueprint.snippet.author', 'OVERRIDDEN')", []).unwrap();
        db.execute("INSERT INTO brain.heimdall_template_categories (template_base, category) VALUES ('legacy.mapping', 'code-test')", []).unwrap();

        seedHeimdall(&db);

        let builder_title: String = db.query_row("SELECT title FROM brain.heimdall_weight_profiles WHERE slug = 'BUILDER'", [], |r| r.get(0)).unwrap();
        assert_eq!(builder_title, "OLD TITLE"); // INSERT OR IGNORE never overwrites
        let override_kept: String = db.query_row("SELECT category FROM brain.heimdall_template_categories WHERE template_base = 'blueprint.snippet.author'", [], |r| r.get(0)).unwrap();
        assert_eq!(override_kept, "OVERRIDDEN");
        // 'code-test' isn't in the code's own mapping, but a pre-existing template_categories row
        // now maps to it — the trailing UPDATE activates the dormant category on this open.
        let activated: String = db.query_row("SELECT status FROM brain.heimdall_categories WHERE slug = 'code-test'", [], |r| r.get(0)).unwrap();
        assert_eq!(activated, "active");
        let maps: i64 = db.query_row("SELECT COUNT(*) FROM brain.heimdall_template_categories", [], |r| r.get(0)).unwrap();
        assert_eq!(maps, 88); // 87 code-inserted (blueprint.snippet.author ignored) + 1 pre-existing legacy row
    }
}
