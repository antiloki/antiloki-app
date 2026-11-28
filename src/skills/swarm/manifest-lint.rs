//! manifest-lint.rs — twin of skills/swarm/manifest-lint.mjs. NAMES ARE CONTRACT:
//! parseManifest · lintManifest · topoOrder · resolveBlockSlice · renderStory (no-db helpers)
//! + manifestLint (default, db lane: compute-only OR driver record-a-verdict path).
// QUALITY: deterministic typed-dataflow lint + story renderer; driver path UPDATE+INSERT via db_after
use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet, VecDeque};

// ── JS-parity helpers ─────────────────────────────────────────────────────────
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `${v}` interpolation.
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// JS `${JSON.stringify(v)}`: undefined → "undefined", else compact JSON.
fn stringify(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(x) => serde_json::to_string(x).unwrap_or_else(|_| "null".into()),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
// JS `obj?.k` (property access that is undefined for non-objects, no throw).
fn oget<'a>(v: &'a Value, k: &str) -> Option<&'a Value> { if v.is_object() { v.get(k) } else { None } }
// JS Map key for a block id / port name (strings stay strings; other values stringify like `${v}`).
fn map_key(v: &Value) -> String { match v { Value::String(s) => s.clone(), _ => tmpl(Some(v)) } }
// truthy `a?.id`
fn block_id(a: &Value) -> Option<&Value> { let id = oget(a, "id"); if truthy(id) { id } else { None } }
fn port_arr<'a>(a: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    a.get(key).and_then(|v| v.as_array()).map(|v| v.iter()).into_iter().flatten()
}

// dupes: values seen more than once, in order of the SECOND occurrence, unique.
fn dupes(arr: &[String]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out_set: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for x in arr {
        if seen.contains(x) {
            if out_set.insert(x.clone()) { out.push(x.clone()); }
        } else {
            seen.insert(x.clone());
        }
    }
    out
}

// ── indexBlocks ──────────────────────────────────────────────────────────────
struct Idx<'a> {
    block: HashMap<String, &'a Value>,
    produces: HashMap<String, HashMap<String, &'a Value>>,
    consumes: HashMap<String, HashMap<String, &'a Value>>,
}
fn index_blocks(blocks: &[Value]) -> Idx<'_> {
    let mut idx = Idx { block: HashMap::new(), produces: HashMap::new(), consumes: HashMap::new() };
    for a in blocks {
        let id = match block_id(a) { Some(v) => v, None => continue };
        let k = map_key(id);
        idx.block.insert(k.clone(), a);
        let mut pm: HashMap<String, &Value> = HashMap::new();
        for p in port_arr(a, "produces") { if let Some(n) = oget(p, "name") { pm.insert(map_key(n), p); } }
        let mut cm: HashMap<String, &Value> = HashMap::new();
        for p in port_arr(a, "consumes") { if let Some(n) = oget(p, "name") { cm.insert(map_key(n), p); } }
        idx.produces.insert(k.clone(), pm);
        idx.consumes.insert(k, cm);
    }
    idx
}

// ── parseManifest ────────────────────────────────────────────────────────────
fn parse_manifest_inner(input: &Value) -> Value {
    if input.is_object() || input.is_array() {
        if let Some(m) = oget(input, "manifest") {
            if truthy(Some(m)) && (m.is_object() || m.is_array()) { return m.clone(); }
        }
    }
    let mj = oget(input, "manifest_json");
    let raw: &Value = nz(mj).unwrap_or(input);
    if let Value::String(s) = raw {
        return serde_json::from_str::<Value>(s).unwrap_or(Value::Null);
    }
    if truthy(Some(raw)) && (raw.is_object() || raw.is_array()) { raw.clone() } else { Value::Null }
}
pub fn parseManifest(input: &Value) -> Value { parse_manifest_inner(input) }

