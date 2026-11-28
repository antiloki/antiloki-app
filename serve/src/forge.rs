//! THE FORGE — antiloki never talks to GitHub (operator 2026-09-24: *"i agree strongly, just not dependency on
//! github should be agnostic with github by default"*).
//!
//! It talks to a `Forge`, and GitHub is one implementation of it. The one that ships first needs no network and
//! no token at all: a patch series and a review document on disk, which you can `git am`, attach to anything, or
//! read. Everything a hosted forge adds — a PR row, a check run, a comment thread — is the same content sent over
//! that forge's own REST API, and the API is plain HTTPS. Not `gh`: shelling out to GitHub's binary to file a
//! merge request on a self-hosted GitLab is exactly the dependency the operator asked not to have.
//!
//! The value is not the PR. It is that the PR arrives carrying how it was made — who decided, who wrote, what
//! each gate proved, what it cost — from the same run ledger the chat reads, so a reviewer stops reconstructing
//! intent out of a diff. That renderer is here, it is pure, and every backend uses it.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn s(v: Option<&Value>) -> String { v.and_then(Value::as_str).unwrap_or("").to_string() }
fn n(v: Option<&Value>) -> f64 { v.and_then(Value::as_f64).unwrap_or(0.0) }
fn arr(v: Option<&Value>) -> Vec<Value> { v.and_then(Value::as_array).cloned().unwrap_or_default() }

/// Which forge a remote points at. Detection is from the URL, never guessed from anything else, and an unknown
/// host is `Patch` rather than a wrong guess. Pure — tested.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind { GitHub, GitLab, Gitea, Patch }
impl Kind {
    pub fn key(self) -> &'static str { match self { Kind::GitHub => "github", Kind::GitLab => "gitlab", Kind::Gitea => "gitea", Kind::Patch => "patch" } }
}
/// `git@github.com:me/repo.git` · `https://gitlab.com/me/repo` → the host and the `owner/name`. Pure — tested.
pub fn parse_remote(url: &str) -> Option<(String, String)> {
    let u = url.trim().trim_end_matches('/');
    let u = u.strip_suffix(".git").unwrap_or(u);
    if u.is_empty() { return None }
    let rest = if let Some(r) = u.strip_prefix("git@") { r.replacen(':', "/", 1) }
        else if let Some(r) = u.strip_prefix("ssh://git@") { r.to_string() }
        else if let Some(r) = u.strip_prefix("https://") { r.to_string() }
        else if let Some(r) = u.strip_prefix("http://") { r.to_string() }
        else { return None };
    // strip a user:pass@ or a port, then split host / owner / name
    let rest = rest.rsplit('@').next().unwrap_or(&rest).to_string();
    let mut parts = rest.split('/').filter(|p| !p.is_empty());
    let host = parts.next()?.split(':').next()?.to_ascii_lowercase();
    let path: Vec<&str> = parts.collect();
    if path.len() < 2 { return None }
    Some((host, path.join("/")))
}
/// The backend for a remote. `overrides` maps a self-hosted host to a kind — a company GitLab at `git.acme.com`
/// is configured once in `.antiloki/forge.json`, not detected by hope. Pure — tested.
pub fn detect(url: &str, overrides: &Value) -> (Kind, String, String) {
    let Some((host, repo)) = parse_remote(url) else { return (Kind::Patch, String::new(), String::new()) };
    if let Some(k) = overrides.get(&host).and_then(Value::as_str) {
        let kind = match k { "github" => Kind::GitHub, "gitlab" => Kind::GitLab, "gitea" | "forgejo" => Kind::Gitea, _ => Kind::Patch };
        return (kind, host, repo);
    }
    let kind = if host == "github.com" { Kind::GitHub }
        else if host == "gitlab.com" { Kind::GitLab }
        else if host == "codeberg.org" { Kind::Gitea }
        // a self-hosted host is NOT guessed from its name: "gitlab.acme.com" may be anything, and filing a merge
        // request against the wrong API is worse than writing a patch
        else { Kind::Patch };
    (kind, host, repo)
}

// ── THE BODY ─────────────────────────────────────────────────────────────────────────────────────

/// IS THIS WORTH A PAGE? A thirty-line body on a three-line fix is spam, and spam teaches reviewers to skip the
/// body — which loses the whole point. Pure — tested.
pub fn wants_full_body(story: &Value) -> bool {
    let sum = story.get("summary");
    let files = sum.and_then(|x| x.get("files")).map(|f| arr(f.get("written")).len()).unwrap_or(0);
    let stages = story.get("stages").map(|x| arr(Some(x)).len()).unwrap_or(0);
    let gates = story.get("gates").map(|x| arr(Some(x)).len()).unwrap_or(0);
    files > 1 || stages > 2 || gates > 0
}

fn ms(v: f64) -> String {
    if v < 1000.0 { return format!("{} ms", v.round() as i64) }
    let secs = v / 1000.0;
    if secs < 90.0 { return format!("{secs:.0} s") }
    let m = (secs / 60.0).floor() as i64; let r = (secs - m as f64 * 60.0).round() as i64;
    if r == 0 { format!("{m} min") } else { format!("{m} min {r} s") }
}

