//! data-story-propose.rs — twin of skills/codebase/data-story-propose.mjs (db, cross-module).
//! NAMES ARE CONTRACT: default `dataStoryPropose`. Proposes GOAL-DRIVEN data stories: gathers a
//! relevant slice of the file-level code graph (goal-term seeds, else detected entry/data
//! heuristics, plus a 2-hop neighborhood) and computes a DETERMINISTIC longest entry→data path per
//! seed (Tarjan SCC condensation of the FULL directed file graph + longest-chain DP over the DAG +
//! in-SCC BFS to expand real edges) — this is the "deterministic-first" v2 path, $0, always
//! computed. AI naming (goal-driven journey titles) is GARNISH layered on top only when the caller
//! passes a goal or `ai:true`.
//!
//! PARITY-DEFERRED: unlike a spine skill whose deferred boundary propagates an Err, this JS skill
//! wraps `aiCall` (+ its JSON.parse of the response) in its OWN try/catch and DEGRADES GRACEFULLY —
//! any aiCall failure returns the deterministic proposals with an explanatory note, never throws.
//! Since `crate::ai_ai_call::aiCall` always returns `Err` (see its module doc), `dataStoryPropose`
//! here ALWAYS takes that graceful-degradation branch whenever `wantAI` is true — this IS the real,
//! reachable, tested behavior (not an approximation): the deterministic-first design means the AI
//! branch's happy path (JSON.parse the response, filter grounded hops, stage proposals) is ported
//! faithfully below but is UNREACHABLE by this twin's own fixture, exactly as book-run.rs's post-
//! wave logic is unreachable past its first `fac_call`. `ensure_prompt` (real, reached, tested)
//! reuses the already-converted `ai_prompt_find`/`ai_prompt_add` twins.
// QUALITY: deterministic Tarjan/longest-chain proposer (real+tested) + AI-garnish shell (deferred,
// graceful-degrade path real+tested; JSON-parse-success path ported, unreachable by fixture)
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::OnceLock;
use regex::Regex;

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
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
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}

fn base_name(p: &str) -> String {
    p.split(|c| c == '/' || c == '\\').filter(|s| !s.is_empty()).last().unwrap_or(p).to_string()
}

fn is_entry_re1() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)(^|/)(controller|route|router|handler|main|index|app|page|cli|server)s?(\.|/|$)").unwrap()) }
fn is_entry_re2() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)(controllers?|routes?|pages?|api|handlers?)/").unwrap()) }
fn is_data_re1() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)(^|/)(db|model|repository|store|dao|schema|migration|entity)s?(\.|/|$)").unwrap()) }
fn is_data_re2() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)\.sql$").unwrap()) }
fn is_data_re3() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)/(db|data|models?|repositor|stores?|entit)").unwrap()) }
fn is_entry(p: &str) -> bool { is_entry_re1().is_match(p) || is_entry_re2().is_match(p) }
fn is_data(p: &str) -> bool { is_data_re1().is_match(p) || is_data_re2().is_match(p) || is_data_re3().is_match(p) }

fn role_re_controller() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)(controller|route|router|handler|gateway|resolver|endpoint)").unwrap()) }
fn role_re_service() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)(service|usecase|use-case|logic|manager|engine|core|domain)").unwrap()) }
fn role_re_repo() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)(repositor|store|dao|model|entit|schema|query|queries)").unwrap()) }
fn role_re_util() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)(util|helper|lib|common|shared|format|parse)").unwrap()) }
fn role_re_component() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)(component|view|page|screen|render)").unwrap()) }

const PROMPT: &str = "atlas.story.propose.v2";
const SYSTEM: &str = "You map a codebase into GOAL-DRIVEN data stories — the realistic end-to-end journey of a user or system goal through the code (e.g. \"user searches for a product\": route → controller → validate → service → repository → db → map → response). You output ONLY strict JSON. You reference ONLY the node ids provided. You build the FULL flow across multiple files in execution order — never the shortest import path, never a 2-hop stub. Invent no node ids.";
const TEMPLATE: &str = "{{goal_line}}\nDirection: {{direction}} (forward = from the user/entry inward to the data layer; backward = from the db outward to the user).\n\nBelow is a slice of the REAL code graph (files + their call/import edges). Construct up to {{n}} GOAL-DRIVEN data stor(y/ies). Each = a named GOAL + the ordered MULTI-FILE flow that goal takes through THESE nodes, in execution order (the realistic journey, 4-10 hops where the code supports it — not the shortest path). Each hop label says what happens at that file.\n\n=== NODES you may reference (node_id · path · [tags] · intent) ===\nTagged nodes are DETECTED entry points: [ENTRY:route]/[ENTRY:ui] = where a forward journey STARTS; [DATA] = the storage boundary where it ENDS. Anchor each story between a real entry and a real data node wherever the graph allows.\n{{nodes_md}}\n\n=== EDGES (from → to: who imports/calls whom) ===\n{{edges_md}}\n\nReturn JSON only as exactly: {\"stories_json\":\"<a JSON array string: [{\\\"name\\\":\\\"<goal>\\\",\\\"description\\\":\\\"<1 line>\\\",\\\"direction\\\":\\\"forward|backward\\\",\\\"hops\\\":[{\\\"node_id\\\":\\\"<id from the list>\\\",\\\"label\\\":\\\"<what happens here>\\\"}]}]>\"}";

