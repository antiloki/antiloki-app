//! constitution-agents.rs — twin of skills/constitution/constitution-agents.mjs (db READ lane).
//! NAMES ARE CONTRACT: `constitutionAgents` (default). THE ENGINEERING AGENTS REGISTRY: a
//! deterministic derivation of every review lens applicable to this workspace, from the same
//! census the judge uses (languages from the latest 'done' snapshot's own judged files,
//! frameworks via resolveFrameworks, active packs via constitutionPacks, ratified laws grouped
//! by concern via categoryOf, plus the fixed engineering axes / book-faculty / sub-specialist
//! tables). Recompute-on-demand, no writes.
// QUALITY: n/a — straight structural port; the `code.code_nodes WHERE kind!='file'` census query
// references a column that does not exist (real column is `type`) — this is the JS source AS
// WRITTEN, so `symbols` always fails to prepare and the swallow-catch collapses it to 0. Kept
// verbatim (never "fixed") per NAMES-ARE-CONTRACT / never edit JS source logic.
use rusqlite::Connection;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

fn cap(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

const AXES: [&str; 7] = ["compliance", "defects", "tests", "structure", "coupling", "cohesion", "change"];

fn axis_desc(ax: &str) -> &'static str {
    match ax {
        "compliance" => "the constitution itself \u{2014} every ratified law, weighted by severity and confidence",
        "defects" => "recorded defects and bench findings against each file",
        "tests" => "coverage presence \u{2014} untested load-bearing code and the tests that cannot fail",
        "structure" => "cognitive complexity, nesting, god-files and transitive import weight",
        "coupling" => "fan-in / fan-out, import cycles, the blast radius of a change",
        "cohesion" => "whether a file\u{2019}s parts actually work together or share an address",
        "change" => "churn \u{d7} complexity \u{2014} the hotspots where the next bug breeds",
        _ => "",
    }
}

fn lang_name(lg: &str) -> String {
    match lg {
        "ts" => "TypeScript".to_string(),
        "js" => "JavaScript".to_string(),
        "py" => "Python".to_string(),
        other => other.to_string(),
    }
}

// CAT_AXIS = { testing: 'tests', structure: 'structure', performance: 'structure', architecture: 'coupling' }
// (object literal insertion order matters for the AXIS-lens reverse lookup below).
const CAT_AXIS: [(&str, &str); 4] = [("testing", "tests"), ("structure", "structure"), ("performance", "structure"), ("architecture", "coupling")];

fn cat_axis_of(cat: &str) -> &'static str {
    for (k, v) in CAT_AXIS { if k == cat { return v; } }
    "compliance"
}
// Object.entries(CAT_AXIS).find(([,v]) => v === ax)?.[0] — first CAT_AXIS entry whose VALUE equals ax.
fn axis_cat_key(ax: &str) -> Option<&'static str> {
    for (k, v) in CAT_AXIS { if v == ax { return Some(k); } }
    None
}

struct SubRole { id: &'static str, title: &'static str, focus: &'static str, re: &'static str, census: Option<fn(&Census) -> Option<String>> }

