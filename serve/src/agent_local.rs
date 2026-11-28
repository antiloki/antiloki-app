//! THE LOCAL WORKER'S OWN TOOL LOOP (RFC 0001 §8.1 E1). The seats Heimdall drives are CLIs that bring their own agent loop; a local model on Ollama brings none, so this
//! is the smallest one that can do a FILL: "fill the stubs in `src/x.ts`; the tests in `x.test.ts` must pass".
//!
//! Four tools, no shell: `list_files`, `read_file`, `write_file`, `finish`. A write is allowed only inside the box (the working folder, through no `..` and no
//! symlink that leaves it) and only to a path the SCOPE names: the same wall the other seats meet in `pane-scope-sentry`, enforced here before the file is touched,
//! because a model that cannot be trusted with a prompt cannot be trusted with a wall that is a sentence. A refused write is told to the model (so it can choose
//! another path) and kept in the outcome (so the report says it tried).
//!
//! The whole loop holds the one local slot (`local::slot`): a fill never runs beside another fill. Every call states its context size (`Config::num_ctx`, never a default).
use crate::local::{self, Config, Usage};
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

pub struct Fill {
    pub prompt: String,
    /// The box: the folder the worker may read in and write under.
    pub cwd: PathBuf,
    /// Globs, relative to `cwd`, that a write may match (`src/features/x.ts`, `src/**/*.test.ts`). Empty means no write is allowed at all.
    pub scope: Vec<String>,
    pub max_turns: u32,
    /// How long to wait for the one local slot before reporting the seat busy.
    pub slot_wait: Duration,
    /// A fill that has run this long stops asking (the turn in flight finishes): a model that keeps going is a result, not a hang.
    pub max_seconds: u64,
}

impl Fill { pub fn new(prompt: &str, cwd: &Path, scope: &[&str]) -> Fill { Fill { prompt: prompt.into(), cwd: cwd.into(), scope: scope.iter().map(|s| s.to_string()).collect(), max_turns: 12, slot_wait: Duration::from_secs(600), max_seconds: 900 } } }

#[derive(Debug, Default, Clone, PartialEq)]
pub struct FillOutcome {
    /// Files the worker wrote (relative paths), in order, once each.
    pub changed: Vec<String>,
    /// Writes the scope or the box refused: `path: reason`.
    pub refused: Vec<String>,
    pub turns: u32,
    pub usage: Usage,
    /// What the worker said it did (from `finish`, else its last words).
    pub summary: String,
    /// True when the worker called `finish`; false when it stopped talking or ran out of turns.
    pub finished: bool,
    /// The model's digest as the server reported it, for the row that records the run.
    pub digest: String,
}

