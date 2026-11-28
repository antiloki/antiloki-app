//! THE ROLES (operator 2026-09-23: "does someone would open an agent for a single job? … what I'd choose would be
//! super jobs — Backender, Frontender, Designer, Product Owner — agents that do multiple jobs under a category").
//!
//! A job (`cli::Job`) is a stage inside one run: plan, edit, verify, judge. Nobody hires a verifier. A ROLE is what a
//! person hires — a domain of the codebase — and inside it every one of the ten jobs still happens. So a role is not
//! an eleventh job; it is a PROFILE OVER ALL TEN, scoped to a domain:
//!
//! ```text
//! Backender = the files it owns · the laws that hold there · the packs it reads · and, per job, the merge that
//!             won ON THAT KIND OF WORK
//! ```
//!
//! The domain is derived from the files a task owns — deterministically, from the path — so every run that has ever
//! happened can be attributed without re-running it, and every new run tags itself.
//!
//! THE ADAPTER IS BIGGER THAN THE ROUTER (operator 2026-09-23: "the adapter is not just the router on the family and
//! the division in planner and worker, but the skills, specialists and tooling the agent has access"). Right — the
//! whole conditioning layer around the frozen models is the adapter: which model per job, the brain/hands split, the
//! SKILLS materialised into its box, the SPECIALISTS it may consult, the PANES it may drive, and the X-ray packs it
//! boots with. A Designer is not a different model from a Backender; it is a different adapter — Frames and the design
//! system in its hands, the design intel in its pack, the style lenses in its head.
//!
//! THREE OF EACH (operator: "under the role 3 options: Smarter, Cheaper and Best — each one is a winner in a
//! circumstance"). The same rows ranked under three objectives: `Cheaper` counts the bill, `Smarter` counts only
//! quality, `Best` is the balanced weight the table already carries. They line up with the effort bar's three stops.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// What kind of work a file is. Deterministic, from the path alone — never a model's opinion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Domain { Backend, Frontend, Design, Tests, Docs, Infra, Mixed, Unknown }
impl Domain {
    pub fn key(self) -> &'static str {
        match self { Domain::Backend => "backend", Domain::Frontend => "frontend", Domain::Design => "design",
                     Domain::Tests => "tests", Domain::Docs => "docs", Domain::Infra => "infra",
                     Domain::Mixed => "mixed", Domain::Unknown => "unknown" }
    }
    /// WHERE A DOMAIN LOOKS WHEN IT HAS NOT BEEN MEASURED (2026-09-23): its nearest neighbour first — design work is
    /// frontend work before it is anything else, infra is backend — then the shared pool, then everything pooled.
    /// A role always gets an answer; the sample count tells the operator how far it had to reach. Pure — tested.
    pub fn fallbacks(self) -> Vec<&'static str> {
        let near: &[&str] = match self {
            Domain::Design => &["frontend"],
            Domain::Frontend => &["design"],
            Domain::Infra | Domain::Docs => &["backend"],
            Domain::Tests => &["backend", "frontend"],
            Domain::Mixed | Domain::Unknown => &["backend", "frontend"],
            Domain::Backend => &[],
        };
        let mut out = vec![self.key()]; out.extend_from_slice(near); out.push("any"); out.push("*"); out
    }
    pub fn of(key: &str) -> Option<Domain> {
        Some(match key { "backend" => Domain::Backend, "frontend" => Domain::Frontend, "design" => Domain::Design,
                         "tests" => Domain::Tests, "docs" => Domain::Docs, "infra" => Domain::Infra,
                         "mixed" => Domain::Mixed, "unknown" => Domain::Unknown, _ => return None })
    }
}

/// The domain of ONE path. Tests win over everything (a test file is test work whatever it tests); then styling;
/// then a component by extension; then the server words in the path; else unknown. Pure — tested.
pub fn domain_of_path(path: &str) -> Domain {
    let p = path.to_ascii_lowercase();
    let file = p.rsplit('/').next().unwrap_or(&p);
    if file.contains(".test.") || file.contains(".spec.") || p.contains("/__tests__/") || p.starts_with("test/") || p.contains("/e2e/") { return Domain::Tests; }
    if file.ends_with(".md") || file.ends_with(".mdx") || p.contains("/docs/") { return Domain::Docs; }
    if file.ends_with(".css") || file.ends_with(".scss") || file.ends_with(".svg") || p.contains("/styles/") || p.contains("/design/") || p.contains("/theme") { return Domain::Design; }
    if file.ends_with(".tsx") || file.ends_with(".jsx") || file.ends_with(".vue") || file.ends_with(".svelte") || p.contains("/components/") || p.contains("/pages/") || p.contains("/screens/") { return Domain::Frontend; }
    if file.ends_with("dockerfile") || file.ends_with(".yml") || file.ends_with(".yaml") || file.ends_with(".tf") || p.contains("/.github/") || p.contains("/ci/") || p.contains("/deploy/") { return Domain::Infra; }
    for w in ["/api/", "/server/", "/db/", "/database/", "/migrations/", "/repositor", "/service", "/handler", "/controller", "/storage", "/persistence"] {
        if p.contains(w) { return Domain::Backend; }
    }
    if file.ends_with(".ts") || file.ends_with(".js") || file.ends_with(".rs") || file.ends_with(".go") || file.ends_with(".py") || file.ends_with(".sql") { return Domain::Backend; }
    Domain::Unknown
}

/// The domain of a SET of paths: the one they agree on, `Mixed` when two real domains are both substantial, and the
/// majority when one clearly dominates (a backend change that touches one stylesheet is still backend). Pure — tested.
pub fn domain_of(paths: &[String]) -> Domain {
    let mut counts: Vec<(Domain, usize)> = vec![];
    for p in paths.iter().filter(|p| !p.contains(['*', '?', '['])) {
        let d = domain_of_path(p);
        if d == Domain::Unknown { continue; }
        match counts.iter_mut().find(|(k, _)| *k == d) { Some((_, n)) => *n += 1, None => counts.push((d, 1)) }
    }
    if counts.is_empty() { return Domain::Unknown; }
    counts.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    let total: usize = counts.iter().map(|(_, n)| *n).sum();
    let (top, n) = counts[0];
    // one domain owns two thirds or more of the files → it is that domain; otherwise the work is genuinely mixed
    if n * 3 >= total * 2 { top } else { Domain::Mixed }
}

/// HOW A ROLE IS RANKED. The same rows, three questions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Objective { Cheaper, Faster, Best, Smarter }
impl Objective {
    /// THE THREE AN OPERATOR PICKS (2026-09-23: "add on the creation of agent: Cheaper, faster and Best"). `Smarter`
    /// is a fourth the API still answers to — it is what stored rows and the older clients say — but it is not one
    /// of the three offered, because "all quality at any price" is a research setting, not a hire.
    pub const OFFERED: [Objective; 3] = [Objective::Cheaper, Objective::Faster, Objective::Best];
    pub const ALL: [Objective; 4] = [Objective::Cheaper, Objective::Faster, Objective::Best, Objective::Smarter];
    pub fn key(self) -> &'static str { match self { Objective::Cheaper => "cheaper", Objective::Faster => "faster", Objective::Best => "best", Objective::Smarter => "smarter" } }
    pub fn of(key: &str) -> Option<Objective> {
        Some(match key { "cheaper" => Objective::Cheaper, "faster" | "fastest" => Objective::Faster, "best" | "balanced" => Objective::Best, "smarter" | "stronger" => Objective::Smarter, _ => return None })
    }
    /// (quality, cost, time) — what this objective counts. `Best` is the table's own weight.
    /// PRICE FIRST, SPEED SECOND, QUALITY AS A TOLERANCE (operator 2026-09-26, after the first real comparison:
    /// "quality almost similar with a lot of price reduction is far more important than speed. Make that a rule").
    ///
    /// The measurement that produced the rule: on SWT-bench, raw Sonnet reproduced 9 of 10 against Qwen's 8 — ONE
    /// instance, which is the resolution of a ten-instance run and therefore noise — while costing 6.7× as much
    /// ($3.74 against $0.56). The old weights called that a win for Sonnet. They were answering "which is best?"
    /// when the product's question is "what should I spend?".
    ///
    /// So inside the quality band the order is COST, then TIME, then quality. Quality has not stopped mattering —
    /// it is the `band` below that enforces it, as a gate rather than as a score, which is the honest shape: a
    /// materially worse answer is disqualified outright, and among answers that are materially the same, the bill
    /// decides.
    pub fn weights(self) -> (f64, f64, f64) {
        match self {
            // the bill above everything the band already let through
            Objective::Cheaper => (0.10, 0.75, 0.15),
            // Faster is Cheaper's mirror: the same tolerance for a slightly worse answer, spent on the clock
            // instead of the bill. It is a real axis because the bench records seconds per run, not an estimate.
            Objective::Faster => (0.15, 0.25, 0.60),
            // BALANCED IS NOT QUALITY-FIRST ANY MORE. It was (0.70, 0.20, 0.10) — a default that would pay 6.7×
            // for one instance in ten.
            // SPEED IS THE LAST THING (operator 2026-09-28: "speed … i told you wasn't important"). It was 0.30 — enough to
            // let the fastest frontier planner beat a model that measured the same for a tenth of the price.
            Objective::Best => (0.15, 0.75, 0.10),
            // the one objective that still buys quality at any price; it is not offered in the UI
            Objective::Smarter => (1.00, 0.00, 0.00),
        }
    }
    /// The quality band this objective allows below the best. `Smarter` allows none — it is asked for the best answer,
    /// not the best bargain; the others allow the operator's tolerance: keep at least 80 % of the best.
    /// THE TOLERANCE — how much worse an answer may be before it is voted down rather than merely out-priced.
    ///
    /// HISTORY: it was 0.06 absolute, narrower than the instrument (a ten-instance bench resolves quality in steps of
    /// 0.10, so 0.06 disqualified a ONE-INSTANCE difference - that is what happened to Qwen on SWT-bench), then 0.10
    /// absolute, one instance in ten. Both were ABSOLUTE and are gone: anything inside the band is "materially the
    /// same answer" and then price decides; anything outside it is a real gap and no discount buys it back. `Smarter`
    /// keeps a band of zero, because its whole point is refusing the trade.
    ///
    /// ONE TOLERANCE, RELATIVE (operator 2026-09-30, after the record showed it stated three ways - "three points" in the UI, 0.10 absolute in
    /// the code, "80 % of the frontier" in the operator's words): the value is the FRACTION OF THE BEST MEASURED QUALITY a merge may give up.
    /// 0.20 = it must keep at least 80 % of the frontier's score, and inside that the order is cost, then time. It is safe to be this generous
    /// because every run ends in the QA gate, which sends unmet work back; the gate itself is chosen on its false-accept rate, not on price
    /// (`cli.rs`), so this band never decides who guards the door. `Smarter` gives up nothing.
    pub fn band(self) -> f64 { match self { Objective::Smarter => 0.0, _ => 0.20 } }
    /// the lowest quality that still counts as close to `q_best` - the one place the tolerance is applied
    pub fn floor(self, q_best: f64) -> f64 { q_best * (1.0 - self.band()) - 1e-9 }
}