fn ensure_prompt(db: &Connection) -> Result<(), String> {
    if let Ok(existing) = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db) {
        if existing.get("template_md").and_then(|v| v.as_str()) == Some(TEMPLATE) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT, "template_md": TEMPLATE, "system_md": SYSTEM,
            "default_tier": "mid", "default_temperature": "cold", "phase_flow": "single_only",
            "max_output_tokens": 1600,
            "output_fields": [{ "field_name": "stories_json", "field_type": "string", "required": true, "min_length": 10 }],
        }),
        db,
    )?;
    Ok(())
}

// res?.output_kv?.find(o => o.field_name === 'stories_json' && o.parent_output_id == null)?.value_text
fn pick(res: &Value) -> Option<String> {
    let arr = res.get("output_kv")?.as_array()?;
    let hit = arr.iter().find(|o| {
        o.get("field_name").and_then(|v| v.as_str()) == Some("stories_json")
            && o.get("parent_output_id").map(|v| v.is_null()).unwrap_or(true)
    })?;
    hit.get("value_text").and_then(|v| v.as_str()).map(|s| s.to_string())
}

// ── gatherSlice: the relevant slice of the FILE graph ───────────────────────────────────────
struct Slice {
    order_ids: Vec<String>,                        // insertion order of `files` (type='file') rows
    by_id: HashMap<String, Value>,                  // file id -> {id,path,intent}
    nodes: Vec<Value>,                              // kept nodes (neighborhood-expanded)
    edges: Vec<(String, String)>,                   // kept edges (both endpoints in `keep`)
    valid_ids: HashSet<String>,                     // `keep`
    ep_by_path: HashMap<String, HashSet<String>>,   // path -> {route|ui|data}
    ep_label: HashMap<String, HashMap<String, String>>, // path -> {kind: firstLabel}
    adj: HashMap<String, HashSet<String>>,          // undirected file adjacency
    dir_edges: Vec<(String, String)>,                // the FULL directed file graph (rawEdges)
}

