//! dep-resolve.rs — twin of skills/codebase/dep-resolve.mjs (standard db lane; no db access). NAMES ARE
//! CONTRACT: `resolveSpecifier` (named, Set-arg → transitive) · `depResolve` (default). Raw import
//! specifiers → atlas Bindings (edges), dispatched per-ecosystem by the importing file's extension.
// QUALITY: node/ts + python/java/rust/c/c++/go/php/c# resolvers, faithful posix path normalize/join/dirname
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;

const TRY_EXTS: [&str; 11] = ["", ".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs", ".py", ".go"];
const TRY_INDEX: [&str; 5] = ["/index.ts", "/index.tsx", "/index.js", "/index.jsx", "/index.mjs"];

// ── JS coercion helpers (the playbook's canonical set) ────────────────────────────────
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// String(v ?? '')
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// String(v) (no ??)
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
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn opt_to_value(o: Option<String>) -> Value { match o { Some(s) => Value::String(s), None => Value::Null } }

// ── node:path posix, ported byte-for-byte (fixtures ASCII/BMP → bytes == UTF-16 code units) ──
fn is_sep(c: u8) -> bool { c == b'/' }

fn normalize_string(path: &[u8], allow_above_root: bool) -> String {
    let mut res: Vec<u8> = Vec::new();
    let mut last_segment_length: isize = 0;
    let mut last_slash: isize = -1;
    let mut dots: isize = 0;
    let mut ch: u8 = 0;
    let n = path.len() as isize;
    let mut i: isize = 0;
    while i <= n {
        if i < n {
            ch = path[i as usize];
        } else if is_sep(ch) {
            break;
        } else {
            ch = b'/';
        }
        if is_sep(ch) {
            if last_slash == i - 1 || dots == 1 {
                // NOOP: `//` collapses, `/./` drops
            } else if dots == 2 {
                if res.len() < 2 || last_segment_length != 2
                    || res[res.len() - 1] != b'.' || res[res.len() - 2] != b'.'
                {
                    if res.len() > 2 {
                        match res.iter().rposition(|&c| c == b'/') {
                            Some(lsi) => {
                                res.truncate(lsi);
                                let new_last = res.iter().rposition(|&c| c == b'/').map(|x| x as isize).unwrap_or(-1);
                                last_segment_length = res.len() as isize - 1 - new_last;
                            }
                            None => { res.clear(); last_segment_length = 0; }
                        }
                        last_slash = i;
                        dots = 0;
                        i += 1;
                        continue;
                    } else if !res.is_empty() {
                        res.clear();
                        last_segment_length = 0;
                        last_slash = i;
                        dots = 0;
                        i += 1;
                        continue;
                    }
                }
                if allow_above_root {
                    if !res.is_empty() { res.push(b'/'); res.push(b'.'); res.push(b'.'); }
                    else { res.push(b'.'); res.push(b'.'); }
                    last_segment_length = 2;
                }
            } else {
                let seg = &path[(last_slash + 1) as usize..i as usize];
                if !res.is_empty() { res.push(b'/'); res.extend_from_slice(seg); }
                else { res.extend_from_slice(seg); }
                last_segment_length = i - last_slash - 1;
            }
            last_slash = i;
            dots = 0;
        } else if ch == b'.' && dots != -1 {
            dots += 1;
        } else {
            dots = -1;
        }
        i += 1;
    }
    String::from_utf8(res).unwrap_or_default()
}

fn normalize(path: &str) -> String {
    let bytes = path.as_bytes();
    if bytes.is_empty() { return ".".into(); }
    let is_absolute = bytes[0] == b'/';
    let trailing_sep = *bytes.last().unwrap() == b'/';
    let mut p = normalize_string(bytes, !is_absolute);
    if p.is_empty() {
        if is_absolute { return "/".into(); }
        return if trailing_sep { "./".into() } else { ".".into() };
    }
    if trailing_sep { p.push('/'); }
    if is_absolute { format!("/{}", p) } else { p }
}

fn join(parts: &[&str]) -> String {
    if parts.is_empty() { return ".".into(); }
    let mut joined: Option<String> = None;
    for &arg in parts {
        if !arg.is_empty() {
            match &mut joined {
                None => joined = Some(arg.to_string()),
                Some(j) => { j.push('/'); j.push_str(arg); }
            }
        }
    }
    match joined { Some(j) => normalize(&j), None => ".".into() }
}

fn dirname(path: &str) -> String {
    let bytes = path.as_bytes();
    if bytes.is_empty() { return ".".into(); }
    let has_root = bytes[0] == b'/';
    let mut end: isize = -1;
    let mut matched_slash = true;
    let mut i: isize = bytes.len() as isize - 1;
    while i >= 1 {
        if bytes[i as usize] == b'/' {
            if !matched_slash { end = i; break; }
        } else {
            matched_slash = false;
        }
        i -= 1;
    }
    if end == -1 { return if has_root { "/".into() } else { ".".into() }; }
    if has_root && end == 1 { return "//".into(); }
    String::from_utf8(bytes[..end as usize].to_vec()).unwrap_or_default()
}

