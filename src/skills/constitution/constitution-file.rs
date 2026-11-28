//! constitution-file.rs — twin of skills/constitution/constitution-file.mjs (db lane, cross-db
//! `laws`+`judgments`, plus unqualified `code_nodes`/`code_edges` atlas tables on the main db).
//! NAMES ARE CONTRACT: `constitutionFile` (default). THE PER-FILE SNAPSHOT DOC — pull everything
//! the latest constitution snapshot knows about ONE path: engineering health + compliance, every
//! axis, the raw signals, the law violations (proof chain), peers (same law broken elsewhere),
//! anchors (importers/imports/bugfinder findings), history, the ratchet (regression vs previous
//! snapshot), per-concern categories, and a rendered markdown doc block. $0, pure reads.
//!
//! Reuses already-converted twins: `ensureConstitutionSchema` (schema.rs), `categoryOf`
//! (categories.rs), `fixHintFor` (fix-hints.rs). `FIX_EXAMPLES` is a private data const on
//! fix-hints.mjs with no `pub` accessor on its Rust twin (same precedent as constitution-report.rs's
//! local `LINTERS` copy) — transcribed verbatim here as a local lookup.
//!
//! No file row for `path` → FOLDER review (aggregates every `judgments.review_scores` member
//! under the `path/` prefix): its own signals/history/doc_md shape, `categories`/`violations`/
//! `peers`(empty)/`anchors`(empty) — a deliberately shallower dossier than the per-file branch.
// QUALITY: 74/100 — a wide but shallow per-path dossier module, mirrors constitution-report.rs's
// aggregation style at file/folder granularity instead of whole-snapshot.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v:&Value)->String{ match v{Value::Null=>"null".into(),Value::String(s)=>s.clone(),Value::Number(n)=>n.to_string(),Value::Bool(b)=>b.to_string(),Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","),Value::Object(_)=>"[object Object]".into()} } // = JS String(v)
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

// JS `Math.round`: floor(x + 0.5) — half-up, unlike Rust's half-away-from-zero `.round()`.
fn js_round(x: f64) -> f64 { (x + 0.5).floor() }