/// A ROLE the operator hires. `globs` scope it; `domain` is what its work is; `general` marks the broad ones from the
/// specialised ones ("a backend database", "a frontend architect") that sit under them.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Role {
    pub key: &'static str, pub name: &'static str, pub domain: Domain, pub general: bool,
    /// the role this one specialises, when it is not general
    pub under: Option<&'static str>,
    pub globs: &'static [&'static str],
    pub about: &'static str,
    /// THE ADAPTER, the rest of it — what this role knows and what it may drive.
    /// `skills` are materialised into its box (skilllib), `specialists` are the lenses it may consult
    /// (constitution agents), `panes` are the antiloki tools it can open and work in, `packs` are the X-ray
    /// items it boots with.
    pub skills: &'static [&'static str],
    pub specialists: &'static [&'static str],
    pub panes: &'static [&'static str],
    pub packs: &'static [&'static str],
}

pub const ROLES: &[Role] = &[
    Role { key: "backender", name: "Backender", domain: Domain::Backend, general: true, under: None,
        globs: &["src/api/**", "src/server/**", "src/services/**", "src/**/*.ts", "src/**/*.rs", "src/**/*.go", "src/**/*.py"],
        about: "the data, the rules and the endpoints — everything the screen calls",
        skills: &["antiloki-xray-first", "antiloki-scoped-diffs", "antiloki-verify-claims", "antiloki-test-after-task"],
        specialists: &["correctness:errors", "correctness:types", "architecture:dependencies", "security:injection", "category:correctness"],
        panes: &["atlas-graph", "atlas-code", "diff"], packs: &["laws", "graph", "blast", "tests"] },
    Role { key: "backend-database", name: "Database", domain: Domain::Backend, general: false, under: Some("backender"),
        globs: &["**/migrations/**", "**/db/**", "**/schema*", "**/*.sql", "**/repositor*/**", "**/storage*"],
        about: "the schema, the migrations and the queries — where the data actually lives",
        skills: &["antiloki-blast-radius", "antiloki-scoped-diffs", "antiloki-verify-claims"],
        specialists: &["correctness:rigor", "security:injection", "architecture:dependencies", "category:security"],
        panes: &["atlas-graph", "diff"], packs: &["laws", "graph", "blast"] },
    Role { key: "backend-api", name: "API", domain: Domain::Backend, general: false, under: Some("backender"),
        globs: &["src/api/**", "**/handlers/**", "**/controllers/**", "**/routes/**"],
        about: "the contract the outside world sees: endpoints, payloads, errors, versions",
        skills: &["antiloki-verify-claims", "antiloki-docs-lookup", "antiloki-test-after-task"],
        specialists: &["correctness:errors", "correctness:types", "security:secrets", "framework:idioms"],
        panes: &["atlas-graph", "intent", "diff"], packs: &["laws", "graph", "doc"] },
    Role { key: "frontender", name: "Frontender", domain: Domain::Frontend, general: true, under: None,
        globs: &["src/**/*.tsx", "src/**/*.jsx", "src/components/**", "src/pages/**", "src/screens/**"],
        about: "the screens: components, state, routing, the things a person clicks",
        skills: &["antiloki-xray-first", "antiloki-scoped-diffs", "antiloki-file-review"],
        specialists: &["framework:idioms", "style:suppressions", "correctness:types", "category:framework"],
        panes: &["design-view:components", "atlas-code", "diff"], packs: &["laws", "graph", "file"] },
    Role { key: "frontend-designer", name: "Frontend designer", domain: Domain::Design, general: false, under: Some("frontender"),
        globs: &["src/**/*.css", "src/**/*.scss", "src/styles/**", "src/theme*", "src/**/*.svg"],
        about: "type, spacing, colour and motion — how the screen looks and feels",
        // THE DESIGNER IS ITS TOOLING (operator 2026-09-23: "the designer could be very good and trained and have
        // control over the design at X-ray and at the design tools like Frames to generate new designs, new pages,
        // or alter some screen design"). Frames and the design system are its hands; the design intel is its pack.
        skills: &["antiloki-xray-first", "antiloki-file-review"],
        specialists: &["category:style", "style:suppressions", "framework:idioms", "axis:cohesion"],
        panes: &["design-frames", "design-view:system", "design-view:components", "design"],
        packs: &["laws", "file", "doc"] },
    Role { key: "frontend-architect", name: "Frontend architect", domain: Domain::Frontend, general: false, under: Some("frontender"),
        globs: &["src/**/*.tsx", "src/**/hooks/**", "src/**/store/**", "src/**/context/**"],
        about: "the shape of the client: state, boundaries, data flow, what re-renders",
        skills: &["antiloki-blast-radius", "antiloki-xray-first", "antiloki-constitution-check"],
        specialists: &["architecture:dependencies", "axis:coupling", "axis:structure", "category:architecture"],
        panes: &["atlas-graph", "atlas-diagram", "design-view:components"], packs: &["graph", "blast", "laws"] },
    Role { key: "tester", name: "Tester", domain: Domain::Tests, general: true, under: None,
        globs: &["**/*.test.*", "**/*.spec.*", "**/__tests__/**", "test/**", "e2e/**"],
        about: "the suite: what is covered, what is not, and what a regression would look like",
        skills: &["antiloki-test-after-task", "antiloki-verify-claims", "antiloki-blast-radius"],
        specialists: &["testing:isolation", "category:testing", "axis:tests", "correctness:rigor"],
        panes: &["run", "atlas-xray", "diff"], packs: &["tests", "blast", "laws"] },
    Role { key: "reviewer", name: "Reviewer", domain: Domain::Mixed, general: true, under: None,
        globs: &["**"],
        about: "reads everything, writes nothing — the verdict, the laws, the risk of a change",
        skills: &["antiloki-file-review", "antiloki-constitution-check", "antiloki-verify-claims", "antiloki-blast-radius"],
        specialists: &["axis:defects", "axis:compliance", "axis:change", "book:warden", "category:correctness"],
        panes: &["atlas-xray", "atlas-constitution", "diff"], packs: &["laws", "graph", "blast", "tests"] },
    Role { key: "product-owner", name: "Product owner", domain: Domain::Docs, general: true, under: None,
        globs: &["**/*.md", "docs/**", "README*"],
        about: "the ask before the code: scope, acceptance, what is in and what is out",
        skills: &["antiloki-docs-lookup", "antiloki-xray-first"],
        specialists: &["book:definer", "book:questioneer", "book:synthesizer", "book:comparer"],
        panes: &["intent", "vibe-todo", "vibe-kanban", "context"], packs: &["doc", "folder"] },
    Role { key: "infra", name: "Infra", domain: Domain::Infra, general: true, under: None,
        globs: &[".github/**", "**/Dockerfile*", "**/*.yml", "**/*.yaml", "deploy/**", "ci/**"],
        about: "the pipeline and the box: builds, images, workflows, deploys",
        skills: &["antiloki-constitution-check", "antiloki-scoped-diffs"],
        specialists: &["security:secrets", "book:warden", "correctness:rigor"],
        panes: &["terminal", "run", "atlas-git"], packs: &["laws", "doc"] },
];

/// THE PURPOSE PICKS THE ROLE (operator 2026-09-23: "on agent creation we prompt the purpose of the agent and a new
/// role is suggested — the best options for that job as the brain and hand, skills and specialists"). Deterministic:
/// the words the operator used against each role's own vocabulary, most specific role first so "database migrations"
/// lands on Database rather than on Backender. Pure — tested.
const WORDS: &[(&str, &[&str])] = &[
    ("backend-database", &["database", "schema", "migration", "sql", "query", "postgres", "sqlite", "orm", "index", "table"]),
    ("backend-api", &["api", "endpoint", "rest", "graphql", "route", "handler", "controller", "payload", "webhook", "contract"]),
    ("frontend-designer", &["design", "designer", "css", "styling", "style", "colour", "color", "typography", "spacing", "figma", "frames", "look", "theme", "visual", "layout"]),
    ("frontend-architect", &["architecture", "state management", "re-render", "rerender", "data flow", "boundaries", "hooks", "store", "context", "refactor the client"]),
    // THE CONTROLS A SCREEN IS MADE OF (2026-09-28: "add a sort dropdown for the products" said no frontend word at all, so
    // the one word left — "products" — sent it to the Product owner and its docs merge)
    ("frontender", &["frontend", "front-end", "screen", "page", "component", "ui", "react", "button", "form", "click", "view", "dropdown", "select", "filter", "filters", "sort", "modal", "menu", "tab", "tabs", "toggle", "checkbox", "input", "search box", "navbar", "sidebar", "card", "grid", "cart", "widget", "hover", "responsive",
        // the parts of a page (found 2026-09-29: "remove the footer" and "remove the search" had no frontend word, so the fallback sent them to the Backender)
        "footer", "header", "hero", "banner", "storefront", "dark", "theme", "themes", "layout", "landing", "heading", "logo", "icon", "badge", "tooltip", "dark mode", "light mode", "dark theme", "search", "search bar", "carousel", "slider", "breadcrumb", "pagination", "drawer", "popover", "dialog", "accordion", "avatar", "product list", "product grid", "product card",
        // …and the same parts in the operator's languages (the layer is language-agnostic; the lists are per-language data)
        "botão", "botões", "botao", "botoes", "rodapé", "rodape", "cabeçalho", "cabecalho", "página", "pagina", "busca", "pesquisa", "filtro", "filtros", "formulário", "formulario", "tela", "modo escuro", "tema escuro", "menu suspenso", "lista suspensa", "barra lateral", "cartão", "cartao", "lista de produtos",
        "botón", "boton", "botones", "pie de página", "encabezado", "búsqueda", "busqueda", "formulario", "modo oscuro", "tema oscuro", "desplegable", "barra lateral", "tarjeta", "lista de productos",
        "bouton", "boutons", "pied de page", "en-tête", "recherche", "formulaire", "mode sombre", "thème sombre", "theme sombre", "carte produit",
        "schaltfläche", "schaltflaeche", "fußzeile", "fusszeile", "kopfzeile", "suche", "formular", "dunkelmodus", "seitenleiste", "produktliste"]),
    ("tester", &["test", "tests", "testing", "coverage", "regression", "vitest", "jest", "spec", "e2e", "flaky"]),
    ("reviewer", &["review", "audit", "judge", "verdict", "quality", "law", "laws", "compliance", "risk", "smell"]),
    // "product" ALONE IS A NOUN OF THE APP, NOT A JOB: a shop's asks are all about products. The role's own phrases stay.
    ("product-owner", &["product owner", "product spec", "product brief", "product requirements", "spec", "requirement", "acceptance", "scope", "roadmap", "user story", "stories", "brief", "documentation", "readme"]),
    ("infra", &["infra", "deploy", "pipeline", "ci", "cd", "docker", "kubernetes", "workflow", "release", "build server"]),
    ("backender", &["backend", "back-end", "server", "service", "business logic", "persistence", "cache", "job queue", "worker"]),
];
/// DOES THIS TEXT ACTUALLY SAY THIS WORD (audit 2026-09-28)? `suggest_role` asked `contains`, and a plain substring
/// finds a word inside every other word that happens to spell it. The misfires were not theoretical: "make the
/// **build** faster" carries `ui` and became a frontend job, "be **specific**" carries both `spec` and `ci` and became
/// documentation or infrastructure, "**performance**" and "trans**form**" carry `orm` and `form`, "**rapid**" carries
/// `api`, "**review**" carries `view`, "accep**table**" carries `table`, "f**law**ed" carries `law`. A role chosen
/// this way sends the work to the wrong specialist and the wrong measured merge with it.
///
/// So a match must begin at a word boundary. A SHORT entry — four characters or fewer, which is where the damage is:
/// `ci`, `ui`, `api`, `orm`, `sql`, `law`, `spec`, `test`, `form`, `page`, `view` — must be the WHOLE word, because a
/// two-letter prefix is barely evidence of anything. A longer entry may match as a prefix, so `index` still finds
/// "indexes" and `design` still finds "designer" without the lists having to spell out every inflection. Pure — tested.
fn says_word(text: &str, word: &str) -> bool {
    if word.is_empty() { return false }
    let whole_only = word.chars().count() <= 4;
    let (t, w) = (text.as_bytes(), word.as_bytes());
    let boundary = |c: Option<&u8>| !matches!(c, Some(b) if b.is_ascii_alphanumeric());
    let mut i = 0usize;
    while let Some(k) = t[i..].windows(w.len()).position(|s| s == w) {
        let at = i + k;
        let end = at + w.len();
        if boundary(at.checked_sub(1).and_then(|j| t.get(j))) && (!whole_only || boundary(t.get(end))) { return true }
        i = at + 1;
        if i + w.len() > t.len() { break }
    }
    false
}