/// `**` crosses folders, `*` does not, `?` is one character that is not `/`. Anchored.
pub fn glob_to_regex(glob: &str) -> regex::Regex {
    let mut re = String::from("^");
    let cs: Vec<char> = glob.trim_start_matches("./").chars().collect();
    let mut i = 0;
    while i < cs.len() {
        match cs[i] {
            '*' if cs.get(i + 1) == Some(&'*') => { re.push_str(".*"); i += 1; if cs.get(i + 1) == Some(&'/') { i += 1; re.push_str("(?:/)?"); } }
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    re.push('$');
    regex::Regex::new(&re).unwrap_or_else(|_| regex::Regex::new("^$").unwrap())
}

pub fn scope_allows(scope: &[String], rel: &str) -> bool { let rel = rel.trim_start_matches("./"); scope.iter().any(|g| glob_to_regex(g).is_match(rel)) }

/// A path the model named, made safe: relative, no `..`, and (after following any symlink in its folders) still under the box. The file itself need not exist yet.
pub fn resolve_inside(cwd: &Path, rel: &str) -> Result<PathBuf, String> {
    let p = Path::new(rel.trim());
    if rel.trim().is_empty() { return Err("no path given".into()); }
    if p.is_absolute() { return Err("an absolute path is outside the box".into()); }
    if p.components().any(|c| matches!(c, Component::ParentDir | Component::Prefix(_) | Component::RootDir)) { return Err("a path with `..` is outside the box".into()); }
    let root = cwd.canonicalize().map_err(|e| format!("the box is not there: {e}"))?;
    // the deepest folder that exists must still be under the box once symlinks are followed
    let full = root.join(p);
    let mut probe = full.as_path();
    while !probe.exists() { match probe.parent() { Some(x) => probe = x, None => break } }
    let real = probe.canonicalize().map_err(|e| e.to_string())?;
    if !real.starts_with(&root) { return Err("that path leaves the box through a link".into()); }
    Ok(full)
}

fn tools() -> Value {
    json!([
        {"type": "function", "function": {"name": "list_files", "description": "List the files in a folder of the project (relative path, '.' for the top).", "parameters": {"type": "object", "properties": {"dir": {"type": "string"}}, "required": ["dir"]}}},
        {"type": "function", "function": {"name": "read_file", "description": "Read a file of the project.", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}}},
        {"type": "function", "function": {"name": "write_file", "description": "Write the whole content of a file. Only the files you were told you may change are accepted.", "parameters": {"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}, "required": ["path", "content"]}}},
        {"type": "function", "function": {"name": "finish", "description": "Say you are done, in one or two sentences.", "parameters": {"type": "object", "properties": {"summary": {"type": "string"}}, "required": ["summary"]}}}
    ])
}

fn system_prompt(scope: &[String]) -> String {
    format!("You are a careful programmer working in a project folder, with four tools: list_files, read_file, write_file, finish.\nYou may change only these files: {}.\nRead what you need, write the complete new content of each file you change, then call finish with a short summary. Do not ask questions; do the work.\nRespond ONLY with tool calls. Never write explanations, plans or reasoning in your reply (small models run out of room doing it): your first reply must be a read_file call.",
        if scope.is_empty() { "none".to_string() } else { scope.join(", ") })
}

fn arguments_of(call: &Value) -> Value {
    match &call["function"]["arguments"] { Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null), v => v.clone() }
}

fn list(cwd: &Path, dir: &str) -> String {
    let Ok(p) = resolve_inside(cwd, if dir.trim().is_empty() || dir.trim() == "." { "." } else { dir }) else { return "refused: that folder is outside the project".into() };
    let Ok(rd) = std::fs::read_dir(&p) else { return format!("{dir}: no such folder") };
    let mut names: Vec<String> = rd.flatten().map(|e| { let n = e.file_name().to_string_lossy().to_string(); if e.path().is_dir() { format!("{n}/") } else { n } })
        .filter(|n| !matches!(n.as_str(), ".git/" | "node_modules/" | ".antiloki/" | "target/")).collect();
    names.sort();
    names.truncate(200);
    names.join("\n")
}

fn read(cwd: &Path, path: &str) -> String {
    let p = match resolve_inside(cwd, path) { Ok(p) => p, Err(e) => return format!("refused: {e}") };
    if path.split('/').any(|s| s == ".git" || s == ".antiloki") { return "refused: not part of the project's files".into(); }
    match std::fs::read_to_string(&p) { Ok(t) => t.chars().take(40_000).collect(), Err(_) => format!("{path}: no such file") }
}

/// Do the fill: ask, run the tools the model calls, until it finishes, stops, or the turns run out.
pub async fn run_fill(cfg: &Config, tag: &str, fill: &Fill) -> Result<FillOutcome, String> {
    let Some(ctx) = cfg.num_ctx else { return Err("no context size is set (ANTILOKI_LOCAL_NUM_CTX): a local worker never runs on the server's default".into()) };
    let _slot = local::slot(fill.slot_wait).await.ok_or("the local seat is busy with another fill")?;
    let http = reqwest::Client::builder().timeout(Duration::from_secs(600)).connect_timeout(Duration::from_secs(3)).build().map_err(|e| e.to_string())?;
    let mut messages = vec![json!({"role": "system", "content": system_prompt(&fill.scope)}), json!({"role": "user", "content": fill.prompt})];
    let mut out = FillOutcome::default();
    let started = Instant::now();
    for turn in 1..=fill.max_turns.max(1) {
        if started.elapsed() > Duration::from_secs(fill.max_seconds) { break; }
        out.turns = turn;
        let t0 = Instant::now();
        let body = json!({"model": tag, "messages": messages, "tools": tools(), "stream": false, "think": false, "keep_alive": "10m",
            "options": {"num_ctx": ctx, "temperature": 0, "seed": 1, "num_predict": 4096}});
        let r: Value = http.post(format!("{}/api/chat", cfg.base)).json(&body).send().await.map_err(|e| format!("the model server did not answer: {e}"))?
            .json().await.map_err(|e| format!("the model server's answer was not JSON: {e}"))?;
        if let Some(e) = r["error"].as_str() { return Err(format!("the model server refused: {e}")); }
        out.usage.add(Usage { prompt_tokens: r["prompt_eval_count"].as_u64().unwrap_or(0), completion_tokens: r["eval_count"].as_u64().unwrap_or(0), ms: t0.elapsed().as_millis() as u64, calls: 1 });
        let msg = r["message"].clone();
        let calls: Vec<Value> = msg["tool_calls"].as_array().cloned().unwrap_or_default();
        messages.push(json!({"role": "assistant", "content": msg["content"].as_str().unwrap_or(""), "tool_calls": calls}));
        if calls.is_empty() { out.summary = msg["content"].as_str().unwrap_or("").trim().to_string(); break; }
        for c in &calls {
            let name = c["function"]["name"].as_str().unwrap_or("");
            let a = arguments_of(c);
            let result = match name {
                "list_files" => list(&fill.cwd, a["dir"].as_str().unwrap_or(".")),
                "read_file" => read(&fill.cwd, a["path"].as_str().unwrap_or("")),
                "write_file" => {
                    let (path, content) = (a["path"].as_str().unwrap_or("").trim().trim_start_matches("./").to_string(), a["content"].as_str().unwrap_or(""));
                    match resolve_inside(&fill.cwd, &path) {
                        Err(e) => { out.refused.push(format!("{path}: {e}")); format!("refused: {e}") }
                        Ok(_) if !scope_allows(&fill.scope, &path) => { let why = format!("not one of the files you may change ({})", fill.scope.join(", ")); out.refused.push(format!("{path}: {why}")); format!("refused: {why}") }
                        Ok(full) => {
                            if let Some(d) = full.parent() { let _ = std::fs::create_dir_all(d); }
                            match std::fs::write(&full, content) { Ok(()) => { if !out.changed.contains(&path) { out.changed.push(path.clone()); } format!("wrote {path} ({} bytes)", content.len()) } Err(e) => format!("could not write {path}: {e}") }
                        }
                    }
                }
                "finish" => { out.summary = a["summary"].as_str().unwrap_or("").trim().to_string(); out.finished = true; "ok".into() }
                other => format!("unknown tool {other}"),
            };
            messages.push(json!({"role": "tool", "content": result}));
            if out.finished { break; }
        }
        if out.finished { break; }
    }
    out.digest = String::new();
    let _ = started;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::tests::server;
    use std::sync::{Arc, Mutex};

    fn cfg(base: &str) -> Config { Config { base: base.into(), num_ctx: Some(65536), headroom_mb: 0, gpu_watts: 170.0, kwh_usd: 0.15 } }

    fn tmp(tag: &str) -> PathBuf { let p = std::env::temp_dir().join(format!("antiloki-local-{tag}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&p); std::fs::create_dir_all(&p).unwrap(); p }

    /// A model that says, turn by turn, exactly these tool calls, and records every request it was sent.
    fn scripted(turns: Vec<Value>) -> (String, Arc<Mutex<Vec<Value>>>) {
        let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(vec![]));
        let (s2, n) = (seen.clone(), Arc::new(Mutex::new(0usize)));
        let base = server(move |path, body| {
            if path != "/api/chat" { return json!({}); }
            s2.lock().unwrap().push(body.clone());
            let mut k = n.lock().unwrap();
            let t = turns.get(*k).cloned().unwrap_or_else(|| json!({"role": "assistant", "content": "done"}));
            *k += 1;
            json!({"message": t, "prompt_eval_count": 100, "eval_count": 20})
        });
        (base, seen)
    }
    fn call(name: &str, args: Value) -> Value { json!({"role": "assistant", "content": "", "tool_calls": [{"function": {"name": name, "arguments": args}}]}) }

    #[test]
    fn a_scope_is_globs_and_an_empty_scope_allows_nothing() {
        let s = |g: &[&str]| g.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(scope_allows(&s(&["src/features/x.ts"]), "src/features/x.ts"));
        assert!(scope_allows(&s(&["src/**/*.test.ts"]), "src/a/b/x.test.ts"));
        assert!(scope_allows(&s(&["src/**"]), "src/deep/er/f.ts"));
        assert!(!scope_allows(&s(&["src/*.ts"]), "src/a/b.ts"), "* does not cross folders");
        assert!(!scope_allows(&s(&["src/features/x.ts"]), "src/features/y.ts"));
        assert!(!scope_allows(&s(&[]), "anything"), "no scope, no write");
        assert!(scope_allows(&s(&["./a.txt"]), "a.txt"));
    }

    #[test]
    fn a_path_cannot_leave_the_box_by_dots_by_root_or_by_a_link() {
        let d = tmp("resolve");
        std::fs::create_dir_all(d.join("src")).unwrap();
        let outside = tmp("resolve-outside");
        std::os::unix::fs::symlink(&outside, d.join("link")).unwrap();
        assert!(resolve_inside(&d, "src/new/f.ts").is_ok(), "a new file in a new folder is fine");
        assert!(resolve_inside(&d, "../x").unwrap_err().contains("`..`"));
        assert!(resolve_inside(&d, "/etc/passwd").unwrap_err().contains("absolute"));
        assert!(resolve_inside(&d, "link/x.txt").unwrap_err().contains("link"), "a symlink out of the box is a way out");
        assert!(resolve_inside(&d, "").is_err());
        let _ = std::fs::remove_dir_all(&d); let _ = std::fs::remove_dir_all(&outside);
    }

    #[tokio::test]
    async fn a_fill_reads_writes_what_the_scope_allows_and_finishes() {
        let _s = crate::local::tests::slot_serial().lock().await;
        let d = tmp("fill");
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src/x.test.ts"), "expect(double(2)).toBe(4)").unwrap();
        let (base, seen) = scripted(vec![
            call("list_files", json!({"dir": "src"})),
            call("read_file", json!({"path": "src/x.test.ts"})),
            call("write_file", json!({"path": "src/x.ts", "content": "export const double = (n: number) => n * 2;\n"})),
            call("finish", json!({"summary": "filled double"}))]);
        let out = run_fill(&cfg(&base), "qwen2.5-coder:14b", &Fill::new("FILL THE STUBS in src/x.ts; the tests in src/x.test.ts must pass.", &d, &["src/x.ts"])).await.unwrap();
        assert!(out.finished && out.summary == "filled double" && out.turns == 4);
        assert_eq!(out.changed, vec!["src/x.ts"]);
        assert!(out.refused.is_empty());
        assert_eq!(std::fs::read_to_string(d.join("src/x.ts")).unwrap(), "export const double = (n: number) => n * 2;\n");
        assert_eq!((out.usage.prompt_tokens, out.usage.completion_tokens, out.usage.calls), (400, 80, 4));
        let reqs = seen.lock().unwrap().clone();
        assert!(reqs.iter().all(|r| r["options"]["num_ctx"] == 65536), "E3: every call states its context size");
        assert!(reqs[0]["messages"][0]["content"].as_str().unwrap().contains("src/x.ts"), "the model is told what it may change");
        let last = reqs[3]["messages"].as_array().unwrap();
        assert!(last.iter().any(|m| m["role"] == "tool" && m["content"].as_str().unwrap_or("").contains("x.test.ts")), "the tool results go back to the model");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn a_write_outside_the_scope_or_the_box_is_refused_told_to_the_model_and_kept() {
        let _s = crate::local::tests::slot_serial().lock().await;
        let d = tmp("wall");
        let outside = tmp("wall-outside");
        let (base, seen) = scripted(vec![
            call("write_file", json!({"path": "package.json", "content": "{}"})),
            call("write_file", json!({"path": "../escape.txt", "content": "x"})),
            call("write_file", json!({"path": "src/ok.ts", "content": "ok"})),
            call("finish", json!({"summary": "done"}))]);
        let out = run_fill(&cfg(&base), "m", &Fill::new("go", &d, &["src/ok.ts"])).await.unwrap();
        assert_eq!(out.changed, vec!["src/ok.ts"]);
        assert_eq!(out.refused.len(), 2, "{:?}", out.refused);
        assert!(out.refused[0].starts_with("package.json: not one of the files you may change"));
        assert!(!d.join("package.json").exists() && !d.parent().unwrap().join("escape.txt").exists());
        let told: Vec<String> = seen.lock().unwrap()[1]["messages"].as_array().unwrap().iter().filter(|m| m["role"] == "tool").map(|m| m["content"].as_str().unwrap().to_string()).collect();
        assert!(told[0].starts_with("refused:"), "the model learns it was refused: {told:?}");
        let _ = std::fs::remove_dir_all(&d); let _ = std::fs::remove_dir_all(&outside);
    }

    #[tokio::test]
    async fn a_worker_that_only_talks_or_runs_out_of_turns_has_not_finished() {
        let _s = crate::local::tests::slot_serial().lock().await;
        let d = tmp("talk");
        let (base, _) = scripted(vec![json!({"role": "assistant", "content": "I would add a function."})]);
        let out = run_fill(&cfg(&base), "m", &Fill::new("go", &d, &["a.txt"])).await.unwrap();
        assert!(!out.finished && out.changed.is_empty() && out.summary == "I would add a function.");
        let (base, _) = scripted((0..5).map(|_| call("list_files", json!({"dir": "."}))).collect());
        let mut f = Fill::new("go", &d, &["a.txt"]); f.max_turns = 3;
        let out = run_fill(&cfg(&base), "m", &f).await.unwrap();
        assert!(!out.finished && out.turns == 3, "the turns are a cap, not a suggestion");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn without_a_context_size_nothing_is_asked() {
        let d = tmp("noctx");
        let mut c = cfg("http://127.0.0.1:1"); c.num_ctx = None;
        let e = run_fill(&c, "m", &Fill::new("go", &d, &["a"])).await.unwrap_err();
        assert!(e.contains("ANTILOKI_LOCAL_NUM_CTX"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn a_dead_server_is_an_error_not_an_empty_fill() {
        let d = tmp("dead");
        let e = run_fill(&cfg("http://127.0.0.1:1"), "m", &Fill::new("go", &d, &["a"])).await.unwrap_err();
        assert!(e.contains("did not answer"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn tool_arguments_may_arrive_as_a_json_string_as_the_openai_shape_has_them() {
        let _s = crate::local::tests::slot_serial().lock().await;
        let d = tmp("strargs");
        let (base, _) = scripted(vec![json!({"role": "assistant", "content": "", "tool_calls": [{"function": {"name": "write_file", "arguments": "{\"path\": \"a.txt\", \"content\": \"hi\"}"}}]}), call("finish", json!({"summary": "ok"}))]);
        let out = run_fill(&cfg(&base), "m", &Fill::new("go", &d, &["a.txt"])).await.unwrap();
        assert_eq!(out.changed, vec!["a.txt"]);
        assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "hi");
        let _ = std::fs::remove_dir_all(&d);
    }
}