// ── lintManifest ─────────────────────────────────────────────────────────────
fn err_obj(code: &str, msg: String) -> Value {
    let mut o = Map::new();
    o.insert("code".into(), json!(code));
    o.insert("msg".into(), json!(msg));
    Value::Object(o)
}
fn lint_manifest_inner(manifest: &Value) -> Value {
    let mut errors: Vec<Value> = Vec::new();
    let mut warnings: Vec<Value> = Vec::new();

    if !(manifest.is_object() || manifest.is_array()) {
        return json!({
            "ok": false,
            "errors": [{ "code": "not-an-object", "msg": "manifest is missing or not an object" }],
            "warnings": []
        });
    }
    if !manifest.get("version").map_or(false, |v| v.is_number()) {
        errors.push(err_obj("no-version", "manifest.version must be a number".into()));
    }
    let goal = manifest.get("goal");
    if !truthy(goal) || !goal.map_or(false, |g| g.is_string()) {
        errors.push(err_obj("no-goal", "manifest.goal must be a non-empty string".into()));
    }
    let blocks_arr = manifest.get("blocks").and_then(|v| v.as_array());
    let bindings: Vec<Value> = manifest.get("bindings").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if blocks_arr.map_or(true, |b| b.is_empty()) {
        errors.push(err_obj("no-blocks", "manifest.blocks must be a non-empty array".into()));
        return json!({ "ok": errors.is_empty(), "errors": errors, "warnings": warnings });
    }
    let blocks: Vec<Value> = blocks_arr.unwrap().clone();
    if !manifest.get("bindings").map_or(false, |v| v.is_array()) {
        errors.push(err_obj("no-bindings-array", "manifest.bindings must be an array (may be empty)".into()));
    }

    // --- block structure ---
    for (i, a) in blocks.iter().enumerate() {
        let at = match block_id(a) {
            Some(id) => format!("block '{}'", tmpl(Some(id))),
            None => format!("blocks[{}]", i),
        };
        if !truthy(Some(a)) || !(a.is_object() || a.is_array()) {
            errors.push(err_obj("bad-block", format!("{} is not an object", at)));
            continue;
        }
        if !truthy(oget(a, "id")) { errors.push(err_obj("block-no-id", format!("{} has no id", at))); }
        if !truthy(oget(a, "file")) { errors.push(err_obj("block-no-file", format!("{} has no file", at))); }
        if !truthy(oget(a, "role")) { errors.push(err_obj("block-no-role", format!("{} has no role", at))); }
        let pr = oget(a, "produces");
        if pr.is_some() && !pr.unwrap().is_null() && !pr.unwrap().is_array() {
            errors.push(err_obj("block-bad-produces", format!("{} produces must be an array", at)));
        }
        let co = oget(a, "consumes");
        if co.is_some() && !co.unwrap().is_null() && !co.unwrap().is_array() {
            errors.push(err_obj("block-bad-consumes", format!("{} consumes must be an array", at)));
        }
        for p in port_arr(a, "produces") {
            if !truthy(oget(p, "name")) { errors.push(err_obj("produce-no-name", format!("{} has a produce port with no name", at))); }
        }
        for p in port_arr(a, "consumes") {
            if !truthy(oget(p, "name")) { errors.push(err_obj("consume-no-name", format!("{} has a consume port with no name", at))); }
        }
        let pn: Vec<String> = port_arr(a, "produces").filter_map(|p| { let n = oget(p, "name"); if truthy(n) { Some(tmpl(n)) } else { None } }).collect();
        let cn: Vec<String> = port_arr(a, "consumes").filter_map(|p| { let n = oget(p, "name"); if truthy(n) { Some(tmpl(n)) } else { None } }).collect();
        for d in dupes(&pn) { errors.push(err_obj("dup-produce", format!("{} produces '{}' more than once", at, d))); }
        for d in dupes(&cn) { errors.push(err_obj("dup-consume", format!("{} consumes '{}' more than once", at, d))); }
    }
    let ids: Vec<String> = blocks.iter().filter_map(|a| { let id = oget(a, "id"); if truthy(id) { Some(tmpl(id)) } else { None } }).collect();
    for d in dupes(&ids) { errors.push(err_obj("dup-block-id", format!("duplicate block id '{}'", d))); }
    let files: Vec<String> = blocks.iter().filter_map(|a| { let f = oget(a, "file"); if truthy(f) { Some(tmpl(f)) } else { None } }).collect();
    for d in dupes(&files) { errors.push(err_obj("dup-block-file", format!("two blocks author the same file '{}'", d))); }

    let idx = index_blocks(&blocks);

    // --- binding integrity + type agreement ---
    let mut fed_consume: HashSet<String> = HashSet::new();
    let mut used_produce: HashSet<String> = HashSet::new();
    let binding_ids: Vec<String> = bindings.iter().filter_map(|b| { let id = oget(b, "id"); if truthy(id) { Some(tmpl(id)) } else { None } }).collect();
    for d in dupes(&binding_ids) { errors.push(err_obj("dup-binding-id", format!("duplicate binding id '{}'", d))); }
    for (i, b) in bindings.iter().enumerate() {
        let bid = oget(b, "id");
        let at = if truthy(bid) { format!("binding '{}'", tmpl(bid)) } else { format!("bindings[{}]", i) };
        let from = oget(b, "from");
        let to = oget(b, "to");
        let fb = from.and_then(|f| oget(f, "block"));
        let fp = from.and_then(|f| oget(f, "port"));
        let tb = to.and_then(|t| oget(t, "block"));
        let tp = to.and_then(|t| oget(t, "port"));
        if !truthy(fb) || !truthy(fp) || !truthy(tb) || !truthy(tp) {
            errors.push(err_obj("binding-malformed", format!("{} must have from:{{block,port}} and to:{{block,port}}", at)));
            continue;
        }
        let fb = fb.unwrap(); let fp = fp.unwrap(); let tb = tb.unwrap(); let tp = tp.unwrap();
        if fb == tb { errors.push(err_obj("binding-self", format!("{} connects block '{}' to itself", at, tmpl(Some(fb))))); }
        let fbk = map_key(fb); let tbk = map_key(tb);
        let src = idx.block.contains_key(&fbk);
        let dst = idx.block.contains_key(&tbk);
        if !src { errors.push(err_obj("binding-bad-from-block", format!("{} from.block '{}' is not a block", at, tmpl(Some(fb))))); }
        if !dst { errors.push(err_obj("binding-bad-to-block", format!("{} to.block '{}' is not a block", at, tmpl(Some(tb))))); }
        if !src || !dst { continue; }
        let sp = idx.produces.get(&fbk).and_then(|m| m.get(&map_key(fp)).copied());
        let dp = idx.consumes.get(&tbk).and_then(|m| m.get(&map_key(tp)).copied());
        if sp.is_none() { errors.push(err_obj("binding-bad-from-port", format!("{} from '{}.{}' is not a produce port", at, tmpl(Some(fb)), tmpl(Some(fp))))); }
        if dp.is_none() { errors.push(err_obj("binding-bad-to-port", format!("{} to '{}.{}' is not a consume port", at, tmpl(Some(tb)), tmpl(Some(tp))))); }
        if sp.is_some() { used_produce.insert(format!("{} {}", tmpl(Some(fb)), tmpl(Some(fp)))); }
        if dp.is_some() { fed_consume.insert(format!("{} {}", tmpl(Some(tb)), tmpl(Some(tp)))); }
        if let (Some(sp_port), Some(dp_port)) = (sp, dp) {
            // types = [b.type, sp.type, dp.type].filter(t != null && t !== '')
            let candidates = [b.get("type"), sp_port.get("type"), dp_port.get("type")];
            let mut kept: Vec<&Value> = Vec::new();
            for t in candidates {
                if let Some(tv) = t {
                    if !tv.is_null() && tv.as_str() != Some("") { kept.push(tv); }
                }
            }
            let mut distinct: Vec<&Value> = Vec::new();
            for k in &kept { if !distinct.iter().any(|d| *d == *k) { distinct.push(k); } }
            if distinct.len() > 1 {
                errors.push(err_obj("binding-type-mismatch", format!(
                    "{} type disagreement: binding={} producer({}.{})={} consumer({}.{})={}",
                    at, stringify(b.get("type")),
                    tmpl(Some(fb)), tmpl(Some(fp)), stringify(sp_port.get("type")),
                    tmpl(Some(tb)), tmpl(Some(tp)), stringify(dp_port.get("type")))));
            }
        }
    }

    // --- dangling inputs / dead outputs / isolation ---
    let mut isolated: i64 = 0;
    for a in &blocks {
        let aid = match block_id(a) { Some(v) => v, None => continue };
        for p in port_arr(a, "consumes") {
            let name = oget(p, "name");
            if !truthy(name) { continue; }
            let key = format!("{} {}", tmpl(Some(aid)), tmpl(name));
            if fed_consume.contains(&key) { continue; }
            if truthy(oget(p, "external")) { continue; }
            errors.push(err_obj("dangling-input", format!(
                "block '{}' input '{}' is not external and no binding feeds it \u{2014} the dataflow story has a gap",
                tmpl(Some(aid)), tmpl(name))));
        }
        for p in port_arr(a, "produces") {
            let name = oget(p, "name");
            if !truthy(name) { continue; }
            if used_produce.contains(&format!("{} {}", tmpl(Some(aid)), tmpl(name))) { continue; }
            warnings.push(err_obj("dead-output", format!(
                "block '{}' output '{}' is produced but no binding consumes it", tmpl(Some(aid)), tmpl(name))));
        }
        let plen = a.get("produces").and_then(|v| v.as_array()).map(|x| x.len()).unwrap_or(0);
        let clen = a.get("consumes").and_then(|v| v.as_array()).map(|x| x.len()).unwrap_or(0);
        if plen == 0 && clen == 0 {
            isolated += 1;
            warnings.push(err_obj("isolated-block", format!("block '{}' produces and consumes nothing", tmpl(Some(aid)))));
        }
    }

    // --- cycle detection ---
    if let Some(cyc) = find_cycle(&blocks, &bindings) {
        warnings.push(err_obj("cycle", format!(
            "the binding graph has a cycle: {} \u{2014} the dataflow story is not a DAG", cyc.join(" -> "))));
    }

    let ok = errors.is_empty();
    let mut out = Map::new();
    out.insert("ok".into(), Value::Bool(ok));
    out.insert("errors".into(), Value::Array(errors));
    out.insert("warnings".into(), Value::Array(warnings));
    out.insert("blocks_total".into(), json!(blocks.len()));
    out.insert("blocks_isolated".into(), json!(isolated));
    Value::Object(out)
}
pub fn lintManifest(manifest: &Value) -> Value { lint_manifest_inner(manifest) }