/// THE REVIEW DOCUMENT — what a pull request body says, and what `REVIEW.md` holds when there is no forge.
///
/// Every line is the run's own ledger, rendered. A reviewer opening this knows who decided, who wrote, which
/// gates are machines and which are opinions, what it cost, and what changed — before reading a line of the diff.
/// Pure — tested.
pub fn pr_body(story: &Value, title: &str, branch: &str) -> String {
    let sum = story.get("summary").cloned().unwrap_or(json!({}));
    let who = story.get("who").cloned().unwrap_or(json!({}));
    let mut out = String::new();

    let title = title.trim();
    if !title.is_empty() { out.push_str(&format!("## {title}\n\n")); }
    // the goal only when it says something the heading did not — an episode named after its first ask would
    // otherwise print the same sentence twice, once truncated
    let goal = s(story.get("goal"));
    let same = goal.trim_end_matches('…').trim() == title.trim_end_matches('…').trim()
        || title.trim_end_matches('…').trim().starts_with(goal.trim_end_matches('…').trim())
        || goal.trim_end_matches('…').trim().starts_with(title.trim_end_matches('…').trim());
    if !goal.is_empty() && !same { out.push_str(&format!("{goal}\n\n")); }

    // the short form: a small change gets a small page
    if !wants_full_body(story) {
        let files = arr(sum.get("files").and_then(|f| f.get("written")));
        out.push_str(&if files.is_empty() { "No files changed.\n\n".to_string() }
            else { format!("Changed {}.\n\n", files.iter().map(|f| format!("`{}`", s(Some(f)))).collect::<Vec<_>>().join(", ")) });
        out.push_str(&format!("_Made by antiloki on `{branch}`._\n"));
        return out;
    }

    out.push_str("## How it was made\n\n");
    let brain = who.get("brain").cloned().unwrap_or(json!({}));
    out.push_str(&format!("**{}** — {}\n", s(brain.get("label")), s(brain.get("did"))));
    if let Some(h) = who.get("hands").filter(|v| !v.is_null()) {
        out.push_str(&format!("**{}** — {}\n", s(h.get("label")), s(h.get("did"))));
    }
    if let Some(note) = who.get("note").and_then(Value::as_str) { out.push_str(&format!("\n{note}\n")); }
    let router = s(story.get("router").and_then(|r| r.get("line")));
    if !router.is_empty() { out.push_str(&format!("\n{router}\n")); }

    let stages = arr(story.get("stages"));
    if !stages.is_empty() {
        out.push_str("\n| stage | who | outcome | time |\n|---|---|---|---|\n");
        for st in stages.iter().take(20) {
            let right = st.get("went_right").and_then(Value::as_bool);
            let mark = match (right, st.get("red_is_good") == Some(&json!(true))) {
                (Some(true), true) => "✓ as intended", (Some(true), false) => "✓",
                (Some(false), _) => "✗", (None, _) => "·",
            };
            out.push_str(&format!("| {} | {} | {} | {} |\n", s(st.get("title")), s(st.get("who")), mark, ms(n(st.get("ms")))));
        }
        if stages.len() > 20 { out.push_str(&format!("| …and {} more | | | |\n", stages.len() - 20)); }
    }

    let gates = arr(story.get("gates"));
    if !gates.is_empty() {
        out.push_str("\n## What was checked\n\n");
        for g in &gates {
            let pass = g.get("passed") == Some(&json!(true));
            out.push_str(&format!("- {} **{}** — {} — _{}_\n", if pass { "✓" } else { "✗" }, s(g.get("name")),
                s(g.get("detail")).chars().take(160).collect::<String>(),
                if s(g.get("kind")) == "deterministic" { "a machine, not an opinion" } else { "a model's judgement" }));
        }
    }

    let files = arr(sum.get("files").and_then(|f| f.get("written")));
    if !files.is_empty() {
        out.push_str(&format!("\n## What changed\n\n{}\n", files.iter().map(|f| format!("- `{}`", s(Some(f)))).collect::<Vec<_>>().join("\n")));
        let read = n(sum.get("files").and_then(|f| f.get("read")));
        if read > 0.0 { out.push_str(&format!("\n…and {} other file(s) were read to do it.\n", read as i64)); }
    }

    out.push_str("\n## What it cost\n\n");
    let tokens = s(sum.get("tokens").and_then(|t| t.get("human")));
    let time = s(sum.get("time").and_then(|t| t.get("human")));
    out.push_str(&format!("{tokens} tokens · {time}"));
    if let Some(m) = sum.get("money").filter(|v| !v.is_null()) {
        out.push_str(&format!(" · {} where one frontier agent doing the same tokens would have cost {} ({}% kept)",
            s(m.get("spent_human")), s(m.get("baseline_human")), n(m.get("saved_pct")).round() as i64));
    }
    out.push_str("\n\nPrices are API-equivalent: a subscription pays $0 metered, so this is a ratio and not a bill.\n");
    out.push_str(&format!("\n_Made by antiloki on `{branch}`. Every figure above is from this run's own ledger._\n"));
    out
}

// ── THE BACKENDS ─────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PrRequest {
    pub root: String, pub branch: String, pub base: String,
    pub title: String, pub body: String,
}
/// Where the work ended up, whatever the backend.
#[derive(Debug, Clone, PartialEq)]
pub struct PrRef { pub kind: &'static str, pub url: String, pub local: Option<PathBuf>, pub note: String }

pub trait Forge {
    fn open_pr(&self, req: &PrRequest) -> Result<PrRef, String>;
}

/// NO NETWORK, NO TOKEN, NO ACCOUNT. A patch series and the review document beside it — `git am` on the other
/// side and the change lands with its history; read `REVIEW.md` and you know how it was made. This is the default
/// and the fallback: every hosted backend can fail, and a failed PR must never be the reason work is stranded.
pub struct PatchForge { pub out: PathBuf }
impl PatchForge {
    pub fn at(root: &str) -> Self { PatchForge { out: Path::new(root).join(".antiloki").join("outbox") } }
}
impl Forge for PatchForge {
    fn open_pr(&self, req: &PrRequest) -> Result<PrRef, String> {
        std::fs::create_dir_all(&self.out).map_err(|e| format!("{}: {e}", self.out.display()))?;
        let safe: String = req.branch.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
        let patch_at = self.out.join(format!("{safe}.patch"));
        let review_at = self.out.join(format!("{safe}.REVIEW.md"));
        let range = format!("{}..{}", req.base, req.branch);
        let out = crate::proc::cmd("git").args(["format-patch", &range, "--stdout"]).current_dir(&req.root).output()
            .map_err(|e| format!("git format-patch could not run: {e}"))?;
        if !out.status.success() {
            return Err(format!("git format-patch {range}: {}", String::from_utf8_lossy(&out.stderr).trim().chars().take(200).collect::<String>()));
        }
        if out.stdout.is_empty() { return Err(format!("nothing to send — {} has no commits the base does not already have", req.branch)) }
        std::fs::write(&patch_at, &out.stdout).map_err(|e| format!("{}: {e}", patch_at.display()))?;
        std::fs::write(&review_at, &req.body).map_err(|e| format!("{}: {e}", review_at.display()))?;
        Ok(PrRef { kind: "patch", url: String::new(), local: Some(patch_at.clone()),
            note: format!("wrote {} and {} — apply it anywhere with `git am {}`",
                patch_at.display(), review_at.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default(), patch_at.display()) })
    }
}

// ── TOKENS ───────────────────────────────────────────────────────────────────────────────────────
//
// A forge token is a real credential and it never goes in `.antiloki/`, which lives inside the repository and is
// therefore shared, committed or pushed by somebody eventually. Three sources, in order:
//
//   1. the environment — `ANTILOKI_FORGE_TOKEN`, then the name that forge's own tooling already uses;
//   2. `~/.antiloki/tokens/<host>`, a file outside every repo, refused unless it is owner-only;
//   3. `gh auth token`, best effort, ONLY for github.com.
//
// Step 3 is the convenience that makes this zero-setup for most people, and it is deliberately last and
// optional: if `gh` is not installed nothing fails, which is the difference between using a tool and depending
// on one. Nothing here ever reads a token for a host it was not asked about.

/// Where a token came from — said out loud, because "it worked" and "it worked with the credential you meant"
/// are different facts.
///
/// `Debug` IS HAND-WRITTEN AND REDACTS THE VALUE. A derived one printed the secret the first time this was
/// exercised: a test called `unwrap_err()` on an `Ok(Token)` and the panic message carried a live GitHub token
/// into a log. Anything that can print itself eventually prints itself somewhere it should not, so the only
/// place the value is readable is the field, on purpose.
#[derive(Clone, PartialEq)]
pub struct Token { pub value: String, pub from: String }
impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Token").field("value", &"<redacted>").field("from", &self.from).finish()
    }
}
impl std::fmt::Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "a token from {}", self.from) }
}