// .replace(/^\.\//, '') — strip a single leading "./"
fn strip_dot_slash(s: &str) -> String {
    match s.strip_prefix("./") { Some(rest) => rest.to_string(), None => s.to_string() }
}

// ── the in-scope file set: JS `new Set(files.map(String))` (dedup, insertion-ordered) ─────
struct FileSet { set: HashSet<String>, order: Vec<String> }
impl FileSet {
    fn has(&self, s: &str) -> bool { self.set.contains(s) }
}
fn build_file_set(files: &[Value]) -> FileSet {
    let mut set = HashSet::new();
    let mut order = Vec::new();
    for f in files {
        let s = js_string(f);
        if set.insert(s.clone()) { order.push(s); }
    }
    FileSet { set, order }
}

// exact match if present, else the shortest suffix match (ties keep first insertion), else null
fn find_by_suffix(path: &str, file_set: &FileSet) -> Option<String> {
    if path.is_empty() { return None; }
    if file_set.has(path) { return Some(path.to_string()); }
    let needle = format!("/{}", path);
    let mut best: Option<&String> = None;
    for f in &file_set.order {
        if f.ends_with(&needle) && best.map_or(true, |b| f.len() < b.len()) { best = Some(f); }
    }
    best.cloned()
}

/// The TypeScript sources a JavaScript-extension specifier can mean: `x.js` -> x.ts, x.tsx · `x.jsx` -> x.tsx · `x.mjs` -> x.mts · `x.cjs` -> x.cts. Empty for any other path. Pure - tested.
fn ts_sources_of(path: &str) -> Vec<String> {
    for (js, ts) in [(".js", [".ts", ".tsx"].as_slice()), (".jsx", [".tsx"].as_slice()), (".mjs", [".mts"].as_slice()), (".cjs", [".cts"].as_slice())] {
        if let Some(stem) = path.strip_suffix(js) { return ts.iter().map(|e| format!("{stem}{e}")).collect(); }
    }
    vec![]
}

// ── Node/TS resolver — also the default + fallback for unknown extensions (NAMES ARE CONTRACT) ──
pub fn resolveSpecifier(from_path: &str, specifier: &Value, file_set: &FileSet) -> Value {
    let spec = str_qq(Some(specifier));
    if !spec.starts_with('.') && !spec.starts_with('/') { return Value::Null; } // package / std-lib / external
    let dir = dirname(from_path);
    let base = strip_dot_slash(&normalize(&join(&[dir.as_str(), spec.as_str()])));
    if base.starts_with("..") { return Value::Null; } // outside the ingested root
    probe_base(&base, file_set).map_or(Value::Null, Value::String)
}

/// A file of the set that the extension-less / directory path `base` names: the file itself, the TypeScript source a `.js` specifier means, `base` + an extension, `base/index.*`.
fn probe_base(base: &str, file_set: &FileSet) -> Option<String> {
    if file_set.has(base) { return Some(base.to_string()); }
    // TypeScript's ESM style names the OUTPUT file: `import "./audit.js"` is `./audit.ts` on disk (TS 4.7+, NodeNext). Every such project showed "0 files depend on it / imported by nothing - an entry point or
    // dead" in X-ray (found 2026-10-01 baking the intel for an experiment on a NodeNext repo), because only `base + ext` was tried, never `stem + ts-ext`.
    for c in ts_sources_of(base) { if file_set.has(&c) { return Some(c); } }
    for ext in TRY_EXTS { let c = format!("{}{}", base, ext); if file_set.has(&c) { return Some(c); } }
    for idx in TRY_INDEX { let c = format!("{}{}", base, idx); if file_set.has(&c) { return Some(c); } }
    None
}

/// A bare specifier that names a package of THIS repo's workspaces (`config.jsWorkspaces`, read from the root `package.json` `workspaces` / `pnpm-workspace.yaml` by `readProjectConfig`): `shared/getComponentNameFromType` is `packages/shared/getComponentNameFromType.js` when `packages/shared/package.json` is named `shared`, and a bare `react-reconciler` is that package's `main` (else its `index`/`src/index`). 2026-10-01: the React repo's X-ray answered "who imports shared/getComponentNameFromType" with "nobody" — all ~17 importers use the bare form, which every monorepo resolves through its workspaces (Jest moduleNameMapper / Rollup / the package manager's links), and this resolver only knew `./relative`. The longest package name wins, on a `/` boundary (`react-dom` never claims `react-dom-bindings/x`).
fn resolve_workspace(specifier: &str, file_set: &FileSet, config: &Value) -> Option<String> {
    let pkgs = config.get("jsWorkspaces").and_then(Value::as_array)?;
    let mut best: Option<(&str, &str, Option<&str>)> = None;
    for p in pkgs {
        let (Some(name), Some(dir)) = (p.get("name").and_then(Value::as_str), p.get("dir").and_then(Value::as_str)) else { continue };
        let hit = specifier == name || specifier.strip_prefix(name).map_or(false, |rest| rest.starts_with('/'));
        if hit && best.map_or(true, |(n, _, _)| name.len() > n.len()) { best = Some((name, dir, p.get("main").and_then(Value::as_str))); }
    }
    let (name, dir, main) = best?;
    let sub = specifier[name.len()..].trim_start_matches('/');
    if !sub.is_empty() { return probe_base(&strip_dot_slash(&normalize(&join(&[dir, sub]))), file_set); }
    // the package itself: its `main`, else the conventional entry points
    if let Some(m) = main {
        if let Some(f) = probe_base(&strip_dot_slash(&normalize(&join(&[dir, m]))), file_set) { return Some(f); }
    }
    for entry in ["index", "src/index"] {
        if let Some(f) = probe_base(&join(&[dir, entry]), file_set) { return Some(f); }
    }
    None
}