pub fn suggest_role(purpose: &str) -> &'static Role { suggest_role_words(purpose).unwrap_or_else(|| role("backender").expect("the fallback role exists")) }
/// The role the ask's own words name - `None` when they name none.
fn suggest_role_words(purpose: &str) -> Option<&'static Role> {
    let p = purpose.to_ascii_lowercase();
    let mut best: Option<(&'static Role, usize)> = None;
    for (key, words) in WORDS {
        let hits = words.iter().filter(|w| says_word(&p, w)).count();
        if hits == 0 { continue; }
        let r = role(key).expect("a word list names a role that exists");
        // more words wins; on a tie the more specific role wins, because it was listed first
        if best.map_or(true, |(_, n)| hits > n) { best = Some((r, hits)); }
    }
    best.map(|(r, _)| r)
}

/// WHEN THE WORDS SAY NOTHING, THE WORKSPACE DOES (operator 2026-09-30, "once more": *"i asked a frontend task and still once more heimdall chose the
/// backender"* - "remove the dark version of the storefront" names no word in any list, so the empty match fell back to the Backender, and the word lists had
/// been extended for the same reason four times). A list of words can never be complete; the repository is the evidence that always is: the ask's own
/// words are searched in the tracked files, and the domain those files belong to is the domain of the work.
///
/// A word that is everywhere says nothing ("remove" is in half the files), so only the words that point somewhere count: at least one file, at most 40.
/// Docs and lockfiles are left out of the search, since they repeat everything. `None` when the files do not agree (two thirds in one domain) or the
/// repository says nothing; the caller then keeps the honest fallback. The search is `git grep` - a few milliseconds a word, and the result is only ever used
/// when the word lists found nothing. Pure over the files list - `role_from_hits` is tested; the search itself is exercised against a real repository.
pub fn role_from_hits(hits: &[Vec<String>]) -> Option<&'static Role> {
    let mut files: Vec<String> = vec![];
    for h in hits.iter().filter(|h| !h.is_empty() && h.len() <= 40) { for f in h { if !files.contains(f) { files.push(f.clone()); } } }
    // A SCREEN IS MADE OF MARKUP AND STYLE (found by the first test of this): "the dark version of the storefront" lives in a .tsx AND a .css, which the domain
    // count calls mixed. For this question the two are one family - the screen - and it is settled by the component files, where the work is done.
    let screen = |f: &&String| matches!(domain_of_path(f), Domain::Frontend | Domain::Design);
    let others = files.iter().filter(|f| domain_of_path(f) != Domain::Unknown).filter(|f| !screen(f)).count();
    let screens: Vec<String> = files.iter().filter(screen).cloned().collect();
    if !screens.is_empty() && screens.len() >= 2 * others {
        let comp: Vec<String> = screens.iter().filter(|f| domain_of_path(f) == Domain::Frontend).cloned().collect();
        return role_for_task(if comp.is_empty() { &screens } else { &comp });
    }
    match domain_of(&files) { Domain::Unknown | Domain::Mixed => None, _ => role_for_task(&files) }
}
pub fn role_by_workspace(ask: &str, cwd: &str) -> Option<&'static Role> {
    if cwd.trim().is_empty() || !std::path::Path::new(cwd).is_dir() { return None }
    const SKIP: &[&str] = &["that", "this", "with", "from", "into", "please", "should", "would", "could", "version", "versions", "remove", "delete", "make", "just", "then", "when", "have", "want", "need", "your", "there", "their", "which", "what", "where", "about", "after", "before", "also", "only", "every", "each", "some", "more", "less", "same", "thing", "things", "change", "update", "create", "write", "build", "show"];
    let mut words: Vec<String> = vec![];
    for w in ask.to_ascii_lowercase().split(|c: char| !c.is_ascii_alphanumeric()) {
        if w.len() >= 4 && !SKIP.contains(&w) && !words.iter().any(|x| x == w) { words.push(w.to_string()); }
        if words.len() >= 8 { break }
    }
    let hits: Vec<Vec<String>> = words.iter().map(|w| {
        let out = crate::proc::cmd("git").args(["grep", "-I", "-l", "-i", "-w", "-F", "-e", w.as_str(), "--", ".", ":!*.md", ":!*.lock", ":!package-lock.json", ":!*.json", ":!*.svg"]).current_dir(cwd).output().ok();
        out.filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).lines().take(41).map(str::to_string).collect()).unwrap_or_default()
    }).collect();
    role_from_hits(&hits)
}
/// `suggest_role`, then - only when the words found nothing - the workspace. A role the words chose is never overruled by the files.
pub fn suggest_role_in(purpose: &str, cwd: &str) -> &'static Role {
    let by_words = suggest_role_words(purpose);
    match by_words { Some(r) => r, None => role_by_workspace(purpose, cwd).unwrap_or_else(|| role("backender").expect("the fallback role exists")) }
}