/// A command's stdout, read as a token: the LAST non-empty line, and only if it looks like one.
///
/// `gh auth token` does not always print just the token. Under mise its stdout began with
/// `mise ~/.config/mise/config.toml tools: gh@2.98.0` and the token was on the second line — so trimming the
/// whole output produced a two-line "token" that every request would have rejected as a 401, with nothing in the
/// message to say why. A token has no spaces; anything that does is somebody's banner. Pure — tested.
pub fn token_from_output(stdout: &str) -> Option<String> {
    stdout.lines().rev()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.contains(char::is_whitespace) && l.len() >= 8)
        .map(String::from)
}

/// The env names worth trying for a host, in order. Pure — tested.
pub fn env_names(kind: Kind) -> Vec<&'static str> {
    let mut v = vec!["ANTILOKI_FORGE_TOKEN"];
    match kind {
        Kind::GitHub => { v.push("GITHUB_TOKEN"); v.push("GH_TOKEN"); }
        Kind::GitLab => { v.push("GITLAB_TOKEN"); v.push("CI_JOB_TOKEN"); }
        Kind::Gitea => { v.push("GITEA_TOKEN"); v.push("FORGEJO_TOKEN"); }
        Kind::Patch => {}
    }
    v
}
/// `~/.antiloki/tokens/<host>` — outside every repository, on purpose.
pub fn token_file(host: &str) -> Option<PathBuf> {
    if host.is_empty() || host.contains('/') || host.contains("..") { return None }
    let home = std::env::var("ANTILOKI_HOME").ok().filter(|h| !h.is_empty()).map(PathBuf::from)
        .or_else(|| std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".antiloki")))?;
    Some(home.join("tokens").join(host))
}
/// A stored token, refused unless the file is owner-only — a credential readable by every process on the box is
/// not a secret, and silently using it teaches the operator it was safe. Pure over the mode — tested.
pub fn mode_is_private(mode: u32) -> bool { mode & 0o077 == 0 }

pub fn find_token(kind: Kind, host: &str) -> Result<Token, String> {
    for name in env_names(kind) {
        if let Ok(v) = std::env::var(name) { let v = v.trim().to_string(); if !v.is_empty() { return Ok(Token { value: v, from: format!("the {name} environment variable") }) } }
    }
    if let Some(p) = token_file(host) {
        if let Ok(md) = std::fs::metadata(&p) {
            #[cfg(unix)]
            { use std::os::unix::fs::PermissionsExt; if !mode_is_private(md.permissions().mode()) {
                return Err(format!("{} is readable by other users — `chmod 600 {}` and try again", p.display(), p.display())) } }
            if let Ok(t) = std::fs::read_to_string(&p) { if let Some(t) = token_from_output(&t) { return Ok(Token { value: t, from: p.display().to_string() }) } }
        }
    }
    if kind == Kind::GitHub {
        if let Ok(o) = crate::proc::cmd("gh").args(["auth", "token"]).output() {
            if o.status.success() {
                if let Some(t) = token_from_output(&String::from_utf8_lossy(&o.stdout)) { return Ok(Token { value: t, from: "gh auth token".into() }) }
            }
        }
    }
    Err(format!("no token for {host}. Paste one in Settings › Seats › forge tokens — or set {} — or sign in with `gh auth login`.",
        env_names(kind).join(" / ")))
}

// ── THE PREFLIGHT AND THE PUBLISH (operator 2026-09-30: "create a connector with github deterministically so we can click in create PR and go there ... on trying close the agent it appeared a PR button but onclick it didnt create it. Also some repos are just local") ──
//
// The forge layer existed and worked - with a token and a remote. The workspace in question had NO remote, so "PR" quietly fell back to writing a patch file, which reads as a button that does
// nothing. The answer is to say what a click WILL do before it is clicked, and to offer the one step that is missing for a local-only repo: create the online repo (with `gh`, the operator's own
// login, only when they ask).

/// What the `gh` CLI knows: is it there, and which account is it logged in as. Pure parse of `gh auth status` - tested.
pub fn gh_account_from(status_output: &str) -> Option<String> {
    status_output.lines().find_map(|l| { let l = l.trim(); let i = l.find(" account ")?; Some(l[i + 9..].split_whitespace().next()?.trim_matches(|c| c == '(' || c == ')').to_string()) })
}
/// `(installed, account)` by asking `gh`.
pub fn gh_state() -> (bool, Option<String>) {
    let mut c = crate::proc::cmd("gh"); c.args(["auth", "status", "--hostname", "github.com"]);
    if crate::proc::which("gh").is_none() { return (false, None); }
    match antiloki_rs::proc::output_within(c, std::time::Duration::from_secs(10)).ok_or(()) { // a probe that timed out is still an INSTALLED gh
        Err(()) => (true, None),
        Ok(o) => { let t = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)); (true, if o.status.success() { gh_account_from(&t) } else { None }) }
    }
}
/// WHAT A "PULL REQUEST" CLICK WILL DO, before anyone clicks it. `remote` is `origin`'s URL ("" = none). Pure over its inputs - tested.
pub fn preflight_of(remote: &str, cfg: &Value, base: &str, branch: &str, gh_installed: bool, gh_account: Option<&str>, token: Result<&str, &str>) -> Value {
    let (kind, host, repo) = detect(remote, cfg);
    let local_only = remote.trim().is_empty();
    let hosted = kind != Kind::Patch;
    let can_pr = hosted && token.is_ok();
    let publish_available = local_only && gh_account.is_some();
    let (label, why) = if can_pr { (format!("open a pull request on {repo} -> {base}"), String::new()) }
        else if local_only && publish_available { ("create a private GitHub repo, then open the pull request".to_string(),
            format!("this repo is local only (no remote). Closing the episode can merge {branch} into {base} here, or create a private repo on GitHub as {} and open the pull request there.", gh_account.unwrap_or(""))) }
        else if local_only { ("no online repo - merge here or write a patch".to_string(), if gh_installed { "this repo is local only and the GitHub CLI is not logged in: run `gh auth login` to be able to publish it.".to_string() } else { "this repo is local only and the GitHub CLI (`gh`) is not installed: install it and run `gh auth login` to publish it.".to_string() }) }
        else if hosted { ("a pull request needs a token".to_string(), token.err().unwrap_or("no token").to_string()) }
        else { ("this remote is not a forge antiloki can file against - a patch is written".to_string(), format!("{host} is not GitHub, GitLab or Codeberg; name it in .antiloki/forge.json if it is a self-hosted GitLab or Gitea.")) };
    json!({ "ok": true, "remote": remote.trim(), "kind": kind.key(), "host": host, "repo": repo, "base": base, "branch": branch, "local_only": local_only,
            "can_pr": can_pr, "publish_available": publish_available, "gh_installed": gh_installed, "gh_account": gh_account, "label": label, "why": why })
}
/// A repository name `gh repo create` will take: letters, digits, `.`, `_`, `-`. Pure - tested.
pub fn repo_name_ok(n: &str) -> bool { !n.is_empty() && n.len() <= 100 && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) && !n.starts_with('.') && !n.starts_with('-') }
/// The repo URL `gh repo create` printed. Pure - tested.
pub fn url_from_gh_output(out: &str) -> Option<String> { out.split_whitespace().find(|w| w.starts_with("https://") && w.contains("github.com/")).map(|w| w.trim_end_matches(['.', ',']).to_string()) }
/// Create the online repo for a local-only one and push it: `gh repo create NAME --private --source ROOT --remote origin --push`. The caller has already checked the confirmation, the login and
/// that there is no remote. Returns the repo's URL.
pub fn publish_repo(root: &str, name: &str, private: bool) -> Result<String, String> {
    if !repo_name_ok(name) { return Err(format!("{name:?} is not a repository name (letters, digits, . _ -)")) }
    let vis = if private { "--private" } else { "--public" };
    let o = crate::proc::cmd("gh").args(["repo", "create", name, vis, "--source", root, "--remote", "origin", "--push"]).current_dir(root).output().map_err(|e| format!("gh: {e}"))?;
    let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    if !o.status.success() { return Err(text.trim().chars().take(400).collect()) }
    Ok(url_from_gh_output(&text).unwrap_or_else(|| format!("https://github.com/{name}")))
}