// PYTHON (.py): dotted module or leading-dot relative import
fn resolve_python(from_path: &str, specifier: &Value, file_set: &FileSet) -> Value {
    let spec = str_qq(Some(specifier));
    if spec.is_empty() { return Value::Null; }
    let dir = dirname(from_path);
    let try_module = |base_path: &str| -> Option<String> {
        if base_path.starts_with("..") { return None; }
        let file = format!("{}.py", base_path);
        if file_set.has(&file) { return Some(file); }
        let pkg = format!("{}/__init__.py", base_path);
        if file_set.has(&pkg) { return Some(pkg); }
        None
    };
    if spec.starts_with('.') {
        let dots = spec.bytes().take_while(|&b| b == b'.').count();
        let rest = &spec[dots..];
        let mut basedir = dir.clone();
        for _ in 0..dots.saturating_sub(1) { basedir = dirname(&basedir); }
        let sub = if rest.is_empty() { String::new() } else { rest.replace('.', "/") };
        let joined = if !sub.is_empty() { join(&[basedir.as_str(), sub.as_str()]) } else { basedir.clone() };
        let base_path = strip_dot_slash(&normalize(&joined));
        return opt_to_value(try_module(&base_path));
    }
    let as_path = spec.replace('.', "/");
    if let Some(d) = try_module(&as_path) { return json!(d); }
    opt_to_value(
        find_by_suffix(&format!("{}.py", as_path), file_set)
            .or_else(|| find_by_suffix(&format!("{}/__init__.py", as_path), file_set)),
    )
}

// JAVA (.java): dotted FQN → ends-with under a source root; wildcard → package dir (skip)
fn resolve_java(specifier: &Value, file_set: &FileSet) -> Value {
    let raw = str_qq(Some(specifier));
    let mut spec = raw.trim().to_string();
    if spec.is_empty() { return Value::Null; }
    if spec.starts_with("static ") { spec = spec["static ".len()..].trim().to_string(); }
    if spec.ends_with(".*") || spec.ends_with('*') { return Value::Null; }
    let as_path = format!("{}.java", spec.replace('.', "/"));
    opt_to_value(find_by_suffix(&as_path, file_set))
}

// RUST (.rs): crate::/super::/self:: paths + bare `mod foo;`
fn rust_try_mod(base_path: &str, file_set: &FileSet) -> Option<String> {
    if base_path.is_empty() || base_path.starts_with("..") { return None; }
    let norm = strip_dot_slash(&normalize(base_path));
    if norm.starts_with("..") { return None; }
    let file = format!("{}.rs", norm);
    if file_set.has(&file) { return Some(file); }
    let md = format!("{}/mod.rs", norm);
    if file_set.has(&md) { return Some(md); }
    None
}
fn rust_try_path_segments(base_dir: &str, segs: &[String], file_set: &FileSet) -> Option<String> {
    let clean: Vec<&str> = segs.iter().map(|s| s.as_str()).filter(|s| !s.is_empty() && *s != "*").collect();
    if clean.is_empty() { return rust_try_mod(base_dir, file_set); }
    if clean.len() > 1 {
        let mut parts: Vec<&str> = Vec::with_capacity(clean.len());
        parts.push(base_dir);
        parts.extend_from_slice(&clean[..clean.len() - 1]);
        if let Some(hit) = rust_try_mod(&join(&parts), file_set) { return Some(hit); }
    }
    let mut parts: Vec<&str> = Vec::with_capacity(clean.len() + 1);
    parts.push(base_dir);
    parts.extend_from_slice(&clean);
    rust_try_mod(&join(&parts), file_set)
}
/// EVERY crate root in the file set, not just the first. A Cargo workspace has one per member, and
/// resolving all of them against a single global root is what left multi-crate atlases with zero
/// cross-crate edges: `crate::` bound to a foreign crate, and `use other_crate::…` never resolved.
fn find_crate_srcs(file_set: &FileSet) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in &file_set.order {
        if f.ends_with("src/lib.rs") || f.ends_with("src/main.rs") || f.as_str() == "lib.rs" || f.as_str() == "main.rs" {
            let d = dirname(f);
            if !out.contains(&d) { out.push(d); }
        }
    }
    out
}