pub fn role(key: &str) -> Option<&'static Role> { ROLES.iter().find(|r| r.key == key) }
/// The roles a set of paths belongs to, most specific first — a specialised role beats the general one it sits under. Pure.
pub fn roles_for(paths: &[String]) -> Vec<&'static Role> {
    let d = domain_of(paths);
    let mut out: Vec<&'static Role> = ROLES.iter().filter(|r| r.domain == d).collect();
    out.sort_by_key(|r| r.general); // specialised first
    if out.is_empty() { out = ROLES.iter().filter(|r| r.domain == Domain::Mixed).collect(); }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn v(xs: &[&str]) -> Vec<String> { xs.iter().map(|s| s.to_string()).collect() }

    /// A WORD INSIDE ANOTHER WORD IS NOT THAT WORD (audit 2026-09-28). `suggest_role` asked `contains`, so every
    /// short entry in the word lists fired on any longer word that happened to spell it. These are the real asks that
    /// went to the wrong specialist — and with the specialist goes the measured merge that does the work.
    #[test]
    fn an_app_noun_is_not_a_role_and_a_control_is_a_frontend_word() {
        for ask in ["add a sort dropdown for the products", "add filters to the product list", "add a toggle to the cart"] {
            assert_eq!(suggest_role(ask).key, "frontender", "{ask}");
        }
        // parts of a page, and the same asks in other languages, are frontend work - not the Backender the empty match falls back to
        for ask in ["remove the dark version of the storefront", "remove the footer", "remove the search", "add a header", "adicione um botão no cabeçalho", "remova o rodapé", "añade un botón de búsqueda", "ajoute un bouton dans le pied de page", "füge eine Suche in die Kopfzeile ein"] {
            assert_eq!(suggest_role(ask).key, "frontender", "{ask}");
        }
        assert_eq!(suggest_role("write the product spec for checkout").key, "product-owner");
        assert_eq!(suggest_role("update the readme").key, "product-owner");
    }

    /// THE WORKSPACE ANSWERS WHEN THE WORDS DO NOT (operator 2026-09-30, "once more"): "remove the dark version of the storefront" names no word in any list.
    #[test]
    fn an_ask_whose_words_name_no_role_is_read_from_the_files_it_talks_about() {
        let hit = |fs: &[&str]| fs.iter().map(|f| f.to_string()).collect::<Vec<_>>();
        // "dark" points at the stylesheets and the screen; "storefront" at the screen; "remove" is everywhere and says nothing (more than 40 files)
        let everywhere: Vec<String> = (0..60).map(|i| format!("server/handler{i}.rs")).collect();
        let r = role_from_hits(&[hit(&["web/src/theme.css", "web/src/Shop.tsx"]), hit(&["web/src/Shop.tsx"]), everywhere]).expect("the files agree");
        assert!(matches!(r.domain, Domain::Frontend | Domain::Design), "{}", r.key);
        // files that disagree say nothing - the honest fallback stands
        assert!(role_from_hits(&[hit(&["web/src/Shop.tsx", "server/db/orders.rs"])]).is_none());
        assert!(role_from_hits(&[]).is_none() && role_from_hits(&[vec![]]).is_none());
    }
    #[test]
    fn the_workspace_is_searched_only_when_the_words_found_nothing() {
        let dir = std::env::temp_dir().join(format!("antiloki-roles-ws-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(dir.join("web/src")).unwrap(); std::fs::create_dir_all(dir.join("server/api")).unwrap();
        std::fs::write(dir.join("web/src/Shop.tsx"), "export const Shop = () => <div className=\"midnight\">midnight palette</div>;\n").unwrap();
        std::fs::write(dir.join("web/src/palette.css"), ".midnight { background: #000 }\n").unwrap();
        std::fs::write(dir.join("server/api/orders.rs"), "fn orders() {}\n").unwrap();
        let git = |a: &[&str]| { crate::proc::cmd("git").args(a).current_dir(&dir).output().unwrap(); };
        git(&["init", "-q"]); git(&["add", "-A"]);
        // no list names "midnight": the words alone fall to the Backender, the files say frontend
        assert_eq!(suggest_role("switch the midnight mode off").key, "backender");
        let r = suggest_role_in("switch the midnight mode off", dir.to_str().unwrap());
        assert!(matches!(r.domain, Domain::Frontend | Domain::Design), "{}", r.key);
        // an ask the words DO name is never overruled by the files
        assert_eq!(suggest_role_in("add an index to the orders table", dir.to_str().unwrap()).key, "backend-database");
        // no workspace, no evidence: the old fallback, exactly
        assert_eq!(suggest_role_in("switch the midnight mode off", "").key, "backender");
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn a_role_is_suggested_by_words_the_ask_actually_says() {
        // the mechanism, at the boundary
        assert!(says_word("make the ui prettier", "ui"));
        assert!(!says_word("make the build faster", "ui"), "build is not a UI word");
        assert!(!says_word("be specific about it", "ci"), "specific is not continuous integration");
        assert!(!says_word("improve performance", "orm"), "performance is not an ORM");
        assert!(!says_word("transform the payload", "form"), "transform is not a form");
        assert!(!says_word("a rapid change", "api"));
        assert!(!says_word("a flawed idea", "law"));
        assert!(!says_word("an acceptable answer", "table"));
        assert!(!says_word("restore the file", "store"));
        assert!(!says_word("review the diff", "view"));
        // …while a longer entry still reaches its own inflections without the list spelling each one out
        assert!(says_word("add an index to orders", "index") && says_word("indexes are missing", "index"));
        assert!(says_word("ask the designer", "design"));
        assert!(says_word("ci/cd is red", "ci"), "a slash is a boundary");
        assert!(says_word("front-end work", "front-end"), "a hyphen inside the entry is part of it");

        // and the whole point: the ask that used to be misrouted
        // it no longer goes to the FRONTEND, which is what "build" containing "ui" used to do. Nothing in the lists
        // claims a bare "build" (infra says "build server"), so it falls back — an honest miss beats a wrong hit.
        assert_eq!(suggest_role("make the build faster").key, "backender");
        assert_eq!(suggest_role("the build server is slow").key, "infra", "named in full, infra takes it");
        assert_eq!(suggest_role("add an index to the orders table").key, "backend-database");
        // "restyle" does NOT carry "style" — it is one word — so the designer is chosen by what the ask really says
        assert_eq!(suggest_role("restyle the buttons").key, "frontender", "buttons are frontend; nothing here says design");
        assert_eq!(suggest_role("the spacing and typography are off").key, "frontend-designer");
        assert_eq!(suggest_role("write tests for the checkout").key, "tester");
        // an ask that names nothing still gets a worker rather than an error
        assert_eq!(suggest_role("do the thing").key, "backender");
    }

    /// THE PATHS CHOOSE, NOT THE DECLARATION ORDER (audit 2026-09-28). `role_for_task` took the first specialised
    /// role of the domain, so `backend-api` and `frontend-architect` could never be chosen by it at all — every
    /// backend task went to the database specialist and every frontend task to the designer, whatever the files said.
    #[test]
    fn a_task_goes_to_the_specialist_its_own_files_name() {
        let r = |xs: &[&str]| role_for_task(&v(xs)).map(|r| r.key).unwrap_or("-");
        assert_eq!(r(&["src/orders/api.ts", "src/routes/webhook.ts"]), "backend-api");
        assert_eq!(r(&["db/schema.sql", "migrations/003_index.sql"]), "backend-database");
        // a .ts file is Backend however it is named, so a FRONTEND example has to actually be frontend
        assert_eq!(r(&["src/hooks/use-cart.tsx", "src/context/cart.tsx"]), "frontend-architect");
        assert_eq!(r(&["src/styles/theme.css"]), "frontend-designer");
        // a backend path that names neither specialty still gets a worker — the narrowest, as before
        let quiet = r(&["src/orders/thing.ts"]);
        assert!(!quiet.is_empty() && quiet != "-", "an unnamed task is still assigned: {quiet}");
        // and paths that belong to nothing are still nobody's
        assert_eq!(role_for_task(&v(&[])), None);
    }

    #[test]
    fn a_path_knows_what_kind_of_work_it_is() {
        assert_eq!(domain_of_path("src/orders/api.ts"), Domain::Backend);
        assert_eq!(domain_of_path("src/orders/OrdersPage.tsx"), Domain::Frontend);
        assert_eq!(domain_of_path("src/orders/api.test.ts"), Domain::Tests, "a test of the API is test work");
        assert_eq!(domain_of_path("src/theme.css"), Domain::Design);
        assert_eq!(domain_of_path("src/styles/tokens.ts"), Domain::Design, "the styles folder wins over the .ts");
        assert_eq!(domain_of_path(".github/workflows/ci.yml"), Domain::Infra);
        assert_eq!(domain_of_path("README.md"), Domain::Docs);
        assert_eq!(domain_of_path("db/migrations/003_orders.sql"), Domain::Backend);
        assert_eq!(domain_of_path("LICENSE"), Domain::Unknown);
    }

    #[test]
    fn a_task_takes_the_domain_of_the_files_it_owns() {
        assert_eq!(domain_of(&v(&["src/orders/api.ts", "src/orders/storage.ts", "src/orders/totals.ts"])), Domain::Backend);
        // a backend change that touches one stylesheet is still backend — two thirds carries it
        assert_eq!(domain_of(&v(&["src/orders/api.ts", "src/orders/storage.ts", "src/orders/orders.css"])), Domain::Backend);
        // an even split is honestly mixed, not a coin toss
        assert_eq!(domain_of(&v(&["src/orders/api.ts", "src/orders/OrdersPage.tsx"])), Domain::Mixed);
        assert_eq!(domain_of(&v(&["**"])), Domain::Unknown, "a glob owns nothing in particular");
        assert_eq!(domain_of(&[]), Domain::Unknown);
    }

    #[test]
    fn a_merge_needs_both_of_its_engines_here_and_an_unbound_side_is_no_obstacle() {
        // under #[cfg(test)] engine_available is always true, so this pins the SHAPE: an unbound side never blocks
        // (the router picks from what is installed), and an engine nobody knows always does.
        assert!(merge_runnable(None, None), "unbound on both sides is the router's job, not a blocker");
        assert!(merge_runnable(Some("claude-code:sonnet"), None));
        // OPENCODE IS OFF BY DEFAULT (2026-09-27), so a merge naming it is NOT runnable until the operator turns Zen
        // on — the point of the switch. Both directions are asserted so the policy cannot silently invert.
        assert!(!merge_runnable(Some("claude-code:sonnet"), Some("opencode:opencode/deepseek-v4-flash")),
                "with Zen off, a merge that names the gateway must not be offered");
        crate::cli::set_provider_flag("zen", true).expect("the switch is writable");
        assert!(merge_runnable(Some("claude-code:sonnet"), Some("opencode:opencode/deepseek-v4-flash")),
                "turned on, the same merge is runnable");
        crate::cli::set_provider_flag("zen", false).expect("back off");
        assert!(!merge_runnable(Some("ollama:llama"), None), "an engine we do not drive is not runnable");
        assert!(!merge_runnable(None, Some("lmstudio:whatever")));
    }


    #[tokio::test]
    async fn a_local_worker_is_runnable_only_as_the_hands_and_only_when_the_local_seat_says_so() {
        let _g = crate::local::tests::env_lock();
        let base = crate::local::tests::server(|path, _| match path { "/api/tags" => json!({"models": [{"name": "tiny:1b", "digest": "d", "size": 1000, "details": {}}]}), _ => json!({}) });
        for k in ["ANTILOKI_LOCAL_WORKER", "ANTILOKI_LOCAL_NUM_CTX"] { std::env::remove_var(k); }
        std::env::set_var("ANTILOKI_LOCAL_URL", &base);
        crate::local::refresh().await;
        let planner = Some("claude-code:sonnet");
        assert!(!merge_runnable(planner, Some("local:tiny:1b")), "E7: the seat is off by default, so no merge names it");
        std::env::set_var("ANTILOKI_LOCAL_WORKER", "1");
        crate::local::refresh().await;
        assert!(!merge_runnable_with(None, Some("local:tiny:1b"), |_| true), "E3: no context size, no local seat");
        std::env::set_var("ANTILOKI_LOCAL_NUM_CTX", "32768");
        crate::local::refresh().await;
        assert!(merge_runnable_with(planner, Some("local:tiny:1b"), |_| true), "on, with a context, and the server holds the model");
        assert!(!merge_runnable_with(planner, Some("local:absent:7b"), |_| true), "a model the server does not hold is not runnable");
        assert!(!merge_runnable_with(Some("local:tiny:1b"), None, |_| true), "the planner is a frontier model, never a local one");
        for k in ["ANTILOKI_LOCAL_WORKER", "ANTILOKI_LOCAL_NUM_CTX", "ANTILOKI_LOCAL_URL"] { std::env::remove_var(k); }
        crate::local::refresh().await;
    }
    #[test]
    fn the_five_seat_machine_can_merge_without_claude_and_without_opencode() {
        use crate::cli::Engine::*;
        let up = |keep: &'static [crate::cli::Engine]| move |e: crate::cli::Engine| keep.contains(&e);
        // the post-Claude machine (2026-10-01 audit): Claude off, Codex + Cursor + Gemini signed in, OpenCode not wanted. Before: `can_merge: false, "install OpenCode to merge"`.
        let p = providers_from(up(&[Codex, Cursor, Gemini]));
        assert_eq!(p["can_merge"], json!(true), "{p}");
        assert_eq!(p["claude-code"], json!(false));
        assert_eq!(p["frontier"], json!(["codex", "cursor", "gemini"]), "a seat that can take a call is a brain; Claude, being off, is not listed");
        assert!(p["cheap"].as_array().unwrap().iter().any(|c| c == "composer"), "Cursor's Composer is a cheap half: {p}");
        // one seat only: no merge, and the sentence says what to do - it does not send the operator to OpenCode
        let one = providers_from(up(&[Codex]));
        assert_eq!(one["can_merge"], json!(false));
        assert!(!one["why"].as_str().unwrap().contains("OpenCode"), "{one}");
        assert_eq!(one["solo"], json!(["codex"]));
        assert_eq!(p["solo"], json!(["codex"]), "the window can only bind Claude, Codex and OpenCode's catalogue as a solo tile: {p}");
        // …and a pair naming Cursor or Gemini is runnable when those seats are (it was "false" for every non-Claude/Codex/OpenCode pair, so the frontend winner Codex x Composer showed as unbindable)
        assert!(merge_runnable_with(Some("codex:gpt-5.6-terra@medium"), Some("cursor:composer-2.5"), up(&[Codex, Cursor])));
        assert!(!merge_runnable_with(Some("codex:gpt-5.6-terra@medium"), Some("cursor:composer-2.5"), up(&[Codex])), "the missing side is what blocks it");
        assert!(!merge_runnable_with(Some("claude-code:sonnet"), Some("cursor:composer-2.5"), up(&[Codex, Cursor])), "Claude is off");
        assert!(merge_runnable_with(Some("gemini:default"), None, up(&[Gemini])));
    }

    #[test]
    fn three_objectives_ask_three_questions_of_the_same_rows() {
        // the three an operator is offered (2026-09-23: "Cheaper, faster and Best")
        assert_eq!(Objective::OFFERED.map(|o| o.key()), ["cheaper", "faster", "best"]);
        // and the fourth the API still answers to, so a stored "smarter" keeps meaning what it meant
        assert_eq!(Objective::of("smarter"), Some(Objective::Smarter));
        let (q, c, t) = Objective::Smarter.weights();
        assert_eq!((q, c, t), (1.0, 0.0, 0.0), "smarter counts quality and nothing else");
        assert_eq!(Objective::Smarter.band(), 0.0, "and allows no quality gap at all");
        let (q2, c2, _) = Objective::Cheaper.weights();
        assert!(c2 > q2, "cheaper counts the bill above the score");
        // faster is cheaper's mirror: the same tolerance, spent on the clock instead of the bill
        let (qf, cf, tf) = Objective::Faster.weights();
        assert!(tf > qf && tf > cf, "faster counts the clock above both");
        assert_eq!(Objective::Faster.band(), 0.20, "it must keep 80 % of the best - the operator's one tolerance");
        assert_eq!(Objective::of("fastest"), Some(Objective::Faster));

        // PRICE FIRST, SPEED SECOND, QUALITY AS A TOLERANCE (operator 2026-09-26). The default used to be
        // (0.70, 0.20, 0.10) — quality-first — and on the first real comparison that default would have paid 6.7x
        // for one instance in ten. Quality did not stop mattering: it moved from the SCORE to the BAND, where a
        // materially worse answer is disqualified outright instead of being out-weighed.
        let (qb, cb, tb) = Objective::Best.weights();
        assert!(cb > qb && qb > tb, "balanced reads cost first, quality as a gate and a small score, and the clock last: {qb} {cb} {tb}");
        assert_eq!((qb, cb, tb), (0.15, 0.75, 0.10));
        // and the band is the instrument's own resolution, not narrower than it
        assert_eq!(Objective::Best.band(), 0.20, "the same single tolerance for every objective but smarter");
        assert!((Objective::Best.floor(0.90) - 0.72).abs() < 1e-6 && Objective::Smarter.floor(0.90) > 0.899, "the floor is 80 % of the best, and smarter gives up nothing");

        // THE CASE THAT PRODUCED THE RULE, as arithmetic: 9/10 against 8/10 is one instance — inside the band —
        // while the bill is 6.7x. The cheap arm must win, and under the old weights it did not.
        let score = |q: f64, usd: f64, s: f64, o: Objective| {
            let (wq, wc, wt) = o.weights();
            let in_band = q >= o.floor(0.90);
            let cost = if in_band { 0.056 / usd } else { 0.0 };
            let time = if in_band { 110.7 / s } else { 0.0 };
            wq * q + wc * cost + wt * time
        };
        let qwen = score(0.80, 0.056, 208.4, Objective::Best);
        let sonnet = score(0.90, 0.374, 110.7, Objective::Best);
        assert!(qwen > sonnet, "6.7x the price for one instance in ten must not win: qwen {qwen:.3} vs sonnet {sonnet:.3}");
        // …and a REAL quality gap still loses, whatever it costs — the band is a gate, not a discount
        let cheap_and_bad = score(0.55, 0.001, 50.0, Objective::Best);
        assert!(cheap_and_bad < sonnet, "outside the band, no price buys it back: {cheap_and_bad:.3}");
        for o in Objective::ALL { let (a, b, c) = o.weights(); assert!((a + b + c - 1.0).abs() < 1e-9, "{} does not sum to one", o.key()); }
        assert_eq!(Objective::of("stronger"), Some(Objective::Smarter), "the effort bar's word for it");
        assert_eq!(Objective::of("balanced"), Some(Objective::Best));
    }

    #[test]
    fn an_unmeasured_domain_reaches_for_its_nearest_neighbour_first() {
        // design has no rows of its own yet — frontend is the nearest thing to it, not the whole pool
        let f = Domain::Design.fallbacks();
        assert_eq!(f[0], "design"); assert_eq!(f[1], "frontend");
        assert_eq!(*f.last().unwrap(), "*", "and everything pooled is the last resort, so a role always gets an answer");
        assert!(f.contains(&"any"));
        assert_eq!(Domain::Backend.fallbacks(), vec!["backend", "any", "*"], "backend has no nearer neighbour");
        assert_eq!(Domain::Infra.fallbacks()[1], "backend");
        for d in [Domain::Backend, Domain::Frontend, Domain::Design, Domain::Tests, Domain::Docs, Domain::Infra, Domain::Mixed, Domain::Unknown] {
            let f = d.fallbacks();
            assert_eq!(f[0], d.key(), "a domain asks itself first");
            assert_eq!(*f.last().unwrap(), "*");
        }
    }

    #[test]
    fn the_purpose_a_person_types_picks_the_role() {
        assert_eq!(suggest_role("keep our database migrations clean and fast").key, "backend-database");
        assert_eq!(suggest_role("design the new checkout screen, colours and spacing").key, "frontend-designer");
        assert_eq!(suggest_role("write the REST endpoints for orders").key, "backend-api");
        assert_eq!(suggest_role("raise our test coverage and kill the flaky specs").key, "tester");
        assert_eq!(suggest_role("review every pull request against our laws").key, "reviewer");
        assert_eq!(suggest_role("turn product briefs into acceptance criteria").key, "product-owner");
        assert_eq!(suggest_role("fix the deploy pipeline and the docker build").key, "infra");
        assert_eq!(suggest_role("build react components for the settings page").key, "frontender");
        // a purpose nobody's vocabulary claims still gets a role rather than nothing
        assert_eq!(suggest_role("").key, "backender");
        assert_eq!(suggest_role("make the thing work").key, "backender");
    }

    #[test]
    fn a_role_is_its_adapter_not_only_its_models() {
        // the operator's point (2026-09-23): the adapter is the router AND the skills, specialists, tools and packs
        for r in ROLES {
            assert!(!r.skills.is_empty(), "{} has no skills", r.key);
            assert!(!r.specialists.is_empty(), "{} consults nobody", r.key);
            assert!(!r.panes.is_empty(), "{} can drive nothing", r.key);
            assert!(!r.packs.is_empty(), "{} boots with no intel", r.key);
        }
        // the Designer is defined by its tooling more than by its model: Frames and the design system are its hands
        let d = role("frontend-designer").unwrap();
        assert!(d.panes.contains(&"design-frames") && d.panes.contains(&"design-view:system"));
        assert!(d.specialists.iter().any(|s| s.starts_with("style") || *s == "category:style"));
        // the Reviewer writes nothing, so it drives no editor — it reads and judges
        let rv = role("reviewer").unwrap();
        assert!(rv.panes.contains(&"atlas-xray") && !rv.panes.contains(&"design-frames"));
        // two roles of the same domain still differ in their adapter, which is the whole point
        let (be, db) = (role("backender").unwrap(), role("backend-database").unwrap());
        assert_eq!(be.domain, db.domain);
        assert_ne!(be.specialists, db.specialists, "same domain, different adapter");
    }

    #[test]
    fn a_role_is_a_domain_and_the_specialised_ones_come_first() {
        assert!(ROLES.iter().filter(|r| r.general).count() >= 5);
        for r in ROLES { if let Some(u) = r.under { assert!(role(u).is_some_and(|p| p.general), "{} sits under {u}, which must be a general role", r.key); } }
        let rs = roles_for(&v(&["src/orders/api.ts", "src/orders/storage.ts"]));
        assert_eq!(rs.first().map(|r| r.general), Some(false), "a specialised role is offered before the general one");
        assert!(rs.iter().any(|r| r.key == "backender"));
        assert!(roles_for(&v(&["src/orders/OrdersPage.tsx", "src/orders/OrderRow.tsx"])).iter().any(|r| r.key == "frontender"));
        assert!(roles_for(&v(&["src/orders/api.ts", "src/orders/OrdersPage.tsx"])).iter().any(|r| r.key == "reviewer"), "mixed work falls to the role that reads everything");
    }

    /// C2 — A TASK CARRIES ITS OWN FORM. The pipeline the operator described: frontend part, then backend part.
    #[test]
    fn a_task_takes_the_role_of_the_files_it_owns() {
        let own = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        assert_eq!(role_for_task(&own(&["src/components/Card.tsx", "src/pages/Home.tsx"])).unwrap().domain, Domain::Frontend);
        assert_eq!(role_for_task(&own(&["src/api/orders.ts"])).unwrap().domain, Domain::Backend);
        assert_eq!(role_for_task(&own(&["src/theme.css"])).unwrap().domain, Domain::Design);
        assert_eq!(role_for_task(&own(&["src/x.test.ts"])).unwrap().domain, Domain::Tests);

        // THE NARROWEST ROLE THAT COVERS IT, so migrations are Database rather than Backender — the same rule an
        // ask follows, because a task should not get a vaguer form than a sentence would.
        assert_eq!(role_for_task(&own(&["db/migrations/003.sql"])).unwrap().key, "backend-database");

        // FILES THAT SAY NOTHING LEAVE THE FORM ALONE. A glob-only task is not a reason to change engines, and
        // guessing here would make the pipeline switch on noise — the exact failure C1 exists to prevent.
        assert!(role_for_task(&own(&["**/*"])).is_none());
        assert!(role_for_task(&[]).is_none());
    }

    /// C0/C1 — WHEN THE CHAMELEON CHANGES FORM, AND WHAT IT COSTS.
    #[test]
    fn a_same_engine_change_is_free_and_a_hand_off_must_be_earned() {
        let route = |role: &str, name: &str, hands: &str| json!({ "role": role, "name": name, "hands": hands, "brain": hands });

        // the ask belongs to who is already working: nothing happens
        assert_eq!(change_for("backender", "opencode", &route("backender", "Backender", "opencode:x"), None, None), Change::None);

        // A DIFFERENT ROLE ON THE SAME ENGINE IS FREE — only the adapter changes (globs, skills, specialists,
        // packs). The band has no say because there is no hand-off to price. This is the common case.
        match change_for("backender", "opencode", &route("tester", "Tester", "opencode:other"), Some((0.95, 50)), Some((0.20, 50))) {
            // the promise is the ENGINE (no restart), which is true; it is not an adapter swap, which does not happen
            Change::Adapter { why, .. } => assert!(why.contains("same engine") && !why.contains("adapter"), "{why}"),
            other => panic!("a same-engine change must never be refused: {other:?}"),
        }

        // CROSSING ENGINES IS PRICED. A difference this record cannot tell apart does not buy a restart.
        match change_for("backender", "opencode", &route("frontender", "Frontender", "claude-code:sonnet"), Some((0.80, 10)), Some((0.90, 10))) {
            Change::Stay { why } => {
                assert!(why.contains("cannot tell apart"), "{why}");
                assert!(why.contains("costs more than the difference"), "it says WHY staying is right: {why}");
            }
            other => panic!("8-of-10 against 9-of-10 is noise and must not move engines: {other:?}"),
        }

        // …and a real, separated gap does
        match change_for("backender", "opencode", &route("frontender", "Frontender", "claude-code:sonnet"), Some((0.30, 40)), Some((0.95, 40))) {
            Change::Handoff { engine, why, .. } => { assert_eq!(engine, "claude-code"); assert!(why.contains("separates them"), "{why}"); }
            other => panic!("a measured gap must move: {other:?}"),
        }

        // AN UNMEASURED ROLE STILL GETS THE WORK. The domain is right even when the merge under it is unproven —
        // refusing here would mean the chameleon can only ever be the forms it has already been.
        match change_for("backender", "opencode", &route("infra", "Infra", "codex:medium"), Some((0.99, 99)), None) {
            // the hand-off stands; the sentence no longer claims the measured side is unmeasured (audit 2026-09-28)
            Change::Handoff { why, .. } => { assert!(why.contains("nothing is measured for it yet"), "{why}");
                                             assert!(why.contains("99%"), "it says what is being given up: {why}"); }
            other => panic!("an unmeasured role is still the right role: {other:?}"),
        }

        // audit 2026-09-28: `separated` is a disjointness test, so it fired just as happily when the new form was
        // measurably WORSE — the run paid a restart to get worse and the sentence claimed the opposite
        match change_for("backender", "opencode", &route("frontender", "Frontender", "claude-code:sonnet"), Some((0.95, 50)), Some((0.20, 50))) {
            Change::Stay { why } => assert!(why.contains("worse, not better"), "{why}"),
            other => panic!("20% must never take the work from 95%: {other:?}"),
        }
        match change_for("backender", "opencode", &route("frontender", "Frontender", "claude-code:sonnet"), Some((0.20, 50)), Some((0.95, 50))) {
            Change::Handoff { why, .. } => assert!(why.contains("beats the current form"), "{why}"),
            other => panic!("95% beating 20% is the case the rule exists for: {other:?}"),
        }
    }

    /// ITEM 4 (2026-10-01): THE ROUTE NAMES A SOLO SEAT WHEN ONE WINS, and says so when it has no solo data to go on.
    #[test]
    fn the_route_names_a_solo_seat_when_it_wins_and_says_when_no_solo_cell_was_measured() {
        let _ = std::fs::remove_file(crate::merges::path());
        let row = |job: &str, planner: &str, worker: &str, audit: f64, usd: f64| crate::merges::Outcome { suite: "fix".into(), stack: "any".into(), job: job.into(), size: "l".into(),
            domain: "backend".into(), planner: planner.into(), worker: worker.into(), qa: String::new(), tasks: 1, pass_first: 1, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0,
            audit: Some(audit), ms: 60_000, usd, source: "bench".into(), audit_only: false };
        let put = |times: usize, o: crate::merges::Outcome| for _ in 0..times { crate::merges::record(&o).unwrap(); };
        put(3, row("planner", "codex:gpt-5.6-terra@medium", "cursor:composer-2.5", 0.95, 0.05));
        put(3, row("editor-l", "codex:gpt-5.6-terra@medium", "cursor:composer-2.5", 0.95, 0.21));
        let ask = "fix the failing endpoint in the api service";

        // NO SOLO CELL: today's answer — a pair plans and writes — and the sentence says why nothing solo took part
        let v = route_role(ask, "l", Objective::Best);
        assert_eq!(v["domain"], "backend");
        assert_eq!(v["solo"], json!(false));
        assert_eq!((v["brain"].as_str(), v["hands"].as_str()), (Some("codex:gpt-5.6-terra@medium"), Some("cursor:composer-2.5")));
        assert_eq!(v["measured"], json!(true));
        let why = v["why"].as_str().unwrap();
        assert!(why.contains("plans") && why.contains("no solo cell is measured here"), "{why}");

        // A SOLO CELL INSIDE THE BAND AND CHEAPER: one seat does the whole job, there is no brain to name, and no pair is bound around it
        put(2, row("editor-l", "raw", "cursor:cursor-grok-4.6-medium", 0.90, 0.02));
        let v = route_role(ask, "l", Objective::Best);
        assert_eq!(v["solo"], json!(true));
        assert!(v["brain"].is_null(), "{v}");
        assert_eq!(v["hands"], "cursor:cursor-grok-4.6-medium");
        assert_eq!(v["measured"], json!(true));
        let why = v["why"].as_str().unwrap();
        assert!(why.contains("writes alone, no planner") && why.contains("solo wins"), "{why}");
        assert!(pair_of_route(&v).is_none(), "a seat that works alone is not a merge to bind");
        // the roles screen (a MERGE to hire) still offers brain × hands, not a solo seat
        let p = proposal(ask, "l");
        assert_eq!(p["merges"][0]["hands"], "cursor:composer-2.5");
    }

    /// A LABEL IS NOT A BINDING (2026-09-26) — the SWE-bench lesson, applied before it can bite the router.
    #[test]
    fn a_route_becomes_a_pair_only_when_every_side_is_dispatchable() {
        let v = |brain: Value, hands: Value, measured: bool| json!({ "role": "backender", "brain": brain, "hands": hands, "measured": measured });

        let p = pair_of_route(&v(json!("claude-code:sonnet"), json!("opencode:opencode/qwen3.6-plus"), true)).expect("a pair");
        assert_eq!(p.brain, crate::cli::Engine::ClaudeCode);
        assert_eq!(p.hands, crate::cli::Engine::Opencode);
        assert_eq!(p.brain_model.as_deref(), Some("sonnet"));
        assert_eq!(p.name, "backender", "the pair is named for the role that earned it");

        // nothing measured → nothing bound; the role's declared binding stands
        assert!(pair_of_route(&v(json!("claude-code:sonnet"), json!("opencode:x"), false)).is_none());
        // `raw` is the yardstick and `-` is the solo marker — neither is an engine
        assert!(pair_of_route(&v(json!("raw"), json!("opencode:x"), true)).is_none());
        assert!(pair_of_route(&v(json!("claude-code:sonnet"), json!("-"), true)).is_none());
        // a label that is not `engine:model` is refused rather than guessed at — this is the exact shape
        // SWE-bench recorded as a winner the router could not have launched
        assert!(pair_of_route(&v(json!("antiloki-opencode-qwen3.6-plus"), json!("opencode:x"), true)).is_none());
        // and an engine this build does not know is not invented
        assert!(pair_of_route(&v(json!("mystery:1"), json!("opencode:x"), true)).is_none());
    }

    /// THE BATON PASS (2026-09-26). The roster's job, done by the router instead of by the operator.
    #[test]
    fn heimdall_routes_the_role_and_says_what_it_read() {
        let v = route_role("fix the failing endpoint in the api service", "l", Objective::Best);
        assert_eq!(v["domain"], "backend", "the ask names the work, the work names the role");
        assert_eq!(v["objective"], "best");
        assert!(v["why"].as_str().unwrap().contains("backend work →"), "{}", v["why"]);

        // THE ADAPTER RIDES WITH THE VERDICT — the part a general agent cannot carry, and the whole reason roles
        // survive the roster being retired.
        for k in ["skills", "specialists", "globs"] {
            assert!(v[k].as_array().map(|a| !a.is_empty()).unwrap_or(false), "a routed role brings its {k}");
        }

        // two different kinds of work must not route to one role
        let d = route_role("the spacing and colour on the settings screen look wrong", "l", Objective::Best);
        assert_ne!(d["role"], v["role"]);

        // THE OBJECTIVE IS THE OPERATOR'S AND IS SAID OUT LOUD — "cheaper" is a different promise from "best",
        // and the line must never claim the one it did not use.
        let c = route_role("fix the failing endpoint in the api service", "l", Objective::Cheaper);
        assert_eq!(c["objective"], "cheaper");

        // an unmeasured cell says so rather than dressing a default as a verdict
        if !v["measured"].as_bool().unwrap_or(false) {
            assert!(v["why"].as_str().unwrap().contains("nothing measured here yet"), "{}", v["why"]);
        }
    }
}

/// HEIMDALL PASSES THE BATON (operator 2026-09-26: "so heimdall not just routes the model, it routes the role,
/// like it passes the baston like on maratons").
///
/// One ask in, one answer out: which role this work belongs to, and the merge that role's own evidence says should
/// do it — under the objective the operator picked, `best` unless they said they would rather spend money or time.
///
/// THIS IS WHAT REPLACES THE ROSTER. Nobody has to create a Backender: the ask names the work, the work names the
/// role, and the role carries its own globs, skills, specialists, panes and packs — the part a general agent in one
/// conversation cannot carry. The switcher stays for the operator who disagrees, which is an override, not a setup
/// step.
///
/// `why` is not decoration. A router that silently changes who is working is worse than one that does not route.
pub fn route_role(ask: &str, size: &str, objective: Objective) -> Value { route_role_in(ask, size, objective, "") }
/// The same, with the workspace as the evidence for an ask whose words name no role.
pub fn route_role_in(ask: &str, size: &str, objective: Objective, cwd: &str) -> Value {
    let r = suggest_role_in(ask, cwd);
    let d = r.domain.key();
    let editor = if size == "s" { "editor-s" } else { "editor-l" };
    let brain = crate::merges::best_for_role("planner", size, d, objective, 1);
    // THE HANDS ARE CHOSEN AMONG SOLO CELLS AND PAIRS ALIKE at the sizes that need no planner (item 4, 2026-10-01): "tasks of
    // size L and smaller do not need a planner" was implemented in the run, but this route still named the single AI only
    // as the hands of the best pair, so a seat that does the whole job for less inside the 80 % band could never be named.
    // `best_cell` ranks both by the one tolerance; where no solo cell is measured it answers exactly what
    // `best_for_role` always did, and its note says so.
    let pick = crate::merges::best_cell(editor, size, d, objective, 1);
    let alone = pick.as_ref().map(|p| p.solo).unwrap_or(false);
    let hands = pick.as_ref().map(|p| (p.planner.clone(), p.worker.clone(), p.n));
    // (REMOVED 2026-09-28, audit) A "writer plans alone when raw wins" branch stood here. It could never fire: `routing_sql`
    // excludes every `planner = 'raw'` row, so `best_for_role` never names raw. One seat planning and writing is benched as
    // a real pairing instead (Composer × Composer); if it wins its cell, this function picks it with no special case.
    // (2026-10-01: a solo cell now competes through `best_cell`, so "raw wins" is a real answer — as `alone`, not as a brain.)
    let brain = if alone { None } else { brain };
    let samples = brain.as_ref().map(|(_, _, n)| *n).unwrap_or(0) + hands.as_ref().map(|(_, _, n)| *n).unwrap_or(0);
    let solo_note = pick.as_ref().map(|p| p.note.clone()).filter(|n| !n.is_empty()).map(|n| format!(" — {n}")).unwrap_or_default();
    // THE HONEST SENTENCE: an unmeasured cell must not read like a measured one.
    let why = match (&brain, &hands) {
        (None, Some((_, w, _))) if alone => format!("{d} work → {} · {w} writes alone, no planner ({}, {samples} samples){solo_note}", r.name, objective.key()),
        (Some((p, _, _)), Some((_, w, _))) => format!("{d} work → {} · {p} plans, {w} writes ({}, {samples} samples){solo_note}", r.name, objective.key()),
        _ => format!("{d} work → {} — nothing measured here yet, so its declared binding stands", r.name),
    };
    json!({
        "role": r.key, "name": r.name, "domain": d, "objective": objective.key(),
        "brain": brain.as_ref().map(|(p, _, _)| p.clone()),
        "hands": hands.as_ref().map(|(_, w, _)| w.clone()),
        // a solo winner has no brain to name: the field says it so a caller never binds a pair around a seat that works alone
        "solo": alone,
        "samples": samples, "measured": hands.is_some() && (alone || brain.is_some()), "why": why,
        "globs": r.globs, "skills": r.skills, "specialists": r.specialists, "panes": r.panes, "packs": r.packs,
    })
}

/// THE FORM ONE TASK WANTS (PLAN-THE-CHAMELEON C2 — the pipeline).
///
/// *"if needs backend on the same task switches again"*. A plan's tasks already declare the files they OWN, and a
/// file's domain is already deterministic (`domain_of_path`), so a task's role needs nothing new: it is the role
/// that works in the domain those files belong to.
///
/// `None` when the files say nothing — a glob-only task, a docs-and-code mixture with no majority — and that is
/// the right answer rather than a guess: the run's own form then keeps the work, which is what happens today.
///
/// THE MOST SPECIFIC ROLE THAT COVERS THE DOMAIN, so migrations land on Database rather than on Backender, the
/// same rule `suggest_role` follows for an ask. Pure — tested.
/// THE PATHS CHOOSE, NOT THE DECLARATION ORDER (audit 2026-09-28). This took the FIRST specialised role of the
/// domain, so every Backend task went to `backend-database` because it happens to be listed above `backend-api` —
/// `src/orders/api.ts`, `routes/webhook.ts` and `handlers/payment.ts` alike. Two specialised roles share Backend and
/// two share Frontend, and in each pair one of them could never be chosen by this function at all.
///
/// The paths are the evidence, and the module already knows how to read evidence: `WORDS` says what each role's work
/// is called, and `says_word` finds those words without finding them inside other words. So the specialised role
/// whose vocabulary the paths actually use wins, by how many of its words they use; declaration order decides only a
/// genuine tie, and the general role still answers when no specialist is named. Pure — tested.
pub fn role_for_task(owns: &[String]) -> Option<&'static Role> {
    let d = domain_of(owns);
    if d == Domain::Unknown { return None }
    let text = owns.join(" ").to_ascii_lowercase();
    let score = |r: &Role| WORDS.iter().find(|(k, _)| *k == r.key)
        .map(|(_, ws)| ws.iter().filter(|w| says_word(&text, w)).count()).unwrap_or(0);
    let best = ROLES.iter().filter(|r| r.domain == d && !r.general)
        // `max_by_key` keeps the LAST maximum, so the order is reversed to keep the first — the narrowest — on a tie
        .rev().max_by_key(|r| score(r)).filter(|r| score(r) > 0);
    best.or_else(|| ROLES.iter().find(|r| r.domain == d && !r.general))
        .or_else(|| ROLES.iter().find(|r| r.domain == d))
}

/// WHAT CHANGING FORM COSTS, AND THEREFORE WHETHER TO DO IT (PLAN-THE-CHAMELEON C0/C1).
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// the ask belongs to the role already running — nothing happens
    None,
    /// a different role whose merge is on the SAME engine, so no restart is needed.
    ///
    /// THE ADAPTER SWAP, WIRED (2026-09-28): `chat_pane::swap_adapter` puts the role's skills and specialists on the pane and
    /// the settings update re-bakes the brief. The history below is kept for why it once only recorded the role.
    /// (was: NOT YET THE ADAPTER SWAP IT IS NAMED FOR, audit 2026-09-28.) The design is that this swaps the pane's globs,
    /// skills, specialists and packs in place — the common case, and free, which is the performance story. What the
    /// code actually does is RECORD the new role in the pane's settings and say so; `settings.role.current` is
    /// written and read by this decision alone, so nothing about how the pane works changes. The message used to
    /// tell the operator "only the adapter changes", which was a swap that never happened.
    ///
    /// The record is not useless — it is what the next ask is weighed against, which is how the stay-put rule stops
    /// the router flapping between two roles. But until the re-bake is wired this variant means "the work moved to
    /// another role on the same engine", and the text below says only that. Left deliberately visible rather than
    /// quietly narrowed, because implementing the swap changes how a live pane behaves and is the operator's call.
    Adapter { role: String, why: String },
    /// a different role on a DIFFERENT engine: the episode consolidates, a new one opens there, and the
    /// conversation rides in front of the next message (`chat_pane`'s existing provider-switch path)
    Handoff { role: String, engine: String, why: String },
    /// the new role's winner is not measurably better than what is already running — stay put
    Stay { why: String },
}