// ── topoOrder ────────────────────────────────────────────────────────────────
fn topo_order_inner(blocks: &[Value], bindings: &[Value]) -> Option<Vec<String>> {
    let ids: Vec<String> = blocks.iter().filter_map(|a| block_id(a).map(map_key)).collect();
    let mut indeg: HashMap<String, i64> = HashMap::new();
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for id in &ids { indeg.entry(id.clone()).or_insert(0); out.entry(id.clone()).or_default(); }
    for b in bindings {
        let fb = oget(b, "from").and_then(|f| oget(f, "block"));
        let tb = oget(b, "to").and_then(|t| oget(t, "block"));
        if !truthy(fb) || !truthy(tb) { continue; }
        let fb = fb.unwrap(); let tb = tb.unwrap();
        let fbk = map_key(fb); let tbk = map_key(tb);
        if !indeg.contains_key(&fbk) || !indeg.contains_key(&tbk) { continue; }
        if fb == tb { continue; }
        out.get_mut(&fbk).unwrap().push(tbk.clone());
        *indeg.get_mut(&tbk).unwrap() += 1;
    }
    let mut queue: VecDeque<String> = ids.iter().filter(|id| indeg.get(*id).copied() == Some(0)).cloned().collect();
    let mut order: Vec<String> = Vec::new();
    while let Some(id) = queue.pop_front() {
        order.push(id.clone());
        if let Some(succ) = out.get(&id).cloned() {
            for n in succ {
                if let Some(d) = indeg.get_mut(&n) { *d -= 1; if *d == 0 { queue.push_back(n.clone()); } }
            }
        }
    }
    if order.len() == ids.len() { Some(order) } else { None }
}
pub fn topoOrder(blocks: &Value, bindings: &Value) -> Value {
    let bl: Vec<Value> = blocks.as_array().cloned().unwrap_or_default();
    let bi: Vec<Value> = bindings.as_array().cloned().unwrap_or_default();
    match topo_order_inner(&bl, &bi) {
        Some(order) => Value::Array(order.into_iter().map(Value::String).collect()),
        None => Value::Null,
    }
}