/// The crate root that OWNS `from_path` — the longest root that prefixes it. `crate::` is relative
/// to the file's own crate, which the first-root-wins reading gets wrong as soon as there are two.
fn owning_crate_src(from_path: &str, roots: &[String]) -> Option<String> {
    let mut best: Option<&String> = None;
    for r in roots {
        if from_path.starts_with(&format!("{r}/")) && best.map(|b| r.len() > b.len()).unwrap_or(true) {
            best = Some(r);
        }
    }
    best.cloned()
}

/// Heads that name a Rust built-in, never a workspace crate. Without this the tail-matching below
/// could turn `use std::fs::File` into an edge to some crate's own `src/fs.rs`.
fn is_builtin_crate_head(head: &str) -> bool {
    matches!(head, "std" | "core" | "alloc" | "proc_macro" | "test")
}
fn resolve_rust(from_path: &str, specifier: &Value, file_set: &FileSet) -> Value {
    let raw_owned = str_qq(Some(specifier));
    let raw = raw_owned.trim();
    if raw.is_empty() { return Value::Null; }
    let dir = dirname(from_path);
    if !raw.contains("::") {
        return opt_to_value(rust_try_mod(&join(&[dir.as_str(), raw]), file_set));
    }
    let segs: Vec<String> = raw.split("::").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    let head = segs.first().map(|s| s.as_str()).unwrap_or("");
    let roots = find_crate_srcs(file_set);
    let owner = owning_crate_src(from_path, &roots).or_else(|| roots.first().cloned());
    if head == "crate" {
        return match &owner {
            None => Value::Null,
            Some(cs) => opt_to_value(rust_try_path_segments(cs, &segs[1..], file_set)),
        };
    }
    if head == "super" {
        return opt_to_value(rust_try_path_segments(&dirname(&dir), &segs[1..], file_set));
    }
    if head == "self" {
        return opt_to_value(rust_try_path_segments(&dir, &segs[1..], file_set));
    }
    // A local path first — preserves the single-crate reading, where the head is a module directory.
    if let Some(cs) = &owner {
        if let Some(hit) = rust_try_path_segments(cs, &segs, file_set) { return json!(hit); }
    }
    // SIBLING CRATE (`use browser_shell::document::…`). The head is a crate NAME, and a name need
    // not match its directory — Cargo's `[lib] name` renames it freely — so with only a file list
    // here (no Cargo.toml) the head cannot be mapped to a root directly. Instead drop it and try the
    // tail under every OTHER crate root, accepting a hit only when exactly one crate answers: a
    // unique hit is a real edge, an ambiguous one is dropped rather than guessed, because a wrong
    // edge silently corrupts every graph walk built on it. Gated on there BEING more than one crate,
    // so single-crate projects keep their old behaviour exactly.
    if roots.len() > 1 && !is_builtin_crate_head(head) {
        let mut hits: Vec<String> = Vec::new();
        for r in &roots {
            if Some(r) == owner.as_ref() { continue; }
            if let Some(h) = rust_try_path_segments(r, &segs[1..], file_set) {
                if !hits.contains(&h) { hits.push(h); }
            }
        }
        if hits.len() == 1 { return json!(hits.remove(0)); }
    }
    Value::Null
}

// C / C++: #include target relative to the including dir, then a suffix fallback (include root)
fn resolve_c(from_path: &str, specifier: &Value, file_set: &FileSet) -> Value {
    let raw = str_qq(Some(specifier));
    let spec = raw.trim();
    if spec.is_empty() { return Value::Null; }
    if spec.starts_with('<') { return Value::Null; } // system header (angle brackets)
    let dir = dirname(from_path);
    let rel = strip_dot_slash(&normalize(&join(&[dir.as_str(), spec])));
    if !rel.starts_with("..") && file_set.has(&rel) { return json!(rel); }
    opt_to_value(find_by_suffix(spec, file_set))
}

// GO (.go): package import → every .go file in the package dir (array); relative → node/ts; else null
fn resolve_go(from_path: &str, specifier: &Value, file_set: &FileSet, config: &Value) -> Value {
    let raw = str_qq(Some(specifier));
    let spec = raw.trim();
    if spec.is_empty() { return Value::Null; }
    if spec.starts_with('.') || spec.starts_with('/') {
        return resolveSpecifier(from_path, &json!(spec), file_set);
    }
    let module_root = match config.get("goModule") {
        Some(v) if truthy(Some(v)) => js_string(v),
        _ => return Value::Null,
    };
    if spec != module_root && !spec.starts_with(&format!("{}/", module_root)) { return Value::Null; }
    let sub = spec[module_root.len()..].trim_start_matches('/');
    if sub.is_empty() { return Value::Null; }
    let suffix = format!("/{}", sub);
    let mut out: Vec<Value> = Vec::new();
    for f in &file_set.order {
        if !f.ends_with(".go") { continue; }
        let d = dirname(f);
        if d.as_str() == sub || d.ends_with(&suffix) { out.push(json!(f)); }
    }
    Value::Array(out)
}