fn gather_slice(db: &Connection, origin: &str, goal: Option<&str>, direction: &str) -> Result<Slice, String> {
    let files_rows = crate::db_json::query_rows(db, "SELECT id, path, intent FROM code_nodes WHERE origin=? AND type='file'", &[json!(origin)])?;
    let files: Vec<Value> = files_rows.as_array().cloned().unwrap_or_default();
    let mut order_ids: Vec<String> = Vec::new();
    let mut by_id: HashMap<String, Value> = HashMap::new();
    let mut file_id_by_path: HashMap<String, String> = HashMap::new();
    for f in &files {
        let id = js_string(f.get("id").unwrap_or(&Value::Null));
        let path = js_string(f.get("path").unwrap_or(&Value::Null));
        order_ids.push(id.clone());
        by_id.insert(id.clone(), f.clone());
        file_id_by_path.insert(path, id);
    }

    let all_rows = crate::db_json::query_rows(db, "SELECT id, path FROM code_nodes WHERE origin=?", &[json!(origin)])?;
    let mut node_file: HashMap<String, String> = HashMap::new();
    for n in all_rows.as_array().cloned().unwrap_or_default() {
        node_file.insert(js_string(n.get("id").unwrap_or(&Value::Null)), js_string(n.get("path").unwrap_or(&Value::Null)));
    }

    let edge_rows = crate::db_json::query_rows(db, "SELECT from_node, to_node FROM code_edges WHERE origin=? AND type IN ('calls','imports','uses')", &[json!(origin)])?;
    const UNDEF: &str = "\u{0}__undefined__";
    let mut adj: HashMap<String, HashSet<String>> = HashMap::new();
    let mut raw_edges: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for e in edge_rows.as_array().cloned().unwrap_or_default() {
        let from = js_string(e.get("from_node").unwrap_or(&Value::Null));
        let to = js_string(e.get("to_node").unwrap_or(&Value::Null));
        let a = node_file.get(&from);
        let b = node_file.get(&to);
        let (Some(a), Some(b)) = (a, b) else { continue };
        if a == b { continue; }
        let k = format!("{a}{b}");
        let fi_a = file_id_by_path.get(a).cloned().unwrap_or_else(|| UNDEF.to_string());
        let fi_b = file_id_by_path.get(b).cloned().unwrap_or_else(|| UNDEF.to_string());
        if !seen.contains(&k) {
            seen.insert(k);
            raw_edges.push((fi_a.clone(), fi_b.clone()));
        }
        adj.entry(fi_a.clone()).or_default().insert(fi_b.clone());
        adj.entry(fi_b).or_default().insert(fi_a);
    }

    let mut ep_by_path: HashMap<String, HashSet<String>> = HashMap::new();
    let mut ep_label: HashMap<String, HashMap<String, String>> = HashMap::new();
    if let Ok(ep_rows) = crate::db_json::query_rows(db, "SELECT path, kind, label FROM code_entry_points WHERE origin=? ORDER BY confidence DESC", &[json!(origin)]) {
        for r in ep_rows.as_array().cloned().unwrap_or_default() {
            let path = js_string(r.get("path").unwrap_or(&Value::Null));
            let kind = js_string(r.get("kind").unwrap_or(&Value::Null));
            let label = js_string(r.get("label").unwrap_or(&Value::Null));
            ep_by_path.entry(path.clone()).or_default().insert(kind.clone());
            let m = ep_label.entry(path).or_default();
            m.entry(kind).or_insert(label);
        }
    }

    // seeds
    let mut seeds: Vec<Value> = Vec::new();
    if let Some(g) = goal {
        if !g.trim().is_empty() {
            let kw: Vec<String> = g.to_lowercase().split(|c: char| !c.is_alphanumeric() && c != '_')
                .filter(|w| w.chars().count() > 2).map(|s| s.to_string()).collect();
            seeds = files.iter().filter(|f| {
                let path = js_string(f.get("path").unwrap_or(&Value::Null)).to_lowercase();
                let intent = js_string(f.get("intent").unwrap_or(&Value::Null)).to_lowercase();
                kw.iter().any(|w| path.contains(w.as_str()) || intent.contains(w.as_str()))
            }).take(20).cloned().collect();
        }
    }
    if seeds.is_empty() {
        let want: &[&str] = if direction == "forward" { &["route", "ui"] } else { &["data"] };
        seeds = files.iter().filter(|f| {
            let path = js_string(f.get("path").unwrap_or(&Value::Null));
            ep_by_path.get(&path).map(|ks| want.iter().any(|w| ks.contains(*w))).unwrap_or(false)
        }).take(12).cloned().collect();
    }
    if seeds.is_empty() {
        seeds = files.iter().filter(|f| {
            let path = js_string(f.get("path").unwrap_or(&Value::Null));
            if direction == "forward" { is_entry(&path) } else { is_data(&path) }
        }).take(12).cloned().collect();
    }
    if seeds.is_empty() {
        seeds = files.iter().take(8).cloned().collect();
    }

    let mut keep: HashSet<String> = HashSet::new();
    let mut q: VecDeque<(String, i32)> = VecDeque::new();
    for s in &seeds {
        let id = js_string(s.get("id").unwrap_or(&Value::Null));
        keep.insert(id.clone());
        q.push_back((id, 0));
    }
    while !q.is_empty() && keep.len() < 60 {
        let (id, d) = q.pop_front().unwrap();
        if d >= 2 { continue; }
        if let Some(nbrs) = adj.get(&id) {
            for nx in nbrs.clone() {
                if !keep.contains(&nx) {
                    keep.insert(nx.clone());
                    q.push_back((nx, d + 1));
                }
            }
        }
    }
    let nodes: Vec<Value> = keep.iter().filter_map(|id| by_id.get(id).cloned()).collect();
    let edges: Vec<(String, String)> = raw_edges.iter().filter(|(a, b)| keep.contains(a) && keep.contains(b)).cloned().collect();

    Ok(Slice { order_ids, by_id, nodes, edges, valid_ids: keep, ep_by_path, ep_label, adj, dir_edges: raw_edges })
}