// ── findCycle (DFS on the residual graph) ────────────────────────────────────
fn find_cycle(blocks: &[Value], bindings: &[Value]) -> Option<Vec<String>> {
    if topo_order_inner(blocks, bindings).is_some() { return None; }
    let mut keys: Vec<String> = Vec::new();
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for a in blocks {
        if let Some(id) = block_id(a) {
            let k = map_key(id);
            if !out.contains_key(&k) { keys.push(k.clone()); }
            out.insert(k, Vec::new());
        }
    }
    for b in bindings {
        let fb = oget(b, "from").and_then(|f| oget(f, "block"));
        let tb = oget(b, "to").and_then(|t| oget(t, "block"));
        if truthy(fb) && truthy(tb) {
            let fb = fb.unwrap(); let tb = tb.unwrap();
            let fbk = map_key(fb); let tbk = map_key(tb);
            if out.contains_key(&fbk) && out.contains_key(&tbk) && fb != tb {
                out.get_mut(&fbk).unwrap().push(tbk);
            }
        }
    }
    let mut color: HashMap<String, u8> = keys.iter().map(|k| (k.clone(), 0u8)).collect();
    let mut stack: Vec<String> = Vec::new();
    let mut cycle: Option<Vec<String>> = None;
    for id in &keys {
        if cycle.is_some() { break; }
        if color.get(id).copied() == Some(0) { dfs_cycle(id, &out, &mut color, &mut stack, &mut cycle); }
    }
    cycle
}
fn dfs_cycle(id: &str, out: &HashMap<String, Vec<String>>, color: &mut HashMap<String, u8>, stack: &mut Vec<String>, cycle: &mut Option<Vec<String>>) {
    color.insert(id.to_string(), 1);
    stack.push(id.to_string());
    if let Some(succ) = out.get(id) {
        for n in succ {
            if cycle.is_some() { return; }
            match color.get(n).copied() {
                Some(1) => {
                    let pos = stack.iter().position(|x| x == n).unwrap();
                    let mut c: Vec<String> = stack[pos..].to_vec();
                    c.push(n.clone());
                    *cycle = Some(c);
                    return;
                }
                Some(0) => dfs_cycle(n, out, color, stack, cycle),
                _ => {}
            }
        }
    }
    stack.pop();
    color.insert(id.to_string(), 2);
}