// PHP (.php): psr-4 `use` via autoload, else best-effort suffix; require/include path → relative (as C)
fn resolve_php(from_path: &str, specifier: &Value, file_set: &FileSet, config: &Value) -> Value {
    let raw_owned = str_qq(Some(specifier));
    let raw = raw_owned.trim();
    if raw.is_empty() { return Value::Null; }
    if raw.starts_with('.') || raw.starts_with('/') || raw.to_ascii_lowercase().ends_with(".php") {
        return resolve_c(from_path, &json!(raw), file_set);
    }
    let fqn = raw.trim_start_matches('\\');
    let empty: Vec<Value> = Vec::new();
    let autoload = config.get("phpAutoload").and_then(|v| v.as_array()).unwrap_or(&empty);
    for entry in autoload {
        let prefix = entry.get("prefix");
        if !truthy(prefix) { continue; }
        let ns_prefix = js_string(prefix.unwrap()).replace('\\', "/");
        let ns_fqn = fqn.replace('\\', "/");
        if !ns_prefix.is_empty() && ns_fqn.starts_with(&ns_prefix) {
            let rest = &ns_fqn[ns_prefix.len()..];
            let base_dir_full = str_qq(entry.get("dir")).replace('\\', "/");
            let base_dir = base_dir_full.trim_end_matches('/');
            let path = format!("{}.php", join(&[base_dir, rest]));
            return opt_to_value(find_by_suffix(&path, file_set));
        }
    }
    opt_to_value(find_by_suffix(&format!("{}.php", fqn.replace('\\', "/")), file_set))
}

// C# (.cs): `using` → namespace→files index (array of declaring files); std/external → null
fn resolve_csharp(specifier: &Value, config: &Value) -> Value {
    let mut spec = str_qq(Some(specifier)).trim().to_string();
    if spec.is_empty() { return Value::Null; }
    let mut is_static = false;
    if spec.starts_with("static ") { is_static = true; spec = spec["static ".len()..].trim().to_string(); }
    if let Some(eq) = spec.find('=') { spec = spec[eq + 1..].trim().to_string(); }
    spec = spec.trim_end_matches(';').trim().to_string();
    if is_static { if let Some(i) = spec.rfind('.') { spec = spec[..i].to_string(); } }
    match config.get("nsIndex") {
        Some(ni) if ni.is_object() || ni.is_array() => match ni.as_object() {
            Some(obj) => nz(obj.get(spec.as_str())).cloned().unwrap_or(Value::Null),
            None => Value::Null,
        },
        _ => Value::Null,
    }
}

// Dispatch by the importing file's lowercased extension; unknown → the Node/TS resolver.
fn resolver_dispatch(from: &str, specifier: &Value, file_set: &FileSet, config: &Value) -> Value {
    let ext = match from.rfind('.') { Some(i) => from[i..].to_lowercase(), None => String::new() };
    match ext.as_str() {
        ".py" => resolve_python(from, specifier, file_set),
        ".java" => resolve_java(specifier, file_set),
        ".rs" => resolve_rust(from, specifier, file_set),
        ".c" | ".h" | ".cpp" | ".cc" | ".cxx" | ".hpp" | ".hh" => resolve_c(from, specifier, file_set),
        ".go" => resolve_go(from, specifier, file_set, config),
        ".php" => resolve_php(from, specifier, file_set, config),
        ".cs" => resolve_csharp(specifier, config),
        _ => {
            let r = resolveSpecifier(from, specifier, file_set);
            if !r.is_null() { return r; }
            let spec = str_qq(Some(specifier));
            if spec.starts_with('.') || spec.starts_with('/') { return r; }
            resolve_workspace(&spec, file_set, config).map_or(Value::Null, Value::String)
        }
    }
}