// `try { JSON.parse(s ?? 'null') } catch { null }`
fn safe(v: Option<&Value>) -> Value {
    let text = match v { Some(Value::String(s)) => s.clone(), None | Some(Value::Null) => "null".to_string(), Some(other) => js_string(other) };
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

// `const mean = (arr) => (arr.length ? Math.round(arr.reduce((a,b)=>a+b,0)/arr.length) : null)`
fn mean(arr: &[f64]) -> Option<i64> {
    if arr.is_empty() { None } else { Some(js_round(arr.iter().sum::<f64>() / arr.len() as f64) as i64) }
}

// fix-hints.mjs's private `FIX_EXAMPLES` — a bad/good pair per law key, no `pub` accessor on
// fix-hints.rs (its unit is only `fixHintFor`). Transcribed verbatim (see module doc).
fn fix_example_for(key: &str) -> Value {
    let pair = match key {
        "no-swallowed-error" => ("try { save() } catch (e) {}", "try { save() } catch (e) { log.error(e); throw e; }"),
        "no-loose-equality" => ("if (a == b) …", "if (a === b) …"),
        "no-var" => ("var total = 0", "let total = 0   // const if never reassigned"),
        "no-debugger" => ("debugger;", "(deleted — set a breakpoint in devtools instead)"),
        "js-no-console" => ("console.log(user)", "logger.debug({ user })"),
        "ts-no-any" => ("function parse(data: any)", "function parse(data: unknown) { if (isUser(data)) … }"),
        "ts-no-ts-ignore" => ("// @ts-ignore\nbroken()", "// @ts-expect-error TICKET-123: lib types lag v3\nbroken()"),
        "no-nested-ternary" => ("a ? b : c ? d : e", "if (a) return b;\nreturn c ? d : e;"),
        "no-ts-non-null" => ("user!.name", "if (!user) return; user.name"),
        "no-as-any" => ("const u = data as any", "const u = data as User"),
        "no-throw-literal" => ("throw 'not found'", "throw new Error('not found')"),
        "no-with" => ("with (obj) { a + b }", "const { a, b } = obj; a + b"),
        "sec-no-hardcoded-secret" => ("const KEY = \"sk-live-4f9…\"", "const KEY = process.env.API_KEY   // + rotate the leaked one"),
        "no-eval" => ("eval(payload)", "JSON.parse(payload)   // or a lookup map of allowed ops"),
        "py-no-exec" => ("exec(user_input)", "json.loads(user_input)  # or a dispatch dict"),
        "no-new-function" => ("new Function('return ' + expr)", "const OPS = { sum, avg }; OPS[name](args)"),
        "no-inner-html-assign" => ("el.innerHTML = userHtml", "el.textContent = userText   // or DOMPurify.sanitize(userHtml)"),
        "no-document-write" => ("document.write('<p>hi</p>')", "container.append(Object.assign(document.createElement('p'), { textContent: 'hi' }))"),
        "no-insecure-http" => ("fetch('http://api.example.com')", "fetch('https://api.example.com')"),
        "react-no-href-javascript" => ("<a href=\"javascript:doIt()\">", "<a onClick={doIt} role=\"button\">"),
        "react-no-dangerously-set-html" => ("<div dangerouslySetInnerHTML={{ __html: raw }} />", "<div>{text}</div>   // or sanitize raw first"),
        "react-no-array-index-key" => ("items.map((it, i) => <Row key={i} />)", "items.map((it) => <Row key={it.id} />)"),
        "react-no-bind-in-render" => ("<Btn onClick={this.go.bind(this)} />", "const go = useCallback(…); <Btn onClick={go} />"),
        "no-only-test" => ("it.only('works', …)", "it('works', …)"),
        "no-skip-test" => ("it.skip('flaky', …)", "it('flaky', …)   // fixed — or deleted with a reason"),
        "no-todo-comments" => ("// TODO: handle errors", "(filed as CARD-123 — comment removed)"),
        "py-no-print" => ("print(order)", "logging.info(\"order %s\", order.id)"),
        "py-no-wildcard-import" => ("from utils import *", "from utils import parse, clean"),
        "py-no-bare-except" => ("except:", "except ValueError:"),
        "py-no-mutable-default" => ("def add(x, acc=[])", "def add(x, acc=None):\n    acc = acc if acc is not None else []"),
        "node-no-process-exit" => ("process.exit(1)", "throw new Error('boot failed')   // the entrypoint decides the exit"),
        "node-no-sync-fs" => ("fs.readFileSync(p)", "await fs.promises.readFile(p)"),
        "no-await-in-loop" => ("for (const it of items) { await save(it) }", "await Promise.all(items.map(save))"),
        "no-nested-loops" => ("for (const a of as) for (const b of bs) if (a.id === b.id) …", "const byId = new Map(bs.map(b => [b.id, b]));\nfor (const a of as) { const b = byId.get(a.id); … }"),
        "no-alloc-in-loop" => ("for (const it of items) { const opts = { deep: true }; walk(it, opts) }", "const opts = { deep: true };\nfor (const it of items) walk(it, opts)"),
        "no-concat-in-loop" => ("let s = \"\"; for (const r of rows) s += render(r)", "const parts = rows.map(render); const s = parts.join(\"\")"),
        "max-nesting-depth" => ("if (a) { if (b) { for (…) { if (c) { … } } } }", "if (!a || !b) return;\nfor (…) handleC(…)   // the deep branch, extracted"),
        "max-boolean-chain" => ("if (user && user.active && !user.banned && user.plan === \"pro\") { … }", "const isEligible = user && user.active && !user.banned && user.plan === \"pro\";\nif (isEligible) { … }"),
        "listener-needs-cleanup" => ("window.addEventListener('resize', onResize)", "window.addEventListener('resize', onResize)\n// teardown: window.removeEventListener('resize', onResize)"),
        "interval-needs-clear" => ("setInterval(poll, 1000)", "const id = setInterval(poll, 1000)\n// teardown: clearInterval(id)"),
        "subscription-needs-unsubscribe" => ("obs.subscribe(onNext)", "const sub = obs.subscribe(onNext)\n// teardown: sub.unsubscribe()"),
        "test-must-assert" => ("it('saves the user', () => { save(user) })", "it('saves the user', () => { expect(save(user)).toBe(true) })"),
        "no-conditional-assert" => ("if (user) expect(user.id).toBe(1)", "expect(user).toBeTruthy();\nexpect(user.id).toBe(1);"),
        "no-empty-test" => ("it('works', () => {})", "it('works', () => { expect(run()).toEqual(expected) })"),
        "no-random-in-tests" => ("const id = Math.random()", "const id = 42   // a fixed fixture — same run, every run"),
        "no-sleep-in-tests" => ("await new Promise(r => setTimeout(r, 500))", "await waitFor(() => expect(el).toBeVisible())"),
        "limit-snapshot-tests" => ("expect(tree).toMatchSnapshot()", "expect(tree.title).toBe('Home')   // the field that matters"),
        "prefer-composition" => ("class Report extends Formatter { … }", "class Report { constructor(private fmt: Formatter) {} }"),
        "discourage-singleton" => ("export const db = new DB()", "export const createDb = () => new DB()   // inject it"),
        "pattern-no-default-export" => ("export default parse", "export { parse }"),
        "pattern-long-parameter-list" => ("draw(x, y, w, h, color, mode)", "draw({ x, y, w, h, color, mode })"),
        _ => return Value::Null,
    };
    json!({ "bad": pair.0, "good": pair.1 })
}

// `${p == null ? '—' : `${Math.round(p * 100)}th pct`}` — pct_repo/pct_folder are REAL [0,1].
fn pct_txt(p: Option<&Value>) -> String {
    match nz(p) { None => "—".to_string(), Some(v) => format!("{}th pct", js_round(js_number(Some(v)) * 100.0) as i64) }
}
// `${x ?? '—'}` — nullish-coalesce then template-stringify.
fn dash_or(v: Option<&Value>) -> String { match nz(v) { Some(x) => tmpl(Some(x)), None => "—".to_string() } }

// the dossier fields every violation row gets: title/rule_md/category/fix/example, appended in
// this exact order onto the row's own selected columns (JS object spread + assignment).
fn with_dossier(v: &Value, law_by_key: &std::collections::HashMap<String, Value>) -> Value {
    let mut m = v.as_object().cloned().unwrap_or_default();
    let law_key = v.get("law_key").and_then(Value::as_str).unwrap_or("").to_string();
    let law_scope = v.get("law_scope").cloned().unwrap_or(Value::Null);
    let law = law_by_key.get(&law_key).cloned().unwrap_or_else(|| json!({ "key": law_key.clone(), "scope": law_scope }));
    let title = nz(law.get("title")).cloned().unwrap_or_else(|| json!(law_key.clone()));
    let rule_md = nz(law.get("rule_md")).cloned().unwrap_or_else(|| json!(""));
    let category = crate::constitution__helpers_categories::categoryOf(&law);
    let fix = crate::constitution__helpers_fix_hints::fixHintFor(&law, v);
    let example = fix_example_for(&law_key);
    m.insert("title".into(), title);
    m.insert("rule_md".into(), rule_md);
    m.insert("category".into(), category);
    m.insert("fix".into(), fix);
    m.insert("example".into(), example);
    Value::Object(m)
}

// JS `const cnt = (arr) => { const m = new Map(); for (const v of arr) m.set(v.law_key, (m.get(v.law_key)??0)+1); return m; }`
// — Map preserves first-seen key order, mirrored here as an order Vec + count HashMap.
fn cnt(arr: &[Value]) -> (Vec<String>, std::collections::HashMap<String, i64>) {
    let mut order: Vec<String> = Vec::new();
    let mut map: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for v in arr {
        let k = v.get("law_key").and_then(Value::as_str).unwrap_or("").to_string();
        if !map.contains_key(&k) { order.push(k.clone()); }
        *map.entry(k).or_insert(0) += 1;
    }
    (order, map)
}

pub fn constitutionFile(input: &Value, db: &Connection) -> Result<Value, String> {
    let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(db);
    let path = str_qq(input.get("path")).replace('\\', "/").trim().to_string();
    if path.is_empty() { return Err("constitution-file: path is required".to_string()); }

    let snap: Option<Value> = if nz(input.get("snapshot_id")).is_some() {
        let n = js_number(input.get("snapshot_id"));
        crate::db_json::query_rows(db, "SELECT id, created_at, engineering_version, constitution_hash FROM judgments.review_snapshots WHERE id=?", &[json!(n as i64)])?
            .as_array().and_then(|a| a.first()).cloned()
    } else {
        crate::db_json::query_rows(db, "SELECT id, created_at, engineering_version, constitution_hash FROM judgments.review_snapshots WHERE status='done' ORDER BY id DESC LIMIT 1", &[])?
            .as_array().and_then(|a| a.first()).cloned()
    };
    let snap = match snap {
        Some(s) => s,
        None => return Ok(json!({ "path": path, "found": false, "doc_md": "_No constitution snapshot yet — rebuild the codebase._" })),
    };
    let snap_id = snap.get("id").cloned().unwrap_or(Value::Null);

    let row: Option<Value> = crate::db_json::query_rows(
        db,
        "SELECT * FROM judgments.review_scores WHERE snapshot_id=? AND unit_kind='file' AND path=?",
        &[snap_id.clone(), json!(path.clone())],
    )?
    .as_array()
    .and_then(|a| a.first())
    .cloned();

    let row = match row {
        Some(r) => r,
        None => return folder_review(db, &path, &snap, &snap_id),
    };

    let axes = { let a = safe(row.get("axes_json")); if a.is_null() { json!({}) } else { a } };

    let law_rows: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT key, title, rule_md, detector, config_json, scope, applies_json FROM laws.review_laws",
        &[],
    )?
    .as_array()
    .cloned()
    .unwrap_or_default();
    let mut law_by_key: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    for l in &law_rows {
        law_by_key.insert(l.get("key").and_then(Value::as_str).unwrap_or("").to_string(), l.clone());
    }

    let viol_rows: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT law_key, law_scope, weight, severity, confidence, line, symbol, evidence FROM judgments.review_violations WHERE snapshot_id=? AND path=? ORDER BY weight DESC",
        &[snap_id.clone(), json!(path.clone())],
    )?
    .as_array()
    .cloned()
    .unwrap_or_default();
    let violations: Vec<Value> = viol_rows.iter().map(|v| with_dossier(v, &law_by_key)).collect();

    // PEERS — the same law broken in OTHER files of this snapshot.
    let mut law_keys: Vec<String> = Vec::new();
    for v in &violations {
        let k = v.get("law_key").and_then(Value::as_str).unwrap_or("").to_string();
        if !law_keys.contains(&k) { law_keys.push(k); }
    }
    let mut peers = Map::new();
    if !law_keys.is_empty() {
        let placeholders = law_keys.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT law_key, path AS p, COUNT(*) n FROM judgments.review_violations WHERE snapshot_id=? AND law_key IN ({placeholders}) AND path<>? GROUP BY law_key, path ORDER BY n DESC"
        );
        let mut params: Vec<Value> = vec![snap_id.clone()];
        for k in &law_keys { params.push(json!(k)); }
        params.push(json!(path.clone()));
        let peer_rows: Vec<Value> = crate::db_json::query_rows(db, &sql, &params)?.as_array().cloned().unwrap_or_default();
        for r in &peer_rows {
            let lk = r.get("law_key").and_then(Value::as_str).unwrap_or("").to_string();
            let entry = peers.entry(lk).or_insert_with(|| json!({ "files": 0, "top": [] }));
            if let Value::Object(eo) = entry {
                let files = eo.get("files").and_then(Value::as_i64).unwrap_or(0) + 1;
                eo.insert("files".into(), json!(files));
                let top_arr = eo.get_mut("top").and_then(Value::as_array_mut).unwrap();
                if top_arr.len() < 4 {
                    top_arr.push(json!({ "path": r.get("p").cloned().unwrap_or(Value::Null), "n": r.get("n").cloned().unwrap_or(Value::Null) }));
                }
            }
        }
    }

    // ANCHORS — importers/imports (atlas code graph) + bugfinder findings.
    let mut anchors_importers: Vec<Value> = Vec::new();
    let mut anchors_imports: Vec<Value> = Vec::new();
    if let Ok(files_v) = crate::db_json::query_rows(db, "SELECT id, path FROM code_nodes WHERE origin='atlas' AND type='file'", &[]) {
        let files_arr = files_v.as_array().cloned().unwrap_or_default();
        let mut id_of: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
        let mut path_of: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for f in &files_arr {
            let p = js_string(f.get("path").unwrap_or(&Value::Null)).replace('\\', "/");
            let id = f.get("id").cloned().unwrap_or(Value::Null);
            path_of.insert(js_string(&id), p.clone());
            id_of.insert(p, id);
        }
        if let Some(fid) = id_of.get(&path).cloned() {
            if let Ok(rows) = crate::db_json::query_rows(db, &format!("SELECT from_node FROM code_edges WHERE origin='atlas' AND {} AND to_node=?", crate::codebase_atlas_build::RUNTIME_IMPORTS), &[fid.clone()]) {
                for e in rows.as_array().cloned().unwrap_or_default() {
                    let from_id = js_string(e.get("from_node").unwrap_or(&Value::Null));
                    if let Some(p) = path_of.get(&from_id) {
                        if p != &path && anchors_importers.len() < 8 && !anchors_importers.iter().any(|v| v.as_str() == Some(p.as_str())) {
                            anchors_importers.push(json!(p));
                        }
                    }
                }
            }
            if let Ok(rows) = crate::db_json::query_rows(db, &format!("SELECT to_node FROM code_edges WHERE origin='atlas' AND {} AND from_node=?", crate::codebase_atlas_build::RUNTIME_IMPORTS), &[fid.clone()]) {
                for e in rows.as_array().cloned().unwrap_or_default() {
                    let to_id = js_string(e.get("to_node").unwrap_or(&Value::Null));
                    if let Some(p) = path_of.get(&to_id) {
                        if p != &path && anchors_imports.len() < 8 && !anchors_imports.iter().any(|v| v.as_str() == Some(p.as_str())) {
                            anchors_imports.push(json!(p));
                        }
                    }
                }
            }
        }
    }
    let anchors_findings: Vec<Value> = crate::db_json::query_rows(db, "SELECT * FROM judgments.bugfinder_findings WHERE dropped=0 AND file=? LIMIT 6", &[json!(path.clone())])
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .map(|r| {
            let line = nz(r.get("line")).cloned().unwrap_or(Value::Null);
            let detail = nz(r.get("detail")).or_else(|| nz(r.get("title"))).or_else(|| nz(r.get("message"))).or_else(|| nz(r.get("category")));
            let text = match detail { Some(v) => str_qq(Some(v)), None => "finding".to_string() };
            let sliced: String = text.chars().take(140).collect();
            json!({ "line": line, "text": sliced })
        })
        .collect();
    let anchors = json!({ "importers": anchors_importers, "imports": anchors_imports, "findings": anchors_findings });

    // HISTORY — per-snapshot health/score + per-axis score timeline, done snapshots, last 8.
    let hist_rows: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT s.id AS snapshot_id, s.created_at, sc.health, sc.score, sc.axes_json FROM judgments.review_scores sc JOIN judgments.review_snapshots s ON s.id = sc.snapshot_id WHERE sc.path=? AND sc.unit_kind='file' AND s.status='done' ORDER BY s.id ASC",
        &[json!(path.clone())],
    )?
    .as_array()
    .cloned()
    .unwrap_or_default();
    let start = hist_rows.len().saturating_sub(8);
    let history: Vec<Value> = hist_rows[start..]
        .iter()
        .map(|h| {
            let parsed = safe(h.get("axes_json"));
            let ax = if !truthy(Some(&parsed)) {
                Value::Null
            } else {
                let mut m = Map::new();
                if let Some(obj) = parsed.as_object() {
                    for (k, v) in obj {
                        if let Some(score) = v.get("score") {
                            if !score.is_null() { m.insert(k.clone(), score.clone()); }
                        }
                    }
                }
                Value::Object(m)
            };
            json!({
                "snapshot_id": h.get("snapshot_id").cloned().unwrap_or(Value::Null),
                "created_at": h.get("created_at").cloned().unwrap_or(Value::Null),
                "health": h.get("health").cloned().unwrap_or(Value::Null),
                "score": h.get("score").cloned().unwrap_or(Value::Null),
                "axes": ax,
            })
        })
        .collect();

    let signals = json!({
        "loc": row.get("loc").cloned().unwrap_or(Value::Null),
        "complexity": row.get("complexity").cloned().unwrap_or(Value::Null),
        "fan_in": row.get("fan_in").cloned().unwrap_or(Value::Null),
        "fan_out": row.get("fan_out").cloned().unwrap_or(Value::Null),
        "transitive": row.get("transitive").cloned().unwrap_or(Value::Null),
        "covered": row.get("covered").cloned().unwrap_or(Value::Null),
        "churn": row.get("churn").cloned().unwrap_or(Value::Null),
        "authors": row.get("authors").cloned().unwrap_or(Value::Null),
        "findings": row.get("findings").cloned().unwrap_or(Value::Null),
    });
    let neighborhood = json!({ "pct_repo": row.get("pct_repo").cloned().unwrap_or(Value::Null), "pct_folder": row.get("pct_folder").cloned().unwrap_or(Value::Null) });

    // THE RATCHET — new/fixed violations + health/score deltas vs the previous done snapshot.
    let prev_snap: Option<Value> = crate::db_json::query_rows(
        db,
        "SELECT s.id, s.created_at, s.engineering_version, s.constitution_hash FROM judgments.review_scores sc JOIN judgments.review_snapshots s ON s.id=sc.snapshot_id WHERE sc.path=? AND sc.unit_kind='file' AND s.status='done' AND s.id<? ORDER BY s.id DESC LIMIT 1",
        &[json!(path.clone()), snap_id.clone()],
    )?
    .as_array()
    .and_then(|a| a.first())
    .cloned();
    let regression: Value = match prev_snap {
        Some(prev) => {
            let prev_id = prev.get("id").cloned().unwrap_or(Value::Null);
            let prev_viol: Vec<Value> = crate::db_json::query_rows(db, "SELECT law_key FROM judgments.review_violations WHERE snapshot_id=? AND path=?", &[prev_id.clone(), json!(path.clone())])?
                .as_array()
                .cloned()
                .unwrap_or_default();
            let (cc_order, cc_map) = cnt(&violations);
            let (pc_order, pc_map) = cnt(&prev_viol);
            let mut added: Vec<Value> = Vec::new();
            for k in &cc_order {
                let n = *cc_map.get(k).unwrap_or(&0);
                let p = *pc_map.get(k).unwrap_or(&0);
                if n > p {
                    let title = law_by_key.get(k).and_then(|l| nz(l.get("title"))).cloned().unwrap_or_else(|| json!(k.clone()));
                    let lines: Vec<Value> = violations
                        .iter()
                        .filter(|v| v.get("law_key").and_then(Value::as_str) == Some(k.as_str()) && v.get("line").map(|l| !l.is_null()).unwrap_or(false))
                        .map(|v| v.get("line").cloned().unwrap_or(Value::Null))
                        .take(4)
                        .collect();
                    added.push(json!({ "law_key": k, "title": title, "count": n - p, "lines": lines }));
                }
            }
            let mut gone: Vec<Value> = Vec::new();
            for k in &pc_order {
                let n = *pc_map.get(k).unwrap_or(&0);
                let c = *cc_map.get(k).unwrap_or(&0);
                if n > c {
                    let title = law_by_key.get(k).and_then(|l| nz(l.get("title"))).cloned().unwrap_or_else(|| json!(k.clone()));
                    gone.push(json!({ "law_key": k, "title": title, "count": n - c }));
                }
            }
            let prev_row: Option<Value> = crate::db_json::query_rows(
                db,
                "SELECT health, score FROM judgments.review_scores WHERE snapshot_id=? AND unit_kind='file' AND path=?",
                &[prev_id.clone(), json!(path.clone())],
            )?
            .as_array()
            .and_then(|a| a.first())
            .cloned();
            let row_health = nz(row.get("health")).cloned();
            let prev_health = prev_row.as_ref().and_then(|p| nz(p.get("health")).cloned());
            let health_delta = match (&row_health, &prev_health) {
                (Some(rh), Some(ph)) => json!(js_number(Some(rh)) as i64 - js_number(Some(ph)) as i64),
                _ => Value::Null,
            };
            let row_score = nz(row.get("score")).cloned();
            let prev_score = prev_row.as_ref().and_then(|p| nz(p.get("score")).cloned());
            let score_delta = match (&row_score, &prev_score) {
                (Some(rs), Some(ps)) => json!(js_number(Some(rs)) as i64 - js_number(Some(ps)) as i64),
                _ => Value::Null,
            };
            // comparable only when the same laws AND the same instrument judged both snapshots —
            // otherwise the delta is the ruler moving, and it is never called a regression
            let prev_version = prev.get("engineering_version").cloned().unwrap_or(Value::Null);
            let prev_hash = prev.get("constitution_hash").cloned().unwrap_or(Value::Null);
            let comparable = prev_version == snap.get("engineering_version").cloned().unwrap_or(Value::Null) && prev_hash == snap.get("constitution_hash").cloned().unwrap_or(Value::Null);
            let regressed = comparable && (!added.is_empty()
                || match (&row_health, &prev_health) {
                    (Some(rh), Some(ph)) => js_number(Some(rh)) < js_number(Some(ph)),
                    _ => false,
                });
            json!({
                "prev_snapshot_id": prev_id,
                "prev_created_at": prev.get("created_at").cloned().unwrap_or(Value::Null),
                "new": added.into_iter().take(12).collect::<Vec<_>>(),
                "fixed": gone.into_iter().take(12).collect::<Vec<_>>(),
                "health_delta": health_delta,
                "score_delta": score_delta,
                "regressed": regressed,
                "comparable": comparable,
                "prev_engineering_version": prev_version,
                "laws_changed": prev_hash != snap.get("constitution_hash").cloned().unwrap_or(Value::Null),
            })
        }
        None => Value::Null,
    };

    // per-file COMPLIANCE decomposed by CONCERN.
    let mut cat_order: Vec<String> = Vec::new();
    let mut cat_map: std::collections::HashMap<String, (i64, i64)> = std::collections::HashMap::new();
    for v in &violations {
        let cat = v.get("category").and_then(Value::as_str).unwrap_or("").to_string();
        if !cat_map.contains_key(&cat) { cat_order.push(cat.clone()); }
        let weight = nz(v.get("weight")).map(|w| js_number(Some(w))).unwrap_or(1.0);
        let confidence = nz(v.get("confidence")).map(|c| js_number(Some(c))).unwrap_or(1.0);
        let penalty = js_round(weight * confidence) as i64;
        let e = cat_map.entry(cat).or_insert((0, 0));
        e.0 += 1;
        e.1 += penalty;
    }
    let mut categories: Vec<Value> = cat_order
        .iter()
        .map(|c| {
            let (violations_n, penalty) = cat_map[c];
            let score = std::cmp::max(10, 100 - std::cmp::min(90, penalty));
            json!({ "category": c, "violations": violations_n, "score": score })
        })
        .collect();
    categories.sort_by(|x, y| x["score"].as_i64().unwrap_or(0).cmp(&y["score"].as_i64().unwrap_or(0)));

    // the rendered doc block.
    let axis_line: String = axes
        .as_object()
        .map(|obj| {
            obj.iter()
                .filter(|(_, a)| truthy(Some(a)))
                .map(|(k, a)| format!("{} {}", k, tmpl(a.get("score"))))
                .collect::<Vec<_>>()
                .join(" · ")
        })
        .unwrap_or_default();
    let last_segment = path.rsplit('/').next().unwrap_or(&path).to_string();
    let glue_note = if row.get("archetype").and_then(Value::as_str) == Some("glue") {
        " _(glue — a re-export/constants/types file; scored only on laws + defects)_"
    } else {
        ""
    };
    let covered_txt = match nz(row.get("covered")) { None => "—".to_string(), Some(v) => if truthy(Some(v)) { "yes".to_string() } else { "no".to_string() } };
    let viol_summary = if !violations.is_empty() {
        let items: Vec<String> = violations
            .iter()
            .take(8)
            .map(|v| {
                let lk = tmpl(v.get("law_key"));
                let line_part = match nz(v.get("line")) { Some(l) => format!(":{}", tmpl(Some(l))), None => String::new() };
                format!("`{lk}`{line_part}")
            })
            .collect();
        format!("**Violations ({}):** {}", violations.len(), items.join(", "))
    } else {
        "**Violations:** none — obeys the constitution.".to_string()
    };
    let created_at_slice: String = js_string(&snap.get("created_at").cloned().unwrap_or(Value::Null)).chars().take(16).collect();

    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("### \u{1F4D0} {last_segment} — engineering snapshot{glue_note}"));
    lines.push(format!(
        "**Health {}** · law-compliance {} · repo {} · folder {}",
        dash_or(row.get("health")),
        dash_or(row.get("score")),
        pct_txt(row.get("pct_repo")),
        pct_txt(row.get("pct_folder")),
    ));
    if !axis_line.is_empty() { lines.push(format!("**Axes:** {axis_line}")); }
    lines.push(format!(
        "**Signals:** loc {} · complexity {} · fan-in {} · fan-out {} · transitive {} · covered {} · churn {} · authors {}",
        dash_or(signals.get("loc")),
        dash_or(signals.get("complexity")),
        dash_or(signals.get("fan_in")),
        dash_or(signals.get("fan_out")),
        dash_or(signals.get("transitive")),
        covered_txt,
        dash_or(signals.get("churn")),
        dash_or(signals.get("authors")),
    ));
    lines.push(viol_summary);
    lines.push(format!("_deterministic · snapshot #{} · {}_", tmpl(snap.get("id")), created_at_slice));
    let doc_md = lines.join("\n\n");

    let blocks = { let b = safe(row.get("blocks_json")); if b.is_null() { json!([]) } else { b } };

    let mut out = Map::new();
    out.insert("path".into(), json!(path));
    out.insert("found".into(), json!(true));
    out.insert("snapshot_id".into(), snap_id);
    out.insert("score".into(), row.get("score").cloned().unwrap_or(Value::Null));
    out.insert("health".into(), row.get("health").cloned().unwrap_or(Value::Null));
    out.insert("ai_readability".into(), row.get("ai_readability").cloned().unwrap_or(Value::Null));
    out.insert("archetype".into(), row.get("archetype").cloned().unwrap_or(Value::Null));
    out.insert("axes".into(), axes);
    out.insert("signals".into(), signals);
    out.insert("violations".into(), json!(violations));
    out.insert("neighborhood".into(), neighborhood);
    out.insert("categories".into(), json!(categories));
    out.insert("peers".into(), Value::Object(peers));
    out.insert("history".into(), json!(history));
    out.insert("anchors".into(), anchors);
    out.insert("regression".into(), regression);
    out.insert("engineering_version".into(), snap.get("engineering_version").cloned().unwrap_or(Value::Null));
    out.insert("created_at".into(), snap.get("created_at").cloned().unwrap_or(Value::Null));
    out.insert("blocks".into(), blocks);
    out.insert("doc_md".into(), json!(doc_md));
    Ok(Value::Object(out))
}