fn subroles_for(cat: &str) -> &'static [SubRole] {
    match cat {
        "architecture" => &[
            SubRole { id: "dependencies", title: "Dependency Cartographer", focus: "the import graph \u{2014} direction, cycles, fan-out, and who is allowed to know whom", re: r"(?i)import|graph|cycle|depend|fan", census: Some(|c| if c.edges > 0 { Some(format!("the atlas records {} live code edges", c.edges)) } else { None }) },
            SubRole { id: "boundaries", title: "Boundary Warden", focus: "layer and module boundaries \u{2014} what crosses a line it should never cross", re: r"(?i)layer|boundar|module|isolat", census: None },
            SubRole { id: "modularity", title: "Modularity Surgeon", focus: "files and functions past human size \u{2014} where a boundary wants to exist and does not", re: r"(?i)complex|loc\b|file-loc|length|lines|god", census: None },
            SubRole { id: "reachability", title: "Reachability Auditor", focus: "code nothing reaches \u{2014} unreferenced and orphan files only maintenance still pays for", re: r"(?i)unreferenced|orphan|dead|unused|unreachable", census: None },
            SubRole { id: "surface", title: "API Surgeon", focus: "the public surface \u{2014} exports, contracts, and what every caller is forced to learn", re: r"(?i)export|api|surface|contract|interface", census: Some(|c| if c.symbols > 0 { Some(format!("the atlas records {} symbols", c.symbols)) } else { None }) },
            SubRole { id: "dataflow", title: "Data-Flow Tracer", focus: "how data actually travels \u{2014} sources, transforms, sinks and the hidden couplings met on the way", re: r"(?i)data|flow|state|store", census: None },
        ],
        "audit" => &[
            SubRole { id: "provenance", title: "Provenance Auditor", focus: "the audit trail \u{2014} every mutation accounted for, every claim traceable to a chain row", re: r"(?i)audit|timeline|provenance|chain", census: None },
            SubRole { id: "drift", title: "Drift Detective", focus: "record vs reality \u{2014} where the code moved and the record stayed behind", re: r"(?i)drift|absent|missing|stale|doc", census: None },
            SubRole { id: "secrets", title: "Secrets Warden", focus: "credentials, tokens and keys \u{2014} nothing secret inside anything committed", re: r"(?i)secret|credential|token|key|env", census: None },
        ],
        "testing" => &[
            SubRole { id: "assertions", title: "Assertion Examiner", focus: "whether the tests can actually FAIL \u{2014} real assertions, none fenced behind conditionals", re: r"(?i)assert|empty-test|conditional", census: None },
            SubRole { id: "hygiene", title: "Test-Hygiene Examiner", focus: "silenced suites \u{2014} .only and .skip markers that quietly turn the suite off", re: r"(?i)only-test|skip-test|todo-test", census: None },
            SubRole { id: "isolation", title: "Test-Isolation Warden", focus: "production code importing test code \u{2014} the seam that must never leak", re: r"(?i)test-import|test-in-prod", census: None },
            SubRole { id: "determinism", title: "Determinism Warden", focus: "flake sources \u{2014} randomness, sleeps, wall-clock time and order dependence in tests", re: r"(?i)random|sleep|time|flak|retry", census: None },
            SubRole { id: "coverage", title: "Coverage Geometer", focus: "the geometry of what is tested \u{2014} load-bearing exports no test ever reaches", re: r"(?i)coverage|untested|test-gap|snapshot", census: Some(|c| if c.test_files > 0 { Some(format!("the snapshot holds {} test files", c.test_files)) } else { None }) },
        ],
        "patterns" => &[
            SubRole { id: "shapes", title: "Pattern Archaeologist", focus: "the house shapes \u{2014} composition over inheritance, singletons, where the code repeats a form or silently forks from it", re: r"(?i)pattern|composition|singleton|factory|observer|inherit", census: None },
            SubRole { id: "signatures", title: "Signature Curator", focus: "what callers must learn \u{2014} parameter lists, export forms, the shape of every entry point", re: r"(?i)parameter|export|signature|arg", census: None },
            SubRole { id: "godobjects", title: "God-Object Splitter", focus: "objects that know everything \u{2014} one name holding three responsibilities", re: r"(?i)god", census: None },
        ],
        "structure" => &[
            SubRole { id: "complexity", title: "Complexity Surgeon", focus: "cognitive load \u{2014} nesting, branching, boolean chains and functions too big for one head", re: r"(?i)complex|nest|cyclomatic|boolean|chain|cognitive", census: None },
            SubRole { id: "godfiles", title: "God-File Splitter", focus: "files doing three jobs at one address \u{2014} where a module boundary wants to exist", re: r"(?i)god|size|loc|lines|length|big", census: None },
            SubRole { id: "deadcode", title: "Dead-Code Undertaker", focus: "code nothing calls \u{2014} branches and exports only maintenance still pays for", re: r"(?i)dead|unused|unreachable", census: None },
        ],
        "security" => &[
            SubRole { id: "injection", title: "Injection Sentinel", focus: "untrusted content meeting an interpreter \u{2014} eval, new Function, innerHTML, document.write, dangerous HTML sinks", re: r"(?i)inject|sql|xss|shell|eval|sanitiz|new-function|inner-html|document-write|dangerously|href-javascript", census: None },
            SubRole { id: "secrets", title: "Secrets Warden", focus: "credentials, tokens and keys \u{2014} nothing secret hardcoded anywhere", re: r"(?i)secret|credential|token|apikey|password", census: None },
            SubRole { id: "transport", title: "Transport Guard", focus: "what leaves the process \u{2014} insecure channels and misplaced trust in the wire", re: r"(?i)http\b|insecure|tls|ssl|cors", census: None },
            SubRole { id: "trust", title: "Boundary-of-Trust Reviewer", focus: "who may do what \u{2014} authentication, authorization and the paths that skip them", re: r"(?i)auth|permission|grant|access", census: None },
        ],
        "framework" => &[
            SubRole { id: "lifecycle", title: "Lifecycle Steward", focus: "what the framework promises vs what the code assumes \u{2014} renders, effects, process lifetime", re: r"(?i)render|bind|lifecycle|process-exit|effect|unmount", census: None },
            SubRole { id: "idioms", title: "Idiom Keeper", focus: "the framework\u{2019}s own idioms \u{2014} keys, hooks, the patterns its docs teach", re: r"(?i)index-key|hook|idiom|deps", census: None },
        ],
        "performance" => &[
            SubRole { id: "hotpaths", title: "Hot-Path Economist", focus: "cost where it compounds \u{2014} loops, awaits-in-loop, allocations and string-building on the busy paths", re: r"(?i)perf|n\+1|loop|alloc|concat|cache|memo", census: None },
            SubRole { id: "leaks", title: "Resource-Lifecycle Warden", focus: "what gets acquired and never released \u{2014} timers, listeners, subscriptions", re: r"(?i)clear|cleanup|unsubscribe|leak|dispose|listener|interval", census: None },
        ],
        "style" => &[
            SubRole { id: "suppressions", title: "Suppression Auditor", focus: "the silenced warnings \u{2014} every ignore pragma is a debt note with no due date", re: r"(?i)ignore|suppress|disable|nocheck", census: None },
        ],
        "correctness" => &[
            SubRole { id: "types", title: "Type-Integrity Examiner", focus: "holes in the type story \u{2014} any, casts, non-null assertions that promise what nobody checked", re: r"(?i)any\b|as-any|non-null|type", census: None },
            SubRole { id: "rigor", title: "Rigor Examiner", focus: "the loose ends \u{2014} coercing equality, var scoping, branching too clever to read", re: r"(?i)loose|equality|var\b|ternary", census: None },
            SubRole { id: "errors", title: "Error-Path Reviewer", focus: "how failure travels \u{2014} what gets thrown, what gets caught, what gets lost", re: r"(?i)throw|error|catch", census: None },
        ],
        "naming" => &[
            SubRole { id: "vocabulary", title: "Vocabulary Curator", focus: "one concept, one name \u{2014} where the code says the same thing three different ways", re: r"(?i)nam|vocab|term", census: None },
        ],
        "docs" => &[
            SubRole { id: "contracts", title: "Contract Scribe", focus: "the promises written above the code \u{2014} docs blocks that still match what the code now does", re: r"(?i)doc|comment|header", census: None },
        ],
        _ => &[],
    }
}