pub fn depResolve(input: &Value, _db: &Connection) -> Result<Value, String> {
    let empty_arr: Vec<Value> = Vec::new();
    let files = input.get("files").and_then(|v| v.as_array()).unwrap_or(&empty_arr);
    let file_set = build_file_set(files);
    let imports = input.get("imports").and_then(|v| v.as_array()).unwrap_or(&empty_arr);
    let default_config = json!({});
    let config = match input.get("config") {
        Some(v) if v.is_object() || v.is_array() => v,
        _ => &default_config,
    };
    let mut edges: Vec<Value> = Vec::new();
    for imp in imports {
        let from_path_v = imp.get("from_path");
        if !truthy(from_path_v) { continue; }
        let specifier_v = imp.get("specifier");
        if specifier_v.map_or(true, |v| v.is_null()) { continue; }
        let from = js_string(from_path_v.unwrap());
        let to = resolver_dispatch(&from, specifier_v.unwrap(), &file_set, config);
        // resolver may return a single path OR an array (Go/C#). Normalize to a list.
        let targets: Vec<Value> = match to {
            Value::Array(a) => a,
            Value::Null => Vec::new(),
            other => vec![other],
        };
        // `kind` rides through untouched: the same resolver answers `import('./x')` and
        // `export … from './x'` — what differs is what the edge MEANS, which readers decide.
        let kind = imp.get("kind").and_then(Value::as_str).unwrap_or("value").to_string();
        for t in targets {
            if let Some(ts) = t.as_str() {
                if !ts.is_empty() && ts != from.as_str() && file_set.has(ts) {
                    edges.push(json!({ "from_node": from, "to_node": ts, "type": "imports", "kind": kind }));
                }
            }
        }
    }
    Ok(json!({ "edges": edges, "total": imports.len() as i64, "resolved": edges.len() as i64 }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fs(v: &[&str]) -> FileSet { build_file_set(&v.iter().map(|s| json!(s)).collect::<Vec<_>>()) }

    #[test]
    fn import_kind_rides_through_resolution_untouched() {
        // the same resolver answers a type import, a lazy import and a barrel; only the edge's
        // meaning differs — and that is carried in `kind` for the readers to decide.
        let db = Connection::open_in_memory().unwrap();
        let r = depResolve(&json!({
            "files": ["a.ts", "b.ts", "c.ts", "d.ts"],
            "imports": [
                { "from_path": "a.ts", "specifier": "./b", "kind": "type" },
                { "from_path": "a.ts", "specifier": "./c", "kind": "dynamic" },
                { "from_path": "a.ts", "specifier": "./d", "kind": "reexport" },
                { "from_path": "a.ts", "specifier": "./b" },
            ]
        }), &db).unwrap();
        let kinds: Vec<(&str, &str)> = r["edges"].as_array().unwrap().iter().map(|e| (e["to_node"].as_str().unwrap(), e["kind"].as_str().unwrap())).collect();
        assert_eq!(kinds, vec![("b.ts", "type"), ("c.ts", "dynamic"), ("d.ts", "reexport"), ("b.ts", "value")], "absent kind defaults to value");
    }
    fn call(input: Value) -> Value { depResolve(&input, &Connection::open_in_memory().unwrap()).unwrap() }

    #[test]
    fn resolve_specifier_node_ts() {
        let f = fs(&["src/Button.tsx", "src/App.tsx", "src/util/index.ts", "a/b.mjs"]);
        assert_eq!(resolveSpecifier("src/App.tsx", &json!("./Button"), &f), json!("src/Button.tsx"));
        assert_eq!(resolveSpecifier("src/App.tsx", &json!("./util"), &f), json!("src/util/index.ts"));
        assert_eq!(resolveSpecifier("a/x.mjs", &json!("./b.mjs"), &f), json!("a/b.mjs"));
        assert_eq!(resolveSpecifier("src/App.tsx", &json!("react"), &f), Value::Null);
        assert_eq!(resolveSpecifier("src/App.tsx", &json!("./nope"), &f), Value::Null);
        let f2 = fs(&["src/components/Button.tsx", "src/hooks/useX.ts"]);
        assert_eq!(resolveSpecifier("src/components/Button.tsx", &json!("../hooks/useX"), &f2), json!("src/hooks/useX.ts"));
        assert_eq!(resolveSpecifier("src/App.tsx", &json!("../../outside/x"), &f2), Value::Null);
    }

    /// A NodeNext project writes `./audit.js` for `audit.ts` (found 2026-10-01: X-ray showed every file of such a repo as imported by nothing).
    #[test]
    fn a_js_specifier_resolves_to_the_typescript_source_it_names() {
        let f = fs(&["src/ledger/rows.ts", "src/audit.ts", "src/ui/Button.tsx", "src/lib/x.mts", "src/lib/y.cts", "src/real.js", "src/real.ts"]);
        assert_eq!(resolveSpecifier("src/ledger/standings.ts", &json!("./rows.js"), &f), json!("src/ledger/rows.ts"));
        assert_eq!(resolveSpecifier("src/ledger/rows.ts", &json!("../audit.js"), &f), json!("src/audit.ts"));
        assert_eq!(resolveSpecifier("src/App.tsx", &json!("./ui/Button.js"), &f), json!("src/ui/Button.tsx"));
        assert_eq!(resolveSpecifier("src/App.tsx", &json!("./ui/Button.jsx"), &f), json!("src/ui/Button.tsx"));
        assert_eq!(resolveSpecifier("src/App.tsx", &json!("./lib/x.mjs"), &f), json!("src/lib/x.mts"));
        assert_eq!(resolveSpecifier("src/App.tsx", &json!("./lib/y.cjs"), &f), json!("src/lib/y.cts"));
        assert_eq!(resolveSpecifier("src/App.tsx", &json!("./real.js"), &f), json!("src/real.js"), "a real .js file wins over a .ts of the same stem");
        assert_eq!(resolveSpecifier("src/App.tsx", &json!("./missing.js"), &f), Value::Null);
        assert!(ts_sources_of("a/b.py").is_empty() && ts_sources_of("a/b").is_empty());
    }

    /// 2026-10-01: React's `import x from 'shared/getComponentNameFromType'` resolved to nothing, so "who imports it" said nobody.
    #[test]
    fn a_bare_specifier_naming_a_workspace_package_resolves_into_it() {
        let config = json!({ "jsWorkspaces": [
            { "name": "shared", "dir": "packages/shared", "main": null },
            { "name": "react-dom", "dir": "packages/react-dom", "main": "index.js" },
            { "name": "react-dom-bindings", "dir": "packages/react-dom-bindings", "main": null },
            { "name": "@org/ui", "dir": "libs/ui", "main": "dist/ui.js" } ] });
        let files = ["packages/shared/getComponentNameFromType.js", "packages/shared/index.js", "packages/react-dom/index.js", "packages/react-dom/src/client/X.js",
            "packages/react-dom-bindings/src/index.js", "libs/ui/src/index.ts", "libs/ui/src/Button.tsx", "app/main.js"];
        let r = depResolve(&json!({ "files": files, "config": config, "imports": [
            { "from_path": "app/main.js", "specifier": "shared/getComponentNameFromType" },
            { "from_path": "app/main.js", "specifier": "shared" },
            { "from_path": "app/main.js", "specifier": "react-dom" },
            { "from_path": "app/main.js", "specifier": "react-dom/src/client/X" },
            { "from_path": "app/main.js", "specifier": "react-dom-bindings" },
            { "from_path": "app/main.js", "specifier": "@org/ui" },
            { "from_path": "app/main.js", "specifier": "@org/ui/src/Button" },
            { "from_path": "app/main.js", "specifier": "react" },
            { "from_path": "app/main.js", "specifier": "shared-not-a-package/x" } ] }), &Connection::open_in_memory().unwrap()).unwrap();
        let to: Vec<String> = r["edges"].as_array().unwrap().iter().map(|e| e["to_node"].as_str().unwrap().to_string()).collect();
        assert_eq!(to, vec![
            "packages/shared/getComponentNameFromType.js", "packages/shared/index.js", "packages/react-dom/index.js", "packages/react-dom/src/client/X.js",
            // `react-dom-bindings` has no `main`: its `src/index` is the entry, and `react-dom` did not claim it
            "packages/react-dom-bindings/src/index.js",
            // `main` points at a build output that is not in the set: fall back to the conventional entry
            "libs/ui/src/index.ts", "libs/ui/src/Button.tsx"]);
        assert_eq!(r["resolved"], json!(7));
        assert_eq!(r["total"], json!(9), "`react` and an unknown prefix stay external");
    }

    #[test]
    fn workspace_names_do_not_apply_to_other_languages_or_relative_paths() {
        let config = json!({ "jsWorkspaces": [{ "name": "shared", "dir": "packages/shared", "main": null }] });
        let r = depResolve(&json!({ "files": ["packages/shared/x.js", "a.py"], "config": config, "imports": [
            { "from_path": "a.py", "specifier": "shared.x" }, { "from_path": "a.js", "specifier": "./shared/x" } ] }), &Connection::open_in_memory().unwrap()).unwrap();
        assert_eq!(r["resolved"], json!(0));
    }

    #[test]
    fn dep_resolve_node_ts_edges() {
        let r = call(json!({ "files": ["a.mjs", "b.mjs"], "imports": [
            { "from_path": "a.mjs", "specifier": "./b.mjs" },
            { "from_path": "a.mjs", "specifier": "lodash" } ] }));
        assert_eq!(r["total"], json!(2));
        assert_eq!(r["resolved"], json!(1));
        assert_eq!(r["edges"], json!([{ "from_node": "a.mjs", "to_node": "b.mjs", "type": "imports", "kind": "value" }]));
    }

    #[test]
    fn language_resolvers() {
        // python: relative, absolute dotted, stdlib
        let py = call(json!({ "files": ["pkg/__init__.py", "pkg/a.py", "pkg/b.py", "pkg/c.py"], "imports": [
            { "from_path": "pkg/a.py", "specifier": ".b" },
            { "from_path": "pkg/a.py", "specifier": "pkg.c" },
            { "from_path": "pkg/a.py", "specifier": "os" } ] }));
        assert_eq!(py["resolved"], json!(2));
        // java: FQN suffix, jdk none, wildcard skip, static member no-edge
        let jv = call(json!({ "files": ["src/main/java/app/Main.java", "src/main/java/app/util/Helper.java"], "imports": [
            { "from_path": "src/main/java/app/Main.java", "specifier": "app.util.Helper" },
            { "from_path": "src/main/java/app/Main.java", "specifier": "java.util.List" },
            { "from_path": "src/main/java/app/Main.java", "specifier": "app.util.*" },
            { "from_path": "src/main/java/app/Main.java", "specifier": "static app.Const.VALUE" } ] }));
        assert_eq!(jv["resolved"], json!(1));
        // rust: super/self/crate mod.rs + external none
        let rs = call(json!({ "files": ["src/lib.rs", "src/shared.rs", "src/a/mod.rs", "src/a/b.rs", "src/a/helper.rs"], "imports": [
            { "from_path": "src/a/b.rs", "specifier": "super::shared::run" },
            { "from_path": "src/a/b.rs", "specifier": "self::helper" },
            { "from_path": "src/a/b.rs", "specifier": "crate::a" },
            { "from_path": "src/a/b.rs", "specifier": "std::collections::HashMap" } ] }));
        assert_eq!(rs["edges"], json!([
            { "from_node": "src/a/b.rs", "to_node": "src/shared.rs", "type": "imports", "kind": "value" },
            { "from_node": "src/a/b.rs", "to_node": "src/a/helper.rs", "type": "imports", "kind": "value" },
            { "from_node": "src/a/b.rs", "to_node": "src/a/mod.rs", "type": "imports", "kind": "value" } ]));
        // go: package import → two edges; stdlib → none
        let go = call(json!({ "files": ["app/main.go", "app/svc/a.go", "app/svc/b.go"], "imports": [
            { "from_path": "app/main.go", "specifier": "github.com/me/app/svc" },
            { "from_path": "app/main.go", "specifier": "fmt" } ], "config": { "goModule": "github.com/me/app" } }));
        assert_eq!(go["resolved"], json!(2));
        // php: psr-4 use + relative require
        let php = call(json!({ "files": ["src/Main.php", "src/Util/Helper.php", "src/x.php"], "imports": [
            { "from_path": "src/Main.php", "specifier": "App\\Util\\Helper" },
            { "from_path": "src/Main.php", "specifier": "./x.php" } ], "config": { "phpAutoload": [{ "prefix": "App\\", "dir": "src/" }] } }));
        assert_eq!(php["resolved"], json!(2));
        // c#: using → namespace index (multi), static reduces, bcl none
        let cs = call(json!({ "files": ["src/Program.cs", "src/Util/Helper.cs", "src/Util/More.cs"], "imports": [
            { "from_path": "src/Program.cs", "specifier": "System" },
            { "from_path": "src/Program.cs", "specifier": "App.Util" },
            { "from_path": "src/Program.cs", "specifier": "static App.Util.Math" } ],
            "config": { "nsIndex": { "App.Util": ["src/Util/Helper.cs", "src/Util/More.cs"] } } }));
        assert_eq!(cs["resolved"], json!(4));
        // robustness: weird specifiers never panic, unknown(.go) → node fallback
        let rob = call(json!({ "files": ["a.go", "b.go"], "imports": [
            { "from_path": "x.py", "specifier": "...." },
            { "from_path": "y.rs", "specifier": "::" },
            { "from_path": "z.java", "specifier": "" },
            { "from_path": "a.go", "specifier": "./b.go" } ] }));
        assert_eq!(rob["edges"], json!([{ "from_node": "a.go", "to_node": "b.go", "type": "imports", "kind": "value" }]));
    }

    #[test]
    fn rust_multi_crate_workspace_resolves_across_crates() {
        // The bookOS shape that produced an atlas with ZERO cross-crate edges: a Cargo workspace
        // whose core crate is renamed by `[lib] name`, so the frontend says `use browser_shell::…`
        // while the directory is crates/core. Resolution has only the file list, so it matches on
        // the tail — and must still refuse to invent edges for std or for an ambiguous name.
        let files = [
            "crates/core/src/lib.rs", "crates/core/src/document.rs", "crates/core/src/memory.rs",
            "crates/core/src/shared.rs",
            "crates/frontend-webview/src/main.rs", "crates/frontend-webview/src/p2p.rs",
            "crates/tui/src/main.rs", "crates/tui/src/shared.rs",
        ];
        let r = call(json!({ "files": files, "imports": [
            { "from_path": "crates/frontend-webview/src/main.rs", "specifier": "browser_shell::document::Document" },
            { "from_path": "crates/frontend-webview/src/main.rs", "specifier": "browser_shell::memory::Memory" },
            { "from_path": "crates/frontend-webview/src/main.rs", "specifier": "crate::p2p" },
            { "from_path": "crates/frontend-webview/src/main.rs", "specifier": "std::collections::HashMap" },
            { "from_path": "crates/frontend-webview/src/main.rs", "specifier": "whatever::shared" } ] }));
        assert_eq!(r["edges"], json!([
            // the crate NAME does not match its directory — resolved by tail, uniquely
            { "from_node": "crates/frontend-webview/src/main.rs", "to_node": "crates/core/src/document.rs", "type": "imports", "kind": "value" },
            { "from_node": "crates/frontend-webview/src/main.rs", "to_node": "crates/core/src/memory.rs", "type": "imports", "kind": "value" },
            // `crate::` binds to the file's OWN crate, not the first one in the workspace
            { "from_node": "crates/frontend-webview/src/main.rs", "to_node": "crates/frontend-webview/src/p2p.rs", "type": "imports", "kind": "value" } ]));
        // std never forges an edge, and `shared` exists in BOTH core and tui — ambiguous, so dropped
        // rather than guessed: a wrong edge corrupts every graph walk built on top of it.
        assert_eq!(r["resolved"], json!(3));
        assert_eq!(r["total"], json!(5));
    }

    #[test]
    fn posix_matches_node() {
        assert_eq!(dirname("src/App.tsx"), "src");
        assert_eq!(dirname("a"), ".");
        assert_eq!(normalize("././b.mjs"), "b.mjs");
        assert_eq!(normalize("..util"), "..util");
        assert_eq!(join(&["x", "/x/y2.ts"]), "x/x/y2.ts");
        assert_eq!(join(&[".", "config"]), "config");
    }
}