/// C1 — THE STAY-PUT RULE.
///
/// A router that changes form on every message is worse than one that never does. Switching costs a process
/// start, a context re-injection and an episode boundary; staying costs, at most, a difference the record has
/// already declared too small to measure.
///
/// So: **cross an engine boundary only when the new role's winner is `separated()` from what is running.** That is
/// not a new policy — it is the price-first tolerance applied to time instead of money, and it needs no setting.
///
/// A SAME-ENGINE CHANGE IS NEVER REFUSED. It costs nothing but a different adapter, so the band has no say: the
/// rule exists to price a hand-off, and there is no hand-off to price.
///
/// `current` and `next` are `(quality, samples)` for the work at hand — the caller reads them from the record.
/// Pure — tested.
pub fn change_for(current_role: &str, current_engine: &str, route: &Value,
                  current: Option<(f64, i64)>, next: Option<(f64, i64)>) -> Change {
    let role = route.get("role").and_then(Value::as_str).unwrap_or("");
    if role.is_empty() || role == current_role { return Change::None }
    let name = route.get("name").and_then(Value::as_str).unwrap_or(role);
    // whichever side does the writing decides which engine the form runs on
    let engine = route.get("hands").and_then(Value::as_str)
        .or_else(|| route.get("brain").and_then(Value::as_str))
        .and_then(|k| k.split(':').next()).unwrap_or("");
    if engine.is_empty() || engine == current_engine {
        // says what happens, not what the variant is named for — see the note on `Change::Adapter`
        return Change::Adapter { role: role.into(), why: format!("{name} — same engine, so the work moves across without a restart, with {name}'s skills and specialists swapped in") };
    }
    match (current, next) {
        // NOTHING MEASURED EITHER SIDE: the record cannot argue, so the ask wins. A role nobody has measured is
        // still the right role for the work — it is the merge under it that is unproven, not the domain.
        // NOTHING MEASURED EITHER SIDE: the record cannot argue, so the ask wins.
        (None, None) => Change::Handoff { role: role.into(), engine: engine.into(),
            why: format!("{name} on {engine} — nothing measured on either side, so the ask decides") },
        // THE CURRENT FORM IS MEASURED AND THE NEW ONE IS NOT — the hand-off still happens, because the DOMAIN is
        // right and refusing would mean the chameleon can only ever be the forms it has already been. What
        // changed (audit 2026-09-28) is the sentence: this took the `(_, None)` branch and said "nothing measured
        // here yet", which is untrue of the side that IS measured, and it is the DEFAULT path on a fresh pane —
        // the current form falls back to the pooled `any` rows while a real domain's cell is usually empty. So it
        // says which side is unmeasured, and what is being given up.
        (Some(c), None) => Change::Handoff { role: role.into(), engine: engine.into(),
            why: format!("{name} on {engine} — the ask is {} work, and nothing is measured for it yet; the current form is at {}%",
                         route.get("domain").and_then(Value::as_str).unwrap_or("this"), (c.0 * 100.0).round()) },
        (None, Some((q, n))) => Change::Handoff { role: role.into(), engine: engine.into(),
            why: format!("{name} on {engine} — {}% over {n} run{}, against nothing measured for the current form", (q * 100.0).round(), if n == 1 { "" } else { "s" }) },
        // SEPARATED IS SYMMETRIC — "BETTER" IS NOT (audit 2026-09-28). `merges::separated` is a disjointness test
        // (`ah < bl || bh < al`), so argument order means nothing to it: this fired just as happily when the new
        // role was measurably WORSE, paying a restart to get worse and printing "20% beats the current form's
        // 95%". A hand-off is only ever worth it when the new form is BOTH better and distinguishably so.
        (Some(c), Some(x)) if x.0 > c.0 && crate::merges::separated(x, c) => Change::Handoff { role: role.into(), engine: engine.into(),
            why: format!("{name} on {engine} — {}% beats the current form's {}%, and the record separates them", (x.0 * 100.0).round(), (c.0 * 100.0).round()) },
        (Some(c), Some(x)) if x.0 <= c.0 && crate::merges::separated(x, c) => Change::Stay {
            why: format!("staying — {name} would score {}% against the current form's {}%, which is worse, not better", (x.0 * 100.0).round(), (c.0 * 100.0).round()) },
        (Some(c), Some(x)) => Change::Stay {
            why: format!("staying — {name} would score {}% against {}%, which this record cannot tell apart; a hand-off costs more than the difference is worth", (x.0 * 100.0).round(), (c.0 * 100.0).round()) },
    }
}