// ── THE TOKEN, FROM SETTINGS (operator 2026-09-30: "we should have a place on settings to add the token from github") ─────────────────────────────────────────────────────────────────────
//
// Until now a token could only come from an environment variable, a file the operator made by hand, or `gh auth token`. Settings can now write the file - and only the file: owner-only
// (0600) in `~/.antiloki/tokens/<host>`, outside every repository, never returned by any endpoint, never logged. A token is checked against the forge BEFORE it is kept (GitHub: GET /user), so a
// typo is told at once instead of at the next pull request.

/// A host name a token file may be called: letters, digits, `.` and `-`. Pure - tested.
pub fn host_ok(h: &str) -> bool { !h.is_empty() && h.len() <= 100 && h.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-')) && !h.starts_with('.') && !h.contains("..") }
/// What may be saved as a token: no spaces or line breaks (a pasted banner is not a token), a sane length. Pure - tested.
pub fn token_shape_ok(t: &str) -> Result<String, String> {
    let t = t.trim();
    if t.len() < 8 { return Err("that is too short to be a token".into()) }
    if t.len() > 400 { return Err("that is too long to be a token".into()) }
    if t.chars().any(char::is_whitespace) { return Err("a token has no spaces or line breaks - paste just the token".into()) }
    Ok(t.to_string())
}
/// Write the token owner-only, atomically (a temp file with the right mode, then a rename): there is never a moment when the secret is readable by others.
pub fn save_token(host: &str, token: &str) -> Result<PathBuf, String> {
    let dir = token_file("x").and_then(|p| p.parent().map(Path::to_path_buf)).ok_or("no place to keep a token on this machine")?;
    save_token_in(&dir, host, token)
}
pub fn save_token_in(dir: &Path, host: &str, token: &str) -> Result<PathBuf, String> {
    if !host_ok(host) { return Err(format!("{host:?} is not a host name")) }
    let token = token_shape_ok(token)?;
    let dir = dir.to_path_buf();
    let path = dir.join(host);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)); }
    let tmp = dir.join(format!(".{host}.tmp-{}", std::process::id()));
    {
        use std::io::Write;
        #[cfg(unix)] let mut f = { use std::os::unix::fs::OpenOptionsExt; std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp).map_err(|e| e.to_string())? };
        #[cfg(not(unix))] let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        f.write_all(token.as_bytes()).map_err(|e| e.to_string())?; f.write_all(b"\n").map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| { let _ = std::fs::remove_file(&tmp); e.to_string() })?;
    Ok(path)
}
/// Forget the stored token. `Ok(false)` when there was none.
pub fn remove_token(host: &str) -> Result<bool, String> {
    let Some(dir) = token_file("x").and_then(|p| p.parent().map(Path::to_path_buf)) else { return Ok(false) };
    remove_token_in(&dir, host)
}
pub fn remove_token_in(dir: &Path, host: &str) -> Result<bool, String> {
    if !host_ok(host) { return Err(format!("{host:?} is not a host name")) }
    let p = dir.join(host);
    match std::fs::remove_file(&p) { Ok(()) => Ok(true), Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false), Err(e) => Err(e.to_string()) }
}
/// Where a token for `host` would come from right now, WITHOUT reading its value into the answer: `(where, a description)`. Pure over what it is asked.
pub fn token_where(kind: Kind, host: &str) -> Option<String> { find_token(kind, host).ok().map(|t| t.from) }
/// Every host that has a stored token file, for the Settings list. Names only.
pub fn stored_hosts() -> Vec<String> {
    let Some(dir) = token_file("x").and_then(|p| p.parent().map(Path::to_path_buf)) else { return vec![] };
    stored_hosts_in(&dir)
}
pub fn stored_hosts_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir).map(|rd| rd.filter_map(Result::ok).filter_map(|e| e.file_name().into_string().ok()).filter(|n| host_ok(n)).collect()).unwrap_or_default();
    v.sort(); v
}
/// Ask GitHub who this token is (GET /user): `Ok(login)`, or why not. 401 means the token itself is wrong; any other failure (offline, rate limit) is reported as unverifiable. Blocking.
pub fn verify_github_token(host: &str, token: &str) -> Result<String, String> {
    let url = format!("{}/user", github_api(host));
    match ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(12)).build().get(&url).set("authorization", &format!("Bearer {token}")).set("accept", "application/vnd.github+json").set("user-agent", "antiloki").call() {
        Ok(r) => r.into_json::<Value>().ok().and_then(|v| v.get("login").and_then(Value::as_str).map(String::from)).ok_or_else(|| "GitHub answered but named no user".to_string()),
        Err(ureq::Error::Status(401, _)) => Err("GitHub says this token is not valid (401) - it may be mistyped, expired or revoked".into()),
        Err(ureq::Error::Status(code, _)) => Err(format!("could not verify (GitHub answered {code})")),
        Err(e) => Err(format!("could not verify ({e})")),
    }
}
/// Does this error mean the token is definitely wrong (so it must not be kept)? Pure - tested.
pub fn verification_rejects(err: &str) -> bool { err.contains("(401)") }