// FOLDER review — no file row for `path`: aggregate every member under the `path/` prefix.
fn folder_review(db: &Connection, path: &str, snap: &Value, snap_id: &Value) -> Result<Value, String> {
    let prefix = path.trim_end_matches('/').to_string();
    let like = format!("{prefix}/%");
    let members: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT * FROM judgments.review_scores WHERE snapshot_id=? AND unit_kind='file' AND path LIKE ?",
        &[snap_id.clone(), json!(like.clone())],
    )?
    .as_array()
    .cloned()
    .unwrap_or_default();
    if members.is_empty() {
        return Ok(json!({ "path": path, "found": false, "doc_md": format!("_{path} is outside the last snapshot's scope._") }));
    }

    let health_vals: Vec<f64> = members.iter().filter_map(|m| nz(m.get("health")).map(|v| js_number(Some(v)))).collect();
    let score_vals: Vec<f64> = members.iter().filter_map(|m| nz(m.get("score")).map(|v| js_number(Some(v)))).collect();
    let health = mean(&health_vals);
    let score = mean(&score_vals);

    const AXIS_KEYS: [&str; 8] = ["compliance", "defects", "tests", "structure", "coupling", "cohesion", "change", "knowledge"];
    let mut axes = Map::new();
    for ax in AXIS_KEYS {
        let mut vals: Vec<f64> = Vec::new();
        for m in &members {
            let parsed = safe(m.get("axes_json"));
            if let Some(s) = parsed.get(ax).and_then(|a| a.get("score")) {
                if !s.is_null() { vals.push(js_number(Some(s))); }
            }
        }
        if vals.is_empty() {
            axes.insert(ax.to_string(), Value::Null);
        } else {
            axes.insert(ax.to_string(), json!({ "score": mean(&vals).unwrap(), "parts": { "files": vals.len() as i64 } }));
        }
    }
    let axes = Value::Object(axes);

    let law_rows: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT key, title, rule_md, detector, config_json, scope, applies_json FROM laws.review_laws",
        &[],
    )?
    .as_array()
    .cloned()
    .unwrap_or_default();
    let mut law_by_key: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    for l in &law_rows {
        law_by_key.insert(l.get("key").and_then(Value::as_str).unwrap_or("").to_string(), l.clone());
    }

    let viol_rows: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT path, law_key, law_scope, weight, severity, confidence, line, symbol, evidence FROM judgments.review_violations WHERE snapshot_id=? AND path LIKE ? ORDER BY weight DESC LIMIT 200",
        &[snap_id.clone(), json!(like.clone())],
    )?
    .as_array()
    .cloned()
    .unwrap_or_default();
    let violations: Vec<Value> = viol_rows.iter().map(|v| with_dossier(v, &law_by_key)).collect();

    let mut cat_order: Vec<String> = Vec::new();
    let mut cat_map: std::collections::HashMap<String, (i64, i64)> = std::collections::HashMap::new();
    for v in &violations {
        let cat = v.get("category").and_then(Value::as_str).unwrap_or("").to_string();
        if !cat_map.contains_key(&cat) { cat_order.push(cat.clone()); }
        let weight = nz(v.get("weight")).map(|w| js_number(Some(w))).unwrap_or(1.0);
        let confidence = nz(v.get("confidence")).map(|c| js_number(Some(c))).unwrap_or(1.0);
        let penalty = js_round(weight * confidence) as i64;
        let e = cat_map.entry(cat).or_insert((0, 0));
        e.0 += 1;
        e.1 += penalty;
    }
    let mut categories: Vec<Value> = cat_order
        .iter()
        .map(|c| {
            let (violations_n, penalty) = cat_map[c];
            let score = std::cmp::max(10, 100 - std::cmp::min(90, js_round(penalty as f64 / members.len().max(1) as f64) as i64));
            json!({ "category": c, "violations": violations_n, "score": score })
        })
        .collect();
    categories.sort_by(|x, y| x["score"].as_i64().unwrap_or(0).cmp(&y["score"].as_i64().unwrap_or(0)));

    let sum_field = |field: &str| -> i64 {
        members
            .iter()
            .map(|m| {
                let v = nz(m.get(field)).cloned().unwrap_or(json!(0));
                js_number(Some(&v)) as i64
            })
            .sum()
    };
    let complexity_vals: Vec<f64> = members.iter().filter_map(|m| nz(m.get("complexity")).map(|v| js_number(Some(v)))).collect();
    let signals = json!({
        "loc": sum_field("loc"),
        "complexity": mean(&complexity_vals).map(|v| json!(v)).unwrap_or(Value::Null),
        "fan_in": sum_field("fan_in"),
        "fan_out": sum_field("fan_out"),
        "transitive": Value::Null,
        "covered": Value::Null,
        "churn": sum_field("churn"),
        "authors": Value::Null,
        "findings": sum_field("findings"),
    });

    let hist_rows: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT s.id AS snapshot_id, s.created_at, s.engineering_version, AVG(sc.health) AS health, AVG(sc.score) AS score, COUNT(*) AS files FROM judgments.review_scores sc JOIN judgments.review_snapshots s ON s.id = sc.snapshot_id WHERE sc.unit_kind='file' AND sc.path LIKE ? AND s.status='done' GROUP BY s.id ORDER BY s.id ASC",
        &[json!(like.clone())],
    )?
    .as_array()
    .cloned()
    .unwrap_or_default();
    let start = hist_rows.len().saturating_sub(8);
    // NOTE: includes the current snapshot — the FE counts `history.length`, never +1.
    let history: Vec<Value> = hist_rows[start..]
        .iter()
        .map(|h| {
            let health_v = nz(h.get("health")).map(|v| json!(js_round(js_number(Some(v))) as i64)).unwrap_or(Value::Null);
            let score_v = nz(h.get("score")).map(|v| json!(js_round(js_number(Some(v))) as i64)).unwrap_or(Value::Null);
            json!({
                "snapshot_id": h.get("snapshot_id").cloned().unwrap_or(Value::Null),
                "created_at": h.get("created_at").cloned().unwrap_or(Value::Null),
                "health": health_v,
                "score": score_v,
                "files": h.get("files").cloned().unwrap_or(Value::Null),
                "engineering_version": h.get("engineering_version").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();

    // THE ROSTER = THE HEADLINE'S POPULATION. Every member, with what the judge knew about it —
    // the paper used to show the 10 riskiest and average all of them, so the number on the cover
    // and the files under it were different sets. `archetype` says how each was scored: `glue`
    // (no logic definitions) is scored on laws + defects only and lands at 100 when clean, which
    // is what pulled the mean up; `health_logic` is the mean without them.
    let mut sorted_members = members.clone();
    sorted_members.sort_by(|x, y| {
        let xr = nz(x.get("risk")).map(|v| js_number(Some(v))).unwrap_or(0.0);
        let yr = nz(y.get("risk")).map(|v| js_number(Some(v))).unwrap_or(0.0);
        yr.partial_cmp(&xr).unwrap_or(std::cmp::Ordering::Equal)
    });
    const MEMBER_CAP: usize = 400;
    let members_truncated = sorted_members.len() > MEMBER_CAP;
    let member_rows: Vec<Value> = sorted_members
        .iter()
        .take(MEMBER_CAP)
        .map(|m| {
            let get = |k: &str| m.get(k).cloned().unwrap_or(Value::Null);
            json!({
                "path": get("path"), "health": get("health"), "score": get("score"), "risk": get("risk"),
                "archetype": get("archetype"), "cached": get("cached"),
                "loc": get("loc"), "complexity": get("complexity"), "fan_in": get("fan_in"), "fan_out": get("fan_out"),
                "covered": get("covered"), "churn": get("churn"),
                "axes": safe(m.get("axes_json")),
            })
        })
        .collect();
    let count_arch = |a: &str| members.iter().filter(|m| m.get("archetype").and_then(Value::as_str) == Some(a)).count() as i64;
    let roster = json!({
        "total": members.len() as i64,
        "logic": count_arch("logic"), "glue": count_arch("glue"), "test": count_arch("test"), "unknown": count_arch("unknown"),
        "carried": members.iter().filter(|m| nz(m.get("cached")).map(|v| js_number(Some(v)) == 1.0).unwrap_or(false)).count() as i64,
    });
    let logic_health: Vec<f64> = members.iter()
        .filter(|m| m.get("archetype").and_then(Value::as_str) == Some("logic"))
        .filter_map(|m| nz(m.get("health")).map(|v| js_number(Some(v))))
        .collect();
    let health_logic = mean(&logic_health);
    // what measured the tests axis, across the members: one basis, or `mixed`, or `unmeasured`
    let bases: std::collections::BTreeSet<String> = members.iter()
        .filter_map(|m| safe(m.get("axes_json")).get("tests").and_then(|t| t.get("parts")).and_then(|p| p.get("basis")).and_then(Value::as_str).map(String::from))
        .collect();
    let tests_basis = match bases.len() { 0 => "unmeasured".to_string(), 1 => bases.iter().next().unwrap().clone(), _ => "mixed".to_string() };

    // THE RATCHET, for a folder: the previous done snapshot that judged files under this prefix.
    // Comparable only when the laws AND the instrument match — otherwise the delta is the
    // instrument moving, and it is reported as such, never as a regression.
    let cur_version = snap.get("engineering_version").cloned().unwrap_or(Value::Null);
    let cur_hash = snap.get("constitution_hash").cloned().unwrap_or(Value::Null);
    let prev_snap: Option<Value> = crate::db_json::query_rows(
        db,
        "SELECT s.id, s.created_at, s.engineering_version, s.constitution_hash, AVG(sc.health) AS health, AVG(sc.score) AS score FROM judgments.review_scores sc JOIN judgments.review_snapshots s ON s.id=sc.snapshot_id WHERE sc.path LIKE ? AND sc.unit_kind='file' AND s.status='done' AND s.id<? GROUP BY s.id ORDER BY s.id DESC LIMIT 1",
        &[json!(like.clone()), snap_id.clone()],
    )?
    .as_array()
    .and_then(|a| a.first())
    .cloned();
    let regression: Value = match prev_snap {
        Some(prev) => {
            let prev_id = prev.get("id").cloned().unwrap_or(Value::Null);
            let prev_viol: Vec<Value> = crate::db_json::query_rows(db, "SELECT law_key FROM judgments.review_violations WHERE snapshot_id=? AND path LIKE ?", &[prev_id.clone(), json!(like.clone())])?
                .as_array().cloned().unwrap_or_default();
            let (cc_order, cc_map) = cnt(&violations);
            let (pc_order, pc_map) = cnt(&prev_viol);
            let title_of = |k: &String| law_by_key.get(k).and_then(|l| nz(l.get("title"))).cloned().unwrap_or_else(|| json!(k.clone()));
            let added: Vec<Value> = cc_order.iter().filter_map(|k| {
                let (n, p) = (*cc_map.get(k).unwrap_or(&0), *pc_map.get(k).unwrap_or(&0));
                (n > p).then(|| json!({ "law_key": k, "title": title_of(k), "count": n - p }))
            }).take(12).collect();
            let gone: Vec<Value> = pc_order.iter().filter_map(|k| {
                let (n, c) = (*pc_map.get(k).unwrap_or(&0), *cc_map.get(k).unwrap_or(&0));
                (n > c).then(|| json!({ "law_key": k, "title": title_of(k), "count": n - c }))
            }).take(12).collect();
            let prev_health: Option<i64> = nz(prev.get("health")).map(|v| js_round(js_number(Some(v))) as i64);
            let prev_score: Option<i64> = nz(prev.get("score")).map(|v| js_round(js_number(Some(v))) as i64);
            let cur_health: Option<i64> = health.map(|h| h as i64);
            let health_delta = match (cur_health, prev_health) { (Some(h), Some(p)) => json!(h - p), _ => Value::Null };
            let score_delta = match (score.map(|v| v as i64), prev_score) { (Some(h), Some(p)) => json!(h - p), _ => Value::Null };
            let prev_version = prev.get("engineering_version").cloned().unwrap_or(Value::Null);
            let prev_hash = prev.get("constitution_hash").cloned().unwrap_or(Value::Null);
            let comparable = prev_version == cur_version && prev_hash == cur_hash;
            let regressed = comparable && (!added.is_empty() || matches!((cur_health, prev_health), (Some(h), Some(p)) if h < p));
            json!({
                "prev_snapshot_id": prev_id,
                "prev_created_at": prev.get("created_at").cloned().unwrap_or(Value::Null),
                "new": added, "fixed": gone,
                "health_delta": health_delta, "score_delta": score_delta,
                "regressed": regressed,
                "comparable": comparable,
                "prev_engineering_version": prev_version,
                "laws_changed": prev_hash != cur_hash,
            })
        }
        None => Value::Null,
    };

    let last_segment = prefix.rsplit('/').next().unwrap_or(&prefix).to_string();
    let health_txt = health.map(|h| h.to_string()).unwrap_or_else(|| "—".to_string());
    let score_txt = score.map(|s| s.to_string()).unwrap_or_else(|| "—".to_string());
    let doc_md = format!(
        "### \u{1F4C1} {last_segment} — folder review\n\n**Health {health_txt}** · compliance {score_txt} · {} files ({} logic · {} glue) · {} violation(s)",
        members.len(),
        roster["logic"], roster["glue"],
        violations.len(),
    );

    let mut out = Map::new();
    out.insert("path".into(), json!(path));
    out.insert("found".into(), json!(true));
    out.insert("is_folder".into(), json!(true));
    out.insert("snapshot_id".into(), snap.get("id").cloned().unwrap_or(Value::Null));
    out.insert("files".into(), json!(members.len() as i64));
    out.insert("health".into(), health.map(|v| json!(v)).unwrap_or(Value::Null));
    out.insert("health_logic".into(), health_logic.map(|v| json!(v)).unwrap_or(Value::Null));
    out.insert("roster".into(), roster);
    out.insert("members_truncated".into(), json!(members_truncated));
    out.insert("tests_basis".into(), json!(tests_basis));
    out.insert("engineering_version".into(), cur_version);
    out.insert("created_at".into(), snap.get("created_at").cloned().unwrap_or(Value::Null));
    out.insert("regression".into(), regression);
    out.insert("score".into(), score.map(|v| json!(v)).unwrap_or(Value::Null));
    out.insert("axes".into(), axes);
    out.insert("signals".into(), signals);
    out.insert("violations".into(), json!(violations));
    out.insert("categories".into(), json!(categories));
    out.insert("peers".into(), json!({}));
    out.insert("history".into(), json!(history));
    out.insert("anchors".into(), json!({ "importers": [], "imports": [], "findings": [] }));
    out.insert("members".into(), json!(member_rows));
    out.insert("neighborhood".into(), json!({}));
    out.insert("doc_md".into(), json!(doc_md));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments").unwrap();
        c
    }

    fn seed_snapshot(c: &Connection, id: i64, created_at: &str) {
        c.execute(
            &format!("INSERT INTO judgments.review_snapshots (id, created_at, scope_json, manifest_json, constitution_hash, status) VALUES ({id}, '{created_at}', '{{}}', '[]', 'h{id}', 'done')"),
            [],
        )
        .unwrap();
    }

    #[test]
    fn no_snapshot_yet() {
        let c = conn();
        let r = constitutionFile(&json!({ "path": "src/a.js" }), &c).unwrap();
        assert_eq!(r, json!({ "path": "src/a.js", "found": false, "doc_md": "_No constitution snapshot yet — rebuild the codebase._" }));
    }

    #[test]
    fn file_row_found_builds_full_dossier() {
        let c = conn();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        seed_snapshot(&c, 1, "2025-12-20 00:00:00");
        seed_snapshot(&c, 2, "2026-01-01 00:00:00");
        // both judged by the same laws and instrument — the −20 below is a REAL regression
        c.execute("UPDATE judgments.review_snapshots SET constitution_hash='h', engineering_version='v3'", []).unwrap();
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, status, enforcement, weight) VALUES ('project','no-var','No var','r','ratified','score',5)", []).unwrap();
        c.execute(
            "INSERT INTO judgments.review_scores (snapshot_id, unit_kind, path, score, health, archetype, loc, complexity, fan_in, fan_out, covered, churn, authors, findings, pct_repo, pct_folder, axes_json) VALUES \
             (1,'file','src/a.js',80,80,'logic',10,2,1,1,1,0,1,0,0.5,0.5,'{\"compliance\":{\"score\":80}}')",
            [],
        ).unwrap();
        c.execute(
            "INSERT INTO judgments.review_scores (snapshot_id, unit_kind, path, score, health, archetype, loc, complexity, fan_in, fan_out, covered, churn, authors, findings, pct_repo, pct_folder, axes_json) VALUES \
             (2,'file','src/a.js',60,60,'logic',10,2,1,1,1,0,1,0,0.5,0.5,'{\"compliance\":{\"score\":60}}')",
            [],
        ).unwrap();
        c.execute("INSERT INTO judgments.review_violations (snapshot_id, path, law_key, law_scope, weight) VALUES (2,'src/a.js','no-var','project',5)", []).unwrap();

        let r = constitutionFile(&json!({ "path": "src/a.js" }), &c).unwrap();
        assert_eq!(r["found"], json!(true));
        assert_eq!(r["snapshot_id"], json!(2));
        assert_eq!(r["health"], json!(60));
        assert_eq!(r["axes"]["compliance"]["score"], json!(60));
        assert_eq!(r["violations"][0]["law_key"], json!("no-var"));
        assert_eq!(r["violations"][0]["title"], json!("No var"));
        assert_eq!(r["violations"][0]["category"], json!("project"));
        assert_eq!(r["categories"][0]["category"], json!("project"));
        assert_eq!(r["regression"]["prev_snapshot_id"], json!(1));
        assert_eq!(r["regression"]["health_delta"], json!(-20));
        assert_eq!(r["regression"]["regressed"], json!(true));
        assert_eq!(r["regression"]["comparable"], json!(true));
        assert_eq!(r["regression"]["new"][0]["law_key"], json!("no-var"));
        // the same delta across an instrument change is a reset, not a regression
        c.execute("UPDATE judgments.review_snapshots SET engineering_version='v2' WHERE id=1", []).unwrap();
        let r2 = constitutionFile(&json!({ "path": "src/a.js" }), &c).unwrap();
        assert_eq!(r2["regression"]["comparable"], json!(false));
        assert_eq!(r2["regression"]["regressed"], json!(false));
        assert_eq!(r2["regression"]["prev_engineering_version"], json!("v2"));
        assert!(r["doc_md"].as_str().unwrap().contains("a.js — engineering snapshot"));
        assert!(r["doc_md"].as_str().unwrap().contains("Violations (1)"));
    }

    #[test]
    fn folder_review_when_no_file_row() {
        let c = conn();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        seed_snapshot(&c, 1, "2026-01-01 00:00:00");
        c.execute(
            "INSERT INTO judgments.review_scores (snapshot_id, unit_kind, path, score, health, loc, risk) VALUES (1,'file','src/utils/a.js',80,80,10,0.5)",
            [],
        ).unwrap();
        c.execute(
            "INSERT INTO judgments.review_scores (snapshot_id, unit_kind, path, score, health, loc, risk) VALUES (1,'file','src/utils/b.js',60,60,20,1.5)",
            [],
        ).unwrap();

        let r = constitutionFile(&json!({ "path": "src/utils" }), &c).unwrap();
        assert_eq!(r["found"], json!(true));
        assert_eq!(r["is_folder"], json!(true));
        assert_eq!(r["files"], json!(2));
        assert_eq!(r["health"], json!(70));
        assert_eq!(r["score"], json!(70));
        assert_eq!(r["signals"]["loc"], json!(30));
        assert_eq!(r["members"][0]["path"], json!("src/utils/b.js")); // higher risk first
        assert!(r["doc_md"].as_str().unwrap().contains("utils — folder review"));
    }

    #[test]
    fn folder_review_roster_is_the_headlines_population_and_glue_is_disclosed() {
        // The audit case in miniature: two logic files and one clean glue barrel. The cover
        // number averages all three (80); the paper must also say the logic-only mean (70), how
        // many were glue, and list every member — not the 10 riskiest.
        let c = conn();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        seed_snapshot(&c, 1, "2026-01-01 00:00:00");
        c.execute("UPDATE judgments.review_snapshots SET engineering_version='v3' WHERE id=1", []).unwrap();
        for (path, health, arche, axes) in [
            ("src/ui/A.tsx", 80, "logic", r#"{"tests":{"score":40,"parts":{"covered":0,"basis":"static-import"}},"structure":{"score":90,"parts":{}}}"#),
            ("src/ui/B.tsx", 60, "logic", r#"{"tests":{"score":20,"parts":{"covered":0,"basis":"static-import"}},"structure":{"score":30,"parts":{"worst_block":"big"}}}"#),
            ("src/ui/index.ts", 100, "glue", r#"{"compliance":{"score":100,"parts":{}}}"#),
        ] {
            c.execute("INSERT INTO judgments.review_scores (snapshot_id, unit_kind, path, score, health, loc, risk, archetype, cached, axes_json) VALUES (1,'file',?1,100,?2,10,?3,?4,1,?5)",
                rusqlite::params![path, health, (100 - health) as f64 / 100.0, arche, axes]).unwrap();
        }
        let r = constitutionFile(&json!({ "path": "src/ui" }), &c).unwrap();
        assert_eq!(r["files"], json!(3));
        assert_eq!(r["health"], json!(80));
        assert_eq!(r["health_logic"], json!(70));
        assert_eq!(r["roster"], json!({ "total": 3, "logic": 2, "glue": 1, "test": 0, "unknown": 0, "carried": 3 }));
        assert_eq!(r["members"].as_array().unwrap().len(), 3, "every member, not a top-10");
        assert_eq!(r["members"][0]["path"], json!("src/ui/B.tsx"), "risk-desc order kept");
        assert_eq!(r["members"][0]["archetype"], json!("logic"));
        assert_eq!(r["members"][0]["axes"]["structure"]["parts"]["worst_block"], json!("big"), "axes come with parts — the exhibit reads them");
        assert_eq!(r["members_truncated"], json!(false));
        assert_eq!(r["tests_basis"], json!("static-import"));
        assert_eq!(r["engineering_version"], json!("v3"));
        assert_eq!(r["regression"], Value::Null, "one snapshot — nothing to compare to");
        assert_eq!(r["history"].as_array().unwrap().len(), 1, "history INCLUDES the current snapshot");
        assert_eq!(r["history"][0]["files"], json!(3));
        assert!(r["doc_md"].as_str().unwrap().contains("3 files (2 logic · 1 glue)"));
    }

    #[test]
    fn folder_review_regression_is_only_a_regression_when_snapshots_are_comparable() {
        let c = conn();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        seed_snapshot(&c, 1, "2026-01-01 00:00:00");
        seed_snapshot(&c, 2, "2026-01-02 00:00:00");
        c.execute("UPDATE judgments.review_snapshots SET engineering_version='v2', constitution_hash='h' WHERE id=1", []).unwrap();
        c.execute("UPDATE judgments.review_snapshots SET engineering_version='v3', constitution_hash='h' WHERE id=2", []).unwrap();
        c.execute("INSERT INTO judgments.review_scores (snapshot_id, unit_kind, path, score, health, risk, archetype) VALUES (1,'file','src/x/a.ts',100,82,0.1,'logic')", []).unwrap();
        c.execute("INSERT INTO judgments.review_scores (snapshot_id, unit_kind, path, score, health, risk, archetype) VALUES (2,'file','src/x/a.ts',100,80,0.2,'logic')", []).unwrap();
        let r = constitutionFile(&json!({ "path": "src/x" }), &c).unwrap();
        let reg = &r["regression"];
        assert_eq!(reg["prev_snapshot_id"], json!(1));
        assert_eq!(reg["health_delta"], json!(-2));
        assert_eq!(reg["comparable"], json!(false), "the instrument changed between the two");
        assert_eq!(reg["regressed"], json!(false), "a -2 across an instrument change is not a regression");
        assert_eq!(reg["prev_engineering_version"], json!("v2"));
        assert_eq!(reg["laws_changed"], json!(false));

        // same instrument, same laws, health fell → that IS a regression
        c.execute("UPDATE judgments.review_snapshots SET engineering_version='v3' WHERE id=1", []).unwrap();
        let r = constitutionFile(&json!({ "path": "src/x" }), &c).unwrap();
        assert_eq!(r["regression"]["comparable"], json!(true));
        assert_eq!(r["regression"]["regressed"], json!(true));

        // laws changed → not comparable even on the same instrument
        c.execute("UPDATE judgments.review_snapshots SET constitution_hash='h2' WHERE id=1", []).unwrap();
        let r = constitutionFile(&json!({ "path": "src/x" }), &c).unwrap();
        assert_eq!(r["regression"]["comparable"], json!(false));
        assert_eq!(r["regression"]["laws_changed"], json!(true));
    }

    #[test]
    fn folder_review_out_of_scope() {
        let c = conn();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        seed_snapshot(&c, 1, "2026-01-01 00:00:00");
        let r = constitutionFile(&json!({ "path": "src/nowhere" }), &c).unwrap();
        assert_eq!(r["found"], json!(false));
        assert_eq!(r["doc_md"], json!("_src/nowhere is outside the last snapshot's scope._"));
    }
}