/// THE ROUTE'S VERDICT AS A BINDING THE ROUTER CAN ACTUALLY LAUNCH.
///
/// `route_role` names the winners as merge KEYS (`engine:model`). A key is a label; a `Pair` is a thing that
/// starts. Turning one into the other is where SWE-bench went wrong earlier today — it recorded
/// `antiloki-opencode-qwen3.6-plus` as a winning worker, which the router could have picked and then failed to
/// start — so this parses strictly and answers `None` rather than half a pair:
///   · `raw` and `-` are not engines, they are the yardstick and the solo marker;
///   · an engine the build does not know is not guessed at;
///   · a route with nothing measured yields nothing, and the declared binding stands.
pub fn pair_of_route(v: &Value) -> Option<crate::cli::Pair> {
    if !v.get("measured").and_then(Value::as_bool).unwrap_or(false) { return None }
    let split = |k: &str| -> Option<(crate::cli::Engine, Option<String>)> {
        let key = v.get(k).and_then(Value::as_str)?.trim();
        if key.is_empty() || key == "raw" || key == "-" { return None }
        let (e, m) = key.split_once(':')?;
        let engine = crate::cli::engine_of(&json!(e))?;
        Some((engine, if m.trim().is_empty() || m == "default" { None } else { Some(m.trim().to_string()) }))
    };
    let (brain, brain_model) = split("brain")?;
    let (hands, hands_model) = split("hands")?;
    let name = v.get("role").and_then(Value::as_str).unwrap_or("routed").to_string();
    Some(crate::cli::Pair { name, brain, hands, brain_model, hands_model })
}

