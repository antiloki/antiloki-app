//! TEAM MEMORY (RFC 0011 §8): examples (golden, regression, trap), notes, and an FTS5 index over both, in the team's own `bench.sqlite`.
//! Retrieval is FTS5 first (free, deterministic, explainable). Consolidation is a deterministic job the engine runs after a run: dedupe by hash,
//! promote a failure a guard caught to the regression set and a verified success to the golden set, assign a split once and never move it.
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Where the question ends and the answer instructions begin in a stored prompt: an example's input is only the question.
pub const ANSWER_MARK: &str = "\nAnswer ONLY JSON, with exactly these fields";
/// One example on the page of a prompt is cut to this many characters (a memory must not eat the context).
const SHOW_MAX: usize = 1200;

#[derive(Clone, Debug, PartialEq)]
pub struct Example {
    pub id: i64,
    pub kind: String,
    pub task_class: String,
    pub input: String,
    pub output: String,
    pub label: String,
    pub source_call: Option<i64>,
    pub verified_by: String,
    pub split: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report { pub golden: usize, pub regression: usize, pub split_assigned: usize }

fn hash(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts { h.update(p.as_bytes()); h.update([0u8]); }
    hex::encode(h.finalize())
}

/// The question alone: the stored prompt up to where the answer instructions (and any examples) begin.
pub fn clean_input(prompt: &str) -> String { prompt.split(ANSWER_MARK).next().unwrap_or(prompt).trim().to_string() }

/// What the index holds for an example (the same text is given back to delete it, so it must be exactly reproducible).
fn fts_text(input: &str, output: &str) -> String { format!("{input}\n{output}") }

fn fts_add(c: &Connection, rowid: i64, text: &str, kind: &str, task_class: &str) -> Result<(), String> {
    c.execute("INSERT INTO memory_fts(rowid, text, kind, task_class) VALUES (?1, ?2, ?3, ?4)", params![rowid, text, kind, task_class]).map_err(|e| e.to_string())?;
    Ok(())
}
fn fts_remove(c: &Connection, rowid: i64, text: &str, kind: &str, task_class: &str) -> Result<(), String> {
    c.execute("INSERT INTO memory_fts(memory_fts, rowid, text, kind, task_class) VALUES ('delete', ?1, ?2, ?3, ?4)", params![rowid, text, kind, task_class]).map_err(|e| e.to_string())?;
    Ok(())
}

/// An FTS5 query from free text: its words, each quoted (so nothing in the text is read as syntax), any of them. None when there is no word.
pub fn fts_query(text: &str) -> Option<String> {
    let mut words: Vec<String> = vec![];
    for w in text.split(|c: char| !c.is_alphanumeric()) {
        let w = w.to_lowercase();
        if w.chars().count() >= 3 && !words.contains(&w) { words.push(w); }
        if words.len() >= 16 { break; }
    }
    if words.is_empty() { None } else { Some(words.iter().map(|w| format!("\"{w}\"")).collect::<Vec<_>>().join(" OR ")) }
}

/// THE CONSOLIDATION (§8.2): promote, dedupe, split. Safe to run any number of times.
pub fn consolidate(c: &Connection) -> Result<Report, String> {
    let mut rep = Report::default();
    // calls a guard has judged and that still have their text (a team with the off switch keeps only hashes: nothing to remember), not yet an example
    let rows: Vec<(i64, String, String, String, String, String)> = {
        let mut st = c.prepare(
            "SELECT id, COALESCE(task_class, node, ''), COALESCE(outcome, ''), prompt, answer, COALESCE(verified_by, '') FROM calls
             WHERE prompt IS NOT NULL AND answer IS NOT NULL
               AND ((verified = 1 AND outcome = 'ok') OR outcome = 'guard_failed')
               AND id NOT IN (SELECT source_call FROM examples WHERE source_call IS NOT NULL)
             ORDER BY id").map_err(|e| e.to_string())?;
        let it = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))).map_err(|e| e.to_string())?;
        it.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
    };
    for (id, class, outcome, prompt, answer, by) in rows {
        let (kind, label) = if outcome == "guard_failed" { ("regression", "caught by a guard".to_string()) } else { ("golden", format!("verified by {}", if by.is_empty() { "a guard" } else { &by })) };
        let input = clean_input(&prompt);
        let h = hash(&[&class, kind, &input, &answer]);
        let n = c.execute(
            "INSERT OR IGNORE INTO examples (cell, task_class, input, output, kind, label, source_call, verified_by, added_ms, split, hash)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'none', ?9)",
            params![class, input, answer, kind, label, id, by, crate::cli::now_ms(), h]).map_err(|e| e.to_string())?;
        if n == 1 {
            fts_add(c, c.last_insert_rowid(), &fts_text(&input, &answer), kind, &class)?;
            if kind == "golden" { rep.golden += 1 } else { rep.regression += 1 }
        }
    }
    rep.split_assigned = assign_splits(c)?;
    Ok(rep)
}