// ── resolveBlockSlice ────────────────────────────────────────────────────────
fn resolve_block_slice_inner(manifest: &Value, block_id_arg: &Value) -> Value {
    let blocks: Vec<Value> = manifest.get("blocks").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let bindings: Vec<Value> = manifest.get("bindings").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let idx = index_blocks(&blocks);
    let a = match blocks.iter().find(|x| oget(x, "id").map_or(false, |v| v == block_id_arg)) {
        Some(a) => a, None => return Value::Null,
    };
    let file_of = |id: &Value| -> Value {
        match idx.block.get(&map_key(id)).and_then(|b| oget(b, "file")).filter(|f| !f.is_null()) {
            Some(f) => f.clone(), None => Value::Null,
        }
    };

    let mut produces: Vec<Value> = Vec::new();
    for p in port_arr(a, "produces") {
        let name = oget(p, "name");
        let ty = oget(p, "type");
        let mut consumed_by: Vec<Value> = Vec::new();
        for b in &bindings {
            let fb = oget(b, "from").and_then(|f| oget(f, "block"));
            let fp = oget(b, "from").and_then(|f| oget(f, "port"));
            let m1 = fb.map_or(false, |v| v == block_id_arg);
            let m2 = matches!((fp, name), (Some(x), Some(y)) if x == y);
            if m1 && m2 {
                let tb = oget(b, "to").and_then(|t| oget(t, "block"));
                let tp = oget(b, "to").and_then(|t| oget(t, "port"));
                let mut o = Map::new();
                if let Some(v) = tb { o.insert("block".into(), v.clone()); }
                o.insert("file".into(), tb.map(|v| file_of(v)).unwrap_or(Value::Null));
                if let Some(v) = tp { o.insert("port".into(), v.clone()); }
                consumed_by.push(Value::Object(o));
            }
        }
        let mut po = Map::new();
        if let Some(n) = name { po.insert("name".into(), n.clone()); }
        if let Some(t) = ty { po.insert("type".into(), t.clone()); }
        po.insert("consumedBy".into(), Value::Array(consumed_by));
        produces.push(Value::Object(po));
    }

    let mut consumes: Vec<Value> = Vec::new();
    for p in port_arr(a, "consumes") {
        let name = oget(p, "name");
        let ty = oget(p, "type");
        let ext = truthy(oget(p, "external"));
        let binding = bindings.iter().find(|b| {
            let tb = oget(b, "to").and_then(|t| oget(t, "block"));
            let tp = oget(b, "to").and_then(|t| oget(t, "port"));
            let m1 = tb.map_or(false, |v| v == block_id_arg);
            let m2 = matches!((tp, name), (Some(x), Some(y)) if x == y);
            m1 && m2
        });
        let source = match binding {
            Some(b) => {
                let fb = oget(b, "from").and_then(|f| oget(f, "block"));
                let fp = oget(b, "from").and_then(|f| oget(f, "port"));
                let mut o = Map::new();
                if let Some(v) = fb { o.insert("block".into(), v.clone()); }
                o.insert("file".into(), fb.map(|v| file_of(v)).unwrap_or(Value::Null));
                if let Some(v) = fp { o.insert("port".into(), v.clone()); }
                Value::Object(o)
            }
            None => Value::Null,
        };
        let mut co = Map::new();
        if let Some(n) = name { co.insert("name".into(), n.clone()); }
        if let Some(t) = ty { co.insert("type".into(), t.clone()); }
        co.insert("external".into(), Value::Bool(ext));
        co.insert("source".into(), source);
        consumes.push(Value::Object(co));
    }

    let mut block_obj = Map::new();
    if let Some(v) = oget(a, "id") { block_obj.insert("id".into(), v.clone()); }
    if let Some(v) = oget(a, "file") { block_obj.insert("file".into(), v.clone()); }
    if let Some(v) = oget(a, "role") { block_obj.insert("role".into(), v.clone()); }
    let mut out = Map::new();
    out.insert("block".into(), Value::Object(block_obj));
    out.insert("produces".into(), Value::Array(produces));
    out.insert("consumes".into(), Value::Array(consumes));
    Value::Object(out)
}
pub fn resolveBlockSlice(manifest: &Value, block_id: &Value) -> Value { resolve_block_slice_inner(manifest, block_id) }