/// THE WHOLE PROPOSAL for an agent: the role, and for each of the three objectives the merge that won this role's
/// kind of work — brain and hands — beside the skills, specialists, panes and packs the role carries. This is what
/// agent creation shows after the operator types what the agent is for.
pub fn proposal(purpose: &str, size: &str) -> Value {
    let r = suggest_role(purpose);
    let d = r.domain.key();
    let merges: Vec<Value> = Objective::OFFERED.iter().map(|o| {
        // the brain decides (planner), the hands produce (the editor of this size)
        let editor = if size == "s" { "editor-s" } else { "editor-l" };
        let brain = crate::merges::best_for_role("planner", size, d, *o, 1);
        let hands = crate::merges::best_for_role(editor, size, d, *o, 1);
        json!({ "objective": o.key(),
                "brain": brain.as_ref().map(|(p, _, _)| p.clone()),
                "hands": hands.as_ref().map(|(_, w, _)| w.clone()),
                "samples": brain.as_ref().map(|(_, _, n)| *n).unwrap_or(0) + hands.as_ref().map(|(_, _, n)| *n).unwrap_or(0) })
    }).collect();
    json!({ "role": { "key": r.key, "name": r.name, "about": r.about, "domain": d, "general": r.general, "under": r.under,
                      "globs": r.globs, "skills": r.skills, "specialists": r.specialists, "panes": r.panes, "packs": r.packs },
            "merges": merges,
            "why": format!("“{}” reads as {} work — {}", purpose.chars().take(60).collect::<String>(), d, r.about) })
}
/// `GET /api/v1/settings/roles` — every role and its adapter; `?purpose=…` adds the proposal for that purpose.
/// WHAT THIS MACHINE CAN ACTUALLY RUN (operator 2026-09-23: "you should also check the available AIs on the machine
/// to make the merged agents available, because the person must have at least 2 providers, 1 frontier and 1 cheap
/// for the merge work"). A merge is never two frontier models — that is the operator's standing rule — so the cheap
/// half always comes through OpenCode. Which means: **no OpenCode, no merges at all**, however many subscriptions
/// the person has. Saying that plainly beats offering a merge that cannot be bound.
///
/// `solo` is what is left when a merge is impossible, and is offered anyway for anyone who wants one model.
pub fn providers_here() -> Value { providers_from(crate::cli::engine_usable) }
/// …over any "can this seat take a call" question, so the five-seat shape is tested without a machine.
///
/// FIVE SEATS, NOT THREE (2026-10-01, the Claude-off audit): this knew Claude, Codex and OpenCode only, so with Claude off and Cursor + Gemini + Codex signed in - the whole post-Claude product -
/// it answered `can_merge: false, "install OpenCode to merge"` and the Add-agent window disabled every objective, while the router was happily running Codex × Composer. A seat that can take a call
/// is a brain (any of them); the cheap half is a seat whose cheap rung is cheap: Cursor's Composer, Gemini's Flash, or OpenCode's catalogue. A merge needs two seats.
pub fn providers_from(usable: impl Fn(crate::cli::Engine) -> bool) -> Value {
    use crate::cli::Engine;
    let (claude, codex, opencode) = (usable(Engine::ClaudeCode), usable(Engine::Codex), usable(Engine::Opencode));
    let (cursor, gemini, grok) = (usable(Engine::Cursor), usable(Engine::Gemini), usable(Engine::Grok));
    let frontier: Vec<&str> = [("claude-code", claude), ("codex", codex), ("cursor", cursor), ("gemini", gemini), ("grok", grok)].iter().filter(|(_, ok)| *ok).map(|(k, _)| *k).collect();
    // the cheap half, by name - the pipe is not a choice, the model is
    let mut cheap: Vec<&str> = vec![];
    if cursor { cheap.push("composer"); }
    if gemini { cheap.push("gemini-flash"); }
    if opencode { cheap.extend(["deepseek", "kimi", "qwen"]); }
    let seats = frontier.len() + opencode as usize;
    let can_merge = seats >= 2;
    let why = if can_merge { String::new() } else { "a merge needs two seats that can take a call - sign in to a second CLI (Settings › Seats)".to_string() };
    // `solo` is what the Add-agent window can BIND as one model (the FE's `soloSide` knows Claude, Codex and OpenCode's catalogue keys): Cursor/Gemini/Grok are brains and hands in a merge, not solo tiles yet
    let mut solo: Vec<&str> = frontier.iter().copied().filter(|k| matches!(*k, "claude-code" | "codex")).collect();
    solo.extend(cheap.iter().copied().filter(|k| matches!(*k, "deepseek" | "kimi" | "qwen")));
    json!({ "claude-code": claude, "codex": codex, "opencode": opencode, "cursor": cursor, "gemini": gemini, "grok": grok,
            "frontier": frontier, "cheap": cheap, "solo": solo, "can_merge": can_merge, "why": why })
}