/// A split is given to a golden example ONCE, by its place in its task class (seven in ten train, two val, one held out), and an assigned split is never touched again.
fn assign_splits(c: &Connection) -> Result<usize, String> {
    let classes: Vec<String> = { let mut st = c.prepare("SELECT DISTINCT task_class FROM examples WHERE kind = 'golden' AND split = 'none'").map_err(|e| e.to_string())?; let it = st.query_map([], |r| r.get(0)).map_err(|e| e.to_string())?; it.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())? };
    let mut done = 0;
    for class in classes {
        let mut idx: i64 = c.query_row("SELECT COUNT(*) FROM examples WHERE kind = 'golden' AND task_class = ?1 AND split != 'none'", [&class], |r| r.get(0)).map_err(|e| e.to_string())?;
        let ids: Vec<i64> = { let mut st = c.prepare("SELECT id FROM examples WHERE kind = 'golden' AND task_class = ?1 AND split = 'none' ORDER BY id").map_err(|e| e.to_string())?; let it = st.query_map([&class], |r| r.get(0)).map_err(|e| e.to_string())?; it.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())? };
        for id in ids {
            let split = match idx % 10 { 9 => "heldout", 7 | 8 => "val", _ => "train" };
            c.execute("UPDATE examples SET split = ?1 WHERE id = ?2 AND split = 'none'", params![split, id]).map_err(|e| e.to_string())?;
            idx += 1; done += 1;
        }
    }
    Ok(done)
}

fn example_of(r: &rusqlite::Row) -> rusqlite::Result<Example> {
    Ok(Example { id: r.get(0)?, kind: r.get(1)?, task_class: r.get::<_, Option<String>>(2)?.unwrap_or_default(), input: r.get::<_, Option<String>>(3)?.unwrap_or_default(), output: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
        label: r.get::<_, Option<String>>(5)?.unwrap_or_default(), source_call: r.get(6)?, verified_by: r.get::<_, Option<String>>(7)?.unwrap_or_default(), split: r.get(8)? })
}
const EX_COLS: &str = "e.id, e.kind, e.task_class, e.input, e.output, e.label, e.source_call, e.verified_by, e.split";

/// Verified golden examples of one task class, nearest first (FTS5 rank, then recency), topped up with the newest ones. Never a regression, trap or held-out item.
fn usable(c: &Connection, task_class: &str, query: &str, k: usize) -> Result<Vec<Example>, String> {
    let mut out: Vec<Example> = vec![];
    if let Some(q) = fts_query(query) {
        let sql = format!("SELECT {EX_COLS} FROM memory_fts JOIN examples e ON e.id = memory_fts.rowid WHERE memory_fts MATCH ?1 AND e.kind = 'golden' AND e.task_class = ?2 AND e.split IN ('train','none') ORDER BY rank, e.added_ms DESC LIMIT ?3");
        let mut st = c.prepare(&sql).map_err(|e| e.to_string())?;
        let it = st.query_map(params![q, task_class, k as i64], example_of).map_err(|e| e.to_string())?;
        for r in it { out.push(r.map_err(|e| e.to_string())?); }
    }
    if out.len() < k {
        let sql = format!("SELECT {EX_COLS} FROM examples e WHERE e.kind = 'golden' AND e.task_class = ?1 AND e.split IN ('train','none') ORDER BY e.added_ms DESC, e.id DESC LIMIT ?2");
        let mut st = c.prepare(&sql).map_err(|e| e.to_string())?;
        let it = st.query_map(params![task_class, k as i64 + out.len() as i64], example_of).map_err(|e| e.to_string())?;
        for r in it { let r = r.map_err(|e| e.to_string())?; if out.len() < k && !out.iter().any(|x| x.id == r.id) { out.push(r); } }
    }
    Ok(out)
}

/// For a cell with `examples_from`: its examples, or none (a memory that cannot be read never stops the call).
pub fn examples_for(root: &str, team: &str, task_class: &str, query: &str, k: usize) -> Vec<Example> {
    if root.is_empty() { return vec![]; }
    crate::ledger::with_db(root, team, |c| usable(c, task_class, query, k)).unwrap_or_default()
}