// ── renderStory ──────────────────────────────────────────────────────────────
fn render_story_inner(manifest: &Value) -> String {
    let blocks: Vec<Value> = manifest.get("blocks").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let bindings: Vec<Value> = manifest.get("bindings").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if blocks.is_empty() { return String::new(); }
    let order: Vec<String> = topo_order_inner(&blocks, &bindings)
        .unwrap_or_else(|| blocks.iter().filter_map(|a| block_id(a).map(map_key)).collect());
    let mut by_id: HashMap<String, &Value> = HashMap::new();
    for a in &blocks { if let Some(id) = block_id(a) { by_id.insert(map_key(id), a); } }
    let goal_str = match nz(manifest.get("goal")) { Some(g) => tmpl(Some(g)), None => "blueprint".to_string() };
    let mut lines: Vec<String> = vec![format!("# {}", goal_str), String::new()];
    for (n, id) in order.iter().enumerate() {
        let a = match by_id.get(id) { Some(a) => *a, None => continue };
        lines.push(format!("{}. **{}** `{}` \u{2014} {}", n + 1, tmpl(oget(a, "id")), tmpl(oget(a, "file")), tmpl(oget(a, "role"))));
        let ins: Vec<String> = bindings.iter().filter(|b| {
            oget(b, "to").and_then(|t| oget(t, "block")).map_or(false, |tb| map_key(tb) == *id)
        }).map(|b| {
            let fb = oget(b, "from").and_then(|f| oget(f, "block"));
            let fp = oget(b, "from").and_then(|f| oget(f, "port"));
            let tp = oget(b, "to").and_then(|t| oget(t, "port"));
            format!("`{}.{}` \u{2192} `{}`", tmpl(fb), tmpl(fp), tmpl(tp))
        }).collect();
        let ext: Vec<String> = port_arr(a, "consumes").filter(|p| truthy(oget(p, "external")))
            .map(|p| format!("`{}` (external)", tmpl(oget(p, "name")))).collect();
        let outs: Vec<String> = port_arr(a, "produces").map(|p| format!("`{}`", tmpl(oget(p, "name")))).collect();
        if !ins.is_empty() || !ext.is_empty() {
            let mut combined = ins.clone();
            combined.extend(ext.clone());
            lines.push(format!("   - consumes: {}", combined.join(", ")));
        }
        if !outs.is_empty() { lines.push(format!("   - produces: {}", outs.join(", "))); }
    }
    lines.join("\n")
}
pub fn renderStory(manifest: &Value) -> Value { json!(render_story_inner(manifest)) }