/// Is a merge bindable here? Both of its sides' engines have to be able to take a call (installed, signed in, switched on). Pure over `engine_usable`.
pub fn merge_runnable(brain: Option<&str>, hands: Option<&str>) -> bool { merge_runnable_with(brain, hands, crate::cli::engine_usable) }
pub fn merge_runnable_with(brain: Option<&str>, hands: Option<&str>, usable: impl Fn(crate::cli::Engine) -> bool) -> bool {
    let side_ok = |k: Option<&str>| match k {
        None => true,   // unbound: the router picks per job from what IS here
        // EVERY SEAT, not three (2026-10-01): a pair naming Cursor, Gemini or Grok - the frontend winner's hands are Composer - was "not runnable" because only claude-code, codex and opencode were known
        // a `local:<tag>` worker (RFC 0001 phase E) is runnable only when the local seat is on, has a context set, and the model server holds the model: the last probe says
        Some(t) if crate::local::is_local_key(t) => crate::local::usable_cached(t),
        Some(t) => crate::cli::engine_of(&json!(t.split(':').next().unwrap_or(""))).map_or(false, &usable),
    };
    // the planner is a frontier model (RFC 0001 §8.2): a local worker is only ever the hands
    side_ok(brain) && !brain.is_some_and(crate::local::is_local_key) && side_ok(hands)
}

pub async fn http_roles(q: std::collections::HashMap<String, String>) -> axum::response::Response {
    use axum::response::IntoResponse;
    let size = q.get("size").cloned().unwrap_or_else(|| "l".into());
    let editor = if size == "s" { "editor-s" } else { "editor-l" };
    // every role carries its three winners — the Studio shows them without asking a second time
    let roles: Vec<Value> = ROLES.iter().map(|r| {
        let d = r.domain.key();
        let merges: Vec<Value> = Objective::OFFERED.iter().map(|o| {
            let brain = crate::merges::best_for_role("planner", &size, d, *o, 1);
            let hands = crate::merges::best_for_role(editor, &size, d, *o, 1);
            json!({ "objective": o.key(), "about": match o { Objective::Cheaper => "the cheapest merge that keeps at least 80% of the best score", Objective::Faster => "the quickest merge that keeps at least 80% of the best score", Objective::Best => "inside the 80% band: price first, then quality, then time", Objective::Smarter => "the best score there is, whatever it costs" },
                    "brain": brain.as_ref().map(|(p, _, _)| p.clone()), "hands": hands.as_ref().map(|(_, w, _)| w.clone()),
                    // a winner whose provider is not installed here is not an option — the window shows the role's
                    // objective as unavailable rather than offering a merge that cannot be bound
                    "runnable": merge_runnable(brain.as_ref().map(|(p, _, _)| p.as_str()), hands.as_ref().map(|(_, w, _)| w.as_str())),
                    "samples": brain.as_ref().map(|(_, _, n)| *n).unwrap_or(0) + hands.as_ref().map(|(_, _, n)| *n).unwrap_or(0) })
        }).collect();
        json!({ "key": r.key, "name": r.name, "about": r.about, "domain": d,
                "general": r.general, "under": r.under, "globs": r.globs, "skills": r.skills,
                "specialists": r.specialists, "panes": r.panes, "packs": r.packs, "merges": merges })
    }).collect();
    let p = q.get("purpose").filter(|s| !s.trim().is_empty()).map(|s| proposal(s, &size));
    axum::Json(json!({ "ok": true, "roles": roles, "objectives": Objective::OFFERED.map(|o| o.key()), "providers": providers_here(), "proposal": p })).into_response()
}