// ── GITHUB ───────────────────────────────────────────────────────────────────────────────────────

/// The API root for a host: github.com is api.github.com, GitHub Enterprise is `<host>/api/v3`. Pure — tested.
pub fn github_api(host: &str) -> String {
    if host == "github.com" { "https://api.github.com".into() } else { format!("https://{host}/api/v3") }
}
/// Gitea and Forgejo speak almost the same API as GitHub, at another address and with another word before the token (found 2026-11-26: their pull requests were sent to GitHub's address). Pure.
pub fn api_base(kind: Kind, host: &str) -> String { if kind == Kind::Gitea { format!("https://{host}/api/v1") } else { github_api(host) } }
pub fn auth_header(kind: Kind, token: &str) -> String { if kind == Kind::Gitea { format!("token {token}") } else { format!("Bearer {token}") } }
/// The refusal, naming the forge that made it. Pure.
pub fn forge_error(kind: Kind, status: u16, body: &str) -> String { if kind != Kind::Gitea { return github_error(status, body); } if status == 409 { return "a pull request for this branch already exists".into(); } github_error(status, body).replace("GitHub", "Gitea") }
/// The body of `POST /repos/{owner}/{repo}/pulls`. Pure — tested.
pub fn github_pr_payload(req: &PrRequest) -> Value {
    json!({ "title": if req.title.trim().is_empty() { req.branch.clone() } else { req.title.clone() },
            "head": req.branch, "base": req.base, "body": req.body, "maintainer_can_modify": true })
}
/// COMMIT STATUSES, NOT CHECK RUNS. A check run needs a GitHub App installation token; a personal token cannot
/// create one. A commit status can, appears in the same checks list on the pull request, and can be named in
/// branch protection as a required check — which is the whole point: a failed gate then blocks the merge through
/// GitHub's own mechanism rather than through ours. Pure — tested.
pub fn github_status_payload(gate: &Value, run_url: &str) -> Value {
    let name = s(gate.get("gate"));
    let skipped = gate.get("skipped").and_then(Value::as_str);
    let (state, desc) = match skipped {
        // a gate that could not run is NOT success: "no linter configured" and "the lint passed" are different
        Some(why) => ("error", format!("not run — {}", why.chars().take(130).collect::<String>())),
        None if gate.get("passed") == Some(&json!(true)) => ("success", format!("passed · {}", s(gate.get("cmd")).chars().take(110).collect::<String>())),
        None => ("failure", format!("exit {} · {}", n(gate.get("exit")) as i64, s(gate.get("cmd")).chars().take(100).collect::<String>())),
    };
    json!({ "state": state, "context": format!("antiloki / {name}"), "description": desc, "target_url": run_url })
}
/// What the API said, read as a sentence a person can act on. Pure — tested.
pub fn github_error(status: u16, body: &str) -> String {
    let msg = serde_json::from_str::<Value>(body).ok().and_then(|v| v.get("message").and_then(Value::as_str).map(String::from)).unwrap_or_default();
    match status {
        401 => "the token was refused (401) — it is wrong, expired, or not for this host".into(),
        403 if msg.contains("rate limit") => "GitHub rate-limited this token (403) — wait, or use a different one".into(),
        403 => format!("the token lacks permission (403){}", if msg.is_empty() { String::new() } else { format!(" — {msg}") }),
        404 => "not found (404) — the repository does not exist, or this token cannot see it".into(),
        422 if msg.contains("already exists") => "a pull request for this branch already exists".into(),
        422 => format!("GitHub refused the request (422){}", if msg.is_empty() { String::new() } else { format!(" — {msg}") }),
        _ => format!("GitHub returned {status}{}", if msg.is_empty() { String::new() } else { format!(" — {msg}") }),
    }
}