// ── default: manifestLint (compute-only OR driver record path) ───────────────
pub fn manifestLint(input: &Value, db: &Connection) -> Result<Value, String> {
    let inline = parse_manifest_inner(input);
    let has_cs = truthy(oget(input, "contract_slug"));
    // Compute-only path.
    if truthy(Some(&inline)) && !has_cs {
        let res = lint_manifest_inner(&inline);
        let ok = res.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
        let story = if ok { render_story_inner(&inline) } else { String::new() };
        let mut o = res.as_object().cloned().unwrap_or_default();
        o.insert("story_md".into(), json!(story));
        return Ok(Value::Object(o));
    }
    if !has_cs { return Err("manifest-lint: manifest | contract_slug is required".into()); }

    // Driver/record path.
    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, manifest_json FROM swarm_manifests WHERE contract_slug = ? AND superseded_by IS NULL ORDER BY version DESC, id DESC LIMIT 1",
        &[oget(input, "contract_slug").cloned().unwrap_or(Value::Null)],
    )?;
    let row = match rows.as_array().and_then(|a| a.first()).cloned() {
        Some(r) => r,
        None => return Err(format!("manifest-lint: swarm_manifest for contract '{}' not found", tmpl(oget(input, "contract_slug")))),
    };
    let raw = match row.get("manifest_json").filter(|v| !v.is_null()) {
        Some(Value::String(s)) => s.clone(),
        Some(other) => tmpl(Some(other)),
        None => "{}".to_string(),
    };
    let manifest = serde_json::from_str::<Value>(&raw).unwrap_or(Value::Null);
    let res = lint_manifest_inner(&manifest);
    let ok = res.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let story = if ok { render_story_inner(&manifest) } else { String::new() };
    let errors = res.get("errors").cloned().unwrap_or_else(|| json!([]));
    let warnings = res.get("warnings").cloned().unwrap_or_else(|| json!([]));
    let id_val = row.get("id").cloned().unwrap_or(Value::Null);

    let lint_json = serde_json::to_string(&json!({ "errors": errors.clone(), "warnings": warnings.clone() })).unwrap();
    db.execute(
        "UPDATE swarm_manifests SET lint_passed = ?1, lint_json = ?2 WHERE id = ?3",
        params![ if ok { 1i64 } else { 0i64 }, lint_json, id_val.as_i64() ],
    ).map_err(|e| e.to_string())?;

    let empty: Vec<Value> = Vec::new();
    let det: Vec<Value> = errors.as_array().unwrap_or(&empty).iter().map(|e| {
        json!({ "severity": "blocker", "code": e.get("code").cloned().unwrap_or(Value::Null), "problem": e.get("msg").cloned().unwrap_or(Value::Null) })
    }).collect();
    let issues_json = serde_json::to_string(&det).unwrap();
    let errors_len = errors.as_array().map(|a| a.len()).unwrap_or(0);
    let warnings_len = warnings.as_array().map(|a| a.len()).unwrap_or(0);
    let findings = if ok {
        let warn_suffix = if warnings_len != 0 { format!(" ({} warning(s))", warnings_len) } else { String::new() };
        format!("Manifest lint GREEN: {} blocks, graph well-formed{}.", tmpl(res.get("blocks_total")), warn_suffix)
    } else {
        let msgs: Vec<String> = errors.as_array().unwrap_or(&empty).iter().map(|e| tmpl(e.get("msg"))).collect();
        format!("Manifest lint RED: {} structural error(s) \u{2014} TL re-designs the manifest. {}", errors_len, msgs.join("; "))
    };
    let reviewer = match nz(oget(input, "reviewer")) {
        Some(Value::String(s)) => s.clone(),
        Some(other) => tmpl(Some(other)),
        None => "driver:auto".to_string(),
    };
    let cs = oget(input, "contract_slug").cloned().unwrap_or(Value::Null);
    db.execute(
        "INSERT INTO contract_qa_reviews (contract_slug, passed, verdict, findings_md, issues_json, reviewer, failure_target, review_mode) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![ cs.as_str().unwrap_or_default(), if ok { 1i64 } else { 0i64 }, if ok { "pass" } else { "fail" }, findings, issues_json, reviewer, "microtasks", "manifest-lint" ],
    ).map_err(|e| e.to_string())?;
    let review_id = db.last_insert_rowid();

    let mut out = Map::new();
    out.insert("contract_slug".into(), cs);
    out.insert("passed".into(), Value::Bool(ok));
    out.insert("verdict".into(), json!(if ok { "pass" } else { "fail" }));
    out.insert("review_mode".into(), json!("manifest-lint"));
    out.insert("review_id".into(), json!(review_id));
    out.insert("manifest_id".into(), id_val);
    out.insert("errors".into(), errors);
    out.insert("warnings".into(), warnings);
    out.insert("story_md".into(), json!(story));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> Value {
        json!({
            "version": 1,
            "goal": "serve config over http",
            "blocks": [
                { "id": "config", "file": "src/config.js", "role": "load + validate config",
                  "produces": [{ "name": "loadConfig", "type": "ConfigFn" }],
                  "consumes": [{ "name": "env", "type": "Env", "external": true }] },
                { "id": "server", "file": "src/server.js", "role": "http server",
                  "produces": [{ "name": "start", "type": "StartFn" }],
                  "consumes": [{ "name": "loadConfig", "type": "ConfigFn" }] }
            ],
            "bindings": [
                { "id": "b1", "from": { "block": "config", "port": "loadConfig" }, "to": { "block": "server", "port": "loadConfig" }, "type": "ConfigFn" }
            ]
        })
    }

    #[test]
    fn parse_and_lint() {
        assert_eq!(parseManifest(&json!({ "manifest": { "version": 1 } })), json!({ "version": 1 }));
        assert_eq!(parseManifest(&json!("{\"a\":1}")), json!({ "a": 1 }));
        assert_eq!(parseManifest(&json!("garbage")), Value::Null);
        let r = lintManifest(&valid());
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["errors"].as_array().unwrap().len(), 0);
        assert_eq!(r["blocks_total"], json!(2));
        assert!(r["warnings"].as_array().unwrap().iter().any(|w| w["code"] == json!("dead-output")));
        let bad = lintManifest(&json!({ "blocks": [] }));
        let codes: Vec<&str> = bad["errors"].as_array().unwrap().iter().map(|e| e["code"].as_str().unwrap()).collect();
        assert!(codes.contains(&"no-version") && codes.contains(&"no-goal") && codes.contains(&"no-blocks"));
    }

    #[test]
    fn topo_and_cycle_and_slice() {
        let m = valid();
        let order = topoOrder(&m["blocks"], &m["bindings"]);
        let arr = order.as_array().unwrap();
        let pos = |id: &str| arr.iter().position(|x| x == id).unwrap();
        assert!(pos("config") < pos("server"));

        let mut mc = valid();
        mc["blocks"][0]["consumes"].as_array_mut().unwrap().push(json!({ "name": "start", "type": "StartFn" }));
        mc["bindings"].as_array_mut().unwrap().push(json!({ "id": "b2", "from": { "block": "server", "port": "start" }, "to": { "block": "config", "port": "start" }, "type": "StartFn" }));
        assert_eq!(topoOrder(&mc["blocks"], &mc["bindings"]), Value::Null);
        assert!(lintManifest(&mc)["warnings"].as_array().unwrap().iter().any(|w| w["code"] == json!("cycle")));

        let slice = resolveBlockSlice(&valid(), &json!("server"));
        assert_eq!(slice["block"]["id"], json!("server"));
        assert_eq!(slice["consumes"][0]["source"], json!({ "block": "config", "file": "src/config.js", "port": "loadConfig" }));
        let cfg = resolveBlockSlice(&valid(), &json!("config"));
        assert_eq!(cfg["produces"][0]["consumedBy"][0]["block"], json!("server"));
    }

    #[test]
    fn render_and_default_paths() {
        let story = render_story_inner(&valid());
        assert!(story.contains("serve config over http"));
        assert!(story.contains("**config** `src/config.js`"));

        // compute-only default path
        let conn = Connection::open_in_memory().unwrap();
        let r = manifestLint(&json!({ "manifest": valid() }), &conn).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert!(r["story_md"].as_str().unwrap().contains("serve config over http"));

        // driver record path
        conn.execute_batch(
            "CREATE TABLE swarm_manifests (id INTEGER PRIMARY KEY, contract_slug TEXT, version INTEGER, manifest_json TEXT, superseded_by INTEGER, lint_passed INTEGER, lint_json TEXT);\
             CREATE TABLE contract_qa_reviews (id INTEGER PRIMARY KEY, contract_slug TEXT, passed INTEGER, verdict TEXT, findings_md TEXT, issues_json TEXT, reviewer TEXT, failure_target TEXT, review_mode TEXT);").unwrap();
        conn.execute(
            "INSERT INTO swarm_manifests (id, contract_slug, version, manifest_json) VALUES (5, 'C1', 1, ?1)",
            params![serde_json::to_string(&valid()).unwrap()],
        ).unwrap();
        let d = manifestLint(&json!({ "contract_slug": "C1" }), &conn).unwrap();
        assert_eq!(d["passed"], json!(true));
        assert_eq!(d["verdict"], json!("pass"));
        assert_eq!(d["manifest_id"], json!(5));
        assert_eq!(d["review_id"], json!(1));
        let passed: i64 = conn.query_row("SELECT lint_passed FROM swarm_manifests WHERE id=5", [], |r| r.get(0)).unwrap();
        assert_eq!(passed, 1);
    }
}