fn role_of(p: &str, first: bool, last: bool, direction: &str, ep_label: &HashMap<String, HashMap<String, String>>) -> String {
    let l = ep_label.get(p);
    if first && direction == "forward" {
        if let Some(r) = l.and_then(|m| m.get("route")) { return format!("entry — {r}"); }
        if let Some(u) = l.and_then(|m| m.get("ui")) { return format!("entry — {u}"); }
        return "entry — the flow starts here".to_string();
    }
    if last && direction == "forward" {
        if let Some(d) = l.and_then(|m| m.get("data")) { return format!("death point — {d}"); }
        return "death point — the data lands here".to_string();
    }
    if first && direction == "backward" {
        if let Some(d) = l.and_then(|m| m.get("data")) { return format!("origin — {d}"); }
        return "origin — the data starts here".to_string();
    }
    if last && direction == "backward" {
        if let Some(r) = l.and_then(|m| m.get("route")) { return format!("surface — {r}"); }
        return "surface — reaches the user here".to_string();
    }
    if role_re_controller().is_match(p) { return "handles + validates the request".to_string(); }
    if role_re_service().is_match(p) { return "business logic".to_string(); }
    if role_re_repo().is_match(p) { return "data access layer".to_string(); }
    if role_re_util().is_match(p) { return "shared helper on the way".to_string(); }
    if role_re_component().is_match(p) { return "renders the state".to_string(); }
    "passes through".to_string()
}

#[derive(Clone, Copy)]
struct Best { len: i64, next: Option<usize> }

fn inside_path(scc_members: &[Vec<usize>], adj_f: &[Vec<usize>], c: usize, from: usize, goal: impl Fn(usize) -> bool) -> Option<Vec<usize>> {
    let members: HashSet<usize> = scc_members[c].iter().cloned().collect();
    let mut prev: HashMap<usize, Option<usize>> = HashMap::new();
    prev.insert(from, None);
    let mut q: VecDeque<usize> = VecDeque::new();
    q.push_back(from);
    let mut hit: Option<usize> = if goal(from) { Some(from) } else { None };
    while !q.is_empty() && hit.is_none() {
        let cur = q.pop_front().unwrap();
        for &nx in &adj_f[cur] {
            if !members.contains(&nx) || prev.contains_key(&nx) { continue; }
            prev.insert(nx, Some(cur));
            if goal(nx) { hit = Some(nx); break; }
            q.push_back(nx);
        }
    }
    let hit = hit?;
    let mut out = vec![];
    let mut cur = Some(hit);
    while let Some(c2) = cur {
        out.push(c2);
        cur = *prev.get(&c2).unwrap();
    }
    out.reverse();
    Some(out)
}