/// The page of examples added to a prompt.
pub fn render_examples(ex: &[Example]) -> String {
    let cut = |t: &str| -> String { if t.chars().count() > SHOW_MAX { format!("{}…", t.chars().take(SHOW_MAX).collect::<String>()) } else { t.to_string() } };
    let mut s = String::from("\n\nVerified examples of this same task (the question, then the answer that was checked and was right):");
    for e in ex { s.push_str(&format!("\n---\nQuestion: {}\nAnswer: {}", cut(&e.input), cut(&e.output))); }
    s.push_str("\n---\nNow the real question above.");
    s
}

// ── the memory node's ops (§8.3) ────────────────────────────────────────────────────────────────────────────────────────

/// `recall` {text?, task_class?, kind?, k?} · `write` {kind?, text, links?} · `forget` {example: id} or {note: id}. Rows carry their provenance.
pub fn node_op(root: &str, team: &str, op: &Value) -> Result<Value, String> {
    let name = op["op"].as_str().unwrap_or("");
    crate::ledger::with_db(root, team, |c| match name {
        "recall" => {
            let k = op["k"].as_u64().unwrap_or(5).clamp(1, 50) as i64;
            let text = op["text"].as_str().unwrap_or("");
            let class = op["task_class"].as_str().filter(|s| !s.is_empty());
            let kind = op["kind"].as_str().filter(|s| !s.is_empty());
            let mut rows: Vec<Value> = vec![];
            let q = fts_query(text);
            let sql = if q.is_some() {
                format!("SELECT {EX_COLS} FROM memory_fts JOIN examples e ON e.id = memory_fts.rowid WHERE memory_fts MATCH ?1 AND (?2 IS NULL OR e.task_class = ?2) AND (?3 IS NULL OR e.kind = ?3) ORDER BY rank, e.added_ms DESC LIMIT ?4")
            } else {
                format!("SELECT {EX_COLS} FROM examples e WHERE ?1 IS NULL AND (?2 IS NULL OR e.task_class = ?2) AND (?3 IS NULL OR e.kind = ?3) ORDER BY e.added_ms DESC, e.id DESC LIMIT ?4")
            };
            let mut st = c.prepare(&sql).map_err(|e| e.to_string())?;
            let it = st.query_map(params![q, class, kind, k], example_of).map_err(|e| e.to_string())?;
            for r in it { let e = r.map_err(|e| e.to_string())?; rows.push(json!({ "source": "example", "id": e.id, "kind": e.kind, "task_class": e.task_class, "input": e.input, "output": e.output, "label": e.label, "source_call": e.source_call, "verified_by": e.verified_by, "split": e.split })); }
            if class.is_none() && kind.map_or(true, |k| k != "golden" && k != "regression" && k != "trap") {
                let nsql = if q.is_some() {
                    "SELECT n.id, n.kind, n.text, n.links, n.ts_ms FROM memory_fts JOIN notes n ON n.id = -memory_fts.rowid WHERE memory_fts MATCH ?1 AND (?2 IS NULL OR n.kind = ?2) ORDER BY rank, n.ts_ms DESC LIMIT ?3"
                } else {
                    "SELECT n.id, n.kind, n.text, n.links, n.ts_ms FROM notes n WHERE ?1 IS NULL AND (?2 IS NULL OR n.kind = ?2) ORDER BY n.ts_ms DESC, n.id DESC LIMIT ?3"
                };
                let mut st = c.prepare(nsql).map_err(|e| e.to_string())?;
                let it = st.query_map(params![q, kind, k], |r| Ok(json!({ "source": "note", "id": r.get::<_, i64>(0)?, "kind": r.get::<_, String>(1)?, "text": r.get::<_, String>(2)?, "links": r.get::<_, Option<String>>(3)?, "ts_ms": r.get::<_, i64>(4)? }))).map_err(|e| e.to_string())?;
                for r in it { rows.push(r.map_err(|e| e.to_string())?); }
            }
            Ok(json!({ "ok": true, "rows": rows }))
        }
        "write" => {
            let text = op["text"].as_str().unwrap_or("").trim();
            if text.is_empty() { return Err("say the `text` to remember".into()); }
            let kind = op["kind"].as_str().filter(|s| !s.is_empty()).unwrap_or("lesson");
            let text = crate::ledger::sanitize(text);
            c.execute("INSERT INTO notes (ts_ms, kind, text, links) VALUES (?1, ?2, ?3, ?4)", params![crate::cli::now_ms(), kind, text, op["links"].as_str()]).map_err(|e| e.to_string())?;
            let id = c.last_insert_rowid();
            fts_add(c, -id, &text, kind, "")?;
            Ok(json!({ "ok": true, "note": id }))
        }
        "forget" => {
            if let Some(id) = op["note"].as_i64() {
                let row: Option<(String, String)> = c.query_row("SELECT text, kind FROM notes WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(|e| e.to_string())?;
                let (text, kind) = row.ok_or_else(|| format!("no note {id}"))?;
                fts_remove(c, -id, &text, &kind, "")?;
                c.execute("DELETE FROM notes WHERE id = ?1", [id]).map_err(|e| e.to_string())?;
                return Ok(json!({ "ok": true, "forgot": { "note": id } }));
            }
            if let Some(id) = op["example"].as_i64() {
                let row: Option<(String, String, String, String)> = c.query_row("SELECT input, output, kind, task_class FROM examples WHERE id = ?1", [id], |r| Ok((r.get::<_, Option<String>>(0)?.unwrap_or_default(), r.get::<_, Option<String>>(1)?.unwrap_or_default(), r.get(2)?, r.get::<_, Option<String>>(3)?.unwrap_or_default()))).optional().map_err(|e| e.to_string())?;
                let (input, output, kind, class) = row.ok_or_else(|| format!("no example {id}"))?;
                fts_remove(c, id, &fts_text(&input, &output), &kind, &class)?;
                c.execute("DELETE FROM examples WHERE id = ?1", [id]).map_err(|e| e.to_string())?;
                return Ok(json!({ "ok": true, "forgot": { "example": id } }));
            }
            Err("say what to forget: `note` or `example` (an id)".into())
        }
        other => Err(format!("memory op is recall, write or forget (not `{other}`)")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{self, Ctx};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NTH: AtomicU64 = AtomicU64::new(0);
    fn ws() -> String {
        let n = NTH.fetch_add(1, Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("antiloki-memory-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        d.to_string_lossy().into_owned()
    }
    /// a call row as the ledger writes it, plus the verdict a guard gave it
    fn call(c: &Connection, class: &str, prompt: &str, answer: &str, outcome: &str, verified: Option<i32>) -> i64 {
        c.execute("INSERT INTO calls (ts_ms, team, node, task_class, caller, source, outcome, bench, prompt, answer, prompt_hash, answer_hash, verified, verified_by) VALUES (?1,'t','n',?2,'team-node','endpoint',?3,1,?4,?5,'p','a',?6,?7)",
            params![crate::cli::now_ms(), class, outcome, format!("{prompt}{ANSWER_MARK}: {{...}}"), answer, verified, verified.map(|_| "guard")]).unwrap();
        c.last_insert_rowid()
    }
    fn db<T>(root: &str, f: impl FnOnce(&Connection) -> T) -> T { ledger::with_db(root, "t", |c| Ok(f(c))).unwrap() }

    #[test]
    fn fts5_recall_finds_golden_examples_by_task_class_and_text() {
        let root = ws();
        db(&root, |c| {
            call(c, "shelf", "New on the shelf: Orchard Morning, price 18", "{\"name\":\"Orchard Morning\"}", "ok", Some(1));
            call(c, "shelf", "Restock: Winter Blend, price 22", "{\"name\":\"Winter Blend\"}", "ok", Some(1));
            call(c, "other", "Orchard cider recipe", "{\"x\":1}", "ok", Some(1));
            consolidate(c).unwrap();
        });
        let hit = node_op(&root, "t", &json!({ "op": "recall", "text": "orchard", "task_class": "shelf" })).unwrap();
        let rows = hit["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{hit}");
        assert!(rows[0]["input"].as_str().unwrap().contains("Orchard Morning"));
        assert_eq!(rows[0]["verified_by"], json!("guard"), "provenance is on the row");
        assert!(rows[0]["source_call"].as_i64().is_some());
        let both = node_op(&root, "t", &json!({ "op": "recall", "task_class": "shelf", "kind": "golden", "k": 10 })).unwrap();
        assert_eq!(both["rows"].as_array().unwrap().len(), 2);
        // words in the text are never read as FTS syntax
        assert!(node_op(&root, "t", &json!({ "op": "recall", "text": "\"orchard\" AND (NOT *" })).is_ok());
    }

    #[test]
    fn consolidation_dedupes_by_hash() {
        let root = ws();
        let (r1, r2, n) = db(&root, |c| {
            call(c, "shelf", "same question", "same answer", "ok", Some(1));
            call(c, "shelf", "same question", "same answer", "ok", Some(1));
            let r1 = consolidate(c).unwrap(); let r2 = consolidate(c).unwrap();
            (r1, r2, c.query_row::<i64, _, _>("SELECT COUNT(*) FROM examples", [], |r| r.get(0)).unwrap())
        });
        assert_eq!(n, 1); assert_eq!(r1.golden, 1); assert_eq!(r2.golden, 0, "a second pass adds nothing");
    }

    #[test]
    fn a_failure_caught_by_a_guard_is_promoted_to_the_regression_set() {
        let root = ws();
        let (kind, label, split) = db(&root, |c| {
            call(c, "shelf", "price of Orchard", "{\"price\": 99}", "guard_failed", Some(0));
            let r = consolidate(c).unwrap(); assert_eq!((r.regression, r.golden), (1, 0));
            c.query_row::<(String, String, String), _, _>("SELECT kind, label, split FROM examples", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap()
        });
        assert_eq!(kind, "regression"); assert!(label.contains("guard")); assert_eq!(split, "none");
    }

    #[test]
    fn a_verified_success_is_promoted_to_the_golden_set() {
        let root = ws();
        db(&root, |c| {
            call(c, "shelf", "q1", "a1", "ok", Some(1));
            call(c, "shelf", "q2", "a2", "ok", None);                    // nobody judged it: not memory
            call(c, "shelf", "q3", "a3", "parse_fail", Some(0));         // did not even answer in shape
            let r = consolidate(c).unwrap(); assert_eq!((r.golden, r.regression), (1, 0));
            let (input, kind): (String, String) = c.query_row("SELECT input, kind FROM examples", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
            assert_eq!((input.as_str(), kind.as_str()), ("q1", "golden"), "the example is the question, without the answer instructions");
        });
    }

    #[test]
    fn a_team_with_the_off_switch_keeps_no_text_so_nothing_is_remembered() {
        let root = ws();
        ledger::set_off(&root, "t", true).unwrap();
        db(&root, |c| {
            c.execute("INSERT INTO calls (ts_ms, team, caller, source, outcome, bench, prompt_hash, answer_hash, verified) VALUES (1,'t','x','cli','ok',1,'p','a',1)", []).unwrap();
            assert_eq!(consolidate(c).unwrap(), Report::default());
        });
    }

    #[test]
    fn a_split_is_assigned_once_and_never_moved() {
        let root = ws();
        let first: Vec<(i64, String)> = db(&root, |c| {
            for i in 0..12 { call(c, "shelf", &format!("q{i}"), &format!("a{i}"), "ok", Some(1)); }
            consolidate(c).unwrap();
            let mut st = c.prepare("SELECT id, split FROM examples ORDER BY id").unwrap();
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect()
        });
        assert_eq!(first.len(), 12);
        assert_eq!(first.iter().filter(|(_, s)| s == "heldout").count(), 1);
        assert!(first.iter().all(|(_, s)| s != "none"));
        let after: Vec<(i64, String)> = db(&root, |c| {
            for i in 12..25 { call(c, "shelf", &format!("q{i}"), &format!("a{i}"), "ok", Some(1)); }
            consolidate(c).unwrap(); consolidate(c).unwrap();
            let mut st = c.prepare("SELECT id, split FROM examples ORDER BY id").unwrap();
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect()
        });
        assert_eq!(after.len(), 25);
        assert_eq!(&after[..12], &first[..], "the first twelve did not move");
        assert_eq!(after.iter().filter(|(_, s)| s == "heldout").count(), 2);
    }

    #[test]
    fn examples_from_fills_the_prompt_with_k_examples_and_the_call_row_records_them() {
        let root = ws();
        db(&root, |c| {
            for (q, a) in [("Orchard Morning price 18", "{\"p\":18}"), ("Winter Blend price 22", "{\"p\":22}"), ("Spring Tonic price 12", "{\"p\":12}")] { call(c, "shelf", q, a, "ok", Some(1)); }
            consolidate(c).unwrap();
        });
        let ex = examples_for(&root, "t", "shelf", "Orchard price", 2);
        assert_eq!(ex.len(), 2);
        assert!(ex[0].input.contains("Orchard"), "nearest first: {ex:?}");
        let page = render_examples(&ex);
        assert!(page.contains("Question: Orchard Morning price 18") && page.contains("Answer: {\"p\":18}"), "{page}");
        // the ledger row for the call carries the ids
        let mut ctx = Ctx { root: root.clone(), team: "t".into(), caller: "team-node".into(), node: Some("n".into()), run: Some("r".into()), ..Default::default() };
        ctx.examples_used = Some(json!(ex.iter().map(|e| e.id).collect::<Vec<_>>()).to_string());
        ledger::record_llm(Some(&ctx), &crate::team_llm::Seat::Endpoint { alias: "a".into(), model: None }, "ask", &Ok(("{}".into(), 0.0, 1)));
        let used: String = db(&root, |c| c.query_row("SELECT examples_used FROM calls WHERE prompt = 'ask'", [], |r| r.get(0)).unwrap());
        assert_eq!(used, ctx.examples_used.unwrap());
        assert_eq!(examples_for(&root, "t", "nothing-here", "x", 3).len(), 0);
    }

    #[test]
    fn held_out_and_regression_items_are_never_used_as_examples() {
        let root = ws();
        db(&root, |c| {
            for i in 0..10 { call(c, "shelf", &format!("question {i}"), &format!("answer {i}"), "ok", Some(1)); }
            call(c, "shelf", "question bad", "answer bad", "guard_failed", Some(0));
            consolidate(c).unwrap();
        });
        let ex = examples_for(&root, "t", "shelf", "question", 50);
        assert!(!ex.is_empty());
        assert!(ex.iter().all(|e| e.kind == "golden" && (e.split == "train" || e.split == "none")), "{ex:?}");
        assert!(!ex.iter().any(|e| e.input.contains("bad")));
        let held: i64 = db(&root, |c| c.query_row("SELECT COUNT(*) FROM examples WHERE split = 'heldout'", [], |r| r.get(0)).unwrap());
        assert_eq!(held, 1);
        assert!(ex.len() < 10, "the held-out and validation items stayed out ({} given)", ex.len());
    }

    #[test]
    fn memory_node_ops_recall_write_forget() {
        let root = ws();
        let w = node_op(&root, "t", &json!({ "op": "write", "kind": "lesson", "text": "Prices come as pounds; always strip the sign. key sk-abcdefghijklmnopqrstuvwxyz123" })).unwrap();
        let id = w["note"].as_i64().unwrap();
        let r = node_op(&root, "t", &json!({ "op": "recall", "text": "pounds sign" })).unwrap();
        assert_eq!(r["rows"].as_array().unwrap().len(), 1, "{r}");
        assert!(!r["rows"][0]["text"].as_str().unwrap().contains("sk-abc"), "a note is sanitized before it is stored");
        assert_eq!(r["rows"][0]["source"], json!("note"));
        node_op(&root, "t", &json!({ "op": "forget", "note": id })).unwrap();
        assert_eq!(node_op(&root, "t", &json!({ "op": "recall", "text": "pounds sign" })).unwrap()["rows"].as_array().unwrap().len(), 0);
        // an example can be forgotten too, and it leaves the index
        db(&root, |c| { call(c, "shelf", "secret customer question", "secret answer", "ok", Some(1)); consolidate(c).unwrap(); });
        let ex = node_op(&root, "t", &json!({ "op": "recall", "text": "customer", "task_class": "shelf" })).unwrap();
        let eid = ex["rows"][0]["id"].as_i64().unwrap();
        node_op(&root, "t", &json!({ "op": "forget", "example": eid })).unwrap();
        assert_eq!(node_op(&root, "t", &json!({ "op": "recall", "text": "customer", "task_class": "shelf" })).unwrap()["rows"].as_array().unwrap().len(), 0);
        assert!(node_op(&root, "t", &json!({ "op": "forget" })).is_err());
        assert!(node_op(&root, "t", &json!({ "op": "write", "text": " " })).is_err());
    }

    #[test]
    fn the_answer_mark_is_what_the_prompt_builder_writes() {
        let spec: crate::team_llm::Spec = serde_json::from_value(json!({ "seat": "endpoint:a", "prompt": "p", "answer": { "x": "string" } })).unwrap();
        assert!(crate::team_llm::answer_note(&spec).starts_with(ANSWER_MARK));
        assert_eq!(clean_input("What is it?\nAnswer ONLY JSON, with exactly these fields: {}\n\nVerified examples"), "What is it?");
    }
}