struct Census { edges: i64, symbols: i64, test_files: i64 }

struct BookAgent { id: &'static str, title: &'static str, description: &'static str, produces: &'static [&'static str] }

const BOOK_FACULTY: [BookAgent; 8] = [
    BookAgent { id: "book:definer", title: "\u{25c7} The Definer", description: "Fills the atlas's definition slots \u{2014} one term, the book's own sense, quote-grounded. The concepts dimension.", produces: &["IR concepts"] },
    BookAgent { id: "book:lawwright", title: "\u{2696} The Lawwright", description: "Presses normative sentences (and Warning/Caution callouts) into ratifiable rules, the book's qualifications kept verbatim. The doctrines dimension.", produces: &["IR doctrines"] },
    BookAgent { id: "book:questioneer", title: "\u{ff1f} The Questioneer", description: "Turns the book's own review questions into interviews our instruments can conduct. The questions dimension.", produces: &["IR questions"] },
    BookAgent { id: "book:skillsmith", title: "\u{2317} The Skillsmith", description: "Presses code listings and numbered procedures into reusable methods. The methods dimension.", produces: &["IR methods"] },
    BookAgent { id: "book:comparer", title: "\u{21cb} The Comparer", description: "When two authorities write about the same key: names SUPPORTS/CONFLICTS/REFINES \u{2014} preserved, never resolved. The dialogue dimension.", produces: &["authority edges"] },
    BookAgent { id: "book:librarian", title: "\u{25c8} The Librarian", description: "Writes each module's rolling snapshot and keeps its concepts \u{2014} the memory of the import.", produces: &["snapshot_md", "concepts"] },
    BookAgent { id: "book:warden", title: "\u{2696} The Quality Warden", description: "Judges every module import for clarity and applicability to THIS codebase \u{2014} the adversarial gate.", produces: &["quality verdict"] },
    BookAgent { id: "book:synthesizer", title: "\u{2697} The Synthesizer", description: "Drafts law and skill candidates \u{2014} only when the module truly carries one. Staging, never ratification.", produces: &["law drafts", "skill drafts"] },
];

fn json_str_vec(v: &[String]) -> Value { json!(v) }

// {...defaults, ...over} where `over` is applied in ITS OWN key order — an existing key updates
// value in place (position kept, gotcha 1), a new key appends. serde_json::Map (preserve_order)
// gives this for free via .insert().
fn push_agent(agents: &mut Vec<Value>, ready: bool, readiness: &str, langs: &[String], over: Vec<(&str, Value)>) {
    let mut m = Map::new();
    m.insert("enabled".to_string(), json!(true));
    m.insert("ready".to_string(), json!(ready));
    m.insert("readiness".to_string(), json!(readiness));
    m.insert("est_cost".to_string(), json!("low"));
    m.insert("est_runtime_s".to_string(), json!(8));
    m.insert("consumes".to_string(), json!(["engineering snapshot", "violations + evidence", "the file source", "neighborhood"]));
    m.insert("produces".to_string(), json!(["findings", "fix sketches", "cited laws", "confidence"]));
    m.insert("languages".to_string(), json_str_vec(langs));
    m.insert("technologies".to_string(), json!([]));
    m.insert("required_packs".to_string(), json!([]));
    for (k, v) in over { m.insert(k.to_string(), v); }
    agents.push(Value::Object(m));
}

fn applies_of(l: &Value) -> Value {
    let s = l.get("applies_json").and_then(|v| v.as_str()).unwrap_or("{}");
    match serde_json::from_str::<Value>(s) {
        Ok(Value::Null) | Err(_) => json!({}),
        Ok(v) => v,
    }
}

fn c1(db: &Connection, sql: &str) -> i64 {
    match crate::db_json::query_rows(db, sql, &[]) {
        Ok(v) => v.as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).and_then(Value::as_i64).unwrap_or(0),
        Err(_) => 0,
    }
}

struct LangRes { ts: Regex, js: Regex, py: Regex, test: Regex }
fn lang_res() -> &'static LangRes {
    static R: OnceLock<LangRes> = OnceLock::new();
    R.get_or_init(|| LangRes {
        ts: Regex::new(r"\.tsx?$").unwrap(),
        js: Regex::new(r"\.[cm]?jsx?$").unwrap(),
        py: Regex::new(r"\.py$").unwrap(),
        test: Regex::new(r"\.(test|spec)\.|__tests__|/tests?/").unwrap(),
    })
}