// DETERMINISTIC proposer v2: longest entry→data chain via Tarjan SCC condensation + DP over the DAG
// + in-SCC BFS expansion into real edges. Zero AI.
fn deterministic_proposals(slice: &Slice, direction: &str, limit: usize) -> Vec<Value> {
    let entryish = |p: &str| -> bool {
        slice.ep_by_path.get(p).map(|ks| ks.contains("route") || ks.contains("ui")).unwrap_or(false) || is_entry(p)
    };
    let dataish = |p: &str| -> bool {
        slice.ep_by_path.get(p).map(|ks| ks.contains("data")).unwrap_or(false) || is_data(p)
    };

    let ids: &Vec<String> = &slice.order_ids;
    let ix: HashMap<&str, usize> = ids.iter().enumerate().map(|(i, s)| (s.as_str(), i)).collect();
    let n = ids.len();
    let mut adj_f: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (a, b) in &slice.dir_edges {
        let (from_p, to_p) = if direction == "forward" { (a, b) } else { (b, a) };
        if let (Some(&x), Some(&y)) = (ix.get(from_p.as_str()), ix.get(to_p.as_str())) {
            if x != y { adj_f[x].push(y); }
        }
    }
    let path_at = |i: usize| -> String { slice.by_id.get(&ids[i]).and_then(|v| v.get("path")).map(js_string).unwrap_or_default() };
    let is_seed_ix = |i: usize| -> bool { let p = path_at(i); if direction == "forward" { entryish(&p) } else { dataish(&p) } };
    let is_sink_ix = |i: usize| -> bool { let p = path_at(i); if direction == "forward" { dataish(&p) } else { entryish(&p) } };

    // iterative Tarjan SCC
    let mut scc_of: Vec<i64> = vec![-1; n];
    let mut low: Vec<i64> = vec![0; n];
    let mut num: Vec<i64> = vec![-1; n];
    let mut on_stk: Vec<bool> = vec![false; n];
    let mut stk: Vec<usize> = Vec::new();
    let mut counter: i64 = 0;
    let mut scc_members: Vec<Vec<usize>> = Vec::new();
    for s in 0..n {
        if num[s] != -1 { continue; }
        let mut frames: Vec<(usize, usize)> = vec![(s, 0)];
        while let Some(&(v, fr1)) = frames.last() {
            if fr1 == 0 {
                num[v] = counter; low[v] = counter; counter += 1;
                stk.push(v); on_stk[v] = true;
            }
            let mut advanced = false;
            loop {
                let cur_fr1 = frames.last().unwrap().1;
                if cur_fr1 >= adj_f[v].len() { break; }
                let w = adj_f[v][cur_fr1];
                frames.last_mut().unwrap().1 += 1;
                if num[w] == -1 {
                    frames.push((w, 0));
                    advanced = true;
                    break;
                }
                if on_stk[w] && num[w] < low[v] { low[v] = num[w]; }
            }
            if advanced { continue; }
            if low[v] == num[v] {
                let mut members = vec![];
                loop {
                    let w = stk.pop().unwrap();
                    on_stk[w] = false;
                    scc_of[w] = scc_members.len() as i64;
                    members.push(w);
                    if w == v { break; }
                }
                scc_members.push(members);
            }
            frames.pop();
            if let Some(&(p, _)) = frames.last() {
                if low[v] < low[p] { low[p] = low[v]; }
            }
        }
    }
    let s_count = scc_members.len();
    let mut scc_adj: Vec<HashSet<usize>> = vec![HashSet::new(); s_count];
    for v in 0..n {
        for &w in &adj_f[v] {
            if scc_of[v] != scc_of[w] { scc_adj[scc_of[v] as usize].insert(scc_of[w] as usize); }
        }
    }
    let scc_has_sink: Vec<bool> = scc_members.iter().map(|ms| ms.iter().any(|&m| is_sink_ix(m))).collect();

    let mut best: Vec<Option<Best>> = vec![None; s_count];
    for c in 0..s_count {
        let mut cand: Option<Best> = if scc_has_sink[c] { Some(Best { len: 1, next: None }) } else { None };
        let mut ds: Vec<usize> = scc_adj[c].iter().cloned().collect();
        ds.sort();
        for d in ds {
            let Some(bd) = best[d] else { continue };
            let len = 1 + bd.len;
            if cand.map(|c2| len > c2.len).unwrap_or(true) {
                cand = Some(Best { len, next: Some(d) });
            }
        }
        best[c] = cand;
    }

    let mut seeds: Vec<usize> = (0..n).filter(|&i| is_seed_ix(i) && best[scc_of[i] as usize].map(|b| b.len >= 2).unwrap_or(false)).collect();
    seeds.sort_by(|&a, &b| {
        let len_a = best[scc_of[a] as usize].unwrap().len;
        let len_b = best[scc_of[b] as usize].unwrap().len;
        len_b.cmp(&len_a).then_with(|| path_at(a).cmp(&path_at(b)))
    });

    let mut out: Vec<Value> = Vec::new();
    let mut used_ends: HashSet<String> = HashSet::new();
    for seed in seeds {
        if out.len() >= limit { break; }
        let mut hops: Vec<usize> = Vec::new();
        let mut c: Option<usize> = Some(scc_of[seed] as usize);
        let mut enter = seed;
        let mut guard: i32 = 0;
        loop {
            let Some(cc) = c else { break };
            let g = guard; guard += 1;
            if g >= 40 || hops.len() >= 24 { break; }
            let Some(b) = best[cc] else { break };
            if b.next.is_none() {
                if let Some(seg) = inside_path(&scc_members, &adj_f, cc, enter, |m| is_sink_ix(m)) {
                    hops.extend(seg);
                }
                break;
            }
            let d = b.next.unwrap();
            let Some(seg) = inside_path(&scc_members, &adj_f, cc, enter, |m| adj_f[m].iter().any(|&w| scc_of[w] as usize == d)) else { break };
            let exit = *seg.last().unwrap();
            let entry_next = adj_f[exit].iter().find(|&&w| scc_of[w] as usize == d).copied();
            hops.extend(seg);
            c = Some(d);
            match entry_next {
                Some(en) => enter = en,
                None => break,
            }
        }
        if hops.len() < 2 || !is_sink_ix(*hops.last().unwrap()) { continue; }
        let end_key = format!("{}→{}", hops[0], hops[hops.len() - 1]);
        if used_ends.contains(&end_key) { continue; }
        used_ends.insert(end_key);
        let seed_p = path_at(hops[0]);
        let sink_p = path_at(hops[hops.len() - 1]);
        let seed_lbl = slice.ep_label.get(&seed_p).and_then(|m| m.get("route").or_else(|| m.get("ui"))).cloned().unwrap_or_else(|| base_name(&seed_p));
        let sink_lbl = slice.ep_label.get(&sink_p).and_then(|m| m.get("data")).cloned().unwrap_or_else(|| base_name(&sink_p));
        let name = if direction == "forward" { format!("{seed_lbl} → {sink_lbl}") } else { format!("{sink_lbl} ← {seed_lbl}") };
        let description = format!("the full computed path — every file the flow crosses ({} hops, {direction})", hops.len());
        let hops_out: Vec<Value> = hops.iter().enumerate().map(|(k, &i)| {
            let p = path_at(i);
            json!({
                "node_id": ids[i],
                "label": format!("{} — {}", base_name(&p), role_of(&p, k == 0, k == hops.len() - 1, direction, &slice.ep_label)),
            })
        }).collect();
        out.push(json!({ "name": name, "description": description, "direction": direction, "hops": hops_out }));
    }
    out
}