/// A pull request on GitHub (or GitHub Enterprise), with the gates posted as commit statuses.
pub struct GitHubForge { pub kind: Kind, pub host: String, pub repo: String, pub token: Token }
impl GitHubForge {
    /// Push the episode branch to the remote. A forge cannot open a pull request for a branch it has never seen.
    fn push(root: &str, branch: &str) -> Result<(), String> {
        let out = crate::proc::cmd("git").args(["push", "--set-upstream", "origin", branch]).current_dir(root).output()
            .map_err(|e| format!("git push could not run: {e}"))?;
        if out.status.success() { return Ok(()) }
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("Everything up-to-date") { return Ok(()) }
        Err(format!("git push: {}", err.trim().chars().take(240).collect::<String>()))
    }
    pub async fn open_pr_async(&self, req: &PrRequest) -> Result<PrRef, String> {
        let (root, branch) = (req.root.clone(), req.branch.clone());
        tokio::task::spawn_blocking(move || Self::push(&root, &branch)).await.map_err(|e| e.to_string())??;
        let url = format!("{}/repos/{}/pulls", api_base(self.kind, &self.host), self.repo);
        let res = reqwest::Client::new().post(&url)
            .header("Authorization", auth_header(self.kind, &self.token.value))
            .header("Accept", if self.kind == Kind::Gitea { "application/json" } else { "application/vnd.github+json" })
            .header("User-Agent", "antiloki")
            .json(&github_pr_payload(req))
            .timeout(std::time::Duration::from_secs(30))
            .send().await.map_err(|e| format!("could not reach {}: {e}", self.host))?;
        let status = res.status().as_u16();
        let body = res.text().await.unwrap_or_default();
        if !(200..300).contains(&status) { return Err(forge_error(self.kind, status, &body)) }
        let v: Value = serde_json::from_str(&body).unwrap_or(json!({}));
        Ok(PrRef { kind: self.kind.key(), url: s(v.get("html_url")), local: None,
            note: format!("opened {} — the review body is the run's own ledger", s(v.get("html_url"))) })
    }
    /// One commit status per gate, on the branch's head.
    pub async fn post_gates(&self, root: &str, branch: &str, gates: &[Value], run_url: &str) -> Result<usize, String> {
        let (root2, branch2) = (root.to_string(), branch.to_string());
        let sha = tokio::task::spawn_blocking(move || {
            crate::proc::cmd("git").args(["rev-parse", &branch2]).current_dir(&root2).output().ok()
                .filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        }).await.map_err(|e| e.to_string())?.ok_or("could not resolve the branch's head commit")?;
        let client = reqwest::Client::new();
        let mut posted = 0;
        for g in gates {
            let url = format!("{}/repos/{}/statuses/{sha}", api_base(self.kind, &self.host), self.repo);
            let ok = client.post(&url)
                .header("Authorization", auth_header(self.kind, &self.token.value))
                .header("Accept", if self.kind == Kind::Gitea { "application/json" } else { "application/vnd.github+json" })
                .header("User-Agent", "antiloki")
                .json(&github_status_payload(g, run_url))
                .timeout(std::time::Duration::from_secs(20))
                .send().await.map(|r| r.status().is_success()).unwrap_or(false);
            if ok { posted += 1 }
        }
        Ok(posted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gitea_has_its_own_address_token_word_and_refusals() {
        assert_eq!(api_base(Kind::Gitea, "codeberg.org"), "https://codeberg.org/api/v1"); assert_eq!(api_base(Kind::GitHub, "github.com"), "https://api.github.com"); assert_eq!(api_base(Kind::GitHub, "git.acme.com"), "https://git.acme.com/api/v3");
        assert_eq!(auth_header(Kind::Gitea, "t"), "token t"); assert_eq!(auth_header(Kind::GitHub, "t"), "Bearer t");
        assert!(forge_error(Kind::Gitea, 409, "{}").contains("already exists")); assert!(forge_error(Kind::Gitea, 500, "x").contains("Gitea returned 500"));
    }
    #[test]
    fn a_remote_names_its_host_and_repo_in_every_shape_git_writes_one() {
        assert_eq!(parse_remote("git@github.com:antiloki/antiloki-site.git"), Some(("github.com".into(), "antiloki/antiloki-site".into())));
        assert_eq!(parse_remote("https://github.com/me/repo"), Some(("github.com".into(), "me/repo".into())));
        assert_eq!(parse_remote("https://token@gitlab.com/group/sub/repo.git"), Some(("gitlab.com".into(), "group/sub/repo".into())));
        assert_eq!(parse_remote("ssh://git@git.acme.com:2222/team/repo.git"), Some(("git.acme.com".into(), "team/repo".into())));
        assert_eq!(parse_remote("/home/user/Projects/ministore"), None, "a local path is not a forge");
        assert_eq!(parse_remote(""), None);
    }

    #[test]
    fn a_self_hosted_host_is_configured_never_guessed() {
        let none = json!({});
        assert_eq!(detect("git@github.com:me/repo.git", &none).0, Kind::GitHub);
        assert_eq!(detect("https://gitlab.com/me/repo", &none).0, Kind::GitLab);
        assert_eq!(detect("https://codeberg.org/me/repo", &none).0, Kind::Gitea);
        // "gitlab.acme.com" LOOKS like GitLab and may be anything; filing against the wrong API is worse than a patch
        assert_eq!(detect("https://gitlab.acme.com/me/repo", &none).0, Kind::Patch);
        // …until somebody says so once, in .antiloki/forge.json
        let cfg = json!({ "gitlab.acme.com": "gitlab", "code.acme.com": "forgejo" });
        assert_eq!(detect("https://gitlab.acme.com/me/repo", &cfg).0, Kind::GitLab);
        assert_eq!(detect("https://code.acme.com/me/repo", &cfg).0, Kind::Gitea);
        // and a local repo falls all the way to the backend that needs nothing
        assert_eq!(detect("/home/user/Projects/ministore", &none), (Kind::Patch, String::new(), String::new()));
    }

    fn story() -> Value {
        json!({
            "goal": "Add an orders feature", "status": "done",
            "who": { "solo": false,
                "brain": { "label": "Claude Sonnet", "did": "decided the plan, split the work and judged what came back" },
                "hands": { "label": "OpenCode DeepSeek v4 Flash", "did": "wrote the code, one file at a time" } },
            "router": { "on": true, "line": "Heimdall sent this to the low tier with high confidence" },
            "stages": [
                { "title": "Agreed what to build", "who": "Claude · mid tier", "went_right": true, "red_is_good": false, "ms": 262707.0 },
                { "title": "Proved the tests fail on empty files", "who": "the compiler and the test runner", "went_right": true, "red_is_good": true, "ms": 4200.0 },
                { "title": "Checked the work on src/orders/api.ts", "who": "the compiler", "went_right": false, "red_is_good": false, "ms": 14600.0 },
            ],
            "gates": [
                { "name": "The tests", "kind": "deterministic", "passed": true, "detail": "npx vitest related 'src/App.tsx' · passed · 730 ms" },
                { "name": "The judge", "kind": "model", "passed": false, "score": 60, "detail": "OrdersPage calls useOrders() independently of App's instance" },
            ],
            "summary": {
                "files": { "written": ["src/orders/api.ts", "src/App.tsx"], "read": 12 },
                "tokens": { "human": "363k" }, "time": { "human": "16 min 35 s" },
                "money": { "spent_human": "$0.16", "baseline_human": "$2.91", "saved_pct": 94.6 },
            },
        })
    }

    #[test]
    fn the_body_is_the_run_told_to_a_reviewer() {
        let b = pr_body(&story(), "Orders", "antiloki/dani/ep-2");
        assert!(b.starts_with("## Orders\n"));
        assert!(b.contains("Add an orders feature"), "a goal that says more than the title still appears");
        // …and one that only repeats the title, truncated, does not print twice
        let echo = pr_body(&story(), "Add an orders feature", "b");
        assert_eq!(echo.matches("Add an orders feature").count(), 1, "{echo}");
        // WHO — the thing a diff cannot tell you
        assert!(b.contains("**Claude Sonnet** — decided the plan"));
        assert!(b.contains("**OpenCode DeepSeek v4 Flash** — wrote the code"));
        assert!(b.contains("Heimdall sent this to the low tier"));
        // the stages, with the red gate read as intended rather than as a failure
        assert!(b.contains("| Agreed what to build | Claude · mid tier | ✓ | 4 min 23 s |"), "{b}");
        assert!(b.contains("| Proved the tests fail on empty files | the compiler and the test runner | ✓ as intended | 4 s |"));
        assert!(b.contains("| Checked the work on src/orders/api.ts | the compiler | ✗ | 15 s |"));
        // the gates, each labelled by what kind of authority it is
        assert!(b.contains("✓ **The tests**"));
        assert!(b.contains("_a machine, not an opinion_"));
        assert!(b.contains("✗ **The judge**"));
        assert!(b.contains("_a model's judgement_"));
        // the cost, with the yardstick named
        assert!(b.contains("363k tokens · 16 min 35 s · $0.16 where one frontier agent"));
        assert!(b.contains("95% kept"));
        assert!(b.contains("a subscription pays $0 metered"));
        assert!(b.contains("- `src/orders/api.ts`"));
        assert!(b.contains("…and 12 other file(s) were read"));
    }

    #[test]
    fn a_small_change_gets_a_small_page() {
        let tiny = json!({ "goal": "fix the title", "stages": [], "gates": [], "who": { "brain": { "label": "Claude" } },
            "summary": { "files": { "written": ["index.html"], "read": 0 }, "tokens": { "human": "2k" }, "time": { "human": "9 s" } } });
        assert!(!wants_full_body(&tiny));
        let b = pr_body(&tiny, "Title", "antiloki/x/ep-1");
        assert!(b.contains("Changed `index.html`."));
        // and a change with no files at all says so rather than leaving the reader guessing
        assert!(pr_body(&json!({ "stages": [], "gates": [], "summary": {} }), "T", "b").contains("No files changed."));
        assert!(!b.contains("## How it was made"), "a thirty-line body on a one-line fix teaches reviewers to skip bodies");
        assert!(b.trim().ends_with("_Made by antiloki on `antiloki/x/ep-1`._"));
        // …but two files, or any gate at all, is worth the page
        assert!(wants_full_body(&story()));
    }

    #[test]
    fn a_token_is_looked_for_where_each_forge_already_keeps_one_and_never_in_the_repo() {
        assert_eq!(env_names(Kind::GitHub), vec!["ANTILOKI_FORGE_TOKEN", "GITHUB_TOKEN", "GH_TOKEN"]);
        assert_eq!(env_names(Kind::GitLab), vec!["ANTILOKI_FORGE_TOKEN", "GITLAB_TOKEN", "CI_JOB_TOKEN"]);
        assert_eq!(env_names(Kind::Patch), vec!["ANTILOKI_FORGE_TOKEN"]);
        // the file lives outside every repository, because `.antiloki/` is inside one and gets shared
        let f = token_file("github.com").unwrap();
        assert!(f.ends_with("tokens/github.com"));
        assert!(!f.to_string_lossy().contains("/Projects/"), "never beside the code: {}", f.display());
        // a host that could climb out of that directory is refused outright
        assert_eq!(token_file("../../etc/passwd"), None);
        assert_eq!(token_file(""), None);
        // and a credential every process on the box can read is not a secret
        assert!(mode_is_private(0o600));
        assert!(mode_is_private(0o400));
        assert!(!mode_is_private(0o644), "world-readable");
        assert!(!mode_is_private(0o660), "group-readable");
    }

    // A TOKEN THAT CAN PRINT ITSELF EVENTUALLY PRINTS ITSELF. The first real run of this feature had a test call
    // `unwrap_err()` on an `Ok(Token)`, and the panic carried a live GitHub credential into a log.
    #[test]
    fn a_token_never_prints_itself_and_a_banner_is_never_mistaken_for_one() {
        let t = Token { value: "gho_supersecretvalue".into(), from: "gh auth token".into() };
        let shown = format!("{t:?} {t}");
        assert!(!shown.contains("gho_supersecretvalue"), "the secret leaked into its own Debug: {shown}");
        assert!(shown.contains("<redacted>") && shown.contains("gh auth token"));

        // `gh auth token` under mise prints a tool banner first; trimming the whole output made a two-line
        // "token" that every request would reject as a 401 with nothing to explain it
        assert_eq!(token_from_output("mise ~/.config/mise/config.toml tools: gh@2.98.0\ngho_realtokenvalue\n"), Some("gho_realtokenvalue".into()));
        assert_eq!(token_from_output("gho_plain\n"), Some("gho_plain".into()));
        assert_eq!(token_from_output("  gho_padded  "), Some("gho_padded".into()));
        assert_eq!(token_from_output("mise tools: gh@2.98.0\n"), None, "a banner alone is not a token");
        assert_eq!(token_from_output(""), None);
        assert_eq!(token_from_output("short\n"), None, "eight characters is the floor");
    }

    #[test]
    fn the_github_payloads_are_what_the_api_expects() {
        assert_eq!(github_api("github.com"), "https://api.github.com");
        assert_eq!(github_api("git.acme.com"), "https://git.acme.com/api/v3", "GitHub Enterprise lives under /api/v3");

        let req = PrRequest { root: "/r".into(), branch: "antiloki/dani/ep-2".into(), base: "main".into(),
            title: "Orders".into(), body: "the body".into() };
        let p = github_pr_payload(&req);
        assert_eq!(p["head"], json!("antiloki/dani/ep-2"));
        assert_eq!(p["base"], json!("main"));
        assert_eq!(p["title"], json!("Orders"));
        assert_eq!(p["maintainer_can_modify"], json!(true));
        // an untitled episode is named by its branch rather than opening a nameless pull request
        assert_eq!(github_pr_payload(&PrRequest { title: "  ".into(), ..req })["title"], json!("antiloki/dani/ep-2"));
    }

    #[test]
    fn a_gate_becomes_a_status_and_a_skipped_one_is_never_a_pass() {
        let pass = json!({ "gate": "test", "cmd": "npx vitest related 'src/a.ts'", "passed": true, "exit": 0 });
        let p = github_status_payload(&pass, "http://localhost/run/75");
        assert_eq!(p["state"], json!("success"));
        assert_eq!(p["context"], json!("antiloki / test"), "the name branch protection would require");
        assert!(p["description"].as_str().unwrap().starts_with("passed · npx vitest"));
        assert_eq!(p["target_url"], json!("http://localhost/run/75"));

        let fail = json!({ "gate": "build", "cmd": "npm run build", "passed": false, "exit": 2 });
        assert_eq!(github_status_payload(&fail, "")["state"], json!("failure"));
        assert!(github_status_payload(&fail, "")["description"].as_str().unwrap().contains("exit 2"));

        // THE ONE THAT MATTERS: a gate that could not run is `error`, never `success`. Reporting "no linter is
        // configured" as a green check is how a required check becomes a rubber stamp.
        let skipped = json!({ "gate": "lint", "passed": false, "skipped": "no `lint` script and no eslint in the box's package.json" });
        let sp = github_status_payload(&skipped, "");
        assert_eq!(sp["state"], json!("error"));
        assert!(sp["description"].as_str().unwrap().starts_with("not run — no `lint` script"));
    }

    #[test]
    fn an_api_refusal_is_read_back_as_something_a_person_can_act_on() {
        assert!(github_error(401, "{}").contains("wrong, expired, or not for this host"));
        assert!(github_error(403, r#"{"message":"API rate limit exceeded"}"#).contains("rate-limited"));
        assert!(github_error(403, r#"{"message":"Resource not accessible"}"#).contains("lacks permission"));
        assert!(github_error(404, "{}").contains("does not exist, or this token cannot see it"));
        assert!(github_error(422, r#"{"message":"A pull request already exists for x."}"#).contains("already exists"));
        assert!(github_error(500, "not json at all").contains("GitHub returned 500"));
    }

    #[test]
    fn the_patch_backend_needs_no_network_and_says_how_to_apply_it() {
        let d = std::env::temp_dir().join(format!("antiloki-forge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src")).unwrap();
        let root = d.to_string_lossy().to_string();
        let git = |a: &[&str]| { let _ = crate::proc::cmd("git").args(a).current_dir(&d).output(); };
        git(&["init", "-q", "-b", "main"]); git(&["config", "user.email", "t@t"]); git(&["config", "user.name", "t"]);
        std::fs::write(d.join("src/a.ts"), "export const a = 1;\n").unwrap();
        git(&["add", "-A"]); git(&["commit", "-qm", "base"]);
        git(&["checkout", "-q", "-b", "antiloki/dani/ep-1"]);
        std::fs::write(d.join("src/a.ts"), "export const a = 2;\n").unwrap();
        git(&["commit", "-qam", "the episode"]);

        let f = PatchForge::at(&root);
        let req = PrRequest { root: root.clone(), branch: "antiloki/dani/ep-1".into(), base: "main".into(),
            title: "Orders".into(), body: pr_body(&story(), "Orders", "antiloki/dani/ep-1") };
        let r = f.open_pr(&req).unwrap();
        assert_eq!(r.kind, "patch");
        let patch = std::fs::read_to_string(r.local.clone().unwrap()).unwrap();
        assert!(patch.contains("the episode"), "the commit message is in the series");
        assert!(patch.contains("-export const a = 1;") && patch.contains("+export const a = 2;"));
        let review = std::fs::read_to_string(d.join(".antiloki/outbox/antiloki-dani-ep-1.REVIEW.md")).unwrap();
        assert!(review.contains("## How it was made"), "the review document sits beside the patch");
        assert!(r.note.contains("git am"), "and it says how to apply it: {}", r.note);

        // a branch with nothing new is an error the operator can act on, not an empty file
        let empty = PrRequest { branch: "main".into(), ..req };
        assert!(f.open_pr(&empty).unwrap_err().contains("nothing to send"));
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod preflight_tests {
    use super::*;
    #[test]
    fn a_token_is_kept_owner_only_checked_for_shape_and_never_left_world_readable() {
        assert!(host_ok("github.com") && host_ok("git.acme-corp.dev") && !host_ok("") && !host_ok("../x") && !host_ok("a/b") && !host_ok(".hidden") && !host_ok("a b"));
        assert!(token_shape_ok("  ghp_abcdefghijklmnop  ").is_ok());
        assert!(token_shape_ok("short").is_err() && token_shape_ok("has a space inside it").is_err() && token_shape_ok("line\nbreak-token-here").is_err() && token_shape_ok(&"x".repeat(401)).is_err());
        assert!(verification_rejects("GitHub says this token is not valid (401) - it may be mistyped") && !verification_rejects("could not verify (connection refused)"));
        let dir = std::env::temp_dir().join(format!("antiloki-forge-tok-{}", std::process::id())).join("tokens"); let _ = std::fs::remove_dir_all(dir.parent().unwrap());
        let host = "forge-test.example";
        let path = save_token_in(&dir, host, "ghp_thisIsATestTokenValue").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "ghp_thisIsATestTokenValue");
        assert_eq!(token_from_output(&std::fs::read_to_string(&path).unwrap()).as_deref(), Some("ghp_thisIsATestTokenValue"), "the router's own reader takes what Settings wrote");
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; assert!(mode_is_private(std::fs::metadata(&path).unwrap().permissions().mode()), "the file is owner-only"); assert!(mode_is_private(std::fs::metadata(&dir).unwrap().permissions().mode()), "and so is its folder"); }
        assert!(stored_hosts_in(&dir).contains(&host.to_string()));
        assert!(save_token_in(&dir, host, "ghp_replacedWithANewTokenValue").is_ok());
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "ghp_replacedWithANewTokenValue");
        assert_eq!(remove_token_in(&dir, host), Ok(true)); assert_eq!(remove_token_in(&dir, host), Ok(false)); assert!(!stored_hosts_in(&dir).contains(&host.to_string()));
        assert!(save_token_in(&dir, "../evil", "ghp_abcdefghijklmnop").is_err());
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
    #[test]
    fn the_preflight_says_what_a_click_will_do_for_every_kind_of_repo() {
        let none = json!({});
        // a GitHub remote with a token: a real pull request, and the button says where
        let p = preflight_of("git@github.com:me/ministore.git", &none, "main", "antiloki/x/ep-1", true, Some("me"), Ok("tok"));
        assert_eq!((p["can_pr"].clone(), p["repo"].as_str(), p["kind"].as_str()), (json!(true), Some("me/ministore"), Some("github")));
        assert!(p["label"].as_str().unwrap().contains("me/ministore -> main"));
        // a GitHub remote but no token: not silently a patch - it says what is missing
        let p = preflight_of("https://github.com/me/r", &none, "main", "b", true, Some("me"), Err("no token for github.com"));
        assert_eq!(p["can_pr"], json!(false)); assert!(p["why"].as_str().unwrap().contains("no token"));
        // THE OPERATOR'S CASE: a local-only repo, gh logged in: offer to create the online repo
        let p = preflight_of("", &none, "main", "antiloki/main-cheeky-double/ep-3", true, Some("acme"), Err("no token"));
        assert_eq!((p["local_only"].clone(), p["publish_available"].clone(), p["can_pr"].clone()), (json!(true), json!(true), json!(false)));
        assert!(p["why"].as_str().unwrap().contains("local only") && p["why"].as_str().unwrap().contains("acme") && p["label"].as_str().unwrap().contains("create a private GitHub repo"));
        // local-only and no gh login: say how to get one
        let p = preflight_of("", &none, "main", "b", true, None, Err("x")); assert_eq!(p["publish_available"], json!(false)); assert!(p["why"].as_str().unwrap().contains("gh auth login"));
        let p = preflight_of("", &none, "main", "b", false, None, Err("x")); assert!(p["why"].as_str().unwrap().contains("not installed"));
        // a self-hosted host is not guessed
        let p = preflight_of("git@git.acme.dev:team/r.git", &none, "main", "b", true, Some("me"), Ok("t")); assert_eq!(p["can_pr"], json!(false)); assert_eq!(p["kind"], "patch");
    }
    #[test]
    fn gh_output_is_read_and_a_repo_name_is_checked() {
        assert_eq!(gh_account_from("github.com\n  ✓ Logged in to github.com account acme (keyring)\n  - Active account: true"), Some("acme".into()));
        assert_eq!(gh_account_from("You are not logged in to any GitHub hosts."), None);
        assert!(repo_name_ok("ministore") && repo_name_ok("my-app_2.0") && !repo_name_ok("") && !repo_name_ok("a b") && !repo_name_ok("../x") && !repo_name_ok("-rf") && !repo_name_ok(&"a".repeat(101)));
        assert_eq!(url_from_gh_output("✓ Created repository me/ministore on GitHub\n  https://github.com/me/ministore\n"), Some("https://github.com/me/ministore".into()));
        assert_eq!(publish_repo("/tmp", "bad name", true).unwrap_err().contains("not a repository name"), true, "validated before gh is ever run");
    }
}