pub fn constitutionAgents(_input: &Value, db: &Connection) -> Result<Value, String> {
    let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(db);
    // JS: `db.prepare(...).get()` is NOT wrapped in try/catch here — a missing judgments schema
    // (table not attached / not created) must propagate, not degrade to "no snapshot".
    let snap: Option<i64> = crate::db_json::query_rows(db, "SELECT id FROM judgments.review_snapshots WHERE status='done' ORDER BY id DESC LIMIT 1", &[])?
        .as_array().and_then(|a| a.first().cloned()).and_then(|r| r.get("id").and_then(Value::as_i64));

    let composed = crate::constitution_constitution_law::composedConstitution(db);
    let laws: Vec<Value> = composed.get("laws").and_then(Value::as_array).cloned().unwrap_or_default();
    let active: Vec<Value> = laws.into_iter().filter(|l| {
        l.get("status").and_then(Value::as_str) == Some("ratified") && l.get("enforcement").and_then(Value::as_str) == Some("score")
    }).collect();
    let frameworks: Vec<String> = crate::constitution_constitution_law::resolveFrameworks(db)
        .as_array().cloned().unwrap_or_default().iter().filter_map(|v| v.as_str().map(String::from)).collect();

    // language census — from the snapshot's own judged files
    let mut langs: Vec<String> = Vec::new();
    let mut test_files: i64 = 0;
    if let Some(sid) = snap {
        let rows = crate::db_json::query_rows(db, "SELECT path FROM judgments.review_scores WHERE snapshot_id=? AND unit_kind='file'", &[json!(sid)])
            .ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
        let r = lang_res();
        for row in rows {
            let p = row.get("path").map(|v| match v { Value::String(s) => s.clone(), Value::Null => "null".to_string(), o => o.to_string() }).unwrap_or_default().to_lowercase();
            if r.ts.is_match(&p) && !langs.iter().any(|x| x == "ts") { langs.push("ts".to_string()); }
            if r.js.is_match(&p) && !langs.iter().any(|x| x == "js") { langs.push("js".to_string()); }
            if r.py.is_match(&p) && !langs.iter().any(|x| x == "py") { langs.push("py".to_string()); }
            if r.test.is_match(&p) { test_files += 1; }
        }
    }
    let census = Census {
        edges: c1(db, "SELECT COUNT(*) AS n FROM code.code_edges"),
        symbols: c1(db, "SELECT COUNT(*) AS n FROM code.code_nodes WHERE kind!='file'"),
        test_files,
    };

    let packs: Vec<Value> = match crate::constitution_constitution_packs::constitutionPacks(&json!({"op": "list"}), db) {
        Ok(v) => v.get("packs").and_then(Value::as_array).cloned().unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    // byCat — insertion order = first-seen category among `active`, like a JS Map.
    let mut cat_order: Vec<String> = Vec::new();
    let mut by_cat: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for l in &active {
        let c = crate::constitution__helpers_categories::categoryOf(l).as_str().unwrap_or("").to_string();
        if !by_cat.contains_key(&c) { cat_order.push(c.clone()); by_cat.insert(c.clone(), Vec::new()); }
        if let Some(k) = l.get("key").and_then(Value::as_str) { by_cat.get_mut(&c).unwrap().push(k.to_string()); }
    }

    let ready = snap.is_some();
    let readiness = if ready { "ready" } else { "rebuild first — no snapshot yet" };
    let mut agents: Vec<Value> = Vec::new();

    // AXIS lenses — one per engineering axis
    for ax in AXES {
        let laws_v: Vec<Value> = if ax == "compliance" {
            active.iter().take(24).filter_map(|l| l.get("key").cloned()).collect()
        } else {
            let cat = axis_cat_key(ax).unwrap_or("");
            by_cat.get(cat).cloned().unwrap_or_default().into_iter().take(24).map(|k| json!(k)).collect()
        };
        push_agent(&mut agents, ready, readiness, &langs, vec![
            ("id", json!(format!("axis:{ax}"))),
            ("title", json!(format!("{} Reviewer", cap(ax)))),
            ("category", json!("axis")),
            ("description", json!(format!("Reviews the {ax} axis: {}.", axis_desc(ax)))),
            ("capabilities", json!([ax])),
            ("laws", json!(laws_v)),
            ("axes", json!([ax])),
        ]);
    }

    // CORE lenses — one per concern that actually has ratified laws here
    for cat in &cat_order {
        let keys = &by_cat[cat];
        let n = keys.len();
        push_agent(&mut agents, ready, readiness, &langs, vec![
            ("id", json!(format!("category:{cat}"))),
            ("title", json!(format!("{} Reviewer", cap(cat)))),
            ("category", json!("core")),
            ("description", json!(format!("Enforces the {n} ratified {cat} law{} of this workspace's constitution.", if n == 1 { "" } else { "s" }))),
            ("capabilities", json!([cat])),
            ("laws", json!(keys.iter().take(24).cloned().collect::<Vec<_>>())),
            ("axes", json!([cat_axis_of(cat)])),
        ]);
    }

    // THE BOOK FACULTY — always ready, feeds on the shelved books not the code snapshot.
    for bf in &BOOK_FACULTY {
        push_agent(&mut agents, ready, readiness, &langs, vec![
            ("id", json!(bf.id)),
            ("title", json!(bf.title)),
            ("description", json!(bf.description)),
            ("produces", json!(bf.produces)),
            ("category", json!("book")),
            ("ready", json!(true)),
            ("readiness", json!("ready — feeds on the shelved books")),
            ("consumes", json!(["the module block", "the import goal", "rolling snapshots"])),
            ("capabilities", json!(["book-import"])),
            ("laws", json!([])),
            ("axes", json!([])),
            ("est_runtime_s", json!(20)),
            ("est_cost", json!("low")),
        ]);
    }

    // SUB-SPECIALISTS — a concern with law-mass decomposes into the roles inside it.
    for cat in &cat_order {
        let keys = &by_cat[cat];
        for sub in subroles_for(cat) {
            let re = Regex::new(sub.re).unwrap();
            let mine: Vec<String> = keys.iter().filter(|k| re.is_match(k.as_str())).cloned().collect();
            let cen: Option<String> = sub.census.and_then(|f| f(&census));
            if mine.is_empty() && cen.is_none() { continue; }
            let reason = if !mine.is_empty() {
                format!("{} {cat} law{} it", mine.len(), if mine.len() == 1 { " targets" } else { "s target" })
            } else {
                cen.clone().unwrap_or_default()
            };
            let laws_v: Vec<String> = if !mine.is_empty() { mine } else { keys.clone() };
            push_agent(&mut agents, ready, readiness, &langs, vec![
                ("id", json!(format!("{cat}:{}", sub.id))),
                ("title", json!(sub.title)),
                ("category", json!("specialist")),
                ("description", json!(format!("{}. Exists because {}.", sub.focus, reason))),
                ("capabilities", json!([cat, sub.id])),
                ("laws", json!(laws_v.into_iter().take(24).collect::<Vec<_>>())),
                ("axes", json!([cat_axis_of(cat)])),
            ]);
        }
    }

    // LANGUAGE lenses — detected languages only
    for lg in &langs {
        let name = lang_name(lg);
        let laws_v: Vec<Value> = active.iter().filter(|l| {
            let a = applies_of(l);
            a.get("languages").and_then(Value::as_array).map_or(false, |arr| arr.iter().any(|x| x.as_str() == Some(lg.as_str())))
        }).take(24).filter_map(|l| l.get("key").cloned()).collect();
        push_agent(&mut agents, ready, readiness, &langs, vec![
            ("id", json!(format!("language:{lg}"))),
            ("title", json!(format!("{name} Reviewer"))),
            ("category", json!("language")),
            ("description", json!(format!("Reviews {name} sources against the laws scoped to the language."))),
            ("capabilities", json!([name])),
            ("languages", json!([lg])),
            ("laws", json!(laws_v)),
            ("axes", json!(["compliance"])),
        ]);
    }

    // FRAMEWORK lenses — detected frameworks only; the matching pack is the requirement
    for fw in &frameworks {
        let pk = packs.iter().find(|p| p.get("id").and_then(Value::as_str) == Some(fw.as_str()));
        let pk_active = pk.map_or(true, |p| p.get("status").and_then(Value::as_str) == Some("active"));
        let fw_ready = ready && pk_active;
        let fw_readiness = if !ready {
            readiness.to_string()
        } else if pk.is_some() && !pk_active {
            format!("enable the {fw} pack")
        } else {
            "ready".to_string()
        };
        let mut seen: Vec<String> = Vec::new();
        for l in active.iter().filter(|l| {
            let a = applies_of(l);
            a.get("frameworks").and_then(Value::as_array).map_or(false, |arr| arr.iter().any(|x| x.as_str() == Some(fw.as_str())))
        }) {
            if let Some(k) = l.get("key").and_then(Value::as_str) { if !seen.iter().any(|x| x == k) { seen.push(k.to_string()); } }
        }
        for l in active.iter().filter(|l| l.get("key").and_then(Value::as_str).map_or(false, |k| k.starts_with(&format!("{fw}-")))) {
            if let Some(k) = l.get("key").and_then(Value::as_str) { if !seen.iter().any(|x| x == k) { seen.push(k.to_string()); } }
        }
        let cap_fw = cap(fw);
        push_agent(&mut agents, ready, readiness, &langs, vec![
            ("id", json!(format!("framework:{fw}"))),
            ("title", json!(format!("{cap_fw} Reviewer"))),
            ("category", json!("framework")),
            ("description", json!(format!("Reviews {cap_fw}-specific usage against the {fw} laws of this constitution."))),
            ("capabilities", json!([cap_fw.clone()])),
            ("technologies", json!([cap_fw])),
            ("required_packs", json!(pk.and_then(|p| p.get("id").cloned()).map(|id| vec![id]).unwrap_or_default())),
            ("ready", json!(fw_ready)),
            ("readiness", json!(fw_readiness)),
            ("laws", json!(seen.into_iter().take(24).collect::<Vec<_>>())),
            ("axes", json!(["compliance"])),
        ]);
    }

    let specialists = agents.iter().filter(|a| a.get("category").and_then(Value::as_str) == Some("specialist")).count() as i64;
    let active_packs: Vec<Value> = packs.iter().filter(|p| p.get("status").and_then(Value::as_str) == Some("active")).filter_map(|p| p.get("id").cloned()).collect();

    let mut out = Map::new();
    out.insert("ok".to_string(), json!(true));
    out.insert("agents".to_string(), json!(agents));
    out.insert("derived_from".to_string(), json!({
        "snapshot_id": snap,
        "languages": langs,
        "frameworks": frameworks,
        "active_packs": active_packs,
        "active_laws": active.len() as i64,
        "specialists": specialists,
        "census": { "edges": census.edges, "symbols": census.symbols, "testFiles": census.test_files },
    }));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(db: &Connection) {
        db.execute_batch("
            ATTACH DATABASE ':memory:' AS laws;
            ATTACH DATABASE ':memory:' AS judgments;
            ATTACH DATABASE ':memory:' AS code;
            CREATE TABLE code.code_nodes (id TEXT PRIMARY KEY, origin TEXT, path TEXT, type TEXT, name TEXT);
            CREATE TABLE code.code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT);
        ").unwrap();
        crate::constitution__helpers_schema::ensureConstitutionSchema(db);
    }

    #[test]
    fn no_snapshot_not_ready() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        let r = constitutionAgents(&json!({}), &db).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["derived_from"]["snapshot_id"], Value::Null);
        // book-category agents feed on the shelved books, not the code snapshot — always ready.
        assert!(r["agents"].as_array().unwrap().iter().all(|a| (a["category"] == json!("book")) == (a["ready"] == json!(true))));
        // AXIS lenses always present (7), CORE/specialists/language/framework empty w/o laws
        assert_eq!(r["agents"].as_array().unwrap().iter().filter(|a| a["category"] == json!("axis")).count(), 7);
        assert_eq!(r["agents"].as_array().unwrap().iter().filter(|a| a["category"] == json!("book")).count(), 8);
    }

    #[test]
    fn derives_language_and_category_lenses() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        db.execute_batch("
            INSERT INTO judgments.review_snapshots (id, status, constitution_hash) VALUES (1, 'done', 'h1');
            INSERT INTO judgments.review_scores (snapshot_id, unit_kind, path) VALUES (1, 'file', 'src/a.ts'), (1, 'file', 'src/b.test.js');
            INSERT INTO laws.review_laws (scope, key, title, rule_md, status, enforcement) VALUES ('language', 'no-eval', 'No eval', 'r', 'ratified', 'score');
        ").unwrap();
        let r = constitutionAgents(&json!({}), &db).unwrap();
        assert_eq!(r["derived_from"]["snapshot_id"], json!(1));
        assert_eq!(r["derived_from"]["languages"], json!(["ts", "js"]));
        assert_eq!(r["derived_from"]["census"]["testFiles"], json!(1));
        assert_eq!(r["derived_from"]["census"]["symbols"], json!(0)); // bad `kind` column -> swallowed to 0
        let lang_agent = r["agents"].as_array().unwrap().iter().find(|a| a["id"] == json!("language:ts")).unwrap();
        assert_eq!(lang_agent["title"], json!("TypeScript Reviewer"));
        assert_eq!(lang_agent["laws"], json!([]));
        let sec_agent = r["agents"].as_array().unwrap().iter().find(|a| a["id"] == json!("category:security")).unwrap();
        assert_eq!(sec_agent["laws"], json!(["no-eval"]));
    }
}