fn strip_fences(s: &str) -> String {
    let t = s.trim();
    let re_open = Regex::new(r"(?i)^```(?:json)?").unwrap();
    let t = re_open.replace(t, "").to_string();
    let t = t.trim_end();
    let t = t.strip_suffix("```").unwrap_or(t);
    t.trim().to_string()
}

pub fn dataStoryPropose(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — unreachable here (Rust's `db: &Connection` is never absent).
    let origin = str_qq(nz(input.get("origin")));
    let origin = if origin.is_empty() { "atlas".to_string() } else { origin };
    let direction = if input.get("direction").and_then(|v| v.as_str()) == Some("backward") { "backward" } else { "forward" }.to_string();

    let limit_input = nz(input.get("limit")).cloned().unwrap_or(json!(4));
    let limit_num = js_number(Some(&limit_input));
    let limit: usize = if limit_num.is_nan() { 0 } else { limit_num.max(1.0).min(6.0).trunc().max(0.0) as usize };
    let goal_val = nz(input.get("goal")).cloned();
    let goal_str = goal_val.as_ref().map(js_string);

    let slice = gather_slice(db, &origin, goal_str.as_deref(), &direction)?;
    if slice.nodes.len() < 2 {
        return Ok(json!({
            "proposals": [], "count": 0, "direction": direction,
            "grounded": { "nodes": slice.nodes.len() as i64, "edges": slice.edges.len() as i64 },
            "error": "not enough graph to propose from",
        }));
    }

    let deterministic = deterministic_proposals(&slice, &direction, limit);

    let want_ai = input.get("ai").and_then(|v| v.as_bool()) == Some(true)
        || goal_str.as_ref().map(|g| !g.trim().is_empty()).unwrap_or(false);
    if !want_ai {
        let note = if !deterministic.is_empty() {
            "computed entry→data paths (deterministic, $0) — type a goal to have AI name goal-driven journeys".to_string()
        } else {
            "no entry→data path found in this slice — the graph may lack route/ui seeds or data sinks".to_string()
        };
        return Ok(json!({
            "proposals": deterministic, "count": deterministic.len() as i64, "direction": direction, "deterministic": true, "note": note,
            "grounded": { "nodes": slice.nodes.len() as i64, "edges": slice.edges.len() as i64 },
        }));
    }

    let ep_tag = |p: &str| -> String {
        match slice.ep_by_path.get(p) {
            None => String::new(),
            Some(ks) => {
                let mut ks_v: Vec<&String> = ks.iter().collect();
                ks_v.sort();
                let parts: Vec<String> = ks_v.iter().map(|k| if k.as_str() == "data" { "DATA".to_string() } else { format!("ENTRY:{k}") }).collect();
                format!(" [{}]", parts.join(","))
            }
        }
    };
    let nodes_md: String = slice.nodes.iter().take(60).map(|n| {
        let id = js_string(n.get("id").unwrap_or(&Value::Null));
        let path = js_string(n.get("path").unwrap_or(&Value::Null));
        let intent = n.get("intent").filter(|v| !v.is_null()).map(js_string).unwrap_or_default();
        let intent_part = if intent.is_empty() { String::new() } else { format!(" · {}", intent.chars().take(80).collect::<String>()) };
        format!("{id} · {path}{}{intent_part}", ep_tag(&path))
    }).collect::<Vec<_>>().join("\n");
    let edges_md_lines: Vec<String> = slice.edges.iter().take(200).map(|(a, b)| format!("{a} → {b}")).collect();
    let edges_md = if edges_md_lines.is_empty() { "(none)".to_string() } else { edges_md_lines.join("\n") };
    let goal_line = match &goal_str {
        Some(g) if !g.trim().is_empty() => format!("Goal: {}", g.trim()),
        _ => "Discover the main user/system goals in this slice.".to_string(),
    };

    ensure_prompt(db)?;

    let ai_input = json!({
        "prompt_name": PROMPT,
        "input_kv": [
            { "key": "goal_line", "value": goal_line },
            { "key": "direction", "value": direction },
            { "key": "n", "value": limit.to_string() },
            { "key": "nodes_md", "value": nodes_md },
            { "key": "edges_md", "value": edges_md },
        ],
        "tier": "mid", "temperature": "cold", "phase_flow": "single_only", "provider_fn": Value::Null,
    });

    // ── PARITY-DEFERRED boundary: the JS wraps aiCall (+ its JSON.parse) in a try/catch that
    // degrades to the deterministic proposals on ANY failure. `crate::ai_ai_call::aiCall` always
    // returns Err, so this twin ALWAYS takes that branch here — this IS the real, reachable
    // behavior of this skill (the deterministic-first design), not an approximation of it.
    let parsed: Value = match crate::ai_ai_call::aiCall(&ai_input, db) {
        Err(e) => {
            let note = format!("AI naming unavailable ({}) — showing computed entry→data paths", e.chars().take(110).collect::<String>());
            return Ok(json!({
                "proposals": deterministic, "count": deterministic.len() as i64, "direction": direction, "deterministic": true, "note": note,
                "grounded": { "nodes": slice.nodes.len() as i64, "edges": slice.edges.len() as i64 },
            }));
        }
        // Ported byte-for-byte from the JS but UNREACHABLE by this twin's own fixture (aiCall
        // never returns Ok here) — mirrors book-run.rs's treatment of post-deferred-boundary code.
        Ok(res) => match pick(&res) {
            Some(raw) => serde_json::from_str::<Value>(&strip_fences(&raw)).unwrap_or(Value::Null),
            None => Value::Null,
        },
    };

    let list: Vec<Value> = match &parsed {
        Value::Array(a) => a.clone(),
        Value::Object(o) => o.get("stories").and_then(|v| v.as_array()).cloned().unwrap_or_default(),
        _ => Vec::new(),
    };
    let mut proposals: Vec<Value> = Vec::new();
    for s in list.into_iter().take(limit) {
        let hops_in = s.get("hops").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let hops: Vec<Value> = hops_in.into_iter().filter_map(|h| {
            let node_id = h.get("node_id")?;
            if node_id.is_null() { return None; }
            let node_id_s = js_string(node_id);
            if !slice.valid_ids.contains(&node_id_s) { return None; }
            let label = h.get("label").filter(|v| !v.is_null()).map(js_string).unwrap_or_default();
            let label: String = label.chars().take(120).collect();
            let label = if label.is_empty() {
                slice.nodes.iter().find(|n| js_string(n.get("id").unwrap_or(&Value::Null)) == node_id_s)
                    .map(|n| base_name(&js_string(n.get("path").unwrap_or(&Value::Null))))
                    .unwrap_or_else(|| node_id_s.clone())
            } else { label };
            Some(json!({ "node_id": node_id.clone(), "label": label }))
        }).collect();
        if hops.len() < 2 { continue; }
        let name: String = str_qq(s.get("name")).chars().take(120).collect();
        let name = if name.is_empty() { "journey".to_string() } else { name };
        let description: String = str_qq(s.get("description")).chars().take(240).collect();
        let s_direction = if s.get("direction").and_then(|v| v.as_str()) == Some("backward") { "backward".to_string() } else { direction.clone() };
        proposals.push(json!({ "name": name, "description": description, "direction": s_direction, "hops": hops }));
    }
    if proposals.is_empty() && !deterministic.is_empty() {
        return Ok(json!({
            "proposals": deterministic, "count": deterministic.len() as i64, "direction": direction, "deterministic": true,
            "note": "AI returned no grounded journeys — showing computed entry→data paths",
            "grounded": { "nodes": slice.nodes.len() as i64, "edges": slice.edges.len() as i64 },
        }));
    }
    Ok(json!({
        "proposals": proposals, "count": proposals.len() as i64, "direction": direction,
        "grounded": { "nodes": slice.nodes.len() as i64, "edges": slice.edges.len() as i64 },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE code_nodes (id TEXT, origin TEXT, name TEXT, path TEXT, type TEXT, intent TEXT);
             CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT);
             CREATE TABLE code_entry_points (path TEXT, origin TEXT, kind TEXT, label TEXT, confidence REAL);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    fn add_node(c: &Connection, id: &str, path: &str, typ: &str) {
        c.execute("INSERT INTO code_nodes (id, origin, name, path, type) VALUES (?, 'atlas', ?, ?, ?)", rusqlite::params![id, path, path, typ]).unwrap();
    }
    fn add_edge(c: &Connection, a: &str, b: &str) {
        c.execute("INSERT INTO code_edges (origin, from_node, to_node, type) VALUES ('atlas', ?, ?, 'calls')", rusqlite::params![a, b]).unwrap();
    }
    fn add_ep(c: &Connection, path: &str, kind: &str, label: &str, conf: f64) {
        c.execute("INSERT INTO code_entry_points (path, origin, kind, label, confidence) VALUES (?, 'atlas', ?, ?, ?)", rusqlite::params![path, kind, label, conf]).unwrap();
    }

    #[test]
    fn not_enough_graph_returns_empty() {
        let c = setup();
        add_node(&c, "a", "route.ts", "file");
        let r = dataStoryPropose(&json!({}), &c).unwrap();
        assert_eq!(r["proposals"], json!([]));
        assert_eq!(r["count"], json!(0));
        assert_eq!(r["error"], json!("not enough graph to propose from"));
    }

    #[test]
    fn deterministic_first_computes_entry_to_data_path_without_ai() {
        let c = setup();
        add_node(&c, "n1", "src/routes/checkout.ts", "file");
        add_node(&c, "n2", "src/services/checkout-service.ts", "file");
        add_node(&c, "n3", "src/db/orders-repository.ts", "file");
        add_edge(&c, "n1", "n2");
        add_edge(&c, "n2", "n3");
        add_ep(&c, "src/routes/checkout.ts", "route", "/checkout", 1.0);
        add_ep(&c, "src/db/orders-repository.ts", "data", "orders", 1.0);
        let r = dataStoryPropose(&json!({}), &c).unwrap();
        assert_eq!(r["deterministic"], json!(true));
        let proposals = r["proposals"].as_array().unwrap();
        assert_eq!(proposals.len(), 1);
        let hops = proposals[0]["hops"].as_array().unwrap();
        assert_eq!(hops.len(), 3);
        assert_eq!(hops[0]["node_id"], json!("n1"));
        assert_eq!(hops[2]["node_id"], json!("n3"));
        assert!(proposals[0]["name"].as_str().unwrap().contains("→"));
    }

    #[test]
    fn goal_present_takes_ai_branch_and_degrades_gracefully_on_deferred_aicall() {
        let c = setup();
        add_node(&c, "n1", "src/routes/checkout.ts", "file");
        add_node(&c, "n2", "src/services/checkout-service.ts", "file");
        add_node(&c, "n3", "src/db/orders-repository.ts", "file");
        add_edge(&c, "n1", "n2");
        add_edge(&c, "n2", "n3");
        add_ep(&c, "src/routes/checkout.ts", "route", "/checkout", 1.0);
        add_ep(&c, "src/db/orders-repository.ts", "data", "orders", 1.0);
        let r = dataStoryPropose(&json!({ "goal": "user checks out" }), &c).unwrap();
        assert_eq!(r["deterministic"], json!(true));
        assert!(r["note"].as_str().unwrap().starts_with("AI naming unavailable"));
        assert_eq!(r["proposals"].as_array().unwrap().len(), 1);
        // the prompt WAS ensured before the deferred call ran
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'atlas.story.propose.v2'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn backward_direction_walks_from_data_to_entry() {
        let c = setup();
        add_node(&c, "n1", "src/routes/checkout.ts", "file");
        add_node(&c, "n2", "src/services/checkout-service.ts", "file");
        add_node(&c, "n3", "src/db/orders-repository.ts", "file");
        add_edge(&c, "n1", "n2");
        add_edge(&c, "n2", "n3");
        add_ep(&c, "src/routes/checkout.ts", "route", "/checkout", 1.0);
        add_ep(&c, "src/db/orders-repository.ts", "data", "orders", 1.0);
        let r = dataStoryPropose(&json!({ "direction": "backward" }), &c).unwrap();
        let proposals = r["proposals"].as_array().unwrap();
        assert_eq!(proposals.len(), 1);
        let hops = proposals[0]["hops"].as_array().unwrap();
        assert_eq!(hops[0]["node_id"], json!("n3"));
        assert_eq!(hops[2]["node_id"], json!("n1"));
    }

    #[test]
    fn no_entry_data_path_still_returns_deterministic_note() {
        let c = setup();
        add_node(&c, "n1", "src/util/format.ts", "file");
        add_node(&c, "n2", "src/util/parse.ts", "file");
        add_edge(&c, "n1", "n2");
        let r = dataStoryPropose(&json!({}), &c).unwrap();
        assert_eq!(r["proposals"], json!([]));
        assert_eq!(r["deterministic"], json!(true));
        assert!(r["note"].as_str().unwrap().contains("no entry→data path found"));
    }

    #[test]
    fn role_of_labels_first_and_last_hops_by_direction() {
        let mut ep_label: HashMap<String, HashMap<String, String>> = HashMap::new();
        let mut m = HashMap::new();
        m.insert("route".to_string(), "/checkout".to_string());
        ep_label.insert("src/routes/checkout.ts".to_string(), m);
        assert_eq!(role_of("src/routes/checkout.ts", true, false, "forward", &ep_label), "entry — /checkout");
        assert_eq!(role_of("src/services/x.ts", false, false, "forward", &HashMap::new()), "business logic");
        assert_eq!(role_of("src/db/orders-repository.ts", false, true, "forward", &HashMap::new()), "death point — the data lands here");
    }
}
