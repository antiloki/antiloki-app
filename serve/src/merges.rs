//! MERGE SCORES (operator 2026-09-23: "keep on settings a score … inspired by the tagging weights we used for
//! prompting and gates, create a new screen and a table on the dbs for weight results of model merges, so we know the
//! best combinations for planner + worker + QA for each kind of job and complexity").
//!
//! One row per COMBINATION × JOB × SIZE: which model planned, which wrote, which judged, on what kind of ask, how big.
//! Every finished run adds its outcome to the row — first-pass verdicts, retries, the QA gate's met/unmet, seconds,
//! dollars — and the row's WEIGHT is recomputed like a tag's: quality first, then cost, then time, each normalised
//! against the other combinations of the same job × size. The router reads `best()`; the settings screen shows the
//! table. Its own small sqlite next to eta.json — never the cockpit db's writer connection from a request path.
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub fn path() -> std::path::PathBuf {
    #[cfg(test)]
    { return std::env::temp_dir().join(format!("antiloki-merges-test-{}-{:?}", std::process::id(), std::thread::current().id())).join("merges.sqlite"); }
    #[cfg(not(test))]
    { crate::eta::History::path().with_file_name("merges.sqlite") }
}
/// The record for a tool outside the crate (the bench -> router import): migrated like every other opener, with the `look_pairs` table made beside `merge_scores`.
pub fn open_db() -> rusqlite::Result<Connection> { let c = open()?; crate::looks::ensure_schema(&c)?; Ok(c) }
pub(crate) fn open() -> rusqlite::Result<Connection> {
    let p = path(); if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
    let c = Connection::open(p)?;
    c.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS merge_scores (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        job TEXT NOT NULL, size TEXT NOT NULL, domain TEXT NOT NULL DEFAULT 'any', stack TEXT NOT NULL DEFAULT 'any',
        suite TEXT NOT NULL DEFAULT '',
        planner TEXT NOT NULL, worker TEXT NOT NULL, qa TEXT NOT NULL DEFAULT '',
        n INTEGER NOT NULL DEFAULT 0, pass_first INTEGER NOT NULL DEFAULT 0, retries INTEGER NOT NULL DEFAULT 0,
        failed INTEGER NOT NULL DEFAULT 0, gate_met INTEGER NOT NULL DEFAULT 0, gate_unmet INTEGER NOT NULL DEFAULT 0,
        audit_sum REAL NOT NULL DEFAULT 0, audit_n INTEGER NOT NULL DEFAULT 0,
        ms_total INTEGER NOT NULL DEFAULT 0, usd_total REAL NOT NULL DEFAULT 0,
        weight REAL NOT NULL DEFAULT 0, source TEXT NOT NULL DEFAULT 'run', updated TEXT NOT NULL DEFAULT (datetime('now')),
        UNIQUE (job, size, domain, stack, suite, planner, worker, qa)
    );")?;
    migrate_domain(&c)?;
    // ADDITIVE FIRST, REBUILD SECOND: the stack rebuild copies `reasked`, so the column has to be there before it runs.
    migrate_reasked(&c)?;
    migrate_stack(&c)?;
    migrate_suite(&c)?;
    // AFTER THE REBUILDS (PLAN-THE-DESIGN S4): the three table rebuilds above copy an EXPLICIT column list, so a column added before them
    // would be dropped silently from an old file
    migrate_looks(&c)?;
    // LAST, because its backfill reads planner/worker and must see the final column set
    migrate_legacy(&c)?;
    // THE BENCHMARKS WE SHIP (operator 2026-10-11): added once per defaults version, as a prior the user's own runs add on top of
    // (not in tests: a test seeds what it wants through `apply_shipped`, and an automatic apply of this build's empty defaults on the next open would take it straight back out)
    // ANTILOKI_SHIPPED_BENCH=off keeps a record empty of our defaults (the journeys, which assert the router's behaviour on a record they build themselves; a tester who wants to see antiloki with no prior)
    #[cfg(not(test))]
    if std::env::var("ANTILOKI_SHIPPED_BENCH").map_or(true, |v| !v.eq_ignore_ascii_case("off")) { apply_shipped(&c, shipped_defaults())?; }
    Ok(c)
}

// ------------------------------------------------------------------------------------------------------------------------------ the benchmarks that ship with the app
/// THE DEFAULTS ARE OURS, THE USE IS THE USER'S (operator 2026-10-11: "the benchmarks are small, completely different from what is running on this machine. My benchmarks are default for all users,
/// I'm a super user, dev-mode benchmarks for everyone; settings and the code must know the defaults coming from us and what the user's bench uses").
///
/// A fresh install had an empty `merge_scores` and the router had nothing to go on but the three seats' own guesses, while the operator's machine held hundreds of measured cells. The
/// measured cells now ship in the binary (`merges-defaults.json`, written by `bench/merges/export-defaults.py` from a dev machine's own record) and are ADDED to the user's table the
/// first time a build with a new defaults version starts. They are a PRIOR: the user's runs add on top of the same rows (counters are additive), so a model the user's work keeps
/// failing at loses its default advantage by evidence, and nothing the user measured is ever replaced.
///
/// What we added is remembered exactly, in `merge_shipped` (same key, same counters). That ledger is how every row can say who it came from - `origin_of` - and how a later release
/// swaps ITS contribution for a newer one by the difference, without touching the user's share. Rows stay `source = 'bench'`: they ARE benchmark measurements, and a source this
/// build does not know would be quarantined (`integrity`).
#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Shipped {
    pub job: String, pub size: String, pub domain: String, pub stack: String, pub suite: String, pub planner: String, pub worker: String, pub qa: String,
    #[serde(default)] pub n: i64, #[serde(default)] pub pass_first: i64, #[serde(default)] pub retries: i64, #[serde(default)] pub failed: i64,
    #[serde(default)] pub gate_met: i64, #[serde(default)] pub gate_unmet: i64, #[serde(default)] pub audit_sum: f64, #[serde(default)] pub audit_n: i64,
    #[serde(default)] pub ms_total: i64, #[serde(default)] pub usd_total: f64, #[serde(default)] pub reasked: i64, #[serde(default)] pub look_sum: f64, #[serde(default)] pub look_n: i64,
}
#[derive(Deserialize, Clone, Debug, Default)]
pub struct Defaults { #[serde(default)] pub version: String, #[serde(default)] pub rows: Vec<Shipped> }
/// The defaults of this build. Tests start with none, so no test's table has rows it did not put there.
pub fn shipped_defaults() -> &'static Defaults {
    static D: std::sync::OnceLock<Defaults> = std::sync::OnceLock::new();
    D.get_or_init(|| {
        #[cfg(test)] { Defaults::default() }
        #[cfg(not(test))] { serde_json::from_str(include_str!("merges-defaults.json")).unwrap_or_default() }
    })
}
const SHIPPED_COLS: &str = "job, size, domain, stack, suite, planner, worker, qa, n, pass_first, retries, failed, gate_met, gate_unmet, audit_sum, audit_n, ms_total, usd_total, reasked, look_sum, look_n";
fn ensure_ledger(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS merge_shipped (
        job TEXT NOT NULL, size TEXT NOT NULL, domain TEXT NOT NULL, stack TEXT NOT NULL, suite TEXT NOT NULL, planner TEXT NOT NULL, worker TEXT NOT NULL, qa TEXT NOT NULL,
        n INTEGER NOT NULL DEFAULT 0, pass_first INTEGER NOT NULL DEFAULT 0, retries INTEGER NOT NULL DEFAULT 0, failed INTEGER NOT NULL DEFAULT 0,
        gate_met INTEGER NOT NULL DEFAULT 0, gate_unmet INTEGER NOT NULL DEFAULT 0, audit_sum REAL NOT NULL DEFAULT 0, audit_n INTEGER NOT NULL DEFAULT 0,
        ms_total INTEGER NOT NULL DEFAULT 0, usd_total REAL NOT NULL DEFAULT 0, reasked INTEGER NOT NULL DEFAULT 0, look_sum REAL NOT NULL DEFAULT 0, look_n INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (job, size, domain, stack, suite, planner, worker, qa));
      CREATE TABLE IF NOT EXISTS merge_shipped_meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);")
}
fn ledger(c: &Connection) -> rusqlite::Result<std::collections::HashMap<(String, String, String, String, String, String, String, String), Shipped>> {
    let mut st = c.prepare(&format!("SELECT {SHIPPED_COLS} FROM merge_shipped"))?;
    let rows = st.query_map([], |r| Ok(Shipped {
        job: r.get(0)?, size: r.get(1)?, domain: r.get(2)?, stack: r.get(3)?, suite: r.get(4)?, planner: r.get(5)?, worker: r.get(6)?, qa: r.get(7)?,
        n: r.get(8)?, pass_first: r.get(9)?, retries: r.get(10)?, failed: r.get(11)?, gate_met: r.get(12)?, gate_unmet: r.get(13)?, audit_sum: r.get(14)?, audit_n: r.get(15)?,
        ms_total: r.get(16)?, usd_total: r.get(17)?, reasked: r.get(18)?, look_sum: r.get(19)?, look_n: r.get(20)? }))?.filter_map(Result::ok).collect::<Vec<_>>();
    Ok(rows.into_iter().map(|x| ((x.job.clone(), x.size.clone(), x.domain.clone(), x.stack.clone(), x.suite.clone(), x.planner.clone(), x.worker.clone(), x.qa.clone()), x)).collect())
}
/// Add `sign` x the counters of `x` to the user's row of the same key (creating it for a positive add). The counters never go below zero.
fn add_counters(c: &Connection, x: &Shipped, sign: i64) -> rusqlite::Result<()> {
    let f = sign as f64;
    c.execute("INSERT INTO merge_scores (job, size, domain, stack, suite, planner, worker, qa, n, pass_first, retries, failed, gate_met, gate_unmet, audit_sum, audit_n, ms_total, usd_total, reasked, look_sum, look_n, source, legacy)
               VALUES (?1,?2,?3,?4,?5,?6,?7,?8, MAX(0,?9), MAX(0,?10), MAX(0,?11), MAX(0,?12), MAX(0,?13), MAX(0,?14), MAX(0,?15), MAX(0,?16), MAX(0,?17), MAX(0,?18), MAX(0,?19), MAX(0,?20), MAX(0,?21), 'bench', 0)
               ON CONFLICT(job, size, domain, stack, suite, planner, worker, qa) DO UPDATE SET
               n = MAX(0, n + ?9), pass_first = MAX(0, pass_first + ?10), retries = MAX(0, retries + ?11), failed = MAX(0, failed + ?12), gate_met = MAX(0, gate_met + ?13), gate_unmet = MAX(0, gate_unmet + ?14),
               audit_sum = MAX(0, audit_sum + ?15), audit_n = MAX(0, audit_n + ?16), ms_total = MAX(0, ms_total + ?17), usd_total = MAX(0, usd_total + ?18), reasked = MAX(0, reasked + ?19),
               look_sum = MAX(0, look_sum + ?20), look_n = MAX(0, look_n + ?21)",
        params![x.job, x.size, x.domain, x.stack, x.suite, x.planner, x.worker, x.qa, x.n * sign, x.pass_first * sign, x.retries * sign, x.failed * sign, x.gate_met * sign, x.gate_unmet * sign,
                x.audit_sum * f, x.audit_n * sign, x.ms_total * sign, x.usd_total * f, x.reasked * sign, x.look_sum * f, x.look_n * sign])?;
    Ok(())
}
/// Bring this record to the given defaults: for every shipped row, add the DIFFERENCE between what this version ships and what the ledger says we already added; a row an older
/// version shipped and this one dropped is taken back out; then the ledger is replaced and the weights are recomputed. A no-op when the ledger already holds this version.
/// Returns how many rows changed. Idempotent, and one transaction.
pub fn apply_shipped(c: &Connection, d: &Defaults) -> rusqlite::Result<usize> {
    ensure_ledger(c)?;
    let have: Option<String> = c.query_row("SELECT v FROM merge_shipped_meta WHERE k = 'version'", [], |r| r.get(0)).ok();
    if have.as_deref().unwrap_or("") == d.version && (!d.version.is_empty() || d.rows.is_empty()) { return Ok(0); }
    let tx = c.unchecked_transaction()?;
    let old = ledger(&tx)?;
    let mut changed = 0usize;
    let mut cells: std::collections::BTreeSet<(String, String)> = Default::default();
    let key = |x: &Shipped| (x.job.clone(), x.size.clone(), x.domain.clone(), x.stack.clone(), x.suite.clone(), x.planner.clone(), x.worker.clone(), x.qa.clone());
    let mut seen: std::collections::HashSet<_> = Default::default();
    for x in &d.rows {
        let k = key(x); seen.insert(k.clone());
        let delta = match old.get(&k) {
            Some(o) => Shipped { n: x.n - o.n, pass_first: x.pass_first - o.pass_first, retries: x.retries - o.retries, failed: x.failed - o.failed, gate_met: x.gate_met - o.gate_met, gate_unmet: x.gate_unmet - o.gate_unmet,
                audit_sum: x.audit_sum - o.audit_sum, audit_n: x.audit_n - o.audit_n, ms_total: x.ms_total - o.ms_total, usd_total: x.usd_total - o.usd_total, reasked: x.reasked - o.reasked,
                look_sum: x.look_sum - o.look_sum, look_n: x.look_n - o.look_n, ..x.clone() },
            None => x.clone(),
        };
        if delta.n == 0 && delta.pass_first == 0 && delta.retries == 0 && delta.failed == 0 && delta.gate_met == 0 && delta.gate_unmet == 0 && delta.audit_n == 0 && delta.ms_total == 0 && delta.usd_total == 0.0 && old.contains_key(&k) { continue; }
        if old.contains_key(&k) || x.n > 0 { add_counters(&tx, &delta, 1)?; changed += 1; cells.insert((x.job.clone(), x.size.clone())); }
    }
    for (k, o) in &old {
        if seen.contains(k) { continue; }
        add_counters(&tx, o, -1)?; changed += 1; cells.insert((o.job.clone(), o.size.clone()));
    }
    // a row only we ever fed, and nothing has used since, is not worth a line
    tx.execute("DELETE FROM merge_scores WHERE n = 0 AND source = 'bench' AND audit_n = 0 AND ms_total = 0 AND job || '/' || size IN (SELECT job || '/' || size FROM merge_shipped)", [])?;
    tx.execute("DELETE FROM merge_shipped", [])?;
    for x in &d.rows {
        tx.execute(&format!("INSERT OR REPLACE INTO merge_shipped ({SHIPPED_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)"),
            params![x.job, x.size, x.domain, x.stack, x.suite, x.planner, x.worker, x.qa, x.n, x.pass_first, x.retries, x.failed, x.gate_met, x.gate_unmet, x.audit_sum, x.audit_n, x.ms_total, x.usd_total, x.reasked, x.look_sum, x.look_n])?;
    }
    tx.execute("INSERT OR REPLACE INTO merge_shipped_meta (k, v) VALUES ('version', ?1)", params![d.version])?;
    for (job, size) in &cells { reweigh(&tx, job, size)?; }
    tx.commit()?;
    Ok(changed)
}

/// WHO A ROW CAME FROM. `ours`: only the benchmarks we shipped. `yours`: only this user's own runs. `both`: ours with the user's runs on top. Pure - tested.
pub fn origin_of(n: i64, shipped_n: i64) -> &'static str {
    if shipped_n <= 0 { "yours" } else if n <= shipped_n { "ours" } else { "both" }
}
/// Mark each row of the table with `origin`, `shipped_n` and `own_n`, from the ledger. A record with no ledger (a test, a first run) marks everything `yours`.
fn mark_origin(c: &Connection, rows: &mut [Value]) {
    let _ = ensure_ledger(c);
    let led = ledger(c).unwrap_or_default();
    for r in rows.iter_mut() {
        let g = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let k = (g("job"), g("size"), g("domain"), g("stack"), g("suite"), g("planner"), g("worker"), g("qa"));
        let sn = led.get(&k).map(|x| x.n).unwrap_or(0);
        let n = r.get("n").and_then(Value::as_i64).unwrap_or(0);
        r["shipped_n"] = json!(sn); r["own_n"] = json!((n - sn).max(0)); r["origin"] = json!(origin_of(n, sn));
    }
}
/// What the Settings screen says about the defaults: the version we shipped, how many rows and samples are ours, and how many of the user's rows have their own runs on top.
pub fn defaults_summary(rows: &[Value]) -> Value {
    let count = |o: &str| rows.iter().filter(|r| r.get("origin").and_then(Value::as_str) == Some(o)).count();
    let own: i64 = rows.iter().map(|r| r.get("own_n").and_then(Value::as_i64).unwrap_or(0)).sum();
    let ours: i64 = rows.iter().map(|r| r.get("shipped_n").and_then(Value::as_i64).unwrap_or(0)).sum();
    json!({ "version": shipped_defaults().version, "ours_rows": count("ours"), "both_rows": count("both"), "yours_rows": count("yours"), "ours_samples": ours, "own_samples": own })
}
/// THE DOMAIN DIMENSION (operator 2026-09-23: roles, not jobs — "how to get winners under these categories?"). A row
/// is now keyed by the KIND OF WORK as well: a merge that wins on backend files need not win on screens. A table from
/// before this migration has no domain and cannot be told one, so every old row becomes `any` — the pool a role falls
/// back to until its own domain has enough samples. The UNIQUE key has to change, which SQLite only does by rebuilding.
/// RE-ASKS, ON THE ROW THAT EARNED THEM (operator 2026-09-25, asked whether a re-ask should move the weight:
/// "yes"). Additive: it changes no key, so no table is rebuilt and nothing already recorded moves.
fn migrate_reasked(c: &Connection) -> rusqlite::Result<()> {
    if !c.prepare("SELECT 1 FROM pragma_table_info('merge_scores') WHERE name = 'reasked'")?.exists([])? {
        c.execute_batch("ALTER TABLE merge_scores ADD COLUMN reasked INTEGER NOT NULL DEFAULT 0")?;
    }
    Ok(())
}

/// HOW A CELL LOOKS (PLAN-THE-DESIGN D6, 2026-10-01). Operator: Claude and Composer both met the same frontend goal and Claude's looked clearly better,
/// while the router - which scored behaviour only - could not tell them apart. `look_sum / look_n` is the cell's `looks` score (0..1, the probability that its
/// page beats the fixed reference's, from the pairwise judge in `looks.rs`) and the number of judged matches behind it. Additive and defaulted like `audit_*`:
/// no table is rebuilt and nothing already recorded moves. Unlike `audit_sum` it is REWRITTEN, never incremented - a Bradley-Terry score is a fit of
/// all the matches, so `import_looks` replaces it whole and a second import is a no-op.
fn migrate_looks(c: &Connection) -> rusqlite::Result<()> {
    if !c.prepare("SELECT 1 FROM pragma_table_info('merge_scores') WHERE name = 'look_sum'")?.exists([])? {
        c.execute_batch("ALTER TABLE merge_scores ADD COLUMN look_sum REAL NOT NULL DEFAULT 0")?;
    }
    if !c.prepare("SELECT 1 FROM pragma_table_info('merge_scores') WHERE name = 'look_n'")?.exists([])? {
        c.execute_batch("ALTER TABLE merge_scores ADD COLUMN look_n INTEGER NOT NULL DEFAULT 0")?;
    }
    Ok(())
}

/// DISCONTINUED, NOT DELETED (operator 2026-09-27: "remove all zen merges completely, add to a legacy benchmark
/// discontinued and not visible by the user. We only use subscriptions — that is the moat of antiloki").
///
/// Every merge antiloki shipped until today routed through OpenCode Zen, which bills metered credit rather than a
/// subscription. Those arms are withdrawn: `Pair::builtin` no longer returns one, so none can be selected or launched.
/// Their MEASUREMENTS stay, because deleting them would destroy evidence that cannot be re-created — 532 samples, and
/// the `source='use'` rows are observed production usage, the irreplaceable class. So the rows are MARKED instead:
/// `legacy = 1` keeps them queryable for an audit while `routing_sql` and `table()` stop showing them to the operator
/// and stop letting them rank anything.
///
/// Additive, so no table is rebuilt and nothing already recorded moves. The backfill asks `cli::side_is_metered` about
/// each side rather than matching model names here, so the rule lives in one place.
fn migrate_legacy(c: &Connection) -> rusqlite::Result<()> {
    if !c.prepare("SELECT 1 FROM pragma_table_info('merge_scores') WHERE name = 'legacy'")?.exists([])? {
        c.execute_batch("ALTER TABLE merge_scores ADD COLUMN legacy INTEGER NOT NULL DEFAULT 0")?;
    }
    // the backfill runs once per new row too: a side that is metered can never belong in the live table
    let rows: Vec<(i64, String, String)> = c
        .prepare("SELECT id, planner, worker FROM merge_scores WHERE legacy = 0")?
        .query_map([], |r| Ok((r.get(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut marked = 0usize;
    for (id, planner, worker) in rows {
        if crate::cli::side_is_metered(&planner).is_some() || crate::cli::side_is_metered(&worker).is_some() {
            c.execute("UPDATE merge_scores SET legacy = 1 WHERE id = ?1", params![id])?;
            marked += 1;
        }
    }
    if marked > 0 { eprintln!("merges: {marked} row(s) moved to the legacy archive (metered, not a subscription)"); }
    Ok(())
}

fn migrate_domain(c: &Connection) -> rusqlite::Result<()> {
    let has: bool = c.prepare("SELECT 1 FROM pragma_table_info('merge_scores') WHERE name = 'domain'")?.exists([])?;
    if has { return Ok(()); }
    c.execute_batch("
        BEGIN;
        ALTER TABLE merge_scores RENAME TO merge_scores_old;
        CREATE TABLE merge_scores (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            job TEXT NOT NULL, size TEXT NOT NULL, domain TEXT NOT NULL DEFAULT 'any',
            planner TEXT NOT NULL, worker TEXT NOT NULL, qa TEXT NOT NULL DEFAULT '',
            n INTEGER NOT NULL DEFAULT 0, pass_first INTEGER NOT NULL DEFAULT 0, retries INTEGER NOT NULL DEFAULT 0,
            failed INTEGER NOT NULL DEFAULT 0, gate_met INTEGER NOT NULL DEFAULT 0, gate_unmet INTEGER NOT NULL DEFAULT 0,
            audit_sum REAL NOT NULL DEFAULT 0, audit_n INTEGER NOT NULL DEFAULT 0,
            ms_total INTEGER NOT NULL DEFAULT 0, usd_total REAL NOT NULL DEFAULT 0,
            weight REAL NOT NULL DEFAULT 0, source TEXT NOT NULL DEFAULT 'run', updated TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE (job, size, domain, planner, worker, qa)
        );
        INSERT INTO merge_scores (job, size, domain, planner, worker, qa, n, pass_first, retries, failed, gate_met, gate_unmet, audit_sum, audit_n, ms_total, usd_total, weight, source, updated)
            SELECT job, size, 'any', planner, worker, qa, n, pass_first, retries, failed, gate_met, gate_unmet, audit_sum, audit_n, ms_total, usd_total, weight, source, updated FROM merge_scores_old;
        DROP TABLE merge_scores_old;
        COMMIT;")
}

/// THE TECHNOLOGY DIMENSION ON THE RECORD ITSELF (PLAN-THE-LEAN-YEARS II.2 · II.7, 2026-09-25). A merge that wins on
/// Rust need not win on Python — that is the whole premise of adopting a polyglot benchmark, and until now the record
/// could not hold the answer: `stack` lived only on `ai_calls`, deliberately, "until the sampler that will use it
/// exists" (audit.rs). It exists, so this is that migration.
///
/// Every row written before today becomes `any` — the POOL, exactly as the domain migration did, and for the same
/// reason: a row measured before the column existed cannot be told which language it was, and guessing would be worse
/// than pooling. `best_for_role` reads a stack's own rows first and falls back to the pool, so nothing that works
/// today stops working: on an unmeasured stack the router sees precisely what it saw yesterday.
///
/// The UNIQUE key has to change, which SQLite only does by rebuilding the table.
fn migrate_stack(c: &Connection) -> rusqlite::Result<()> {
    let has: bool = c.prepare("SELECT 1 FROM pragma_table_info('merge_scores') WHERE name = 'stack'")?.exists([])?;
    if has { return Ok(()); }
    c.execute_batch("
        BEGIN;
        ALTER TABLE merge_scores RENAME TO merge_scores_pre_stack;
        CREATE TABLE merge_scores (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            job TEXT NOT NULL, size TEXT NOT NULL, domain TEXT NOT NULL DEFAULT 'any', stack TEXT NOT NULL DEFAULT 'any',
            planner TEXT NOT NULL, worker TEXT NOT NULL, qa TEXT NOT NULL DEFAULT '',
            n INTEGER NOT NULL DEFAULT 0, pass_first INTEGER NOT NULL DEFAULT 0, retries INTEGER NOT NULL DEFAULT 0,
            failed INTEGER NOT NULL DEFAULT 0, gate_met INTEGER NOT NULL DEFAULT 0, gate_unmet INTEGER NOT NULL DEFAULT 0,
            audit_sum REAL NOT NULL DEFAULT 0, audit_n INTEGER NOT NULL DEFAULT 0,
            ms_total INTEGER NOT NULL DEFAULT 0, usd_total REAL NOT NULL DEFAULT 0,
            weight REAL NOT NULL DEFAULT 0, source TEXT NOT NULL DEFAULT 'run', updated TEXT NOT NULL DEFAULT (datetime('now')),
            reasked INTEGER NOT NULL DEFAULT 0,
            UNIQUE (job, size, domain, stack, planner, worker, qa)
        );
        INSERT INTO merge_scores (job, size, domain, stack, planner, worker, qa, n, pass_first, retries, failed, gate_met, gate_unmet, audit_sum, audit_n, ms_total, usd_total, weight, source, updated, reasked)
            SELECT job, size, domain, 'any', planner, worker, qa, n, pass_first, retries, failed, gate_met, gate_unmet, audit_sum, audit_n, ms_total, usd_total, weight, source, updated, reasked
            FROM merge_scores_pre_stack;
        DROP TABLE merge_scores_pre_stack;
        COMMIT;")
}

/// WHICH BENCHMARK A ROW CAME FROM (operator 2026-09-26: "keep each bench separated on memory so we can compare").
///
/// `source` says the evidence CLASS — a bench or real use. It does not say WHICH bench, and after SWE-bench wrote
/// three rows into the same table as ministore and polyglot, that gap stopped being theoretical: two suites that
/// measure different abilities were being averaged into one number, and one of them measured abilities antiloki does
/// not even use.
///
/// So the suite joins the key. Two benchmarks that happen to land on the same job × size × domain × language stay two
/// rows and can be read against each other, instead of silently becoming one row that is neither.
///
/// Rows written before today have no suite and keep `''` — "some bench, we no longer know which" — which is the truth
/// about them and is exactly why the column exists.
fn migrate_suite(c: &Connection) -> rusqlite::Result<()> {
    let has: bool = c.prepare("SELECT 1 FROM pragma_table_info('merge_scores') WHERE name = 'suite'")?.exists([])?;
    if has { return Ok(()); }
    c.execute_batch("
        BEGIN;
        ALTER TABLE merge_scores RENAME TO merge_scores_pre_suite;
        CREATE TABLE merge_scores (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            job TEXT NOT NULL, size TEXT NOT NULL, domain TEXT NOT NULL DEFAULT 'any', stack TEXT NOT NULL DEFAULT 'any',
            suite TEXT NOT NULL DEFAULT '',
            planner TEXT NOT NULL, worker TEXT NOT NULL, qa TEXT NOT NULL DEFAULT '',
            n INTEGER NOT NULL DEFAULT 0, pass_first INTEGER NOT NULL DEFAULT 0, retries INTEGER NOT NULL DEFAULT 0,
            failed INTEGER NOT NULL DEFAULT 0, gate_met INTEGER NOT NULL DEFAULT 0, gate_unmet INTEGER NOT NULL DEFAULT 0,
            audit_sum REAL NOT NULL DEFAULT 0, audit_n INTEGER NOT NULL DEFAULT 0,
            ms_total INTEGER NOT NULL DEFAULT 0, usd_total REAL NOT NULL DEFAULT 0,
            weight REAL NOT NULL DEFAULT 0, source TEXT NOT NULL DEFAULT 'run', updated TEXT NOT NULL DEFAULT (datetime('now')),
            reasked INTEGER NOT NULL DEFAULT 0,
            UNIQUE (job, size, domain, stack, suite, planner, worker, qa)
        );
        INSERT INTO merge_scores (job, size, domain, stack, suite, planner, worker, qa, n, pass_first, retries, failed, gate_met, gate_unmet, audit_sum, audit_n, ms_total, usd_total, weight, source, updated, reasked)
            SELECT job, size, domain, stack, '', planner, worker, qa, n, pass_first, retries, failed, gate_met, gate_unmet, audit_sum, audit_n, ms_total, usd_total, weight, source, updated, reasked
            FROM merge_scores_pre_suite;
        DROP TABLE merge_scores_pre_suite;
        COMMIT;")
}

/// A SUITE THAT DOES NOT ROUTE. Some benchmarks are CONTROLS: they answer "can it do this at all, and what does that
/// cost" without measuring anything the router chooses between. SWE-bench is the worked example — it sees 3 of
/// antiloki's 8 claims, is blind to 4 and penalises decomposition, so a number from it must not move a table that
/// routes them. Named here rather than in a config, because the list is an argument and should be read as one.
///
/// SPELLED ONCE, ASKED TWICE. The rule is needed in Rust (ranking rows in memory) and in SQL (picking a winner in the
/// database), and a `matches!` beside a hand-written `NOT IN` is how a guard half-applies for months without anyone
/// noticing. One list; both callers derive from it.
pub const CONTROL_SUITES: &[&str] = &["swebench", "swe", "control"];

pub fn suite_routes(suite: &str) -> bool { !CONTROL_SUITES.contains(&suite.trim()) }

/// the same rule as a SQL predicate, for the queries that choose a winner. Built from the list above, never typed out.
/// A QUALITY THAT WAS CHECKED, in SQL. `quality_of` calls a row AUDITED when `audit_n > 0` and GATED when the test gate
/// ran; everything else is CLAIMED — a weight derived from "no task errored", which is not a statement about whether the
/// work was right. `verifier` and `planner` hold ~75 such samples across four domains.
pub fn measured_sql() -> &'static str { "(audit_n > 0 OR gate_met + gate_unmet > 0)" }

pub fn routing_sql() -> String {
    // `legacy = 0` FIRST: a withdrawn arm must not win a route. The Zen merges hold the largest sample in the table
    // (532), so without this clause they would out-vote every subscription arm for months on volume alone.
    //
    // THE BASELINE IS NOT A CANDIDATE (audit 2026-09-28) — and this is what "it is always choosing sonnet" actually
    // was. `planner = 'raw'` marks the SOLO yardstick: one model, no pipeline, no brain. 77 such rows were routable,
    // and they win on weight because a raw arm carries no pipeline overhead — so `best("editor-l", "l")` returned a
    // row whose planner is `raw`. `best_for` then hits its own guard (`if key == "raw" … return None`) and answers
    // NOTHING, so `job_binding_learned` falls through to `job_binding_any` — the static default, which is Claude at
    // its tier. Every measured arm in the cell was skipped, every time, and the record looked healthy throughout.
    //
    // The guard was in the wrong place: discarding a winner after choosing it is not the same as not choosing it.
    // Same rule as the control suites above — an arm the router can never pick must not appear in the contest.
    // `raw_compare` reads the baseline with its own query and is unaffected.
    format!("legacy = 0 AND planner NOT IN ('raw', '-') AND suite NOT IN ({})", control_list())
}
fn control_list() -> String { CONTROL_SUITES.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(", ") }

/// THE SOLO CELLS ARE CANDIDATES TOO (operator 2026-09-30: "tasks L and smaller doesnt need a planner"; item 4 of the
/// 2026-10-01 queue). `routing_sql` keeps the `raw` baseline out of every contest, which is right for the jobs that
/// need a brain and wrong for the one question the no-planner rule asks: at size S or L, is ONE seat doing the whole
/// job (a `raw` row — no pipeline, no planner, no second model) better value than a planner × worker pair? A solo
/// row competes only when its quality was MEASURED (audited or gated — a bench's `raw` arm always is), never on "no
/// task errored"; the control suites and the withdrawn archive stay out exactly as in `routing_sql`.
pub fn routing_solo_sql() -> String {
    format!("legacy = 0 AND planner != '-' AND (planner != 'raw' OR (worker NOT IN ('', '-', 'raw') AND {})) AND suite NOT IN ({})",
            measured_sql(), control_list())
}
/// A solo cell is believed on two audited instances, not one: one is an anecdote, and the pair rows it competes with
/// are the product of a pipeline that has been benched for weeks. Fewer than this and the solo cell is "not measured
/// enough", which falls back to today's behaviour (a pair's hands run the job alone) and says so.
pub const SOLO_MIN_N: i64 = 2;
/// Do solo cells compete in this cell? Only the EDITING jobs at the sizes that need no planner (s and l).
pub fn solo_competes(job: &str, size: &str) -> bool { matches!(job, "editor-s" | "editor-l") && matches!(size, "s" | "l") }

fn any_domain() -> String { "any".into() }
/// an empty domain is the pool every old row lives in
fn dom(d: &str) -> String { let d = d.trim(); if d.is_empty() { "any".into() } else { d.to_string() } }
/// and so is an empty stack — a caller that does not know the technology must land in the pool, never in a language
fn stk(s: &str) -> String { let s = s.trim(); if s.is_empty() { "any".into() } else { s.to_ascii_lowercase() } }
/// an unnamed suite stays unnamed: "" means "some bench, we no longer know which", which is a fact, not a default
fn suite_of(s: &str) -> String { s.trim().to_ascii_lowercase() }
/// What one finished run contributes. `audit` is an external quality score 0..1 when one exists (a bench's audit,
/// a test suite's pass ratio); the run's own signals are the verifier's first-pass verdicts and the gate.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Outcome {
    pub job: String, pub size: String, pub planner: String, pub worker: String, pub qa: String,
    pub tasks: i64, pub pass_first: i64, pub retries: i64, pub failed: i64,
    pub gate_met: i64, pub gate_unmet: i64, pub audit: Option<f64>, pub ms: i64, pub usd: f64,
    #[serde(default)] pub source: String,
    /// WHAT KIND OF WORK this was — `roles::domain_of` over the files the tasks owned; "any" when nothing says. A
    /// role reads its own domain first and falls back to `any` (2026-09-23).
    #[serde(default = "any_domain")] pub domain: String,
    /// WHAT THE REPO IS BUILT IN — `gates::stack_at` over its manifests (rust · web · node · go · python · dotnet ·
    /// jvm · mixed), or the language a polyglot exercise is written in. "any" is the pool every row written before the
    /// column existed lives in, and where a caller that does not know the technology belongs (II.2 · II.7).
    #[serde(default = "any_domain")] pub stack: String,
    /// WHICH BENCHMARK MEASURED THIS (2026-09-26): `ministore` · `polyglot` · `swtbench` · `clarify` · `fea` …
    /// Empty for real use and for rows written before the column existed. Part of the key, so two suites never
    /// average into one number.
    #[serde(default)] pub suite: String,
    /// true = only the audit joins an existing row (a bench scoring a run the serve already recorded); n and the
    /// counters stay — the run is one sample, not two (2026-09-23: the matrix double-counted every arm)
    #[serde(default)] pub audit_only: bool,
}
/// "claude-code:sonnet" — the key a side is filed under. Pure.
/// WHAT KIND OF EVIDENCE A ROW IS — and the one thing in this file that must never be got wrong.
///
/// Operator 2026-09-25, on being told the two were mixed: *"corrupted? im worry. Make impossible to be
/// corrupted, virtually impossible."*
///
/// `merge_scores` holds two incomparable things. **Controlled** rows come from a bench: the same task, the same
/// fixture, every pairing given the same work, so their costs and times mean the same thing across rows.
/// **Observed** rows come from real use: different asks, different repos, different context sizes. A cheap
/// observed row is cheap because someone asked it something small, not because the pairing is efficient.
///
/// Ranking them together does not just blur an average. `reweigh_in` normalises every row against the best
/// cost, best time and best quality IN ITS CELL, so one flattering observed row moves the baselines and can
/// push a genuinely-measured winner out of the 80 % band — the merge the operator should be told to use
/// loses to one that was never measured against it.
///
/// THE RULE THAT MAKES THIS HARD TO GET WRONG AGAIN: this function is total over the sources this project
/// writes, and it returns `None` for anything it does not recognise. An unrecognised source is **quarantined**
/// — excluded from weighting entirely and counted where the operator can see it — rather than defaulting into
/// either pool. A future source string cannot quietly become controlled evidence; the worst it can do is not
/// count, which is visible, instead of counting wrongly, which is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Evidence {
    /// a bench: same task, same fixture, comparable across rows
    Controlled,
    /// real use: comparable only with itself
    Observed,
}

/// Every source string this codebase writes, and nothing else. `None` = quarantine, never a default.
pub fn evidence_of(source: &str) -> Option<Evidence> {
    match source.trim() {
        // `bench` is controlled. `bench+run` is a bench row a later real run also landed on: it carries the
        // bench's comparable task, so it stays controlled — dropping it would throw away the strongest rows.
        "bench" | "bench+run" => Some(Evidence::Controlled),
        // real use, in its three spellings. "" is the historical default of `record`, which meant `run`.
        "run" | "use" | "run+audit" | "" => Some(Evidence::Observed),
        _ => None,
    }
}

/// THE NAME OF ONE SIDE OF A MERGE — `provider:model`, always both (operator 2026-09-28: "Antropic-sonnet is
/// different from cursor-sonnet and also from opencode-sonnet ... the origins must be explicitly so we know whats is
/// spending our money").
///
/// AN EMPTY MODEL IS NOT A MODEL (audit 2026-09-28). This read `model.unwrap_or("default")`, which catches `None`
/// and lets `Some("")` straight through — so a pair whose brain names no model, like `gemini-composer`, wrote twelve
/// rows keyed `gemini:` with nothing after the colon. That is the one shape the operator's rule forbids: a provider
/// with no model beside it. A blank is an absent model and is named as such. Pure — tested.
pub fn key(engine: &str, model: Option<&str>) -> String {
    let m = model.map(str::trim).filter(|m| !m.is_empty()).unwrap_or("default");
    // ONE SPELLING PER MODEL (audit 2026-09-28). A model arrives here under the alias the binding asked for and
    // again under the full id the CLI reported, so one model's spend was split across two rows. `model_canon`
    // folds only where the mapping cannot be wrong; the PROVIDER half is never touched.
    let m = match crate::cli::engine_of(&json!(engine)) { Some(e) => crate::cli::model_canon(e, m), None => m.to_string() };
    format!("{engine}:{m}")
}
/// The size bucket of a run: s = one task on one file and a short ask · xxl = a wave of two or more · l = the rest. Pure.
pub fn size_of(tasks: usize, one_file: bool, goal_chars: usize) -> &'static str { if tasks >= 2 { "xxl" } else if one_file && goal_chars <= 400 { "s" } else { "l" } }

/// Add a run's outcome to its row (upsert) and reweigh the job × size. Returns the row's new weight.
/// A QA gate's own record: no task of its own, only the claims it checked (met / unmet). Its counts and cost join the run's row; the run is
/// not counted a second time. An unjudged raw comparison (`tasks: 0` and no gate) IS a run and still counts. Pure — tested.
/// The signature is the QA seat: the gate names one and has no task. Its counts may be all zero — a gate whose every claim came back
/// UNPROVEN has neither met nor unmet, and keying on the counts let those still count the run twice (N4, 2026-09-29: n = 6 for 3 runs).
pub fn is_gate_only(o: &Outcome) -> bool { o.tasks == 0 && !o.qa.trim().is_empty() }
pub fn record(o: &Outcome) -> Result<f64, String> {
    let c = open().map_err(|e| e.to_string())?;
    if o.audit_only {
        // AN AUDIT MUST FIND ITS RUN, OR IT IS THROWN AWAY (2026-09-26). This UPDATEd a row matched on
        // job+size+planner+worker+qa+domain+stack+suite. The bench cannot know three of those: the serve INFERS
        // `domain` and `stack` from the work (a docs ask filed as domain=docs, stack=web) and sets `suite` itself, so
        // the bench posted any/any/'' , nothing matched, the endpoint answered 409, and the audit vanished.
        //
        // It vanished 14 times out of 14 on one run and left 92 samples across 22 rows with no audit at all — which is
        // the real reason `planner` showed 25 samples and 9 audits. The traceback went to a log nobody reads, so the
        // row count looked healthy the whole time.
        //
        // THE AUDIT NOW FOLLOWS THE ROW instead of demanding the row match the audit: the exact key first, then the
        // newest row for this job+size+planner+worker whatever its domain, stack, suite or qa — because a bench that
        // just ran an arm is unambiguously auditing that arm's row.
        let exact = c.execute("UPDATE merge_scores SET audit_sum = audit_sum + ?6, audit_n = audit_n + 1, source = CASE WHEN source = 'run' THEN 'run+audit' ELSE source END, updated = datetime('now') WHERE job=?1 AND size=?2 AND planner=?3 AND worker=?4 AND qa=?5 AND domain=?7 AND stack=?8 AND suite=?9",
            params![o.job, o.size, o.planner, o.worker, o.qa, o.audit.unwrap_or(0.0), dom(&o.domain), stk(&o.stack), suite_of(&o.suite)]).map_err(|e| e.to_string())?;
        let (qa_key, d_key, s_key, su_key) = if exact > 0 {
            (o.qa.clone(), dom(&o.domain).to_string(), stk(&o.stack).to_string(), suite_of(&o.suite).to_string())
        } else {
            let found: Option<(String, String, String, String)> = c.query_row(
                "SELECT qa, domain, stack, suite FROM merge_scores WHERE job=?1 AND size=?2 AND planner=?3 AND worker=?4 ORDER BY updated DESC, id DESC LIMIT 1",
                params![o.job, o.size, o.planner, o.worker],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).ok();
            let Some((qa, d, st, su)) = found else {
                return Err(format!("no such row to audit — record the run first (job {} size {} {} / {})", o.job, o.size, o.planner, o.worker));
            };
            c.execute("UPDATE merge_scores SET audit_sum = audit_sum + ?6, audit_n = audit_n + 1, source = CASE WHEN source = 'run' THEN 'run+audit' ELSE source END, updated = datetime('now') WHERE job=?1 AND size=?2 AND planner=?3 AND worker=?4 AND qa=?5 AND domain=?7 AND stack=?8 AND suite=?9",
                params![o.job, o.size, o.planner, o.worker, qa, o.audit.unwrap_or(0.0), d, st, su]).map_err(|e| e.to_string())?;
            (qa, d, st, su)
        };
        reweigh(&c, &o.job, &o.size).map_err(|e| e.to_string())?;
        return c.query_row("SELECT weight FROM merge_scores WHERE job=?1 AND size=?2 AND planner=?3 AND worker=?4 AND qa=?5 AND domain=?6 AND stack=?7 AND suite=?8", params![o.job, o.size, o.planner, o.worker, qa_key, d_key, s_key, su_key], |r| r.get(0)).map_err(|e| e.to_string());
    }
    // A METERED RESULT NEVER ENTERS THE LIVE TABLE (operator 2026-09-27: "we only use subscriptions — that is the moat
    // of antiloki"). It is still RECORDED — a measurement is evidence even when the arm is withdrawn, and the
    // `source='use'` rows are the irreplaceable kind — but it lands in the legacy archive, invisible to the operator
    // and excluded from routing. Decided here, on the way in, so no later pass has to remember.
    let legacy = i64::from(crate::cli::side_is_metered(&o.planner).is_some() || crate::cli::side_is_metered(&o.worker).is_some());
    // A RECORD WITH NO TASK IS NOT A SAMPLE (audit 2026-09-29). The QA gate records its met/unmet — and what the check cost — into the row
    // of the run it checked, with `tasks: 0`; this added one to `n` regardless, and since QA became obligatory (2026-09-28) EVERY run was
    // counted twice (S2: 16 samples for 8 arms). Its counts and cost still join the row; the run is counted once, by the run.
    c.execute("INSERT INTO merge_scores (job, size, domain, stack, suite, planner, worker, qa, n, pass_first, retries, failed, gate_met, gate_unmet, audit_sum, audit_n, ms_total, usd_total, source, legacy)
               VALUES (?1, ?2, ?16, ?17, ?18, ?3, ?4, ?5, ?20, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?19)
               ON CONFLICT(job, size, domain, stack, suite, planner, worker, qa) DO UPDATE SET n = n + excluded.n, pass_first = pass_first + excluded.pass_first, retries = retries + excluded.retries,
               failed = failed + excluded.failed, gate_met = gate_met + excluded.gate_met, gate_unmet = gate_unmet + excluded.gate_unmet,
               audit_sum = audit_sum + excluded.audit_sum, audit_n = audit_n + excluded.audit_n, ms_total = ms_total + excluded.ms_total, usd_total = usd_total + excluded.usd_total,
               source = CASE WHEN source = 'bench' AND excluded.source = 'run' THEN 'bench+run' ELSE source END, updated = datetime('now')",
        params![o.job, o.size, o.planner, o.worker, o.qa, o.pass_first, o.retries, o.failed, o.gate_met, o.gate_unmet, o.audit.unwrap_or(0.0), i64::from(o.audit.is_some()), o.ms, o.usd, if o.source.is_empty() { "run" } else { o.source.as_str() }, dom(&o.domain), stk(&o.stack), suite_of(&o.suite), legacy, i64::from(!is_gate_only(o))]).map_err(|e| e.to_string())?;
    reweigh(&c, &o.job, &o.size).map_err(|e| e.to_string())?;
    c.query_row("SELECT weight FROM merge_scores WHERE job=?1 AND size=?2 AND planner=?3 AND worker=?4 AND qa=?5 AND domain=?6 AND stack=?7 AND suite=?8", params![o.job, o.size, o.planner, o.worker, o.qa, dom(&o.domain), stk(&o.stack), suite_of(&o.suite)], |r| r.get(0)).map_err(|e| e.to_string())
}
/// THE WEIGHT (the tags' idea: one number per row, 0..1, recomputed from evidence). quality 0.7 · cost 0.2 · time 0.1 —
/// under the operator's rule "keep at least 80 % of the best and the cheaper wins": only rows keeping 80 % of the best
/// quality compete on cost and time — a 1-point gap loses to a 10× price gap, a 4-point gap never does (tested).
/// each against the best row of the same job × size, so a weight is a rank among peers, not an absolute.
///   quality = the mean of what is known: the audit (0..1), the first-pass rate, the gate's met ratio; a failed task is 0
///   cost, time = best/this (1 for the cheapest / fastest peer)
/// A row with fewer than 2 samples is shrunk toward 0.5 — one lucky run is not a winner.
fn reweigh(c: &Connection, job: &str, size: &str) -> rusqlite::Result<()> { reweigh_in(c, job, size, "", "")?; Ok(()) }
/// A ROW'S PEERS ARE THE ROWS OF THE SAME SUITE (2026-09-26). Cost, time and the 80 %-of-the-best quality band are all
/// relative to the best peer — and the best peer of a ministore row is another ministore row, never a polyglot
/// exercise that takes thirty seconds or a SWE-bench instance that takes six minutes. Comparing across suites made
/// the cheapest BENCHMARK look like the cheapest MERGE.
/// The peers of a row are the rows of the SAME kind of work (2026-09-23) AND THE SAME TECHNOLOGY (2026-09-25): a
/// backend merge is not ranked against a screen merge, and a merge measured on Rust is not ranked against one measured
/// on Python — that is the premise of a polyglot benchmark, and ranking across languages would bury it.
///
/// `""` for either means "every value present, each pool reweighed on its own"; `any` is itself a pool and is
/// reweighed like the rest. Every (domain, stack) combination the table holds for this job × size gets its own pass.
fn reweigh_in(c: &Connection, job: &str, size: &str, domain: &str, stack: &str) -> rusqlite::Result<()> {
    if domain.is_empty() || stack.is_empty() {
        let cells: Vec<(String, String, String)> = c.prepare("SELECT DISTINCT domain, stack, suite FROM merge_scores WHERE job=?1 AND size=?2 AND legacy = 0")?
            .query_map(params![job, size], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.filter_map(Result::ok)
            .filter(|(d, st, _): &(String, String, String)| (domain.is_empty() || d == domain) && (stack.is_empty() || st == stack)).collect();
        for (d, st, su) in cells { reweigh_one(c, job, size, &d, &st, &su)?; }
        return Ok(());
    }
    reweigh_one(c, job, size, domain, stack, "")
}
/// one comparable set: a job × size × kind of work × language × SUITE
fn reweigh_one(c: &Connection, job: &str, size: &str, domain: &str, stack: &str, suite: &str) -> rusqlite::Result<()> {
    // A WITHDRAWN ROW IS NOT A PEER (2026-09-27). Quality, cost and time are all scored RELATIVE to the best row in
    // the pool, so leaving the archived Zen arms in this query would let them keep setting the baseline every surviving
    // arm is normalised against — which is influence over routing, the exact thing archiving removes. Hiding them from
    // `table()` and from `routing_sql` is not enough while `q_best` can still come from one.
    let mut st = c.prepare("SELECT id, n, pass_first, retries, failed, gate_met, gate_unmet, audit_sum, audit_n, ms_total, usd_total, source, reasked FROM merge_scores WHERE job=?1 AND size=?2 AND domain=?3 AND stack=?4 AND suite=?5 AND legacy = 0")?;
    let all: Vec<(Option<Evidence>, (i64, i64, i64, i64, i64, i64, i64, f64, i64, i64, f64, i64))> = st
        .query_map(params![job, size, domain, stack, suite], |r| {
            let src: String = r.get(11)?;
            Ok((evidence_of(&src), (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?, r.get(12)?)))
        })?.filter_map(Result::ok).collect();
    if all.is_empty() { return Ok(()); }

    // A QUARANTINED ROW IS NOT SCORED AT ALL. Its weight is zeroed rather than left at whatever it last held,
    // so an unrecognised source can never keep an old rank it no longer earns. `integrity()` counts these and
    // `doctor` prints the count, because a row that silently stops counting is its own kind of lie.
    for (_, r) in all.iter().filter(|(e, _)| e.is_none()) {
        c.execute("UPDATE merge_scores SET weight = 0 WHERE id = ?1", params![r.0])?;
    }

    // …AND THE TWO CLASSES ARE SCORED SEPARATELY. This is the whole fix: the baselines below — best cost, best
    // time, best quality, and the 80 % band drawn from them — are computed WITHIN a class. Before this,
    // one observed row that happened to be cheap (because someone asked it something small) moved the
    // baseline and could push a genuinely-measured merge out of the band it deserved to win.
    for class in [Evidence::Controlled, Evidence::Observed] {
        let rows: Vec<(i64, i64, i64, i64, i64, i64, i64, f64, i64, i64, f64, i64)> =
            all.iter().filter(|(e, _)| *e == Some(class)).map(|(_, r)| *r).collect();
        if rows.is_empty() { continue }
        reweigh_rows(c, &rows)?;
    }
    Ok(())
}

/// Score one comparable set of rows against each other. Never call this with rows of two evidence classes —
/// that is the bug this split exists to prevent.
fn reweigh_rows(c: &Connection, rows: &[(i64, i64, i64, i64, i64, i64, i64, f64, i64, i64, f64, i64)]) -> rusqlite::Result<()> {
    {
    let per = |r: &(i64, i64, i64, i64, i64, i64, i64, f64, i64, i64, f64, i64)| -> (f64, f64, f64, bool) {
        let n = r.1.max(1) as f64;
          // QUALITY RANKS ITS SOURCES AND INVENTS NONE (2026-09-25). `quality_of` counts the gates even when
          // an audit exists — a run that scored 37/37 and then failed its build is not perfect — and answers
          // None where there is no evidence at all, which this used to fill with 0.5: a made-up number sitting
          // in the same column as measured ones. No evidence scores zero on the term rather than average.
          let graded = quality_of(r.7, r.8, r.2, r.3, r.4, r.5, r.6, r.11, r.1);
          let quality = graded.map(|(q, _)| q).unwrap_or(0.0);
          // IS THIS A MEASUREMENT, OR THE AGENT'S OWN WORD? `Claimed` is `pass_first / tasks` — what the run SAID
          // about itself when no audit and no gate spoke. It belongs in the column, and it must not set the bar.
          let measured = matches!(graded, Some((_, QualityKind::Audited)) | Some((_, QualityKind::Gated)));
        (quality, r.9 as f64 / n, r.10 / n, measured)
    };
    let scored: Vec<(i64, f64, f64, f64, i64, bool)> = rows.iter().map(|r| { let (q, ms, usd, m) = per(r); (r.0, q, ms, usd, r.1, m) }).collect();
    let best_ms = scored.iter().map(|x| x.2).filter(|v| *v > 0.0).fold(f64::INFINITY, f64::min);
    let best_usd = scored.iter().map(|x| x.3).filter(|v| *v > 0.0).fold(f64::INFINITY, f64::min);
    // PRICE FIRST, SPEED SECOND, QUALITY AS A TOLERANCE (operator 2026-09-26: "quality almost similar with a lot of
    // price reduction is far more important than speed. Make that a rule").
    //
    // THE TABLE HAD ITS OWN COPY OF THE SCORING and it was the quality-first one (0.7/0.2/0.1, band 0.06) — so
    // changing `Objective::weights` moved the ROUTER and left the number on the screen saying the opposite. One
    // rule, asked in both places: `Objective::Best` is what the table's weight means.
    //
    // The band is RELATIVE (2026-09-30): a row keeps its place while it holds at least 80 % of the best measured
    // quality. It was 0.06 absolute, which read a ten-instance bench's noise floor (steps of 0.10) as a verdict: on the
    // first real comparison that disqualified an arm costing 6.7x less for a ONE-instance difference. Inside the band
    // the bill decides; outside it, no discount buys the row back.
    let (wq, wc, wt) = crate::roles::Objective::Best.weights();
    // THE BAND IS ONE FIXED NUMBER (0.20 relative since 2026-09-30; it was 0.10 absolute), AND THE ATTEMPT TO DERIVE IT FROM n
    // FAILED (2026-09-26). Deriving it as `1/n` is the right
    // INSTINCT — the band is the instrument's resolution, and 0.10 is only the resolution of a ten-instance bench —
    // but `n` here counts RECORDED OUTCOMES, not graded items. The readers rows carry n=2 while each encodes 20
    // questions, so `1/n` gave a band of 0.25 and swallowed the measured four-point gap that keeps Sonnet on
    // synthesis. The record does not store the grading denominator, so the band cannot be derived from it.
    let band = crate::roles::Objective::Best.band();
    // AN UNMEASURED ROW MUST NOT SET THE BAR (2026-09-27). `q_best` took the maximum over EVERY row, including ones
    // whose only evidence is `Claimed` — the agent reporting its own success. In the design pool that handed the bar
    // to `claude-code:sonnet / flash` at audit_n=0 and a claimed 1.000, which pushed four genuinely MEASURED arms
    // (0.958-0.993) outside the band, zeroed their cost and time terms, and collapsed them all to ~0.2. The audited
    // arms were penalised for having been measured while an unmeasured row won the pool on its own say-so.
    //
    // The bar is now the best MEASURED row — audited or gated. With nothing measured in the cell the old behaviour
    // stands, because then a claim is the only evidence there is.
    let measured_best = scored.iter().filter(|x| x.5).map(|x| x.1).fold(f64::NEG_INFINITY, f64::max);
    let q_best = if measured_best.is_finite() { measured_best } else { scored.iter().map(|x| x.1).fold(0.0_f64, f64::max) };
    for (id, q, ms, usd, n, _measured) in scored {
        let in_band = in_band_of(q, n, q_best, band);
        let t = if !in_band { 0.0 } else if ms > 0.0 && best_ms.is_finite() { best_ms / ms } else { 0.5 };
        let cst = if !in_band { 0.0 } else if usd > 0.0 && best_usd.is_finite() { best_usd / usd } else { 0.5 };
        let raw = wq * q + wc * cst + wt * t;
        let shrink = (n as f64 / (n as f64 + 1.0)).min(1.0); // n=1 → ½ weight toward 0.5, n=3 → ¾ …
        let w = 0.5 + (raw - 0.5) * shrink;
        c.execute("UPDATE merge_scores SET weight=?1 WHERE id=?2", params![w.clamp(0.0, 1.0), id])?;
    }
    }
    Ok(())
}
/// The best combination for a job × size with at least `min_n` samples — what the router may prefer over the table.
///
/// THE POOLED READ FOLLOWS THE RULE, NOT THE TABLE'S ORDER (item 4 of the 2026-10-01 queue, from the HANDOFF's open audit
/// findings: "the router's pooled, domain-blind table still names some winners that are outside the 80 % band or not the
/// cheapest inside it"). This used to take the single row with the highest `weight` — and a weight only means something
/// among the rows of ONE pool (job × size × domain × stack × suite): `reweigh_rows` normalises cost, time and the band
/// against the best peer of THAT pool. A row that is the cheapest of a four-row suite carried a 0.87 that a row of a
/// different suite, 3× dearer per run for the same quality, could never be compared against — so `best("editor-l","l")`
/// answered with a frontend/web/ministore-shaped row because its pool was small, not because it was good value.
///
/// Now the rows are POOLED PER PAIRING (every suite, domain and stack of one planner × worker, weighted by samples —
/// the same pooling `best_in` has always done for its "*" rung), the unmeasured and the unbenched are set aside the way
/// the domain ladder sets them aside, and the ONE tolerance decides: keep at least 80 % of the best measured quality,
/// then price, then time (`rank`). It is the same function the domain-aware path ends in, so the pooled answer and the
/// domain answer can differ only because they were given different evidence, never because they were ranked differently.
/// A pairing with fewer than `min_n` samples is not offered (as before: "with at least `min_n` samples").
pub fn best(job: &str, size: &str, min_n: i64) -> Option<Value> {
    let c = open().ok()?;
    let rows: Vec<Value> = pool_rows(&c, job, size, "*", "*", false).into_iter()
        .filter(|r| r.get("n").and_then(Value::as_i64).unwrap_or(0) >= min_n).collect();
    // CONTROLLED EVIDENCE OUTRANKS OBSERVED, ALWAYS (the two classes' weights were never comparable): when anything in the
    // pool was benched, the pairings that were only ever watched do not compete; with no bench at all they do.
    let cands = contest(&rows);
    let (k, score) = rank(&cands, crate::roles::Objective::Best).into_iter().next()?;
    let (p, w) = k.split_once(" · ")?;
    let won = cands.iter().find(|r| s_of(r, "planner") == p && s_of(r, "worker") == w)?;
    // the QA seat that ran with that pairing: the one on its most-sampled row
    let qa: String = c.query_row("SELECT qa FROM merge_scores WHERE job=?1 AND size=?2 AND planner=?3 AND worker=?4 AND qa != '' AND legacy = 0 ORDER BY n DESC, id DESC LIMIT 1",
        params![job, size, p, w], |r| r.get(0)).unwrap_or_default();
    Some(json!({ "planner": p, "worker": w, "qa": qa, "weight": score, "n": won.get("n").and_then(Value::as_i64).unwrap_or(0) }))
}
/// WHAT THE TABLE HAS LEARNED FOR A JOB (2026-09-23, "the router reads the merge table"): the best-weighted row of
/// the editor job at this size with at least `min_n` samples names the planner, the worker and the QA that won —
/// the side this `job` belongs to is the answer. None until the table has enough real runs. The static jobs table
/// stays the fallback and the tie-breaker.
/// EVERY SEAT THAT HAS BEEN MEASURED ON A JOB OF ITS OWN, best first (L9, 2026-09-29). `best` answers with the top ROW;
/// a job that has its own rows — screening files them under `job = "screening"`, the seat in `planner` — needs the whole
/// ranking, because the winner may be a seat this box does not have and the next one down is then the answer.
///
/// RANKED BY THE RULE, NOT BY `weight` (2026-10-01, the same fix as `best`): the order is what `rank` gives the seats
/// pooled across every suite — the 80 % band first, then price, then time — so a seat that is 8× cheaper inside the band
/// is first, and the seat this box lacks is skipped for the next one down the SAME order.
pub fn ranked_seats(job: &str, size: &str, min_n: i64) -> Vec<String> {
    let Ok(c) = open() else { return vec![] };
    let rows: Vec<Value> = pool_rows(&c, job, size, "*", "*", false).into_iter()
        .filter(|r| r.get("n").and_then(Value::as_i64).unwrap_or(0) >= min_n).collect();
    let cands = contest(&rows);
    rank(&cands, crate::roles::Objective::Best).into_iter()
        .filter_map(|(k, _)| k.split_once(" · ").map(|(p, _)| p.to_string()))
        .filter(|k| k != "raw" && k != "-" && !k.is_empty()).collect()
}
/// THE SAME RANKING FOR ONE DOMAIN'S OWN POOL (2026-10-05). The pooled ranking above reads every domain together and asks `min_n` of EACH ROW, and a judge bench files one row per seat whose `n` is 1 however many
/// verdicts stand behind it (60 on L10) - so those rows never qualified, and the verifier fell through to the planner half of the best EDITING pair (Codex terra, which verifies backend work at 38 %). Here the
/// pool is the domain's own (its fallback chain: the domain, then `any`, then every domain) and `min_n` is asked of the POOL, the way `pick_in` asks it. A seat that cannot be called (`side_blocked`) is not listed.
pub fn ranked_seats_in(job: &str, size: &str, domain: &str, min_n: i64) -> Vec<String> {
    let Ok(c) = open() else { return vec![] };
    let chain: Vec<&str> = crate::roles::Domain::of(domain).map(|d| d.fallbacks()).unwrap_or_else(|| vec![domain, "any", "*"]);
    for d in chain {
        let rows = pool_rows(&c, job, size, d, "*", false);
        let total: i64 = rows.iter().map(|r| r.get("n").and_then(Value::as_i64).unwrap_or(0)).sum();
        if rows.is_empty() || total < min_n { continue; }
        let keys: Vec<String> = rank(&contest(&rows), crate::roles::Objective::Best).into_iter()
            .filter_map(|(k, _)| k.split_once(" · ").map(|(p, _)| p.to_string()))
            .filter(|k| k != "raw" && k != "-" && !k.is_empty() && !side_blocked(k)).collect();
        if !keys.is_empty() { return keys; }
    }
    vec![]
}
fn seat_of_key(k: &str, usable: &impl Fn(crate::cli::Engine) -> bool) -> Option<(crate::cli::Engine, String)> {
    let (e, m) = k.split_once(':')?;
    if m.trim().is_empty() { return None }
    let engine = crate::cli::engine_of(&json!(e))?;
    if usable(engine) { Some((engine, m.to_string())) } else { None }
}
/// The best-ranked seat of a job's OWN rows that `usable` accepts. Pure over the record.
fn own_seat(job: &str, size: &str, min_n: i64, usable: impl Fn(crate::cli::Engine) -> bool) -> Option<(crate::cli::Engine, String)> {
    ranked_seats(job, size, min_n).into_iter().find_map(|k| {
        let (e, m) = k.split_once(':')?;
        // A PROVIDER WITH NO MODEL IS NOT A SEAT (audit 2026-09-28: twelve rows are filed as `gemini:`): binding it would call the CLI with an empty model
        if m.trim().is_empty() { return None }
        let engine = crate::cli::engine_of(&json!(e))?;
        if usable(engine) { Some((engine, m.to_string())) } else { None }
    })
}
/// THE BRAIN JOBS READ THEIR OWN CELL FIRST (item 4, 2026-10-01 — "which table it reads", HANDOFF §6.1). The planner, the verifier and the
/// test-writer were bound to the PLANNER HALF OF THE BEST EDITING PAIRING, a proxy: the editor row says which brain steered a good
/// edit, not which seat judges a diff well — and `bench/judges` (L10) exists to measure exactly that, in a cell no router read. So the
/// job's own cell answers when it holds `min_n` samples, ranked by the one rule (`ranked_seats`: the band, then price, then time, the
/// measured above the claimed), and the editing pairing's brain stays the fallback. A small ask (size s) has no cell of its own for
/// the judges, so it reads the L cell; a wave (xxl) reads only its own — a model measured on L work is not assumed to plan a wave.
fn brain_seat(job: &str, size: &str, min_n: i64, usable: impl Fn(crate::cli::Engine) -> bool) -> Option<(crate::cli::Engine, String)> {
    let sizes: &[&str] = if size == "s" { &["s", "l"] } else { &[size] };
    sizes.iter().find_map(|sz| own_seat(job, sz, min_n, &usable))
}
pub fn best_for(job: crate::cli::Job, size: &str, min_n: i64) -> Option<(crate::cli::Engine, String)> {
    best_for_with(job, size, min_n, crate::cli::engine_usable)
}
/// `best_for` that knows the WORK'S DOMAIN. Only the verifier reads its domain's own pool (L10 measured it per domain: a seat that verifies docs well can wave broken backend work through); every other job, and a
/// verifier with no known domain or no rows for it, answers exactly as `best_for` does.
pub fn best_for_in(job: crate::cli::Job, size: &str, domain: Option<&str>, min_n: i64) -> Option<(crate::cli::Engine, String)> {
    best_for_in_with(job, size, domain, min_n, crate::cli::engine_usable)
}
fn best_for_in_with(job: crate::cli::Job, size: &str, domain: Option<&str>, min_n: i64, usable: impl Fn(crate::cli::Engine) -> bool) -> Option<(crate::cli::Engine, String)> {
    if job == crate::cli::Job::Verifier {
        if let Some(d) = domain.filter(|d| !d.is_empty() && !matches!(*d, "any" | "unknown" | "*")) {
            let sizes: &[&str] = if size == "s" { &["s", "l"] } else { &[size] };
            if let Some(hit) = sizes.iter().find_map(|sz| ranked_seats_in("verifier", sz, d, min_n).iter().find_map(|k| seat_of_key(k, &usable))) { return Some(hit) }
        }
    }
    best_for_with(job, size, min_n, usable)
}
fn best_for_with(job: crate::cli::Job, size: &str, min_n: i64, usable: impl Fn(crate::cli::Engine) -> bool) -> Option<(crate::cli::Engine, String)> {
    use crate::cli::Job::*;
    // SCREENING READ THE EDITOR'S ROW (L9): the planner half of the best editing pairing — the seat that plans, not the
    // seat measured at screening. Its own rows (bench/screen-solo) say Gemini and Haiku, at a third of Sonnet's price.
    if job == Screening { return own_seat("screening", "call", min_n, usable); }
    if matches!(job, Planner | Verifier | TestWriter) { if let Some(hit) = brain_seat(job.key(), size, min_n, &usable) { return Some(hit) } }
    let editor = if size == "s" { "editor-s" } else { "editor-l" };
    let row = best(editor, size, min_n)?;
    let key = match job { Planner | Verifier | TestWriter | Synthesis | Screening => row["planner"].as_str()?, QaGate => row["qa"].as_str().filter(|q| !q.is_empty())?, _ => row["worker"].as_str()? };
    if key == "raw" || key == "-" { return None; }
    let (e, m) = key.split_once(':')?;
    if m.trim().is_empty() { return None }
    let engine = crate::cli::engine_of(&json!(e))?;
    Some((engine, m.to_string()))
}
/// THE BEST MODEL FOR THIS WORK *WITHIN ONE ENGINE* (2026-09-26).
///
/// `job_binding_learned` returns early when a pair is set, so under a merge NOTHING size-aware is consulted:
/// `job_binding_paired` falls through to `job_defaults`, which is blind to size, and a one-line fix is planned as
/// expensively as a subsystem. The early return is DEFENSIBLE — `best_for` returns the globally best (engine, model)
/// and consulting it would silently override the provider the operator chose, which the record must never do — so the
/// gap is not "it skips the record", it is that the only size-aware function available cannot respect a pair.
///
/// This is that function: the record's winner for this cell, CONSTRAINED to one engine. It can move the model the
/// pair's side runs; it can never move the side's engine.
pub fn best_model_within(job: crate::cli::Job, size: &str, engine: crate::cli::Engine, min_n: i64) -> Option<String> {
    use crate::cli::Job::*;
    if job == Screening { return own_seat("screening", "call", min_n, |e| e == engine).map(|(_, m)| m); }
    if matches!(job, Planner | Verifier | TestWriter) { if let Some((_, m)) = brain_seat(job.key(), size, min_n, |e| e == engine) { return Some(m) } }
    let editor = if size == "s" { "editor-s" } else { "editor-l" };
    let row = best(editor, size, min_n)?;
    let key = match job {
        Planner | Verifier | TestWriter | Synthesis | Screening => row["planner"].as_str()?,
        QaGate => row["qa"].as_str().filter(|q| !q.is_empty())?,
        _ => row["worker"].as_str()?,
    };
    if key == "raw" || key == "-" { return None }
    let (e, m) = key.split_once(':')?;
    // the winner is only usable here if it is on the side's OWN engine — otherwise the pair's choice stands
    if crate::cli::engine_of(&json!(e))? != engine { return None }
    Some(m.to_string())
}

/// WHAT A ROLE'S WINNER SCORES, for the stay-put rule (PLAN-THE-CHAMELEON C1).
///
/// `change_for` has to weigh the form that is running against the form the ask asks for, and it cannot do that
/// with a pairing alone — it needs `(quality, samples)` for each. This is that number: the best ROUTABLE row of
/// the cell, with the evidence behind it.
///
/// `None` when nothing has been measured for this role, which is its own answer and not a zero: an unmeasured
/// role still deserves the work, it is only the merge under it that is unproven.
pub fn role_quality(job: &str, size: &str, domain: &str) -> Option<(f64, i64)> {
    let c = open().ok()?;
    // A CHECKED QUALITY OUTRANKS AN UNCHECKED ONE (2026-09-27), the same doctrine as "controlled outranks observed"
    // two functions above: pick the CLASS first and the weight second. Without this a row whose verdict was never
    // audited — weight from "no task errored" — could answer for a role over an audited row that scored lower.
    let sql = format!("SELECT audit_sum, audit_n, pass_first, retries, failed, gate_met, gate_unmet, reasked, n, weight                        FROM merge_scores WHERE job=?1 AND size=?2 AND domain=?3 AND {} ORDER BY {} DESC, weight DESC LIMIT 1", routing_sql(), measured_sql());
    let row = c.query_row(&sql, params![job, size, domain], |r| {
        Ok((r.get::<_, f64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?,
            r.get::<_, i64>(5)?, r.get::<_, i64>(6)?, r.get::<_, i64>(7)?, r.get::<_, i64>(8)?))
    }).ok()?;
    let (q, _) = quality_of(row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8)?;
    Some((q, row.8))
}

/// HOW MUCH OF A NUMBER IS MEASUREMENT AND HOW MUCH IS LUCK (operator 2026-09-26: "of course making clear the
/// winners reason, and error margin").
///
/// A quality of 0.80 from ten instances and a quality of 0.80 from one are the same number and not the same fact,
/// and the record is full of the second kind: 96 of its 164 rows have n = 1. Printing a weight beside them without
/// a margin invites exactly the mistake the price-first rule was written to stop — reading noise as a verdict.
///
/// WILSON, NOT THE NORMAL APPROXIMATION. The textbook `p ± 1.96·√(p(1−p)/n)` collapses to ZERO WIDTH at p = 0 or
/// p = 1 — it would claim a single perfect run is certain, which is the worst possible moment to be confident.
/// Wilson stays honest there: one success out of one gives roughly 0.21–1.00.
///
/// Returns the 95% interval for the quality. Pure — tested.
pub fn margin(quality: f64, n: i64) -> (f64, f64) {
    if n <= 0 { return (0.0, 1.0) }
    let (p, nn, z) = (quality.clamp(0.0, 1.0), n as f64, 1.96_f64);
    let denom = 1.0 + z * z / nn;
    let centre = (p + z * z / (2.0 * nn)) / denom;
    let half = (z / denom) * ((p * (1.0 - p) / nn) + (z * z / (4.0 * nn * nn))).sqrt();
    ((centre - half).max(0.0), (centre + half).min(1.0))
}

/// IS THIS ROW AS GOOD AS THE BAR — and why this stayed a FIXED tolerance (a stated number, not one inferred from `n`). It is RELATIVE to the best since 2026-09-30: keep 80 %.
///
/// Operator 2026-09-28: *"statistical or fixed? wich one produces the best impact? knowing the tolerance rule to
/// get cheaper impact"*. The answer, arrived at by trying the other one twice and measuring what it did:
/// **fixed** — not because it is elegant, but because this record cannot support the alternative.
///
/// `separated()` below is a real test and it is used for real: it decides what the TABLE SAYS about two arms
/// ("the record cannot tell these apart"). It cannot decide the BAND, because the band multiplies the cost and
/// time terms, and `n` here is not what a confidence interval needs. **`n` counts RECORDED OUTCOMES, not graded
/// items.** The readers rows carry `n = 2` while each encodes twenty questions; `merge_scores` has nowhere to
/// put the grading denominator. Any interval computed from that `n` is an interval about the wrong quantity.
///
/// What that cost, twice, when tried anyway:
///   · `separated()` above a floor of 8 samples — at the floor a row measuring **0.21** counted as
///     indistinguishable from a 0.9 bar, and with cost weighted 0.55 a cheap arm that failed four times in five
///     would have taken the routing. It also put a cliff at the floor: one more sample multiplied a weight
///     sevenfold, so measuring a row MORE could flip it into a better category.
///   · judging the row on the top of its interval — monotone and cliff-free, but on `n = 2` rows the interval is
///     nearly the whole unit line, so it swallowed the measured four-point gap that keeps Sonnet on synthesis.
///     There is a test for that gap, written before any of this, and it failed.
///
/// So the tolerance is a PRODUCT decision — how far below the frontier still counts as close to it — and it is
/// stated as one number rather than inferred from evidence the record does not actually have. The price of that
/// is a hard edge: Cursor at 11/14 against Sonnet's 9/10 is excluded by fourteen thousandths, and the honest
/// reading is that the edge is arbitrary, not that the arm is bad. `separated()` is what says so on the screen.
///
/// The way to earn a statistical band later is to store the grading denominator beside `n`, not to guess it.
/// Pure — tested.
pub fn in_band_of(q: f64, _n: i64, q_best: f64, band: f64) -> bool { q >= q_best * (1.0 - band) - 1e-9 }   // RELATIVE since 2026-09-30: `band` is the fraction of the best it may give up (0.20 = keep 80 %)

/// DOES THE RECORD ACTUALLY SEPARATE THESE TWO? When the intervals overlap, the ranking between them is an
/// ordering of noise — worth saying out loud, because a table that always names a winner teaches the reader that
/// there always is one. Pure — tested.
pub fn separated(a: (f64, i64), b: (f64, i64)) -> bool {
    let (al, ah) = margin(a.0, a.1);
    let (bl, bh) = margin(b.0, b.1);
    ah < bl || bh < al
}

/// WHY THIS ROW WON, in the words of the rule that picked it. `rows` are the cell's rows as `table()` renders them,
/// already ranked. Pure — tested.
pub fn why_won(rows: &[Value]) -> String {
    let f = |r: &Value, k: &str| r.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let i = |r: &Value, k: &str| r.get(k).and_then(Value::as_i64).unwrap_or(0);
    let Some(w) = rows.first() else { return String::new() };
    let q = |r: &Value| r.get("audit").and_then(Value::as_f64).unwrap_or(0.0);
    let (wq, wn) = (q(w), i(w, "n"));
    if rows.len() == 1 {
        return format!("the only pairing measured here — {}", counted(wq, wn));
    }
    let best_q = rows.iter().map(q).fold(0.0_f64, f64::max);
    let cheapest = rows.iter().map(|r| f(r, "usd")).filter(|v| *v > 0.0).fold(f64::INFINITY, f64::min);
    let head = if (wq - best_q).abs() < 1e-9 && f(w, "usd") <= cheapest + 1e-9 {
        "best measured quality AND the cheapest — nothing to trade".to_string()
    } else if f(w, "usd") <= cheapest + 1e-9 {
        let band = crate::roles::Objective::Best.band();
        format!("cheapest keeping at least {:.0}% of the best measured quality: {} against a best of {:.0}%",
                (1.0 - band) * 100.0, counted(wq, wn), best_q * 100.0)
    } else {
        format!("best measured quality — {}", counted(wq, wn))
    };
    // THE PART THAT MAKES THE TABLE HONEST: is the row below it actually behind, or only behind on paper?
    match rows.get(1) {
        Some(r2) if !separated((wq, wn), (q(r2), i(r2, "n"))) =>
            format!("{head} · NOT separated from the runner-up at this sample size — the order between them is noise"),
        _ => head,
    }
}

fn counted(q: f64, n: i64) -> String {
    let (lo, hi) = margin(q, n);
    format!("{:.0}% over {n} run{} (95%: {:.0}–{:.0}%)", q * 100.0, if n == 1 { "" } else { "s" }, lo * 100.0, hi * 100.0)
}

/// A ROW NOBODY PRICED IS PRICED AT ITS LIST RATE (operator 2026-09-28: "i asked for filters and heimdall chose sonnet, a
/// frontier model, for a simple frontend task, having gemini, composer, grok and many others active and benchmarked … it
/// is ignoring the benchmarks, or relying on speed, or a small gain in quality against a big gain in price").
///
/// Cause, read off the live record: the `planner` job's rows carry no spend (`usd_total = 0` — a planner call's price was
/// never written against its row), and `rank` gives every unpriced row the same flat cost score, 0.5. Price therefore
/// dropped out of the comparison entirely and the only thing left that told two planners apart was SECONDS, which the
/// operator had said does not matter — so the fastest planner, Claude Sonnet at 137 s, won a job a $0.07 model measured
/// the same on.
///
/// Now such a row is priced deterministically: the tokens one call of this job typically takes, at the OFFICIAL list rate
/// of each model in the pairing (`cli::prices_of`). A planner reads the code and writes a short plan (~30k in, ~3k out);
/// a worker reads, edits and reports (~60k in, ~5k out). `raw` is no model and costs nothing. Real recorded spend always
/// wins over this; it only fills a hole. Pure — tested.
pub fn estimated_usd(job: &str, planner: &str, worker: &str) -> f64 {
    let rate = |key: &str| -> Option<(f64, f64)> {
        let (e, m) = key.split_once(':')?;
        let eng = crate::cli::engine_of(&json!(e))?;
        Some(crate::cli::prices_of(eng, Some(m)))
    };
    let cost = |r: Option<(f64, f64)>, tin: f64, tout: f64| r.map(|(i, o)| (tin * i + tout * o) / 1e6).unwrap_or(0.0);
    let p = cost(rate(planner), 30_000.0, 3_000.0);
    let w = cost(rate(worker), 60_000.0, 5_000.0);
    if job == "planner" { p } else { p + w }
}
/// Every row, ranked within its job × size, for the screen.
/// THE THREE WINNERS OF A ROLE (operator 2026-09-23: "under the role 3 options: Smarter, Cheaper and Best — each one
/// is a winner in a circumstance"). The same rows, scored three ways: `Smarter` counts only quality and allows no gap
/// below the best; `Cheaper` counts the bill above the score; `Best` is the balanced weight. A row is compared only
/// against rows of the same job, size and DOMAIN — a backend merge is never ranked against a screen merge. Pure over
/// the rows it is given — tested.
pub fn rank(rows: &[Value], objective: crate::roles::Objective) -> Vec<(String, f64)> { rank_with(rows, objective, false) }

/// A LOOK TAKES PART IN A RANKING only when this many matches stand behind it: one comparison is an anecdote, and the gate it would open is a hard edge.
pub const LOOK_MIN_N: i64 = 4;

/// `rank`, with the looks as a SECOND MEASURED QUALITY when `looks` is true (PLAN-THE-DESIGN D6, 2026-10-01). The same single tolerance, applied on both axes: a
/// row is eligible only if it keeps at least 80 % of the best on the BEHAVIOUR (audit) AND, where a judge has compared its pages (`look_n >= LOOK_MIN_N`), at
/// least 80 % of the best look - unless the shortfall is within the noise of the matches behind it, see `look_in_band` (the bar is the best look among the rows that pass on behaviour); among the eligible the order is cost, then time. A row nobody has looked at is NOT penalised on looks (it stays eligible on its
/// behaviour) and is never PREFERRED on looks - a look can only take a place away from a measured row, it never gives one to an unmeasured row.
/// `looks = false` ignores the look columns entirely, which is what the router does until the judge agrees with the operator (`looks::looks_may_route`).
/// Rows that are not eligible rank below every row that is, whatever their scores. Pure over the rows - tested.
/// THE TOP OF A LOOK'S 95 % INTERVAL (Wilson), from the `n` judged matches it rests on. A look is a win probability, so a modest split is noise: 8 wins to 4 is 0.59 against 0.41, which is 70 % of the best and
/// would be out of an 80 % band on the point estimate alone, though twelve matches cannot tell the two pages apart.
pub fn look_upper(p: f64, n: i64) -> f64 {
    if n <= 0 { return 1.0 }
    let (n, z) = (n as f64, 1.959964_f64);
    let p = p.clamp(0.0, 1.0);
    ((p + z * z / (2.0 * n) + z * (p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt()) / (1.0 + z * z / n)).min(1.0)
}
/// Is a look inside the looks band? Yes when it keeps the floor, or when it falls short by less than the noise of the matches behind it (its interval still reaches the floor): only a look that is CLEARLY worse
/// takes a place away. Pure - tested.
pub fn look_in_band(look: f64, n: i64, floor: f64) -> bool { look >= floor || look_upper(look, n) >= floor }
pub fn rank_with(rows: &[Value], objective: crate::roles::Objective, looks: bool) -> Vec<(String, f64)> {
    let f = |r: &Value, k: &str| r.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    // THE AUDIT IS THE QUALITY (2026-09-23). It used to be averaged with the first-pass ratio, which double-counted
    // the verifier: a run whose worker was sent back once and then scored 37/37 came out at 0.5 — ranked below a run
    // that passed first time and scored 34. But a retry is exactly the pipeline working, and what it costs is time
    // and money, which the other two terms already count. When an audit exists it IS the score; the run's own signals
    // answer only when nothing measured the artefact. A failed task still sinks the row.
    let quality = |r: &Value| -> f64 {
        let failed = f(r, "failed");
        let tasks = (f(r, "pass_first") + f(r, "retries") + failed).max(1.0);
        let q = match r.get("audit").and_then(Value::as_f64) {
            Some(a) => a,
            None => {
                let mut parts = vec![];
                let t = f(r, "pass_first") + f(r, "retries") + failed;
                if t > 0.0 { parts.push(f(r, "pass_first") / t); }
                if parts.is_empty() { 0.5 } else { parts.iter().sum::<f64>() / parts.len() as f64 }
            }
        };
        q * (1.0 - (failed / tasks).min(1.0))
    };
    if rows.is_empty() { return vec![]; }
    let qs: Vec<f64> = rows.iter().map(quality).collect();
    let q_best = qs.iter().cloned().fold(0.0_f64, f64::max);
    // THE CHEAPEST ROW OF THE BAND, NOT OF THE CELL (audit F-04, 2026-10-01). `best_usd`/`best_s` were minima over ALL rows, so a
    // cheaper row that had already been voted out of the band still set the baseline: every in-band row's `best/this` ratio shrank
    // toward 0, the cost term (0.75 of `Best`) shrank with it, and the fixed quality term (0.15) came to outweigh the bill —
    // contradicting the rule above ("inside the band the order is COST"). Out-of-band rows score 0 on both terms anyway, so they
    // have no business moving the scale of the rows that are being compared.
    let band_floor = objective.floor(q_best);
    // THE SECOND AXIS: the judge's look, where one stands on enough matches. The bar is the best measured look of the contest, the tolerance the same one.
    let look_of = |r: &Value| -> Option<(f64, i64)> {
        let n = r.get("look_n").and_then(Value::as_i64).unwrap_or(0);
        if !looks || n < LOOK_MIN_N { return None }
        r.get("look").and_then(Value::as_f64).map(|l| (l, n))
    };
    // …drawn from the rows that PASS ON BEHAVIOUR: a page that is lovely and broken is not a contender, and letting its look set the bar would put every working page out of the
    // looks band behind it (the same reason F-04 stopped an out-of-band row setting the price scale).
    let l_best = rows.iter().zip(&qs).filter(|(_, q)| **q >= band_floor).filter_map(|(r, _)| look_of(r)).map(|(l, _)| l).fold(f64::NEG_INFINITY, f64::max);
    let look_floor = objective.floor(l_best);
    let eligible: Vec<bool> = rows.iter().zip(&qs).map(|(r, q)| *q >= band_floor && look_of(r).map_or(true, |(l, n)| look_in_band(l, n, look_floor))).collect();
    let in_band_rows = || rows.iter().zip(&eligible).filter(|(_, ok)| **ok).map(|(r, _)| r);
    let best_usd = in_band_rows().map(|r| f(r, "usd")).filter(|v| *v > 0.0).fold(f64::INFINITY, f64::min);
    let best_s = in_band_rows().map(|r| f(r, "s")).filter(|v| *v > 0.0).fold(f64::INFINITY, f64::min);
    let (wq, wc, wt) = objective.weights();
    let mut out: Vec<(String, f64, bool)> = rows.iter().zip(&qs).zip(&eligible).map(|((r, q), in_band)| {
        let in_band = *in_band;
        let usd = f(r, "usd"); let s = f(r, "s");
        let cost = if !in_band { 0.0 } else if usd > 0.0 && best_usd.is_finite() { best_usd / usd } else { 0.5 };
        let time = if !in_band { 0.0 } else if s > 0.0 && best_s.is_finite() { best_s / s } else { 0.5 };
        let n = f(r, "n").max(1.0);
        let raw = wq * q + wc * cost + wt * time;
        // the same shrink the table uses: one sample never outranks a repeated one on a whisker
        let score = 0.5 + (raw - 0.5) * (n / (n + 1.0));
        (format!("{} · {}", s_of(r, "planner"), s_of(r, "worker")), score, in_band)
    }).collect();
    // a row outside either band ranks below every row inside both: its quality term alone can out-score a dear row's whole bill
    out.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.total_cmp(&a.1)));
    out.into_iter().map(|(k, sc, _)| (k, sc)).collect()
}
fn s_of(r: &Value, k: &str) -> String { r.get(k).and_then(Value::as_str).unwrap_or("").to_string() }
/// The winner of a job for a role: its own domain when that pool has `min_n` samples, else the `any` pool. Returns
/// `(planner, worker, samples)`. Stack-blind — every technology pooled; `best_for_stack` is the one that asks for a
/// language, and this stays for callers that have no repo in hand.
pub fn best_for_role(job: &str, size: &str, domain: &str, objective: crate::roles::Objective, min_n: i64) -> Option<(String, String, i64)> {
    best_for_stack(job, size, domain, "", objective, min_n)
}
/// THE WINNER FOR THIS WORK IN THIS LANGUAGE (II.2 · II.7, 2026-09-25) — two ladders, walked in the honest order.
///
/// The technology is the OUTER one: a merge measured on Rust says more about the next Rust task than a merge measured
/// on anything at all, so every domain of this stack is tried before the pooled stack is. Within each, the existing
/// domain ladder runs unchanged. `stack = ""` or `any` skips straight to the pool, which is exactly what every caller
/// did before this existed — so a repo whose language nothing has measured sees precisely what it saw yesterday.
///
/// The sample count returned says how far the search had to reach: a winner found in the pool carries the pool's `n`,
/// not a promise about this language.
///
/// WATCHING IS NOT MEASURING (2026-09-28). The ladder used to stop at the first pool holding `min_n` samples, whatever
/// those samples were — so a pool made ENTIRELY of real use could end the search while controlled evidence sat one
/// rung further along, unread. On this record `editor-s/s/backend/web` held one observed row (`claude-code:sonnet ×
/// cursor:composer-2.5`, 13 samples watched in the cockpit) and `editor-s/s/backend/*` held 29 benched arms, so the
/// router answered Sonnet to every frontend ask and never looked at what had been measured. The operator asked the
/// obvious question: "why is it not using composer, grok, gemini?"
///
/// This is the plan's own rule (II.4, the one `blend` states): observation MOVES a benched score, it does not set
/// one. So the ladder is walked TWICE. The first pass will only stop at a pool that holds at least one controlled
/// row — a bench, comparable across pairings. The second accepts observation alone, because "we have only watched it
/// work" is still evidence and the alternative is answering nothing. A pool with both was always fine and is
/// untouched: `blend` already combines them there.
pub fn best_for_stack(job: &str, size: &str, domain: &str, stack: &str, objective: crate::roles::Objective, min_n: i64) -> Option<(String, String, i64)> {
    cell_for_stack(job, size, domain, stack, objective, min_n, false).map(|p| (p.planner, p.worker, p.n))
}
/// WHO WON A CELL, and whether it won as ONE seat doing the whole job. `planner == "raw"` is the record's spelling of a
/// solo arm (no planner, no second model), so a solo winner is `("raw", worker)`; `note` is the sentence that says what
/// the solo cells contributed, empty when they were not part of the contest.
#[derive(Clone, Debug, PartialEq)]
pub struct Pick { pub planner: String, pub worker: String, pub n: i64, pub solo: bool, pub note: String }

/// THE WINNER OF AN EDITING CELL WHEN ONE SEAT MAY DO THE WHOLE JOB (the single-AI half of "tasks of size L and smaller
/// do not need a planner"). Until now the single AI was only ever the HANDS of the best pair — `routing_sql` kept every
/// `raw` row out of the contest, so a solo cell that was cheaper inside the band could never be named, however well
/// it was measured. This is the same ladder as `best_for_role` with the solo cells let in at the sizes that need no
/// planner (`solo_competes`): both kinds of cell are ranked together by the ONE tolerance — keep at least 80 % of the
/// best measured quality, then price, then time — and `Pick.note` says what the solo cells did to the answer.
///
/// WHERE THE SOLO CELLS ARE NOT MEASURED ENOUGH (fewer than `SOLO_MIN_N` measured runs of one seat in the pool) nothing
/// changes: the pair's hands answer, exactly as before, and the note says that no solo cell was there to compete —
/// no quality is invented for a shape the record has not measured. Stack-blind, like `best_for_role`.
pub fn best_cell(job: &str, size: &str, domain: &str, objective: crate::roles::Objective, min_n: i64) -> Option<Pick> {
    cell_for_stack(job, size, domain, "", objective, min_n, solo_competes(job, size))
}
fn cell_for_stack(job: &str, size: &str, domain: &str, stack: &str, objective: crate::roles::Objective, min_n: i64, solo: bool) -> Option<Pick> {
    let st = stk(stack);
    let stacks: Vec<&str> = if st == "any" { vec!["*"] } else { vec![st.as_str(), "*"] };
    for benched_only in [true, false] {
        for s in &stacks {
            if let Some(hit) = pick_in(job, size, domain, s, objective, min_n, benched_only, solo) { return Some(hit) }
        }
    }
    None
}
/// THE TWO BODIES OF EVIDENCE, COMBINED THE WAY THE PLAN ASKS (PLAN-THE-LEAN-YEARS II.4: "the router combines them
/// explicitly — observational evidence moves the prior, controlled evidence sets the likelihood … never one number
/// with two meanings").
///
/// A bench is the same task on the same fixture, comparable across pairings: it SETS the score. Real use is whatever
/// happened to be asked that day, comparable only with itself — but it is the only evidence that says "this pairing
/// keeps failing its gates on actual work", which is worth a great deal and is not a benchmark result. So observation
/// MOVES the benched score toward what use says, by an amount that grows with how much use there is and is CAPPED AT A
/// THIRD — the same cap, for the same reason, as the re-ask penalty: it nudges a ranking, it does not decide one. Six
/// observed runs move it half of that third; one moves it a seventh.
///
/// With no bench at all the observed number stands alone, because "we have only watched it work" is still evidence and
/// the alternative is answering nothing. `basis` says which case produced the number, so no caller has to guess.
/// Cost and time come from the bench when there is one (they are comparable there) and from use otherwise. Pure — tested.
/// Two rows of the same pairing and class, as one: samples summed, quality, cost and time weighted by samples. Pure — tested.
pub fn pool(a: Option<(f64, i64, f64, f64)>, b: (f64, i64, f64, f64)) -> Option<(f64, i64, f64, f64)> {
    let Some((qa, na, ua, sa)) = a else { return Some(b) };
    let (qb, nb, ub, sb) = b;
    let n = (na + nb).max(1) as f64;
    let w = |x: f64, y: f64| (x * na as f64 + y * nb as f64) / n;
    Some((w(qa, qb), na + nb, w(ua, ub), w(sa, sb)))
}
pub fn blend(ctrl: Option<(f64, i64, f64, f64)>, obs: Option<(f64, i64, f64, f64)>) -> Option<(f64, i64, f64, f64, &'static str)> {
    match (ctrl, obs) {
        (Some((qc, nc, uc, sc)), Some((qo, no, _, _))) => {
            let pull = (no as f64 / (no as f64 + 6.0)).min(0.33);
            Some(((qc + (qo - qc) * pull).clamp(0.0, 1.0), nc + no, uc, sc, "bench+use"))
        }
        (Some((qc, nc, uc, sc)), None) => Some((qc, nc, uc, sc, "bench")),
        (None, Some((qo, no, uo, so))) => Some((qo, no, uo, so, "use")),
        (None, None) => None,
    }
}

/// A SIDE THAT CANNOT BE CALLED IS NOT A CANDIDATE (audit 2026-10-01, the Claude-off run). The ladder named winners from
/// the record alone, so with Claude switched off in Settings the best pairing of a domain could still be one with a
/// Claude side — and the run then crossed to a fallback seat the record had never measured in that role, while the
/// seat the record would have chosen next sat unread. `cli::model_blocked` is the one gate every router path asks
/// (a seat switched off, Zen off, a Chinese model off, a Cursor/Gemini model that is not the seat's own); a key
/// that is not `engine:model` (`raw`, `-`) is not a seat and is never blocked. Quota is not asked here: that is a
/// minutes-long state (`engine_routable`), and the record's order must not flap with it.
fn side_blocked(key: &str) -> bool {
    let Some((e, m)) = key.split_once(':') else { return false };
    let Some(engine) = crate::cli::engine_of(&json!(e)) else { return false };
    crate::cli::model_blocked(engine, Some(m).filter(|m| !m.trim().is_empty())).is_some()
}

/// ONE OBSERVATION OF A PAIRING, whichever way the record was read (SQL rows for the router, `table()` rows for the
/// screen), so both pool and rank through the same code. `q` is the quality `quality_of` answered, `measured` whether
/// it was AUDITED or GATED (a bench's check, the project's own test) rather than CLAIMED ("no task errored").
pub struct Seen { pub planner: String, pub worker: String, pub class: Option<Evidence>, pub q: f64, pub n: i64, pub usd: f64, pub sec: f64, pub measured: bool,
                  /// HOW MANY OF THE `n` WERE MEASURED (audited, or gated): a pairing watched in use carries samples that were never checked, and `n` counts them all
                  pub proof: i64,
                  /// HOW IT LOOKS, when a judge compared its pages: `(looks 0..1, matches behind it)`. `None` = nobody has looked - which is not a bad look
                  pub look: Option<(f64, i64)> }

/// TWO LOOK READINGS OF ONE PAIRING AS ONE: the mean weighted by matches, and the LARGER match count - not the sum. `import_looks` writes the same ranking to every
/// row of a seat (one per size, stack and suite it was benched in), so adding the counts would count each judged match once per row. Pure - tested.
pub fn pool_look(a: Option<(f64, i64)>, b: Option<(f64, i64)>) -> Option<(f64, i64)> {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some((la, na)), Some((lb, nb))) => { let w = (na + nb).max(1) as f64; Some(((la * na as f64 + lb * nb as f64) / w, na.max(nb))) }
    }
}

/// EVERY OBSERVATION OF ONE PAIRING AS ONE CANDIDATE: the two evidence classes blended (`blend`), the suites pooled
/// (`pool`), a hole in the price filled from the list rate (`estimated_usd`). A seat that cannot be called is dropped
/// when `skip_blocked`; a SOLO arm (`planner = raw`) needs `SOLO_MIN_N` MEASURED samples (audited or gated, not just counted) to be a candidate at all. A brain job's `worker` column is not gated: only the editing jobs call both sides.
/// Pure over its input apart from the seat gate — tested.
pub fn pool_seen(job: &str, seen: Vec<Seen>, skip_blocked: bool) -> Vec<Value> {
    let mut by: std::collections::BTreeMap<(String, String), (Option<(f64, i64, f64, f64)>, Option<(f64, i64, f64, f64)>, bool, i64, Option<(f64, i64)>)> = Default::default();
    for x in seen {
        let look = x.look;
        let e = by.entry((x.planner, x.worker)).or_insert((None, None, false, 0, None));
        let row = (x.q, x.n, x.usd, x.sec);
        // a quarantined source (`None`) is not evidence of anything and is dropped here as it is everywhere else
        // POOLED, NOT OVERWRITTEN (audit 2026-09-28): one pairing can hold several rows of one class in a pool — one per suite
        match x.class {
            Some(Evidence::Controlled) => { e.0 = pool(e.0, row); e.2 |= x.measured; e.3 += x.proof; e.4 = pool_look(e.4, look) }
            Some(Evidence::Observed) => { e.1 = pool(e.1, row); e.2 |= x.measured; e.3 += x.proof; e.4 = pool_look(e.4, look) }
            None => {}
        }
    }
    // WHICH SIDES OF A ROW THE JOB USES (review 2026-10-01). An EDITING pair runs as a unit - the planner steers, the worker types - so
    // either side being switched off ends the pairing. A BRAIN job (planner, verifier, test-writer, screening, the gate) calls only the
    // `planner` column: its `worker` is the editing hands the row was benched with (every test-writer row has `cursor:composer-2.5`), which
    // that job never uses - with Cursor off, the Sonnet planner benched with Composer hands was skipped as a planner for a seat it never calls.
    let editing = job.starts_with("editor-");
    by.into_iter().filter_map(|((p, w), (c0, o0, measured, proof, look))| {
        if skip_blocked && (side_blocked(&p) || (editing && side_blocked(&w))) { return None }
        let (q, n, usd, sec, basis) = blend(c0, o0)?;
        // A SOLO CELL IS BELIEVED ON `SOLO_MIN_N` MEASURED INSTANCES, not on one, and not on `n` samples of which one was audited (review
        // 2026-10-01: a watched solo run is filed as `raw` too, and an audit lands on only some of them, so `n` = 10 with `audit_n` = 1
        // passed and one audit set its quality). A pair is never held to that here: its pool total is.
        if p == "raw" && proof < SOLO_MIN_N { return None }
        // `rank` reads `audit` as the quality when it is present, which is exactly what a blended score is
        // a hole in the record is filled from the list price, so price can take part in the ranking at all
        // …and a PLANNER row is never cheaper than its own list price: rows filed before replays stopped being recorded as
        // planner evidence carry a cache hit's $0.00–0.03 and 8 s (Sonnet, 15 samples), which no fresh planning call can match
        let est = estimated_usd(job, &p, &w);
        let usd = if job == "planner" { usd.max(est) } else if usd > 0.0 { usd } else { est };
        Some(json!({ "planner": p, "worker": w, "n": n, "pass_first": 0, "retries": 0, "failed": 0,
                     "audit": q, "usd": usd, "s": sec, "basis": basis, "measured": measured,
                     "look": look.map(|(l, _)| l), "look_n": look.map(|(_, n)| n).unwrap_or(0) }))
    }).collect()
}

/// the candidates of ONE pool of the record (a domain × a technology of one job × size), pooled per pairing
fn pool_rows(c: &Connection, job: &str, size: &str, domain: &str, stack: &str, solo: bool) -> Vec<Value> {
    // "*" is every domain (or technology) pooled — the last resort, so a role always gets an answer and the sample count
    // says how far it had to reach for it
    let cols = "planner, worker, n, pass_first, retries, failed, audit_sum, audit_n, ms_total, usd_total, source, gate_met, gate_unmet, reasked, suite, look_sum, look_n";
    let where_dom = if domain == "*" { "?3 = '*'" } else { "domain=?3" };
    let where_stack = if stack == "*" { "?4 = '*'" } else { "stack=?4" };
    // THE ARCHIVE IS NOT A CANDIDATE (audit 2026-09-28). This is what the ROLE ROUTER gets its brain and hands from, and it
    // had neither `legacy = 0` nor `routing_sql()`. The withdrawn Zen rows are the largest block in the table (532
    // samples), so a metered arm the product no longer ships could still be handed a job. The control suites go with
    // it, for the reason `routing_sql` already states.
    let sql = format!("SELECT {cols} FROM merge_scores WHERE job=?1 AND size=?2 AND {where_dom} AND {where_stack} AND {}",
                      if solo { routing_solo_sql() } else { routing_sql() });
    let Ok(mut st) = c.prepare(&sql) else { return vec![] };
    let seen: Vec<Seen> = match st.query_map(params![job, size, domain, stack], |r| {
        let (n, an): (i64, i64) = (r.get(2)?, r.get(7)?);
        let graded = quality_of(r.get(6)?, an, r.get(3)?, r.get(4)?, r.get(5)?, r.get(11)?, r.get(12)?, r.get(13)?, n);
        let measured = matches!(graded, Some((_, QualityKind::Audited)) | Some((_, QualityKind::Gated)));
        Ok((Seen { planner: r.get(0)?, worker: r.get(1)?, class: evidence_of(&r.get::<_, String>(10)?), q: graded.map(|(q, _)| q).unwrap_or(0.0), n,
                   usd: r.get::<_, f64>(9)? / n.max(1) as f64, sec: r.get::<_, i64>(8)? as f64 / 1000.0 / n.max(1) as f64, measured,
                   proof: an.max(r.get::<_, i64>(11)? + r.get::<_, i64>(12)?),
                   look: { let (ls, ln): (f64, i64) = (r.get(15)?, r.get(16)?); if ln > 0 { Some((ls / ln as f64, ln)) } else { None } } },
            r.get::<_, String>(14)?))
    }) {
        // A CONTROL SUITE NEVER MOVES THE TABLE (2026-09-26). SWE-bench is the worked example: it measures three of
        // antiloki's eight claims and penalises a fourth, so its verdicts are for a person to read, not for the
        // router to obey. `suite_routes` names which are which, and the rows stay on disk either way.
        Ok(it) => it.filter_map(Result::ok).filter(|(_, su)| suite_routes(su)).map(|(x, _)| x).collect(),
        Err(_) => return vec![],
    };
    pool_seen(job, seen, true)
}

/// WHO IS ALLOWED TO WIN A POOL. Two rules the record has always stated and the ranking did not enforce:
///   · CONTROLLED EVIDENCE OUTRANKS OBSERVED (II.4): when any pairing of the pool was benched, the pairings that were only
///     ever watched do not compete — observation moves a benched score (`blend` already did that), it does not set one;
///   · MEASURED QUALITY OUTRANKS CLAIMED: when any pairing has an audited or gated quality, the pairings whose quality is
///     only "no task errored" do not compete, and cannot set the bar the band is drawn from (`reweigh_rows` stated this
///     for the table's weight on 2026-09-27; `rank` never did).
/// With nothing benched, or nothing measured, everyone competes — "we have only watched it work" is still evidence and the
/// alternative is answering nothing. Pure — tested.
fn contest(rows: &[Value]) -> Vec<Value> {
    let basis = |r: &Value| r.get("basis").and_then(Value::as_str).unwrap_or("").to_string();
    let measured = |r: &Value| r.get("measured").and_then(Value::as_bool).unwrap_or(false);
    let benched = rows.iter().any(|r| basis(r) != "use");
    let mut v: Vec<Value> = rows.iter().filter(|r| !benched || basis(r) != "use").cloned().collect();
    if v.iter().any(&measured) { v.retain(|r| measured(r)); }
    v
}

/// one pass of the domain ladder inside one technology (`"*"` = every technology pooled)
#[allow(clippy::too_many_arguments)]
fn pick_in(job: &str, size: &str, domain: &str, stack: &str, objective: crate::roles::Objective, min_n: i64, benched_only: bool, solo: bool) -> Option<Pick> {
    pick_in_with(job, size, domain, stack, objective, min_n, benched_only, solo, crate::looks::looks_standing())
}
/// THE LOOKS TAKE PART ONLY WHEN THE JUDGE HAS EARNED IT (PLAN-THE-DESIGN D6 + D3): `standing` is `Routes` only after the operator labelled enough real pairs and the
/// judge agreed with them at >= 80 %. Any other standing leaves the ranking exactly as it was and puts the looks in the route sentence as ADVISORY text.
#[allow(clippy::too_many_arguments)]
fn pick_in_with(job: &str, size: &str, domain: &str, stack: &str, objective: crate::roles::Objective, min_n: i64, benched_only: bool, solo: bool, standing: crate::looks::Standing) -> Option<Pick> {
    let c = open().ok()?;
    let chain: Vec<&str> = if domain == "*" { vec!["*"] } else { crate::roles::Domain::of(domain).map(|d| d.fallbacks()).unwrap_or_else(|| vec![domain, "any", "*"]) };
    for d in chain {
        let rows = pool_rows(&c, job, size, d, stack, solo);
        let total: i64 = rows.iter().map(|r| r.get("n").and_then(Value::as_i64).unwrap_or(0)).sum();
        if rows.is_empty() || total < min_n { continue; }
        // on the first pass a pool must hold something BENCHED, not merely enough samples — see `best_for_stack`
        if benched_only && !rows.iter().any(|r| r.get("basis").and_then(Value::as_str) != Some("use")) { continue; }
        let cands = contest(&rows);
        // the looks matter to the work they describe: an editing job in a screen domain (the pool's own domain, not the ladder's first rung)
        let on_looks = looks_apply(job, d) && standing == crate::looks::Standing::Routes;
        let ranked = rank_with(&cands, objective, on_looks);
        // THE NEXT BEST MERGE WHEN A SEAT IS AT ITS LIMIT (operator 2026-10-09: "when a subscription gets to its limits you flag it and use the next best merge besides them"): a cell whose planner or worker seat
        // cannot take a call right now (out of quota, a published or declared window at 95 %, switched off) is passed over; the route sentence says whose limit moved the pick.
        let ok = |key: &str| seats_of_key_ok(key, &|e| crate::cli::engine_of(&json!(e)).map(|en| !cfg!(test) && !crate::cli::engine_routable(en)).map(|bad| !bad).unwrap_or(true));
        let first = ranked.first().map(|(k, _)| k.clone());
        if let Some((k, _)) = ranked.iter().find(|(k, _)| ok(k)) {
            let (p, w) = k.split_once(" · ").unwrap_or((k.as_str(), "-"));
            let mut note = if solo { solo_note(&cands, p, w, objective) } else { String::new() };
            if let Some(f) = first.filter(|f| f != k) {
                let why = limited_seat_of(&f, &|e| crate::cli::engine_of(&json!(e)).and_then(|en| crate::cli::seat_limit(en)).map(|(_, said)| said));
                if !note.is_empty() { note.push_str("; "); }
                note.push_str(&format!("the best merge ({}) is passed over{} - this is the next best", f.replace(" · ", " × "), why.map(|w| format!(": {w}")).unwrap_or_default()));
            }
            if looks_apply(job, d) {
                let l = look_note(&cands, p, w, objective, standing);
                if !l.is_empty() { if !note.is_empty() { note.push_str("; "); } note.push_str(&l); }
            }
            return Some(Pick { planner: p.to_string(), worker: w.to_string(), n: total, solo: p == "raw", note });
        }
    }
    None
}

/// Can every real seat of a merge key ("planner · worker", each `engine:model` or `raw` / `-`) take a call? `seat_ok` answers for one engine key. Pure over the answer - tested.
pub fn seats_of_key_ok(key: &str, seat_ok: &dyn Fn(&str) -> bool) -> bool {
    let (p, w) = key.split_once(" · ").unwrap_or((key, "-"));
    [p, w].iter().all(|side| matches!(*side, "raw" | "-" | "") || seat_ok(side.split(':').next().unwrap_or("")))
}
/// The reason the first seat of a merge key that is out gives, when one is (its own words, or "at its limit"). Pure over the lookup.
pub fn limited_seat_of(key: &str, said: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let (p, w) = key.split_once(" · ").unwrap_or((key, "-"));
    [p, w].iter().filter(|s| !matches!(**s, "raw" | "-" | "")).find_map(|side| { let e = side.split(':').next().unwrap_or(""); said(e).map(|t| format!("{e} - {t}")) })
}

/// Do looks describe this work? An EDITING job in the two screen domains - the pages a judge compared are frontend and design work, and a brain job's row never carries a look.
pub fn looks_apply(job: &str, domain: &str) -> bool { job.starts_with("editor-") && matches!(domain, "design" | "frontend") }

/// WHAT THE LOOKS DID TO AN ANSWER, in words (PLAN-THE-DESIGN D6): the winner against the best-looking other cell the judge compared, with the behaviour and the bill
/// beside it, and - while the judge has not earned the router's trust - the plain statement that this is ADVISORY. Operator wording: "Claude x Composer: looks 0.71
/// vs 0.48 (advisory - not yet calibrated to your taste), same behaviour, $0.04 dearer". Empty when nobody has looked at any candidate. Pure over the candidates - tested.
pub fn look_note(cands: &[Value], planner: &str, worker: &str, objective: crate::roles::Objective, standing: crate::looks::Standing) -> String {
    use crate::looks::Standing;
    let f = |r: &Value, k: &str| r.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let measured = |r: &Value| r.get("look_n").and_then(Value::as_i64).unwrap_or(0) >= LOOK_MIN_N && r.get("look").and_then(Value::as_f64).is_some();
    let look = |r: &Value| f(r, "look");
    let label = |r: &Value| if s_of(r, "planner") == "raw" { s_of(r, "worker") } else { format!("{} × {}", s_of(r, "planner"), s_of(r, "worker")) };
    let seen: Vec<&Value> = cands.iter().filter(|r| measured(r)).collect();
    if seen.is_empty() { return String::new() }
    let tail = match standing {
        Standing::Routes => "",
        Standing::Advisory => " (advisory - not yet calibrated to your taste)",
        Standing::NotTrusted => " (advisory - the judge and you disagree too often to trust its looks)",
    };
    let won = cands.iter().find(|r| s_of(r, "planner") == planner && s_of(r, "worker") == worker);
    let Some(won) = won else { return String::new() };
    let name = label(won);
    if !measured(won) {
        return format!("{name}: no judge has compared its pages, while {} cell{} {} measured on looks{tail} - so looks neither help nor hurt it",
                       seen.len(), if seen.len() == 1 { "" } else { "s" }, if seen.len() == 1 { "is" } else { "are" });
    }
    // the best-looking OTHER cell (the cheaper of two equal looks)
    let other = seen.iter().filter(|r| !(s_of(r, "planner") == planner && s_of(r, "worker") == worker))
        .max_by(|a, b| look(a).total_cmp(&look(b)).then(f(b, "usd").total_cmp(&f(a, "usd")))).copied();
    let Some(other) = other else { return format!("{name}: looks {:.2}, the only cell a judge compared{tail}", look(won)) };
    let q = |r: &Value| f(r, "audit");
    let behaviour = if (q(won) - q(other)).abs() < 0.005 { "same behaviour".to_string() } else { format!("behaviour {:.0}% vs {:.0}%", q(won) * 100.0, q(other) * 100.0) };
    let d = f(won, "usd") - f(other, "usd");
    let price = if d.abs() < 0.005 { "same price".to_string() } else if d > 0.0 { format!("${:.2} dearer", d) } else { format!("${:.2} cheaper", -d) };
    let keep = ((1.0 - objective.band()) * 100.0).round();
    let band = if standing == Standing::Routes {
        let best = seen.iter().map(|r| look(r)).fold(0.0_f64, f64::max);
        if look_in_band(look(won), f(won, "look_n") as i64, objective.floor(best)) { format!(" - kept inside the {keep:.0}% looks band") } else { String::new() }
    } else { String::new() };
    format!("{name}: looks {:.2} vs {:.2} for {}{tail}, {behaviour}, {price}{band}", look(won), look(other), label(other))
}

/// WHAT THE SOLO CELLS DID TO AN ANSWER, in the words of the rule — the sentence a run prints beside its route, so a solo
/// seat that won says why it needed no planner, one that lost says what beat it, and a cell nobody measured alone
/// says exactly that instead of dressing a default as a verdict. `cands` are the contest's candidates (solo ones carry
/// `planner = raw`); `planner`/`worker` the winner. Pure — tested.
pub fn solo_note(cands: &[Value], planner: &str, worker: &str, objective: crate::roles::Objective) -> String {
    let f = |r: &Value, k: &str| r.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let q = |r: &Value| f(r, "audit");
    let keep = ((1.0 - objective.band()) * 100.0).round();
    let solos: Vec<&Value> = cands.iter().filter(|r| s_of(r, "planner") == "raw").collect();
    if solos.is_empty() {
        return format!("no solo cell is measured here (it takes {SOLO_MIN_N}+ audited runs of one seat alone), so a pair's hands run the job alone, as before");
    }
    let best_q = cands.iter().map(q).fold(0.0_f64, f64::max);
    let floor = objective.floor(best_q);
    let won = cands.iter().find(|r| s_of(r, "planner") == planner && s_of(r, "worker") == worker);
    if planner == "raw" {
        let cheapest_pair = cands.iter().filter(|r| s_of(r, "planner") != "raw" && q(r) >= floor)
            .min_by(|a, b| f(a, "usd").total_cmp(&f(b, "usd")));
        let (wq, wu) = won.map(|r| (q(r), f(r, "usd"))).unwrap_or((0.0, 0.0));
        return match cheapest_pair {
            Some(p) => format!("solo wins: {worker} alone keeps {:.0}% of the best measured {:.0}% at ${wu:.3} a run, while the cheapest pair inside the {keep:.0}% band ({} × {}) costs ${:.3} — so no planner is paid for",
                               wq * 100.0, best_q * 100.0, s_of(p, "planner"), s_of(p, "worker"), f(p, "usd")),
            None => format!("solo is the only shape measured inside the {keep:.0}% band: {worker} alone at {:.0}% of a best of {:.0}%, ${wu:.3} a run", wq * 100.0, best_q * 100.0),
        };
    }
    // a pair won: say why the best solo cell did not
    let s = solos.iter().max_by(|a, b| q(a).total_cmp(&q(b))).copied();
    let Some(s) = s else { return String::new() };
    let why = if q(s) < floor {
        format!("falls outside the {keep:.0}% band ({:.0}% against a best of {:.0}%)", q(s) * 100.0, best_q * 100.0)
    } else {
        format!("keeps the band but is not cheaper (${:.3} a run against the pair's ${:.3})", f(s, "usd"), won.map(|r| f(r, "usd")).unwrap_or(0.0))
    };
    format!("a solo cell was measured ({} alone, {:.0}%, ${:.3} a run) but it {why}, so the pair's hands run the job", s_of(s, "worker"), q(s) * 100.0, f(s, "usd"))
}
/// the jobs whose instrument does not exist yet - shown as "unmeasured" whatever their rows say
pub const READING_JOBS: [&str; 2] = ["lookup", "synthesis"];
pub fn table() -> Result<Vec<Value>, String> {
    let c = open().map_err(|e| e.to_string())?;
    // NOT VISIBLE BY THE USER (operator 2026-09-27) — the withdrawn Zen arms are readable through
    // `legacy_table()` for an audit and nowhere else.
    let mut st = c.prepare("SELECT job, size, planner, worker, qa, n, pass_first, retries, failed, gate_met, gate_unmet, audit_sum, audit_n, ms_total, usd_total, weight, source, updated, domain, stack, suite, look_sum, look_n FROM merge_scores WHERE legacy = 0 ORDER BY job, size, weight DESC, n DESC").map_err(|e| e.to_string())?;
    let rows = st.query_map([], |r| {
        let n: i64 = r.get(5)?; let nn = n.max(1) as f64; let audit_n: i64 = r.get(12)?; let job_of_row: String = r.get(0)?;
        let look_n: i64 = r.get(22)?;
        let look = if look_n > 0 { Some((r.get::<_, f64>(21)? / look_n as f64 * 1000.0).round() / 1000.0) } else { None };
        let usd = { let u = r.get::<_, f64>(14)? / nn; if u > 0.0 { Some((u * 1000.0).round() / 1000.0) } else { None } };
        Ok(json!({ "job": r.get::<_, String>(0)?, "size": r.get::<_, String>(1)?, "planner": r.get::<_, String>(2)?, "worker": r.get::<_, String>(3)?, "qa": r.get::<_, String>(4)?,
            "n": n, "pass_first": r.get::<_, i64>(6)?, "retries": r.get::<_, i64>(7)?, "failed": r.get::<_, i64>(8)?, "gate_met": r.get::<_, i64>(9)?, "gate_unmet": r.get::<_, i64>(10)?,
            "audit": if audit_n > 0 { Some(r.get::<_, f64>(11)? / audit_n as f64) } else { None }, "audit_n": audit_n, "s": (r.get::<_, i64>(13)? as f64 / nn / 1000.0).round(),
            // A PRICE OF ZERO IS A PRICE NOBODY TOOK (audit 2026-09-28). Until `estimate::price_run` landed today,
            // the flat seats' silent `0` was recorded as a measurement, so 53 of 224 live rows carry `usd_total = 0`
            // for work that really was paid for — the tokens were simply never priced, and they are not stored, so
            // the figure cannot be recovered. Reporting it as `$0.00` is the exact claim the operator called out.
            // `null` is the module's existing answer for "not measured" (`savings_of`, `priced_from_tokens`), and
            // the screen prints a dash. New rows carry a real number and are unaffected.
            "usd": usd,
            // HOW IT LOOKS (PLAN-THE-DESIGN D6): the judge's score for this cell, `null` when no page of it was ever compared (a dash on the screen - not a bad look)
            "look": look, "look_n": look_n,
            "weight": r.get::<_, f64>(15)?, "source": r.get::<_, String>(16)?, "updated": r.get::<_, String>(17)?, "domain": r.get::<_, String>(18)?, "stack": r.get::<_, String>(19)?, "suite": r.get::<_, String>(20)?,
            // HOW WE KNOW, not just how good (2026-09-27). `quality_of` already separates a quality that was AUDITED
            // from one merely CLAIMED — "no task errored" — but the table only showed `audit: null` beside a real
            // `weight`, so a reader could not tell that a weight rested on a claim. `verifier` and `planner` carry
            // ~100 samples of exactly that in four domains, and the plan's own complaint is that the gap is invisible
            // from the UI. This is the distinction the code already draws, surfaced.
            "quality_kind": quality_of(r.get::<_, f64>(11)?, audit_n, r.get::<_, i64>(6)?, r.get::<_, i64>(7)?, r.get::<_, i64>(8)?,
                                       r.get::<_, i64>(9)?, r.get::<_, i64>(10)?, 0, n)
                .map(|(_, k)| match k { QualityKind::Audited => "audited", QualityKind::Gated => "gated", QualityKind::Claimed => "claimed" })
                // THE READING JOBS HAVE NO INSTRUMENT (operator 2026-09-28: mark them unmeasured). `lookup` and `synthesis` are questions about a
                // repo, not work on one; two samples each and a ~49-point interval measure nothing. Their rows stay on disk and say so.
                .map(|k| if READING_JOBS.contains(&job_of_row.as_str()) { "unmeasured" } else { k }) }))
    }).map_err(|e| e.to_string())?.filter_map(Result::ok).collect();
    let mut rows: Vec<Value> = rows;
    mark_origin(&c, &mut rows);
    Ok(rows)
}
/// EVERY USE OF A MODEL IS A SAMPLE (operator 2026-09-23: "all that auditory feeding the benchmark, adding weights
/// each time a model is used"). A lone call — a chat turn, a verifier, a reader — is filed under the job it did at
/// size `call`, the model alone (planner = the model for the deciding jobs, worker = the model for the producing
/// ones), ok/failed as its only quality signal until an audit says more. Never mixed with the run rows.
/// HOW GOOD THE WORK WAS, AND HOW WE KNOW — the quality term, which is 70% of a row's weight.
///
/// Operator 2026-09-25: *"About correctness, quality and confidence we should have at least some deterministic
/// way to measure that dont you think? Because if not our weights on the benchmark only measure speed and
/// cost."* Quality was already in the weight, but its sources were ranked wrongly and one of them was invented.
///
/// Three signals, and they are not equals:
///
/// * **Audited** — a behavioural suite over the artefact (the bench's 37 checks). Deterministic, and the
///   strongest evidence there is.
/// * **Gated** — the project's own test · build · lint. Also deterministic, and the ONLY quality signal real
///   use produces, which is why it is first-class here. It used to count only when no audit existed, so a run
///   that scored 37/37 and then failed its build still read as perfect.
/// * **Claimed** — the verifier accepted the task. An opinion, and it answers only when neither deterministic
///   signal does.
///
/// `None` means no quality evidence of any kind. That case used to be `0.5` — a made-up number sitting in the
/// same column as measured ones, which is the fault this codebase spent a day removing. A failed task sinks the
/// row whatever the evidence, because a failure is a fact about the artefact. Pure — tested.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QualityKind { Audited, Gated, Claimed }

pub fn quality_of(audit_sum: f64, audit_n: i64, pass_first: i64, retries: i64, failed: i64, gate_met: i64, gate_unmet: i64, reasked: i64, n: i64) -> Option<(f64, QualityKind)> {
    let mut parts: Vec<f64> = vec![];
    let mut kind: Option<QualityKind> = None;
    if audit_n > 0 { parts.push(audit_sum / audit_n as f64); kind = Some(QualityKind::Audited); }
    let gates = (gate_met + gate_unmet) as f64;
    if gates > 0.0 {
        parts.push(gate_met as f64 / gates);
        // the label names the STRONGEST evidence present; both still count toward the number
        if kind.is_none() { kind = Some(QualityKind::Gated); }
    }
    let tasks = (pass_first + retries + failed) as f64;
    if parts.is_empty() {
        if tasks <= 0.0 { return None }
        parts.push(pass_first as f64 / tasks);
        kind = Some(QualityKind::Claimed);
    }
    let q = parts.iter().sum::<f64>() / parts.len() as f64;
    let penalty = if failed > 0 { 1.0 - (failed as f64 / tasks.max(1.0)).min(1.0) } else { 1.0 };
    // THE RE-ASK IS A SOFT SIGNAL AND STAYS ONE. A RATIO, never a count: a pairing that answered ten asks with
    // one repeat is not the pairing that answered two with two. Capped at a third, because an operator also
    // re-asks to refine and this must not be able to sink a row on its own — it nudges a ranking, it does not
    // decide one.
    let reask_penalty = if reasked > 0 && n > 0 { 1.0 - (reasked as f64 / n as f64).min(1.0) * 0.33 } else { 1.0 };
    Some(((q * penalty * reask_penalty).clamp(0.0, 1.0), kind?))
}

/// EVERY PAIR THE RECORD HAS ACTUALLY MEASURED, as a merge the wizard can offer.
///
/// Operator 2026-09-25: *"we were combining frontier models to chinese models … but i feel that this is a
/// limitation that we shouldnt have, you should be able to merge any model to any model."*
///
/// The limitation was never here. `merge_scores` is keyed on (job, size, domain, planner, worker, qa) with no
/// constraint on either side, and already holds codex×deepseek, deepseek×codex, qwen×codex, kimi×qwen and
/// qwen×qwen. What was hardcoded was a list of six in the agent wizard. This replaces that list with a view
/// over the evidence: a pair appears here the moment it has rows, and disappears from nothing.
///
/// `raw` rows are excluded — a raw arm is the thing a merge is measured AGAINST, not a merge you can hire.
/// So are solo rows (an empty worker), for the same reason. Pure over the table — tested.
pub fn pairs_from(rows: &[Value]) -> Vec<Value> {
    use std::collections::BTreeMap;
    struct Acc { n: i64, rows: i64, q: f64, qn: f64, usd: f64, s: f64, jobs: std::collections::BTreeSet<String>, domains: std::collections::BTreeSet<String> }
    let mut by: BTreeMap<(String, String), Acc> = BTreeMap::new();
    for r in rows {
        let planner = s_of(r, "planner");
        let worker = s_of(r, "worker");
        // A SOLO ROW STORES "-" AS ITS WORKER, not an empty string (52 of the 158 rows here), and a raw arm
        // stores "raw" as its planner. Neither is a merge anyone can hire — one is a single model doing the
        // whole job, the other is the yardstick a merge is measured against — and letting them through
        // produced options labelled "sonnet × -" in the picker.
        let solo = |x: &str| x.is_empty() || x == "-";
        if solo(&planner) || solo(&worker) || planner == "raw" || worker == "raw" { continue }
        // AND NEITHER IS A CONTROL SUITE'S ROW (2026-09-26). This list is what the picker OFFERS and the quality it
        // shows beside each option; a row from a bench the router will not obey has no business raising or lowering
        // that number. Same rule as `best`, `best_in` and the sampler — a pair measured by nothing else simply is
        // not offered, because nothing that counts has measured it.
        if !suite_routes(&s_of(r, "suite")) { continue }
        let n = r.get("n").and_then(Value::as_i64).unwrap_or(0);
        if n <= 0 { continue }
        let e = by.entry((planner, worker)).or_insert(Acc { n: 0, rows: 0, q: 0.0, qn: 0.0, usd: 0.0, s: 0.0, jobs: Default::default(), domains: Default::default() });
        e.n += n; e.rows += 1;
        // the per-row figures from `table()` are already per-run averages, so they weight by n to re-average
        if let Some(a) = r.get("audit").and_then(Value::as_f64) { e.q += a * n as f64; e.qn += n as f64; }
        e.usd += r.get("usd").and_then(Value::as_f64).unwrap_or(0.0) * n as f64;
        e.s += r.get("s").and_then(Value::as_f64).unwrap_or(0.0) * n as f64;
        let j = s_of(r, "job"); if !j.is_empty() { e.jobs.insert(j); }
        let d = s_of(r, "domain"); if !d.is_empty() && d != "any" { e.domains.insert(d); }
    }
    let mut out: Vec<Value> = by.into_iter().map(|((planner, worker), a)| {
        let nn = a.n.max(1) as f64;
        json!({
            "planner": planner, "worker": worker,
            "runs": a.n, "rows": a.rows,
            // quality is only reported when something actually audited an artefact; a pair with no audited run
            // says null rather than a number that means "we guessed"
            "quality": if a.qn > 0.0 { Some((a.q / a.qn * 1000.0).round() / 1000.0) } else { None },
            "usd": ((a.usd / nn) * 1000.0).round() / 1000.0,
            "s": (a.s / nn).round(),
            "jobs": a.jobs.into_iter().collect::<Vec<_>>(),
            "domains": a.domains.into_iter().collect::<Vec<_>>(),
        })
    }).collect();
    // most-measured first: the wizard should lead with the pairs whose numbers mean something
    out.sort_by(|x, y| y.get("runs").and_then(Value::as_i64).cmp(&x.get("runs").and_then(Value::as_i64))
        .then_with(|| s_of(x, "planner").cmp(&s_of(y, "planner"))));
    out
}

/// The same, read from the record.
pub fn pairs() -> Result<Vec<Value>, String> { Ok(pairs_from(&table()?)) }

/// RE-SCORE EVERY CELL WITH THE CURRENT RULES.
///
/// Splitting the evidence classes stops NEW corruption, but a weight already on disk keeps whatever it was
/// given under the old rules until something writes to its cell again — so a row could carry a contaminated
/// rank for weeks. This walks every (job, size) once and recomputes, which is cheap (the whole table is
/// hundreds of rows) and idempotent. Run at startup; safe to run at any time.
///
/// Returns how many cells were re-scored.
pub fn rescore_all() -> Result<usize, String> {
    let c = open().map_err(|e| e.to_string())?;
    let cells: Vec<(String, String)> = c.prepare("SELECT DISTINCT job, size FROM merge_scores").map_err(|e| e.to_string())?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(|e| e.to_string())?.filter_map(Result::ok).collect();
    let n = cells.len();
    for (job, size) in cells { reweigh(&c, &job, &size).map_err(|e| e.to_string())?; }
    Ok(n)
}

/// WHAT THE RECORD LOOKS LIKE FROM THE OUTSIDE — so "is it corrupted?" is a question with an answer.
///
/// Counts the rows and runs in each evidence class and, separately, anything quarantined: a row whose `source`
/// this build does not recognise. A quarantined row is scored 0 and cannot influence a suggestion, so the
/// count is the only way to notice it — `doctor` prints it.
pub fn integrity() -> Result<Value, String> {
    let c = open().map_err(|e| e.to_string())?;
    let mut st = c.prepare("SELECT source, COUNT(*), COALESCE(SUM(n),0) FROM merge_scores GROUP BY source").map_err(|e| e.to_string())?;
    let mut controlled = (0i64, 0i64);
    let mut observed = (0i64, 0i64);
    let mut quarantined: Vec<Value> = vec![];
    for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))).map_err(|e| e.to_string())?.filter_map(Result::ok) {
        let (src, rows, runs) = row;
        match evidence_of(&src) {
            Some(Evidence::Controlled) => { controlled.0 += rows; controlled.1 += runs; }
            Some(Evidence::Observed) => { observed.0 += rows; observed.1 += runs; }
            None => quarantined.push(json!({ "source": src, "rows": rows, "runs": runs })),
        }
    }
    Ok(json!({
        "controlled": { "rows": controlled.0, "runs": controlled.1 },
        "observed": { "rows": observed.0, "runs": observed.1 },
        "quarantined": quarantined,
    }))
}

/// THE GATES OF A TURN, ONTO THE RUN THAT EARNED THEM.
///
/// The three deterministic gates run after an agent claims it is done, and until now their verdicts only
/// reached the transcript: `gate_met`/`gate_unmet` was written by the goal-run path alone, which is why a
/// record of 158 rows contains not one gated row. So the one quality signal real use produces was measured,
/// shown to the operator, and thrown away.
///
/// This updates rather than inserts, and deliberately does not touch `n`: the gates describe the SAME run the
/// turn already recorded, and counting them as a second sample would inflate every pairing that happens to
/// have gates switched on. Same reasoning as `audit_only`, which exists for the same reason.
///
/// Returns whether a row was found to attach them to — a gate result with no run to belong to is dropped, not
/// filed under a row it did not come from.
pub fn record_gates(job: &str, engine: &str, model: Option<&str>, domain: &str, met: i64, unmet: i64) -> Result<bool, String> {
    if met + unmet <= 0 { return Ok(false) }
    let c = open().map_err(|e| e.to_string())?;
    let k = key(engine, model);
    // the same row shape `record_use` files a turn under: a deciding job sits on the planner, anything else on
    // the worker with `raw` as its planner
    let deciding = matches!(job, "planner" | "verifier" | "qa-gate" | "test-writer" | "synthesis" | "lookup" | "screening" | "report" | "chat");
    let (planner, worker) = if deciding { (k, "-".to_string()) } else { ("raw".to_string(), k) };
    let n = c.execute(
        "UPDATE merge_scores SET gate_met = gate_met + ?5, gate_unmet = gate_unmet + ?6, updated = datetime('now')
         WHERE id = (SELECT id FROM merge_scores WHERE job=?1 AND size='call' AND planner=?2 AND worker=?3 AND domain=?4
                     ORDER BY updated DESC, id DESC LIMIT 1)",
        params![job, planner, worker, dom(domain), met, unmet]).map_err(|e| e.to_string())?;
    if n > 0 { reweigh(&c, job, "call").map_err(|e| e.to_string())?; }
    Ok(n > 0)
}

/// A RE-ASK, ONTO THE ROW WHOSE ANSWER DID NOT HOLD.
///
/// Operator 2026-09-25, asked whether asking twice should move the weight: *"yes"*.
///
/// It attaches to the PREVIOUS turn's row, because the row being judged is the one that answered first — the
/// new ask has not been answered yet and cannot have failed. Like the gates, this updates without touching
/// `n`: it is a fact about a run already counted, not a second sample of it.
///
/// It is a soft signal on purpose, and `quality_of` treats it as one: an operator also re-asks to refine. A
/// pairing answering ten asks with one repeat is not the same as one answering two with two repeats, and the
/// ratio is what the weight reads, never the raw count.
pub fn record_reask(job: &str, engine: &str, model: Option<&str>, domain: &str) -> Result<bool, String> {
    let c = open().map_err(|e| e.to_string())?;
    let k = key(engine, model);
    let deciding = matches!(job, "planner" | "verifier" | "qa-gate" | "test-writer" | "synthesis" | "lookup" | "screening" | "report" | "chat");
    let (planner, worker) = if deciding { (k, "-".to_string()) } else { ("raw".to_string(), k) };
    let n = c.execute(
        "UPDATE merge_scores SET reasked = reasked + 1, updated = datetime('now')
         WHERE id = (SELECT id FROM merge_scores WHERE job=?1 AND size='call' AND planner=?2 AND worker=?3 AND domain=?4
                     ORDER BY updated DESC, id DESC LIMIT 1)",
        params![job, planner, worker, dom(domain)]).map_err(|e| e.to_string())?;
    if n > 0 { reweigh(&c, job, "call").map_err(|e| e.to_string())?; }
    Ok(n > 0)
}

/// THE OPERATOR UNDID WHAT THE QA GATE ACCEPTED (2026-09-30, the operator's agreed item 7): a run the gate passed and the operator then reverted is a
/// FALSE ACCEPT - the one error nothing else catches, so it is the ground truth the gate's own row needs. It lands on the judge's newest live `qa-gate`
/// row as a re-ask (the same soft signal `quality_of` already reads as a ratio, never a raw count), and the row is re-weighed. Returns whether a row
/// of that judge existed to take the mark.
pub fn record_override(job: &str, judge_key: &str) -> Result<bool, String> {
    let c = open().map_err(|e| e.to_string())?;
    let row: Option<(i64, String)> = c.query_row("SELECT id, size FROM merge_scores WHERE job=?1 AND planner=?2 AND legacy=0 ORDER BY updated DESC, id DESC LIMIT 1",
        params![job, judge_key], |r| Ok((r.get(0)?, r.get(1)?))).ok();
    let Some((id, size)) = row else { return Ok(false) };
    c.execute("UPDATE merge_scores SET reasked = reasked + 1, updated = datetime('now') WHERE id = ?1", params![id]).map_err(|e| e.to_string())?;
    reweigh(&c, job, &size).map_err(|e| e.to_string())?;
    Ok(true)
}

/// `stack` (2026-09-25): the technology the call was made in, so real work fills the language cells the polyglot bench
/// measures. "" lands in the pool, exactly as it did before the column existed.
pub fn record_use(job: &str, engine: &str, model: Option<&str>, ok: bool, ms: i64, usd: f64, stack: &str) -> Result<f64, String> {
    let k = key(engine, model);
    let deciding = matches!(job, "planner" | "verifier" | "qa-gate" | "test-writer" | "synthesis" | "lookup" | "screening" | "report" | "chat");
    let (planner, worker) = if deciding { (k, "-".to_string()) } else { ("raw".to_string(), k) };
    record(&Outcome { suite: String::new(), stack: stk(stack), domain: "any".into(), job: job.into(), size: "call".into(), planner, worker, qa: String::new(), tasks: 1, pass_first: i64::from(ok), retries: 0, failed: i64::from(!ok), gate_met: 0, gate_unmet: 0, audit: None, ms, usd, source: "use".into(), audit_only: false })
}
/// THE PLANNER'S SEED (2026-09-23): the sized L/XXL bench rows measure their planner as much as their worker — a merged
/// run is one planner over one worker — so each becomes a `planner` row too, once, when the table has none yet. Pure SQL.
pub fn seed_planner(c: &Connection) -> Result<usize, String> {
    let have: i64 = c.query_row("SELECT count(*) FROM merge_scores WHERE job = 'planner' AND size != 'call'", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    if have > 0 { return Ok(0); }
    let n = c.execute("INSERT OR IGNORE INTO merge_scores (job, size, planner, worker, qa, n, pass_first, retries, failed, gate_met, gate_unmet, audit_sum, audit_n, ms_total, usd_total, weight, source)
                       SELECT 'planner', size, planner, worker, qa, n, pass_first, retries, failed, gate_met, gate_unmet, audit_sum, audit_n, ms_total, usd_total, weight, source
                       FROM merge_scores WHERE job = 'editor-l' AND size != 'call' AND planner NOT IN ('raw', '-') AND planner != worker", []).map_err(|e| e.to_string())?;
    for size in ["l", "xxl"] { reweigh(c, "planner", size).map_err(|e| e.to_string())?; }
    Ok(n)
}
/// THE BENCHES OF 2026-09-22 AS FIRST ROWS: the table is not empty on day one; every row says `source: bench`
/// and is overtaken by real runs as they land. Idempotent (the unique key upserts; seeding twice adds a sample —
/// so it refuses when a bench row already exists).
pub fn seed() -> Result<usize, String> {
    let c = open().map_err(|e| e.to_string())?;
    let have: i64 = c.query_row("SELECT count(*) FROM merge_scores WHERE source LIKE 'bench%'", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    if have > 0 { return seed_planner(&c); }
    let cc = |m: &str| key("claude-code", Some(m)); let oc = |m: &str| key("opencode", Some(m));
    let rows: Vec<Outcome> = vec![
        // L, one worker under a verifier (ONE-WORKER-RESULTS.md): planner · worker · qa — audit /37, seconds, dollars
        Outcome { suite: String::new(), stack: "any".into(), job: "editor-l".into(), size: "l".into(), planner: cc("opus"), worker: oc("opencode/deepseek-v4-flash"), qa: "".into(), tasks: 1, pass_first: 1, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(37.0 / 37.0), ms: 249_000, usd: 0.25, source: "bench".into(), audit_only: false, domain: "any".into()},
        Outcome { suite: String::new(), stack: "any".into(), job: "editor-l".into(), size: "l".into(), planner: cc("sonnet"), worker: oc("opencode/deepseek-v4-flash"), qa: cc("opus"), tasks: 1, pass_first: 1, retries: 0, failed: 0, gate_met: 0, gate_unmet: 1, audit: Some(36.0 / 37.0), ms: 179_000, usd: 0.42, source: "bench".into(), audit_only: false, domain: "any".into()},
        Outcome { suite: String::new(), stack: "any".into(), job: "editor-l".into(), size: "l".into(), planner: cc("sonnet"), worker: oc("opencode/deepseek-v4-flash"), qa: "".into(), tasks: 1, pass_first: 0, retries: 1, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(35.0 / 37.0), ms: 266_000, usd: 0.31, source: "bench".into(), audit_only: false, domain: "any".into()},
        Outcome { suite: String::new(), stack: "any".into(), job: "editor-l".into(), size: "l".into(), planner: cc("opus"), worker: oc("opencode/deepseek-v4-flash"), qa: cc("opus"), tasks: 1, pass_first: 0, retries: 1, failed: 0, gate_met: 0, gate_unmet: 1, audit: Some(36.0 / 37.0), ms: 311_000, usd: 0.54, source: "bench".into(), audit_only: false, domain: "any".into()},
        Outcome { suite: String::new(), stack: "any".into(), job: "editor-l".into(), size: "l".into(), planner: oc("opencode/kimi-k2.7-code"), worker: oc("opencode/deepseek-v4-flash"), qa: "".into(), tasks: 1, pass_first: 1, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(37.0 / 37.0), ms: 163_000, usd: 0.04, source: "bench".into(), audit_only: false, domain: "any".into()},
        Outcome { suite: String::new(), stack: "any".into(), job: "editor-l".into(), size: "l".into(), planner: "raw".into(), worker: cc("opus"), qa: "".into(), tasks: 1, pass_first: 1, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(37.0 / 37.0), ms: 303_000, usd: 1.28, source: "bench".into(), audit_only: false, domain: "any".into()},
        // S, raw ×5: right/5 as the audit
        Outcome { suite: String::new(), stack: "any".into(), job: "editor-s".into(), size: "s".into(), planner: "raw".into(), worker: cc("sonnet"), qa: "".into(), tasks: 5, pass_first: 5, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(1.0), ms: 22_000 * 5, usd: 0.093 * 5.0, source: "bench".into(), audit_only: false, domain: "any".into()},
        Outcome { suite: String::new(), stack: "any".into(), job: "editor-s".into(), size: "s".into(), planner: "raw".into(), worker: cc("opus"), qa: "".into(), tasks: 5, pass_first: 5, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(1.0), ms: 26_000 * 5, usd: 0.188 * 5.0, source: "bench".into(), audit_only: false, domain: "any".into()},
        Outcome { suite: String::new(), stack: "any".into(), job: "editor-s".into(), size: "s".into(), planner: "raw".into(), worker: oc("opencode/deepseek-v4-pro"), qa: "".into(), tasks: 5, pass_first: 4, retries: 0, failed: 1, gate_met: 0, gate_unmet: 0, audit: Some(0.8), ms: 22_000 * 5, usd: 0.021 * 5.0, source: "bench".into(), audit_only: false, domain: "any".into()},
        Outcome { suite: String::new(), stack: "any".into(), job: "editor-s".into(), size: "s".into(), planner: "raw".into(), worker: oc("opencode/qwen3.6-plus"), qa: "".into(), tasks: 5, pass_first: 5, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(1.0), ms: 34_000 * 5, usd: 0.011 * 5.0, source: "bench".into(), audit_only: false, domain: "any".into()},
        Outcome { suite: String::new(), stack: "any".into(), job: "editor-s".into(), size: "s".into(), planner: "raw".into(), worker: oc("opencode/deepseek-v4-flash"), qa: "".into(), tasks: 5, pass_first: 2, retries: 0, failed: 3, gate_met: 0, gate_unmet: 0, audit: Some(0.4), ms: 31_000 * 5, usd: 0.003 * 5.0, source: "bench".into(), audit_only: false, domain: "any".into()},
        Outcome { suite: String::new(), stack: "any".into(), job: "editor-s".into(), size: "s".into(), planner: cc("sonnet"), worker: oc("opencode/deepseek-v4-flash"), qa: "".into(), tasks: 1, pass_first: 1, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(1.0), ms: 44_000, usd: 0.12, source: "bench".into(), audit_only: false, domain: "any".into()},
        // XXL, the wave (RESULTS.md 2026-09-22): blueprint + 10 flash fills vs raw Opus
        Outcome { suite: String::new(), stack: "any".into(), job: "editor-l".into(), size: "xxl".into(), planner: cc("sonnet"), worker: oc("opencode/deepseek-v4-flash"), qa: "".into(), tasks: 10, pass_first: 10, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(1.0), ms: 677_000, usd: 1.02, source: "bench".into(), audit_only: false, domain: "any".into()},
        Outcome { suite: String::new(), stack: "any".into(), job: "editor-l".into(), size: "xxl".into(), planner: "raw".into(), worker: cc("opus"), qa: "".into(), tasks: 1, pass_first: 1, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(1.0), ms: 926_000, usd: 6.47, source: "bench".into(), audit_only: false, domain: "any".into()},
        Outcome { suite: String::new(), stack: "any".into(), job: "editor-l".into(), size: "xxl".into(), planner: cc("opus"), worker: cc("sonnet"), qa: "".into(), tasks: 1, pass_first: 1, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(112.0 / 169.0), ms: 921_000, usd: 2.64, source: "bench".into(), audit_only: false, domain: "any".into()},
        Outcome { suite: String::new(), stack: "any".into(), job: "editor-l".into(), size: "xxl".into(), planner: cc("sonnet"), worker: cc("haiku"), qa: "".into(), tasks: 8, pass_first: 3, retries: 1, failed: 5, gate_met: 0, gate_unmet: 0, audit: Some(3.0 / 8.0), ms: 327_000, usd: 1.30, source: "bench".into(), audit_only: false, domain: "any".into()},
        // readers (bench/min, /50): planner = the reader itself, worker = "-"
        Outcome { suite: String::new(), stack: "any".into(), job: "lookup".into(), size: "s".into(), planner: cc("haiku"), worker: "-".into(), qa: "".into(), tasks: 3, pass_first: 3, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(28.0 / 30.0), ms: 43_000, usd: 0.12, source: "bench".into(), audit_only: false, domain: "any".into()},
        Outcome { suite: String::new(), stack: "any".into(), job: "lookup".into(), size: "s".into(), planner: oc("opencode/deepseek-v4-pro"), worker: "-".into(), qa: "".into(), tasks: 3, pass_first: 3, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(28.0 / 30.0), ms: 72_000, usd: 0.09, source: "bench".into(), audit_only: false, domain: "any".into()},
        Outcome { suite: String::new(), stack: "any".into(), job: "lookup".into(), size: "s".into(), planner: cc("sonnet"), worker: "-".into(), qa: "".into(), tasks: 3, pass_first: 3, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(25.0 / 30.0), ms: 77_000, usd: 0.76, source: "bench".into(), audit_only: false, domain: "any".into()},
        Outcome { suite: String::new(), stack: "any".into(), job: "synthesis".into(), size: "s".into(), planner: cc("sonnet"), worker: "-".into(), qa: "".into(), tasks: 2, pass_first: 2, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(17.0 / 20.0), ms: 88_000, usd: 0.33, source: "bench".into(), audit_only: false, domain: "any".into()},
        Outcome { suite: String::new(), stack: "any".into(), job: "synthesis".into(), size: "s".into(), planner: oc("opencode/deepseek-v4-pro"), worker: "-".into(), qa: "".into(), tasks: 2, pass_first: 2, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(13.0 / 20.0), ms: 33_000, usd: 0.04, source: "bench".into(), audit_only: false, domain: "any".into()},
        Outcome { suite: String::new(), stack: "any".into(), job: "synthesis".into(), size: "s".into(), planner: cc("haiku"), worker: "-".into(), qa: "".into(), tasks: 2, pass_first: 2, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(8.0 / 20.0), ms: 47_000, usd: 0.14, source: "bench".into(), audit_only: false, domain: "any".into()},
        Outcome { suite: String::new(), stack: "any".into(), job: "lookup".into(), size: "s".into(), planner: oc("opencode/qwen3.6-plus"), worker: "-".into(), qa: "".into(), tasks: 3, pass_first: 3, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(24.0 / 30.0), ms: 76_000, usd: 0.04, source: "bench".into(), audit_only: false, domain: "any".into()},
        Outcome { suite: String::new(), stack: "any".into(), job: "synthesis".into(), size: "s".into(), planner: oc("opencode/qwen3.6-plus"), worker: "-".into(), qa: "".into(), tasks: 2, pass_first: 2, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(14.0 / 20.0), ms: 88_000, usd: 0.05, source: "bench".into(), audit_only: false, domain: "any".into()},
    ];
    let mut n = 0;
    for o in &rows {
        // an S row carries five raw runs: file five samples with per-run time and cost, so the means are real
        if o.tasks == 5 && o.planner == "raw" {
            let per = |i: i64| Outcome { tasks: 1, pass_first: i64::from(i < o.pass_first), failed: i64::from(i >= o.pass_first), audit: Some(if i < o.pass_first { 1.0 } else { 0.0 }), ms: o.ms / 5, usd: o.usd / 5.0, ..o.clone() };
            for i in 0..5 { record(&per(i))?; n += 1; }
        } else { record(o)?; n += 1; }
    }
    let _ = seed_planner(&c);
    Ok(n)
}
/// THE WINNER OF ONE CELL OF THE TABLE, BY THE ROUTER'S OWN RULE: the cell's routable rows (not the `raw` baseline, not a
/// control suite) pooled per pairing across suites, domains and stacks, the benched and the measured set above the
/// watched and the claimed, then `rank` — keep at least 80 % of the best measured quality, then price, then time. In the
/// editing cells at the sizes that need no planner the audited `raw` (solo) rows compete too, as they do for the router
/// (`solo_competes`). The QA gate's cells are the documented exception — a gate is chosen on its false-accept rate, never
/// on price — so they are ranked on quality alone. Returns the winning pairing's most-sampled CONTROLLED row (a bench sets
/// the score; a watched row only moves it), so the screen still points at a row that exists. Pure over `table()`'s rows of
/// ONE job × size — tested.
pub fn rule_winner(cell: &[Value]) -> Option<Value> { rule_winner_with(cell, crate::looks::looks_standing()) }
/// `rule_winner` with the looks' standing given (PLAN-THE-DESIGN D6): the screen's winner is the router's, so the rows' looks take part exactly when the router lets them.
pub fn rule_winner_with(cell: &[Value], standing: crate::looks::Standing) -> Option<Value> {
    let job = cell.first()?.get("job").and_then(Value::as_str).unwrap_or("").to_string();
    let size = cell.first()?.get("size").and_then(Value::as_str).unwrap_or("").to_string();
    let solo = solo_competes(&job, &size);
    let f = |r: &Value, k: &str| r.get(k).and_then(Value::as_f64);
    let i = |r: &Value, k: &str| r.get(k).and_then(Value::as_i64).unwrap_or(0);
    let measured = |r: &Value| matches!(r.get("quality_kind").and_then(Value::as_str), Some("audited" | "gated"));
    let routable: Vec<&Value> = cell.iter().filter(|r| {
        let planner = s_of(r, "planner");
        let seat = !matches!(planner.as_str(), "raw" | "-" | "");
        let solo_row = solo && planner == "raw" && !matches!(s_of(r, "worker").as_str(), "" | "-" | "raw") && measured(r);
        // A SEAT THAT IS SWITCHED OFF CANNOT WIN (audit F-19/F-20, 2026-10-05): the screen showed - and the QA column named - a Claude pairing as the winner of a cell on a machine whose Claude was off, a route the router would never take
        (seat || solo_row) && suite_routes(&s_of(r, "suite")) && !side_blocked(&planner) && !side_blocked(&s_of(r, "worker"))
    }).collect();
    let seen: Vec<Seen> = routable.iter().map(|r| {
        // the quality `rank` itself would read: the audit when there is one, else the gate, else the first-pass ratio
        let tasks = (i(r, "pass_first") + i(r, "retries") + i(r, "failed")) as f64;
        let gates = (i(r, "gate_met") + i(r, "gate_unmet")) as f64;
        let q = f(r, "audit").unwrap_or_else(|| if gates > 0.0 { i(r, "gate_met") as f64 / gates } else if tasks > 0.0 { i(r, "pass_first") as f64 / tasks } else { 0.0 });
        Seen { planner: s_of(r, "planner"), worker: s_of(r, "worker"), class: evidence_of(&s_of(r, "source")), q, n: i(r, "n"),
               usd: f(r, "usd").unwrap_or(0.0), sec: f(r, "s").unwrap_or(0.0), measured: measured(r), proof: i(r, "audit_n").max(i(r, "gate_met") + i(r, "gate_unmet")),
               look: f(r, "look").filter(|_| i(r, "look_n") > 0).map(|l| (l, i(r, "look_n"))) }
    }).collect();
    let cands = contest(&pool_seen(&job, seen, false));
    let objective = if job == "qa-gate" { crate::roles::Objective::Smarter } else { crate::roles::Objective::Best };
    let on_looks = job.starts_with("editor-") && standing == crate::looks::Standing::Routes;
    let (k, _) = rank_with(&cands, objective, on_looks).into_iter().next()?;
    let (p, w) = k.split_once(" · ")?;
    routable.into_iter().filter(|r| s_of(r, "planner") == p && s_of(r, "worker") == w)
        .max_by_key(|r| (evidence_of(&s_of(r, "source")) == Some(Evidence::Controlled), i(r, "n"))).cloned()
}
/// THE LOOKS OF THE RECORD, FROM THE JUDGED MATCHES (PLAN-THE-DESIGN D6, 2026-10-01).
///
/// `look_pairs` (written by `looks::record_pair`, from `bench/design/out/pairs.jsonl` or from a product judge) is the source of truth; this turns it into the two columns
/// the router reads. The whole ranking is refitted from every stored match of `pair_domain` and REWRITTEN onto the rows - never incremented, so importing twice changes
/// nothing, and a match added later moves every cell it touches. A cell's looks attach to EVERY row whose WORKER is that seat (the solo row, `planner = 'raw'`, and the rows where
/// another seat plans and this one builds) of the editing jobs in `targets` (the two screen domains): a bench page was built by one seat's hands alone, and an editing job is the worker's - a pair whose hands are a seat
/// with a bad-looking page must not walk round the gate because a planner stands in front of it.
/// A seat with looks and NO behavioural row there is reported as unattached (`rows = 0`), not given a row: a row made only to hold a look would be a candidate nobody has
/// measured for behaviour. Returns what was written, per cell.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LookImport { pub matches: usize, pub method: String, pub cells: Vec<LookCell> }
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LookCell { pub cell: String, pub key: String, pub looks: f64, pub lo: f64, pub hi: f64, pub n: usize, pub rows: usize }
pub fn import_looks(c: &Connection, pair_domain: &str, keymap: &std::collections::BTreeMap<String, String>, targets: &[&str], boots: usize, seed: u64, judge: Option<&str>) -> rusqlite::Result<LookImport> {
    crate::looks::ensure_schema(c)?;
    let ms = crate::looks::matches_of(c, pair_domain, judge);
    let rk = crate::looks::rank(&ms, boots, seed);
    // one transaction: a failure part-way, or a router read in between, never sees the looks wiped and half written back
    let tx = c.unchecked_transaction()?;
    for d in targets { tx.execute("UPDATE merge_scores SET look_sum = 0, look_n = 0 WHERE domain = ?1 AND look_n > 0", params![d])?; }
    let mut cells = vec![];
    for l in &rk.cells {
        let key = keymap.get(&l.cell).cloned().unwrap_or_else(|| match l.cell.split_once(':') { Some((e, m)) => key(e, Some(m)), None => l.cell.clone() });
        let mut rows = 0;
        for d in targets {
            rows += tx.execute("UPDATE merge_scores SET look_sum = ?1, look_n = ?2 WHERE legacy = 0 AND job LIKE 'editor-%' AND worker = ?3 AND domain = ?4",
                              params![l.looks * l.n as f64, l.n as i64, key, d])?;
        }
        cells.push(LookCell { cell: l.cell.clone(), key, looks: l.looks, lo: l.lo, hi: l.hi, n: l.n, rows });
    }
    tx.commit()?;
    Ok(LookImport { matches: rk.matches, method: rk.method, cells })
}
/// seat name as the bench spells it -> the merge key (`engine:model`, canonical), from the bench's runs (`{seat, engine, model}` per line). Pure - tested.
pub fn keymap_from_runs(runs: &[Value]) -> std::collections::BTreeMap<String, String> {
    runs.iter().filter_map(|r| Some((r.get("seat")?.as_str()?.to_string(), key(r.get("engine")?.as_str()?, r.get("model").and_then(Value::as_str))))).collect()
}

/// THE TABLE AS THE BENCHMARK SCREEN READS IT — assembled here rather than inside the handler so the one thing that
/// can go wrong is testable.
///
/// THE TECHNOLOGY AXIS, SURFACED (operator's plan II.7: "the benchmark screen will display the type of technologies
/// that were tested"). A weight means nothing without saying what it was measured on.
///
/// TWO DIFFERENT AXES ride along, and they are NOT the same vocabulary — conflating them would print a confident
/// falsehood: `domain` (on every row) is the kind of WORK, from the paths a run touched (frontend · backend · tests ·
/// infra · mixed · any), while `stack` is what THIS repo is BUILT in, from its manifests (rust · web · go · python …).
/// A rust repo whose measurements all say "backend" is measured here; calling that a mismatch because "rust" is not
/// "backend" would be a bug. Both travel as themselves and the screen prints them apart.
///
/// `best` is the first row per job × size, which `table()`'s ORDER BY already put at the top of each cell — pooled over
/// domains on purpose: it answers "the winner of this job", and the per-domain winner is the `by job × work` view.
/// `evidence` is the controlled/observed split behind the numbers, `null` when the record cannot be read.
pub fn table_payload(rows: Vec<Value>, stack: &str, evidence: Option<Value>) -> Value { table_payload_with(rows, stack, evidence, crate::looks::looks_standing()) }
/// `table_payload` with the looks' standing given. The payload carries the looks of every cell (`rows[].look`, `look_n`), a sentence per editing cell that has any (`look_why`), and the
/// flag the screen needs to print them honestly: `looks.advisory` is true while the judge has not earned the router's trust, and then the ranking ignores them.
pub fn table_payload_with(rows: Vec<Value>, stack: &str, evidence: Option<Value>, standing: crate::looks::Standing) -> Value {
    // EVERY ROW CARRIES ITS OWN MARGIN (2026-09-26). A quality of 0.80 over ten runs and one over a single run are
    // the same number and not the same fact — and 96 of this record's 164 rows are the second kind. The interval
    // travels with the row so no screen can print the number without it.
    let rows: Vec<Value> = rows.into_iter().map(|mut r| {
        let q = r.get("audit").and_then(Value::as_f64).unwrap_or(0.0);
        let n = r.get("n").and_then(Value::as_i64).unwrap_or(0);
        let (lo, hi) = margin(q, n);
        r["q_low"] = json!((lo * 1000.0).round() / 1000.0);
        r["q_high"] = json!((hi * 1000.0).round() / 1000.0);
        r
    }).collect();
    // …AND EVERY CELL SAYS WHY ITS WINNER WON, including "I cannot tell these two apart".
    //
    // THE WINNER IS THE ROUTER'S, NOT THE TOP OF THE LIST (2026-10-01). `best` was `table()`'s first row of the cell — the
    // highest `weight`, and a weight is only comparable inside one suite × domain × stack pool — so the audit that reads this
    // field ("the router's winner") reported a Sonnet × Composer row at 0.76 as the winner of editor-l/l while the router,
    // reading `best_for_role` / `best`, did something else. It is now `rule_winner`: the same pooling and the same
    // band → price → time ranking the router applies. A cell with no routable row (control suites only) keeps the top row.
    let mut best: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut by_cell: std::collections::BTreeMap<String, Vec<Value>> = Default::default();
    for r in &rows {
        let k = format!("{}/{}", r["job"].as_str().unwrap_or(""), r["size"].as_str().unwrap_or(""));
        by_cell.entry(k).or_default().push(r.clone());
    }
    for (k, rs) in by_cell.iter_mut() {
        let top = rule_winner_with(rs, standing).unwrap_or_else(|| rs[0].clone());
        // the explanation reads the winner first, the rest in the table's order
        if let Some(i) = rs.iter().position(|r| *r == top) { let w = rs.remove(i); rs.insert(0, w); }
        best.insert(k.clone(), top);
    }
    let mut look_why: serde_json::Map<String, Value> = serde_json::Map::new();
    for (k, rs) in &by_cell {
        let job = rs.first().map(|r| s_of(r, "job")).unwrap_or_default();
        if !job.starts_with("editor-") { continue }
        let w = &rs[0];
        let note = look_note(rs, &s_of(w, "planner"), &s_of(w, "worker"), crate::roles::Objective::Best, standing);
        if !note.is_empty() { look_why.insert(k.clone(), json!(note)); }
    }
    let why: serde_json::Map<String, Value> = by_cell.into_iter()
        .map(|(k, rs)| (k, json!(why_won(&rs)))).collect();
    let looks = json!({ "standing": match standing { crate::looks::Standing::Routes => "routes", crate::looks::Standing::Advisory => "advisory", crate::looks::Standing::NotTrusted => "not-trusted" },
                        "advisory": standing != crate::looks::Standing::Routes, "min_n": LOOK_MIN_N, "judge": crate::looks::JUDGE_VERSION,
                        "text": match standing { crate::looks::Standing::Routes => "the judge agrees with your taste: looks take part in the ranking",
                                                 crate::looks::Standing::Advisory => "advisory - not yet calibrated to your taste: looks are shown, never ranked",
                                                 crate::looks::Standing::NotTrusted => "advisory - the judge and you disagree too often: looks are shown, never ranked" } });
    let defaults = defaults_summary(&rows);
    json!({ "ok": true, "rows": rows, "defaults": defaults, "best": best, "why": why, "look_why": look_why, "looks": looks, "file": path().to_string_lossy(),
            "stack": stack, "evidence": evidence.unwrap_or(Value::Null) })
}
/// `GET /api/v1/settings/merges` — the table, the best row per job × size, and where the numbers came from.
pub async fn http_table() -> axum::response::Response {
    use axum::response::IntoResponse;
    match tokio::task::spawn_blocking(|| { let _ = seed(); table() }).await.unwrap_or_else(|e| Err(e.to_string())) {
        Ok(rows) => axum::Json(table_payload(rows, crate::audit::workspace_stack(), integrity().ok())).into_response(),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, axum::Json(json!({ "ok": false, "error": e }))).into_response(),
    }
}

/// `POST /api/v1/settings/merges/record` — a bench adds an outcome it measured outside a run (with the audit the serve
/// cannot know); the body is an `Outcome`. The merge bench (bench/merges) uses it after each arm's audit.
pub async fn http_record(body: axum::body::Bytes) -> axum::response::Response {
    use axum::response::IntoResponse;
    let o: Result<Outcome, _> = serde_json::from_slice(&body);
    match o {
        Ok(o) if !o.job.is_empty() && !o.size.is_empty() && !o.planner.is_empty() && !o.worker.is_empty() => match tokio::task::spawn_blocking(move || record(&o)).await.unwrap_or_else(|e| Err(e.to_string())) {
            Ok(w) => axum::Json(json!({ "ok": true, "weight": w })).into_response(),
            // AN AUDIT WITH NO ROW TO JOIN IS NOT A SERVER FAULT (2026-09-26, found when a bench audited a run that
            // had failed before it produced one). 500 says "we broke" and sends a caller looking at our logs; this is
            // "there is nothing here to audit", which is a 409 and a sentence they can act on.
            Err(e) if e.contains("no such row") => (axum::http::StatusCode::CONFLICT, axum::Json(json!({ "ok": false, "error": e }))).into_response(),
            Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, axum::Json(json!({ "ok": false, "error": e }))).into_response(),
        },
        Ok(_) => (axum::http::StatusCode::BAD_REQUEST, axum::Json(json!({ "ok": false, "error": "job, size, planner and worker are required" }))).into_response(),
        Err(e) => (axum::http::StatusCode::BAD_REQUEST, axum::Json(json!({ "ok": false, "error": e.to_string() }))).into_response(),
    }
}

#[cfg(test)]
mod tests {
    /// 2026-09-29: the obligatory QA gate recorded into the run's row and counted the run twice (S2: 16 samples for 8 arms).
    #[test]
    fn a_gate_record_joins_its_run_without_counting_it_again() {
        let run = Outcome { job: "editor-s".into(), size: "s".into(), planner: "p:x".into(), worker: "w:y".into(), qa: "q:z".into(), tasks: 1, pass_first: 1, audit: None, ms: 1000, usd: 0.05, source: "bench".into(), suite: "t".into(), ..Default::default() };
        let gate = Outcome { tasks: 0, pass_first: 0, gate_met: 2, gate_unmet: 1, usd: 0.01, ms: 0, ..run.clone() };
        assert!(!is_gate_only(&run) && is_gate_only(&gate));
        assert!(!is_gate_only(&Outcome { tasks: 0, ..Default::default() }), "an unjudged raw comparison is still a run");
        assert!(is_gate_only(&Outcome { tasks: 0, gate_met: 0, gate_unmet: 0, qa: "q:z".into(), ..Default::default() }), "a gate whose claims were all unproven is still only a gate");
        record(&run).unwrap(); record(&gate).unwrap();
        let c = open().unwrap();
        let (n, met, usd): (i64, i64, f64) = c.query_row("SELECT n, gate_met, usd_total FROM merge_scores WHERE job='editor-s' AND planner='p:x'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((n, met), (1, 2), "one run, the gate's counts joined");
        assert!((usd - 0.06).abs() < 1e-9, "and what the check cost is part of what the pairing cost");
    }

    /// THE VERIFIER READS ITS DOMAIN'S OWN POOL (2026-10-05, the L10 backend bench). One row per judge seat, each `n = 1` over 60 verdicts: the pooled per-row `min_n` never saw them, so the verifier was the planner half
    /// of the best editing pair (Codex terra, 38 % on backend). Sonnet 0.80 and Cursor Grok-high 0.75 are in the band; Composer 0.48 and Codex 0.38 are not - and with Claude off the next one down is Grok-high.
    #[test]
    fn the_verifier_of_a_domain_follows_that_domains_judge_rows_and_skips_a_seat_that_is_off() {
        let judge = |arm: &str, acc: f64, usd: f64, domain: &str| Outcome { job: "verifier".into(), size: "l".into(), planner: arm.into(), worker: "-".into(), tasks: 60, pass_first: 60, audit: Some(acc), ms: 30_000, usd, source: "bench".into(), domain: domain.into(), stack: "web".into(), suite: format!("judges-L10-{domain}"), ..Default::default() };
        for (arm, acc, usd) in [("claude-code:sonnet", 0.80, 0.063), ("claude-code:opus", 0.717, 0.136), ("cursor:cursor-grok-4.6-high", 0.75, 0.137), ("cursor:composer-2.5", 0.483, 0.014), ("codex:medium", 0.383, 0.034)] { record(&judge(arm, acc, usd, "backend")).unwrap(); }
        // another domain's older, thin rows must not decide a backend verifier
        record(&judge("cursor:composer-2.5", 1.0, 0.011, "docs")).unwrap();
        let first = |o: Vec<String>| o.first().cloned().unwrap_or_default();
        assert_eq!(first(ranked_seats_in("verifier", "l", "backend", 3)), "claude-code:sonnet", "Claude on: the cheapest seat inside the band");
        let list = ranked_seats_in("verifier", "l", "backend", 3);
        let at = |k: &str| list.iter().position(|x| x == k);
        assert!(at("cursor:cursor-grok-4.6-high") < at("cursor:composer-2.5") && at("cursor:composer-2.5").is_some(), "{list:?}");
        crate::cli::set_provider_flag("claude-code", false).unwrap();
        let off = ranked_seats_in("verifier", "l", "backend", 3);
        crate::cli::set_provider_flag("claude-code", true).unwrap();
        assert_eq!(first(off.clone()), "cursor:cursor-grok-4.6-high", "Claude off: Grok-high, not Composer: {off:?}");
        assert!(off.iter().all(|k| !k.starts_with("claude-code")), "{off:?}");
        let usable = |_: crate::cli::Engine| true;
        assert_eq!(best_for_in_with(crate::cli::Job::Verifier, "l", Some("backend"), 3, usable), Some((crate::cli::Engine::ClaudeCode, "sonnet".to_string())));
        // docs has ONE thin row of its own (Composer, 1.0, below min_n), so it reads on down its chain - docs -> backend - rather than trust it
        assert_eq!(first(ranked_seats_in("verifier", "l", "docs", 3)), "claude-code:sonnet", "a thin pool does not decide");
    }

    #[test]
    fn the_reading_jobs_are_named_as_unmeasured() { assert!(READING_JOBS.contains(&"lookup") && READING_JOBS.contains(&"synthesis") && !READING_JOBS.contains(&"planner")); }

    #[test]
    fn a_pairing_measured_on_two_suites_is_pooled_not_overwritten() {
        assert_eq!(pool(None, (1.0, 30, 0.07, 71.0)), Some((1.0, 30, 0.07, 71.0)));
        let (q, n, usd, s) = pool(Some((1.0, 30, 0.07, 71.0)), (0.5, 10, 0.03, 31.0)).unwrap();
        assert_eq!(n, 40);
        assert!((q - 0.875).abs() < 1e-12 && (usd - 0.06).abs() < 1e-12 && (s - 61.0).abs() < 1e-12, "{q} {usd} {s}");
    }

    /// operator 2026-09-28: Heimdall handed a simple frontend task to Claude Sonnet because planner rows had no price and
    /// speed was the only thing left. With list prices filled in, the cheaper planner of equal quality wins.
    #[test]
    fn an_unpriced_planner_row_is_priced_from_the_list_and_the_cheaper_one_wins() {
        use crate::roles::Objective;
        let sonnet = estimated_usd("planner", "claude-code:sonnet", "cursor:composer-2.5");
        let terra = estimated_usd("planner", "codex:gpt-5.6-terra@medium", "cursor:composer-2.5");
        let opus = estimated_usd("planner", "claude-code:opus", "cursor:composer-2.5");
        assert!(opus > sonnet && sonnet > terra && terra > 0.0, "opus {opus} · sonnet {sonnet} · terra {terra}");
        assert_eq!(estimated_usd("planner", "raw", "cursor:composer-2.5"), 0.0, "no planner, no planner spend");
        assert!(estimated_usd("editor-l", "claude-code:sonnet", "cursor:composer-2.5") > sonnet, "an editor row carries the worker too");
        // same quality, sonnet is FASTER — the row the old ranking picked
        let row = |p: &str, usd: f64, s: f64| json!({ "planner": p, "worker": "cursor:composer-2.5", "n": 2, "audit": 0.9, "usd": usd, "s": s });
        let rows = vec![row("claude-code:sonnet", sonnet, 137.0), row("codex:gpt-5.6-terra@medium", terra, 168.0)];
        for o in [Objective::Best, Objective::Cheaper] {
            assert!(rank(&rows, o)[0].0.starts_with("codex:gpt-5.6-terra"), "{o:?} must take the cheaper planner of equal quality");
        }
    }


    /// A GATE RESULT BELONGS TO THE RUN THAT EARNED IT (2026-09-25). Before this the pane's gates only reached
    /// the transcript: gate_met/gate_unmet was written by the goal-run path alone, so a 158-row record held not
    /// one gated row and every observed pairing was ranked on the verifier's opinion.
    #[test]
    fn a_turns_gates_attach_to_its_row_without_counting_a_second_sample() {
        let src = |mut x: Outcome, s: &str| { x.source = s.into(); x };
        // a turn, recorded the way record_use files one: a chat job on the planner side, size "call"
        record(&src(Outcome { suite: String::new(), stack: "any".into(), job: "chat".into(), size: "call".into(), domain: "backend".into(),
            planner: "claude-code:sonnet".into(), worker: "-".into(), qa: String::new(), tasks: 1,
            pass_first: 1, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: None, ms: 1000, usd: 0.01,
            source: String::new(), audit_only: false }, "use")).unwrap();
        let row = || table().unwrap().into_iter().find(|r| r["planner"] == "claude-code:sonnet").unwrap();
        assert_eq!(row()["n"], json!(1));

        // its gates land afterwards: two met, one failed
        assert!(record_gates("chat", "claude-code", Some("sonnet"), "backend", 2, 1).unwrap());
        let r = row();
        assert_eq!(r["gate_met"], json!(2));
        assert_eq!(r["gate_unmet"], json!(1));
        // AND n IS UNCHANGED — the gates describe the same run, and counting them again would inflate every
        // pairing that happens to have gates switched on
        assert_eq!(r["n"], json!(1), "the gates were counted as a second sample");

        // quality now comes from the gates rather than the verifier's word
        let (_, kind) = quality_of(0.0, 0, 1, 0, 0, 2, 1, 0, 1).unwrap();
        assert_eq!(kind, QualityKind::Gated);

        // a gate result with no run to belong to is dropped, not filed under a row it did not come from
        assert!(!record_gates("chat", "codex", Some("medium"), "backend", 1, 0).unwrap());
        // and a turn where every gate was skipped records nothing: skipped is not met and not unmet
        assert!(!record_gates("chat", "claude-code", Some("sonnet"), "backend", 0, 0).unwrap());
    }

    /// A RE-ASK NUDGES, IT DOES NOT DECIDE (operator 2026-09-25, asked whether asking twice should move the
    /// weight: "yes"). Read as a RATIO, and capped — an operator also re-asks to refine, and a signal that
    /// could sink a row on its own would make the router wrong in a new way.
    #[test]
    fn a_reask_is_a_ratio_and_cannot_sink_a_row_by_itself() {
        let q = |reasked, n| quality_of(1.0, 1, 1, 0, 0, 0, 0, reasked, n).unwrap().0;
        assert_eq!(q(0, 10), 1.0, "no repeat, no penalty");
        // ten asks with one repeat is barely touched; two asks with two repeats takes the full cap
        assert!((q(1, 10) - 0.967).abs() < 0.001, "{}", q(1, 10));
        assert!((q(2, 2) - 0.67).abs() < 0.01, "{}", q(2, 2));
        // THE CAP: even every ask repeated leaves two thirds of the quality standing, because this is
        // evidence that something went wrong, not proof of what
        assert!(q(99, 10) >= 0.66, "a re-ask must not be able to zero a row: {}", q(99, 10));
        // and it is a ratio, so more traffic with the same repeat rate is not punished harder
        assert!((q(1, 10) - q(10, 100)).abs() < 0.001);
    }

    #[test]
    fn a_reask_lands_on_the_row_that_answered_first() {
        let src = |mut x: Outcome, s: &str| { x.source = s.into(); x };
        record(&src(Outcome { suite: String::new(), stack: "any".into(), job: "chat".into(), size: "call".into(), domain: "any".into(),
            planner: "claude-code:sonnet".into(), worker: "-".into(), qa: String::new(), tasks: 1,
            pass_first: 1, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: None, ms: 1000, usd: 0.01,
            source: String::new(), audit_only: false }, "use")).unwrap();
        let before = table().unwrap().into_iter().find(|r| r["planner"] == "claude-code:sonnet").unwrap()["weight"].as_f64().unwrap();

        assert!(record_reask("chat", "claude-code", Some("sonnet"), "any").unwrap());
        let after = table().unwrap().into_iter().find(|r| r["planner"] == "claude-code:sonnet").unwrap();
        assert_eq!(after["n"], json!(1), "a re-ask is a fact about a run already counted, not a second sample");
        assert!(after["weight"].as_f64().unwrap() < before, "the repeat did not move the weight");

        // a pairing with no run to attach to is not invented
        assert!(!record_reask("chat", "codex", Some("medium"), "any").unwrap());
    }

    /// GATES ARE FIRST-CLASS EVIDENCE (operator 2026-09-25: "if not our weights on the benchmark only measure
    /// speed and cost"). They used to count only when no audit existed, and a row with no evidence at all was
    /// given 0.5 — a made-up number in the same column as measured ones.
    #[test]
    fn quality_ranks_its_sources_and_refuses_to_invent_one() {
        // audited: the behavioural suite is the number
        assert_eq!(quality_of(1.0, 1, 1, 0, 0, 0, 0, 0, 1), Some((1.0, QualityKind::Audited)));

        // A 37/37 RUN THAT THEN FAILED ITS BUILD IS NOT PERFECT. Both deterministic signals count, so a
        // perfect audit with one gate of two met lands halfway — this used to report 1.0.
        assert_eq!(quality_of(1.0, 1, 1, 0, 0, 1, 1, 0, 1), Some((0.75, QualityKind::Audited)));

        // gated only — real use's one deterministic signal, and now a first-class source rather than a fallback
        assert_eq!(quality_of(0.0, 0, 1, 0, 0, 2, 0, 0, 1), Some((1.0, QualityKind::Gated)));
        assert_eq!(quality_of(0.0, 0, 1, 0, 0, 0, 2, 0, 1), Some((0.0, QualityKind::Gated)));

        // claimed: the verifier's opinion, and only when neither deterministic signal spoke
        assert_eq!(quality_of(0.0, 0, 3, 1, 0, 0, 0, 0, 1), Some((0.75, QualityKind::Claimed)));

        // NO EVIDENCE IS NOT HALF GOOD
        assert_eq!(quality_of(0.0, 0, 0, 0, 0, 0, 0, 0, 1), None);

        // a failure sinks the row whatever the evidence, because it is a fact about the artefact
        let (q, k) = quality_of(1.0, 1, 0, 0, 1, 0, 0, 0, 1).unwrap();
        assert_eq!(k, QualityKind::Audited);
        assert_eq!(q, 0.0, "a perfect audit on a failed task is not quality");
        assert!(quality_of(0.0, 0, 1, 0, 1, 2, 0, 0, 1).unwrap().0 < 1.0, "green gates do not erase a failure");
    }

    /// EVERY SOURCE THIS CODEBASE WRITES MUST CLASSIFY, and anything else must quarantine rather than default.
    /// The list is not decorative: it is grepped out of the writers below, so adding a new `source` string
    /// without teaching `evidence_of` about it fails here instead of silently becoming controlled evidence.
    #[test]
    fn every_source_is_classified_and_the_unknown_is_quarantined_not_assumed() {
        assert_eq!(evidence_of("bench"), Some(Evidence::Controlled));
        // a bench row a later real run also landed on keeps the bench's comparable task
        assert_eq!(evidence_of("bench+run"), Some(Evidence::Controlled));
        assert_eq!(evidence_of("run"), Some(Evidence::Observed));
        assert_eq!(evidence_of("use"), Some(Evidence::Observed));
        assert_eq!(evidence_of("run+audit"), Some(Evidence::Observed));
        assert_eq!(evidence_of(""), Some(Evidence::Observed), "the historical default of record() meant run");
        // THE POINT: an unknown source is not controlled, not observed, and not counted
        assert_eq!(evidence_of("bench-v2"), None);
        assert_eq!(evidence_of("anything-a-future-me-invents"), None);
        // and the SQL spelling used by `best` must agree with the Rust one, or the two halves disagree silently
        for src in ["bench", "bench+run"] { assert!(src.starts_with("bench"), "`source LIKE 'bench%'` no longer matches {src}"); }
        for src in ["run", "use", "run+audit", ""] { assert!(!src.starts_with("bench"), "`source LIKE 'bench%'` wrongly matches {src}"); }
    }

    /// THE CORRUPTION, REPRODUCED — as an exact invariant, not a threshold.
    ///
    /// A first version of this test asserted the measured winner stayed "above 0.6" and PASSED WITHOUT THE
    /// FIX: the corrupted weight came out at 0.601. A regression test that is green on the broken code is
    /// worse than no test, so this asserts the thing that is actually true — adding observed evidence to a
    /// cell must not move a controlled row's weight AT ALL, to the bit.
    #[test]
    fn an_observed_row_cannot_move_the_weight_of_a_controlled_row() {
        let src = |mut x: Outcome, s: &str| { x.source = s.into(); x };
        let w = |p: &str| table().unwrap().iter().find(|r| r["planner"] == p).unwrap()["weight"].as_f64().unwrap();

        // two benchmark rows: the same task, so their costs and times are comparable with each other
        record(&src(o("editor-l", "l", "claude-code:opus", "cursor:composer-2.5", 1, 0, 0, Some(1.0), 249_000, 0.25), "bench")).unwrap();
        record(&src(o("editor-l", "l", "claude-code:sonnet", "cursor:composer-2.5", 1, 0, 0, Some(0.97), 300_000, 0.40), "bench")).unwrap();
        let before = w("claude-code:opus");
        assert!(before > 0.0);

        // now a real-use row lands in the same cell — cheap and fast because the ask was SMALL, which is the
        // shape of most live calls and the reason 211 of this machine's 446 recorded runs are of this kind
        record(&src(o("editor-l", "l", "cursor:cursor-grok-4.6-high", "cursor:composer-2.5", 1, 0, 0, Some(0.98), 5_000, 0.001), "use")).unwrap();

        assert_eq!(w("claude-code:opus"), before,
            "observed evidence moved a controlled row's weight from {before} — the baselines are shared again");
        assert!(w("cursor:cursor-grok-4.6-high") > 0.0, "observed evidence is separated, not discarded");
    }

    /// AND THE READ SIDE PREFERS CONTROLLED EVIDENCE, because a weight only means something within its class.
    #[test]
    fn a_controlled_row_wins_even_when_an_observed_row_scores_higher() {
        let src = |mut x: Outcome, s: &str| { x.source = s.into(); x };
        // the observed row is given every advantage: perfect audit, free, instant
        record(&src(o("chat", "call", "measured:model", "cursor:composer-2.5", 1, 0, 0, Some(0.80), 200_000, 0.50), "bench")).unwrap();
        record(&src(o("chat", "call", "unmeasured:model", "cursor:composer-2.5", 1, 0, 0, Some(1.00), 1_000, 0.001), "use")).unwrap();
        let b = best("chat", "call", 1).expect("a row");
        assert_eq!(b["planner"], json!("measured:model"),
            "an observed row outranked a controlled one — weights are not comparable across classes");
    }

    /// L9 (2026-09-29): SCREENING READS ITS OWN ROWS. It used to take the planner half of the best editing pairing — the
    /// seat that plans, which the record says is Sonnet (68.8 % recall at $0.041 a screen) — while bench/screen-solo had
    /// filed Gemini and Haiku (87.5 %, a third of the price) under `job = "screening"` where nothing read them. And a
    /// winner this box does not have is skipped for the next one down, not for a static default.
    #[test]
    fn screening_reads_its_own_rows_and_falls_to_the_next_seat_the_box_has() {
        let src = |mut x: Outcome, s: &str| { x.source = s.into(); x };
        record(&src(o("editor-l", "l", "claude-code:sonnet", "cursor:composer-2.5", 1, 0, 0, Some(1.0), 249_000, 0.25), "bench")).unwrap();
        record(&src(o("screening", "call", "gemini:default", "-", 4, 0, 0, Some(0.875), 25_000, 0.006), "bench")).unwrap();
        record(&src(o("screening", "call", "claude-code:haiku", "-", 4, 0, 0, Some(0.875), 9_000, 0.014), "bench")).unwrap();
        record(&src(o("screening", "call", "claude-code:sonnet", "-", 4, 0, 0, Some(0.688), 5_000, 0.041), "bench")).unwrap();
        let (e, m) = best_for(crate::cli::Job::Screening, "l", 1).expect("a screening seat");
        assert_ne!((e, m.as_str()), (crate::cli::Engine::ClaudeCode, "sonnet"), "the editing pairing\'s planner decided the screen");
        // a box with no Gemini gets the next best seat of the same job — Haiku, never Sonnet
        let only_claude = own_seat("screening", "call", 1, |e| e == crate::cli::Engine::ClaudeCode).expect("a seat");
        assert_eq!(only_claude, (crate::cli::Engine::ClaudeCode, "haiku".to_string()));
        assert_eq!(best_model_within(crate::cli::Job::Screening, "l", crate::cli::Engine::ClaudeCode, 1).as_deref(), Some("haiku"));
        assert!(own_seat("screening", "call", 1, |_| false).is_none(), "no usable seat is None, not a guess");
    }

    #[test]
    fn rescoring_repairs_weights_written_under_the_old_rules_and_is_idempotent() {
        let src = |mut x: Outcome, s: &str| { x.source = s.into(); x };
        record(&src(o("editor-l", "l", "claude-code:opus", "cursor:composer-2.5", 1, 0, 0, Some(1.0), 249_000, 0.25), "bench")).unwrap();
        record(&src(o("editor-l", "l", "cursor:cursor-grok-4.6-high", "cursor:composer-2.5", 1, 0, 0, Some(0.98), 5_000, 0.001), "use")).unwrap();
        let w = |p: &str| table().unwrap().iter().find(|r| r["planner"] == p).unwrap()["weight"].as_f64().unwrap();

        // a weight left behind by the old rules, sitting on disk where nothing will touch it
        let c = open().unwrap();
        c.execute("UPDATE merge_scores SET weight = 0.123 WHERE planner = 'claude-code:opus'", []).unwrap();
        assert_eq!(w("claude-code:opus"), 0.123);

        let cells = rescore_all().unwrap();
        assert!(cells >= 1);
        let fixed = w("claude-code:opus");
        assert_ne!(fixed, 0.123, "rescoring left a stale weight in place");
        // …and running it again changes nothing, so it is safe at every startup
        rescore_all().unwrap();
        assert_eq!(w("claude-code:opus"), fixed, "rescoring is not idempotent");
    }

    #[test]
    fn integrity_separates_the_two_and_names_anything_it_cannot_place() {
        let src = |mut x: Outcome, s: &str| { x.source = s.into(); x };
        for (p, s) in [("a:1", "bench"), ("b:1", "bench+run"), ("c:1", "use"), ("d:1", "run"), ("e:1", "bench-v2")] {
            record(&src(o("j", "s", p, "w:1", 1, 0, 0, Some(1.0), 1000, 0.01), s)).unwrap();
        }
        let r = integrity().unwrap();
        assert_eq!(r["controlled"]["rows"], json!(2));
        assert_eq!(r["observed"]["rows"], json!(2));
        assert_eq!(r["quarantined"][0]["source"], json!("bench-v2"), "an unknown source must be NAMED, not absorbed");
        // …and it is scored zero, so it cannot keep a rank it no longer earns
        let rows = table().unwrap();
        assert_eq!(rows.iter().find(|x| x["planner"] == "e:1").unwrap()["weight"].as_f64().unwrap(), 0.0);
    }

    fn row(planner: &str, worker: &str, n: i64, audit: Option<f64>, usd: f64, sec: f64, job: &str, domain: &str) -> Value {
        json!({ "planner": planner, "worker": worker, "n": n, "audit": audit, "usd": usd, "s": sec, "job": job, "domain": domain })
    }

    /// THE CATALOGUE IS THE EVIDENCE (operator 2026-09-25: "you should be able to merge any model to any
    /// model"). The wizard offered six hardcoded pairs; the table has always held any-to-any.
    #[test]
    fn the_pairs_offered_are_the_pairs_measured() {
        let rows = vec![
            row("claude-code:opus", "cursor:composer-2.5", 6, Some(1.0), 0.25, 249.0, "editor-l", "backend"),
            row("claude-code:opus", "cursor:composer-2.5", 4, Some(0.9), 0.35, 300.0, "planner", "frontend"),
            row("gemini:gemini-3.8-flash", "cursor:composer-2.5", 2, Some(1.0), 0.04, 163.0, "editor-l", "backend"),
            // a raw arm is what a merge is measured AGAINST — it is not a merge anyone can hire
            row("raw", "claude-code:opus", 9, Some(1.0), 1.28, 303.0, "editor-l", "backend"),
            // …and neither is a solo row. It stores "-" as its worker, not an empty string — 52 of this
            // machine's 158 rows do, and missing that put options like "sonnet × -" in the picker.
            row("codex:medium", "-", 7, Some(0.8), 0.5, 100.0, "planner", "any"),
            row("codex:medium", "", 7, Some(0.8), 0.5, 100.0, "planner", "any"),
        ];
        let out = pairs_from(&rows);
        assert_eq!(out.len(), 2, "raw and solo rows are not merges: {out:?}");
        assert_eq!(s_of(&out[0], "planner"), "claude-code:opus");
        assert_eq!(out[0]["runs"], json!(10), "the pair's two rows are one option with their runs added");
        assert_eq!(out[0]["rows"], json!(2));
        // the figures re-average WEIGHTED BY RUNS: six runs at 1.0 and four at 0.9 is 0.96, not 0.95
        assert_eq!(out[0]["quality"], json!(0.96));
        assert_eq!(out[0]["usd"], json!(0.29));
        assert_eq!(out[0]["jobs"], json!(["editor-l", "planner"]));
        assert_eq!(out[0]["domains"], json!(["backend", "frontend"]), "`any` is not a technology");
        // most-measured first: the wizard leads with the pairs whose numbers mean something
        assert_eq!(s_of(&out[1], "planner"), "gemini:gemini-3.8-flash");
    }

    #[test]
    fn a_pair_nothing_audited_says_so_instead_of_guessing() {
        let out = pairs_from(&[row("codex:medium", "cursor:cursor-grok-4.6-high", 3, None, 0.1, 50.0, "editor-s", "any")]);
        assert_eq!(out[0]["quality"], json!(null), "no audited run means no quality number, not a made-up one");
        assert_eq!(out[0]["runs"], json!(3));
        assert_eq!(out[0]["domains"], json!([]));
    }

    #[test]
    fn rows_with_no_runs_are_not_evidence() {
        assert!(pairs_from(&[row("a:1", "b:2", 0, Some(1.0), 0.0, 0.0, "j", "any")]).is_empty());
        assert!(pairs_from(&[]).is_empty());

        // A CONTROL SUITE'S ROW IS NOT AN OFFER EITHER (2026-09-26). The picker shows a quality beside each merge;
        // a bench the router will not obey must not raise or lower it, and a pair measured by nothing else is
        // simply not offered — nothing that counts has measured it.
        let mut ctrl = row("claude-code:opus", "cursor:composer-2.5", 9, Some(1.0), 0.1, 10.0, "editor-l", "backend");
        ctrl["suite"] = json!("swebench");
        let mini = row("claude-code:opus", "cursor:composer-2.5", 1, Some(0.5), 0.2, 20.0, "editor-l", "backend");
        let out = pairs_from(&[ctrl.clone(), mini]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["runs"], json!(1), "the control's 9 runs did not join the count");
        assert_eq!(out[0]["quality"], json!(0.5), "nor did its perfect score move the quality");
        assert!(pairs_from(&[ctrl]).is_empty(), "a pair with only control evidence is not offered");
    }
    use super::*;

    #[test]
    fn a_merge_with_a_seat_that_cannot_take_a_call_is_passed_over() {
        let cursor_out = |e: &str| e != "cursor";
        assert!(!seats_of_key_ok("codex:gpt-5.6-terra@medium · cursor:composer-2.5", &cursor_out), "the hands are at their limit");
        assert!(seats_of_key_ok("codex:gpt-5.6-terra@medium · codex:gpt-5.6-terra@low", &cursor_out));
        assert!(seats_of_key_ok("raw · codex:gpt-5.6-terra@medium", &cursor_out), "raw is no seat");
        assert!(!seats_of_key_ok("cursor:composer-2.5 · -", &cursor_out), "a solo cell on the limited seat");
        let said = |e: &str| if e == "cursor" { Some("cursor week ~96% est.".to_string()) } else { None };
        assert_eq!(limited_seat_of("codex:x · cursor:composer-2.5", &said).as_deref(), Some("cursor - cursor week ~96% est."));
        assert_eq!(limited_seat_of("codex:x · codex:y", &said), None);
    }
    fn o(job: &str, size: &str, planner: &str, worker: &str, pass: i64, retries: i64, failed: i64, audit: Option<f64>, ms: i64, usd: f64) -> Outcome {
        Outcome { suite: String::new(), stack: "any".into(), job: job.into(), size: size.into(), planner: planner.into(), worker: worker.into(), qa: String::new(), tasks: pass + retries + failed, pass_first: pass, retries, failed, gate_met: 0, gate_unmet: 0, audit, ms, usd, source: "run".into(), audit_only: false, domain: "any".into()}
    }
    /// RULE 1 NEEDS NO HARD-CODED DEFAULT — IT NEEDS THE ROW PRICED (2026-09-27). The planner row was cloned from the
    /// editor outcome whole, so it carried the WHOLE RUN's bill, and the one cell that could answer "which brain?"
    /// answered "which pair?" instead: at size=l all four brains sat within 0.024 of each other on a figure none of
    /// them earned. Measured honestly the spread is not subtle — Codex plans at $0.0039 against Sonnet's $0.093 and
    /// Opus's $0.188, 24-48x for the same job, with quality tied across every pair.
    ///
    /// So the fix is arithmetic, not policy: give the row the planner's own cost and the record prefers the cheap
    /// brain by itself — and keeps preferring it if a cheaper one appears, which a hard-coded default never would.
    /// The `call` cells already behaved this way (codex 0.972 against sonnet 0.875) because they were priced all
    /// along; `l` could not, because the clone overwrote the figure.
    #[test]
    fn a_planner_row_priced_at_its_own_call_prefers_the_cheap_brain_with_no_rule_to_say_so() {
        // A CELL OF ITS OWN (2026-09-27). The test DB is keyed per THREAD, so tests sharing one share a database —
        // and this asserts on `best(...)`, which reads the whole pool. Written against the real `l` cell it passed or
        // failed on which test ran first: a 398/1 flake that reruns green. A size nothing else writes makes the
        // assertion about the arithmetic instead of the schedule.
        const SZ: &str = "planner-rule-1";
        // the same plan, equally good, from two brains — the only difference is the bill
        let codex  = record(&o("planner", SZ, "codex:gpt-5.6-terra@medium", "cursor:composer-2.5", 1, 0, 0, Some(0.875), 11_900, 0.0039)).unwrap();
        let sonnet = record(&o("planner", SZ, "claude-code:sonnet",         "cursor:composer-2.5", 1, 0, 0, Some(0.875), 17_400, 0.0929)).unwrap();
        assert!(codex > sonnet, "24x cheaper at the same quality must weigh more: codex {codex:.3} vs sonnet {sonnet:.3}");

        // and it is the ROUTER's answer, not just a number in a column
        let best = best("planner", SZ, 1).expect("a planner cell with samples answers");
        assert_eq!(best["planner"], "codex:gpt-5.6-terra@medium", "the record picked the brain it measured as cheapest at equal quality");

        // THE OLD SHAPE IS WHAT THIS GUARDS AGAINST: both rows carrying the whole run's bill instead of the plan's.
        // The brains then differ by nothing a weight can see, and the cell stops being evidence about brains at all.
        const SZ2: &str = "planner-rule-1-same-bill";
        let a = record(&o("planner", SZ2, "codex:gpt-5.6-terra@medium", "cursor:composer-2.5", 1, 0, 0, Some(0.875), 60_000, 0.0142)).unwrap();
        let b = record(&o("planner", SZ2, "claude-code:sonnet",         "cursor:composer-2.5", 1, 0, 0, Some(0.875), 60_000, 0.0142)).unwrap();
        assert!((a - b).abs() < 1e-9, "priced identically they are indistinguishable — which is exactly the bug");
    }

    #[test]
    fn a_run_adds_to_its_row_and_the_weight_ranks_peers_by_quality_then_cost_then_time() {
        // two combinations on the same job × size: the same quality, one five times cheaper → it weighs more
        let w1 = record(&o("editor-l", "l", "claude-code:opus", "cursor:composer-2.5", 1, 0, 0, Some(1.0), 249_000, 0.25)).unwrap();
        let _ = record(&o("editor-l", "l", "raw", "claude-code:opus", 1, 0, 0, Some(1.0), 303_000, 1.28)).unwrap();
        let rows = table().unwrap();
        let w = |p: &str| rows.iter().find(|r| r["planner"] == p).unwrap()["weight"].as_f64().unwrap();
        assert!(w("claude-code:opus") > w("raw"), "cheaper and faster at equal quality weighs more: {} vs {}", w("claude-code:opus"), w("raw"));
        assert!(w1 > 0.0 && w1 <= 1.0);
        // a second sample of the first row: n counts, the retry is remembered, the weight shrinks less toward the middle
        let w2 = record(&o("editor-l", "l", "claude-code:opus", "cursor:composer-2.5", 0, 1, 0, Some(35.0 / 37.0), 266_000, 0.31)).unwrap();
        let row = table().unwrap().into_iter().find(|r| r["planner"] == "claude-code:opus").unwrap();
        assert_eq!((row["n"].as_i64(), row["retries"].as_i64(), row["pass_first"].as_i64()), (Some(2), Some(1), Some(1)));
        assert!((w2 - w1).abs() < 0.5);
        // a failed combination sinks
        let wf = record(&o("editor-l", "l", "claude-code:sonnet", "claude-code:haiku", 3, 1, 5, Some(3.0 / 8.0), 327_000, 1.30)).unwrap();
        assert!(wf < w("raw"), "the failed wave weighs least: {wf}");
        // best() wants at least min_n samples
        assert_eq!(best("editor-l", "l", 2).unwrap()["planner"], "claude-code:opus");
        assert!(best("editor-l", "l", 3).is_none());
        assert!(best("lookup", "s", 1).is_none(), "an unknown job has no best");
    }
    /// AN AUDIT THE BENCH CANNOT KEY EXACTLY STILL LANDS (2026-09-26). The serve INFERS `domain` and `stack` from the
    /// work and sets `suite` itself, so a bench posting an audit knows none of the three. Requiring them to match
    /// threw the audit away with a 409 — 14 times out of 14 on one run, and 92 samples across 22 rows hold no audit
    /// because of it. The audit now follows the row.
    /// AN UNMEASURED ROW MUST NOT SET THE BAR (2026-09-27). `q_best` was the max over every row, so a row whose only
    /// evidence is `Claimed` — the agent's own `pass_first / tasks` — could define the quality every MEASURED arm had
    /// to clear. In the design pool that put an audit_n=0 row at a claimed 1.000 above four audited arms of
    /// 0.958-0.993, pushing all four outside the band and collapsing their weights to ~0.2. The arms were penalised
    /// for having been measured.
    #[test]
    fn a_claimed_row_cannot_outrank_the_arms_that_were_actually_measured() {
        // three audited arms, all excellent and cheap
        for (pl, q, usd) in [("m:a", 0.99, 0.007), ("m:b", 0.98, 0.008), ("m:c", 0.96, 0.009)] {
            record(&o("editor-l", "l", pl, "w:x", 1, 0, 0, Some(q), 60_000, usd)).unwrap();
        }
        // and one row with NO audit and no gate: its only evidence is that it says it passed
        record(&o("editor-l", "l", "claimed:one", "w:x", 1, 0, 0, None, 60_000, 0.020)).unwrap();

        let rows = table().unwrap();
        let w = |pl: &str| rows.iter().find(|r| r["planner"] == pl).map(|r| r["weight"].as_f64().unwrap_or(0.0)).unwrap_or(0.0);

        // the measured arms must not be buried by the claim: the cheapest measured arm leads the pool
        assert!(w("m:a") > w("claimed:one"),
                "a measured 0.99 at $0.007 must outrank an unmeasured claim at $0.020: {} vs {}", w("m:a"), w("claimed:one"));
        // and every measured arm keeps a real weight rather than collapsing outside a band the claim defined
        for pl in ["m:a", "m:b", "m:c"] {
            assert!(w(pl) > 0.3, "{pl} was measured and cheap; it must not collapse: {}", w(pl));
        }
        // best() must also prefer the measured arm
        assert_eq!(best("editor-l", "l", 1).unwrap()["planner"], "m:a");
    }

    #[test]
    fn an_audit_finds_its_row_even_when_the_bench_cannot_name_the_domain() {
        // the serve files the run under the domain and stack it inferred
        let mut run = o("editor-l", "l", "codex:terra", "cursor:composer-2.5", 1, 0, 0, None, 70_000, 0.01);
        run.domain = "docs".into(); run.stack = "web".into(); run.suite = "".into();
        record(&run).unwrap();

        // the bench audits it knowing only job+size+planner+worker — domain/stack default to any/any
        let mut a = o("editor-l", "l", "codex:terra", "cursor:composer-2.5", 0, 0, 0, Some(0.96), 0, 0.0);
        a.audit_only = true;
        let w = record(&a).expect("the audit must find the inferred-domain row, not 409");
        assert!(w > 0.0, "the audited row has a weight: {w}");

        let row = table().unwrap().into_iter()
            .find(|r| r["planner"] == "codex:terra" && r["domain"] == "docs")
            .expect("the row kept its inferred domain");
        assert!((row["audit"].as_f64().unwrap_or(0.0) - 0.96).abs() < 1e-6,
                "the audit landed on the row the serve actually filed, and carries its value: {row:?}");

        // an audit for a pairing that never ran is still an honest error, not a silent no-op
        let mut ghost = o("editor-l", "l", "nobody:x", "nobody:y", 0, 0, 0, Some(1.0), 0, 0.0);
        ghost.audit_only = true;
        assert!(record(&ghost).is_err(), "auditing a run that never happened must fail loudly");
    }

    #[test]
    fn a_one_point_gap_loses_to_a_tenfold_price_but_a_four_point_gap_does_not() {
        // readers on synthesis: Sonnet 17/20 at $0.33 · Pro 13/20 at $0.04 — four points apart → Sonnet stays best
        record(&o("synthesis", "s", "claude-code:sonnet", "-", 2, 0, 0, Some(17.0 / 20.0), 88_000, 0.33)).unwrap();
        record(&o("synthesis", "s", "gemini:gemini-3.8-flash", "-", 2, 0, 0, Some(13.0 / 20.0), 33_000, 0.04)).unwrap();
        assert_eq!(best("synthesis", "s", 1).unwrap()["planner"], "claude-code:sonnet");
        // readers on lookup: Sonnet 42 · Pro 41 (one point) at a tenth of the price → Pro is best
        record(&o("lookup", "s", "claude-code:sonnet", "-", 5, 0, 0, Some(42.0 / 50.0), 167_000, 1.09)).unwrap();
        record(&o("lookup", "s", "gemini:gemini-3.8-flash", "-", 5, 0, 0, Some(41.0 / 50.0), 106_000, 0.13)).unwrap();
        assert_eq!(best("lookup", "s", 1).unwrap()["planner"], "gemini:gemini-3.8-flash");
    }
    /// A WITHDRAWN ARM MUST NOT SET THE BASELINE ITS SURVIVORS ARE MEASURED AGAINST (2026-09-27).
    ///
    /// Hiding the Zen rows from `table()` and from `routing_sql` is not enough: quality, cost and time are all scored
    /// RELATIVE to the best row in the pool, so a legacy row left in `reweigh_one`'s query would keep normalising every
    /// surviving arm — influence over routing by another route. This records a metered arm that is better on every
    /// axis and asserts the subscription arm's weight does not move because of it.
    /// A WEIGHT THAT RESTS ON A CLAIM MUST SAY SO (2026-09-27). `verifier` and `planner` carry roughly a hundred
    /// samples across four domains with `audit_n = 0`: the record noted that the job RAN, never that its answer was
    /// right. Those rows still get a weight — from "no task errored" — and the table showed it beside `audit: null`,
    /// which reads as a measured row with a missing number rather than an unmeasured one. `quality_of` already knows
    /// the difference; this asserts the table reports it.
    #[test]
    fn the_table_says_whether_a_quality_was_audited_or_merely_claimed() {
        let su = "kind-probe";
        let mk = |worker: &str, audit: Option<f64>| Outcome {
            suite: su.into(), stack: "any".into(), job: "verifier".into(), size: "call".into(),
            planner: "claude-code:sonnet".into(), worker: worker.into(), qa: String::new(), tasks: 1,
            pass_first: 1, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit,
            ms: 1000, usd: 0.01, source: "bench".into(), audit_only: false, domain: "any".into(),
        };
        record(&mk("cursor:composer-2.5", Some(0.9))).unwrap();   // audited
        record(&mk("claude-code:haiku", None)).unwrap();          // ran, never checked
        let t = table().unwrap();
        let audited = t.iter().find(|r| r["suite"] == su && r["worker"] == "cursor:composer-2.5").expect("the audited row");
        let claimed = t.iter().find(|r| r["suite"] == su && r["worker"] == "claude-code:haiku").expect("the claimed row");
        assert_eq!(audited["quality_kind"], "audited");
        assert_eq!(claimed["quality_kind"], "claimed", "a row nobody checked must not look measured: {claimed}");
        // and the claimed row still carries a weight — which is exactly why the kind has to be visible
        assert!(claimed["weight"].as_f64().is_some_and(|w| w > 0.0));
        assert!(claimed["audit"].is_null());
    }

    /// A ROW NOBODY CHECKED MUST NOT WIN A ROUTE (2026-09-27). `best()` already put CONTROLLED evidence above OBSERVED —
    /// class first, weight second — but drew no line between a quality that was AUDITED and one merely CLAIMED, where
    /// the weight comes from "no task errored". `verifier` and `planner` carry ~75 claimed samples across four domains,
    /// so the unchecked row could out-weigh a checked one and answer for the role.
    #[test]
    fn a_quality_nobody_checked_cannot_outrank_one_that_was_audited() {
        let mk = |worker: &str, audit: Option<f64>, usd: f64| Outcome {
            suite: "checked-probe".into(), stack: "any".into(), job: "editor-l".into(), size: "l".into(),
            planner: "claude-code:sonnet".into(), worker: worker.into(), qa: String::new(), tasks: 1,
            pass_first: 1, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit,
            ms: 1000, usd, source: "bench".into(), audit_only: false, domain: "checked".into(),
        };
        // the CLAIMED row is cheaper and faster, so on weight alone it wins; it was never audited
        record(&mk("cursor:composer-2.5", None, 0.001)).unwrap();
        // the AUDITED row is dearer and scores 0.80 — a real, checked number
        record(&mk("claude-code:haiku", Some(0.80), 0.50)).unwrap();
        let c = open().unwrap();
        let claimed_w: f64 = c.query_row("SELECT weight FROM merge_scores WHERE suite='checked-probe' AND worker='cursor:composer-2.5'", [], |r| r.get(0)).unwrap();
        let audited_w: f64 = c.query_row("SELECT weight FROM merge_scores WHERE suite='checked-probe' AND worker='claude-code:haiku'", [], |r| r.get(0)).unwrap();
        assert!(claimed_w > audited_w, "the premise: on weight alone the unchecked row wins ({claimed_w} vs {audited_w})");
        // role_quality must still answer with the AUDITED row, because a checked class outranks an unchecked one
        let (q, n) = role_quality("editor-l", "l", "checked").expect("a row for this role");
        assert!((q - 0.80).abs() < 0.2, "the audited row must answer for the role, not the unchecked one: got {q} over {n}");
    }

    #[test]
    fn an_archived_arm_cannot_set_the_baseline_for_the_arms_that_replaced_it() {
        // its own suite, so the pool is exactly these two rows — `suite` is part of the peer key, and the other tests
        // on this thread share the database
        let su = "archive-baseline";
        let mk = |worker: &str, audit: f64, ms: i64, usd: f64| Outcome {
            suite: su.into(), stack: "any".into(), job: "editor-l".into(), size: "l".into(),
            planner: "claude-code:sonnet".into(), worker: worker.into(), qa: String::new(), tasks: 1,
            pass_first: 1, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(audit),
            ms, usd, source: "bench".into(), audit_only: false, domain: "any".into(),
        };
        // one subscription arm, alone in its pool
        let w_alone = record(&mk("cursor:composer-2.5", 0.80, 200_000, 0.20)).unwrap();
        // now a metered arm better on quality, cost AND time — it must be archived on the way in
        record(&mk("opencode:opencode/deepseek-v4-flash", 1.0, 1_000, 0.001)).unwrap();
        let c = open().unwrap();
        let leg: i64 = c.query_row("SELECT legacy FROM merge_scores WHERE suite=?1 AND worker='opencode:opencode/deepseek-v4-flash'", params![su], |r| r.get(0)).unwrap();
        assert_eq!(leg, 1, "a metered arm must be archived on the way in, never scored beside a subscription arm");
        // the subscription arm's weight has not moved: the archived row was never its peer
        let w_after: f64 = c.query_row("SELECT weight FROM merge_scores WHERE suite=?1 AND worker='cursor:composer-2.5'", params![su], |r| r.get(0)).unwrap();
        assert!((w_after - w_alone).abs() < 1e-9, "the archive moved a live weight: {w_alone} → {w_after}");
        // invisible to the operator, still readable for an audit
        assert!(table().unwrap().iter().all(|r| r["worker"] != "opencode:opencode/deepseek-v4-flash"));
        assert!(legacy_table().unwrap().iter().any(|r| r["worker"] == "opencode:opencode/deepseek-v4-flash"));
    }

    #[test]
    fn a_bench_audit_joins_the_runs_row_without_counting_a_second_sample_and_the_table_answers_by_side() {
        let _ = record(&o("editor-l", "l", "claude-code:sonnet", "cursor:composer-2.5", 1, 0, 0, None, 180_000, 0.10)).unwrap();
        let mut a = o("editor-l", "l", "claude-code:sonnet", "cursor:composer-2.5", 0, 0, 0, Some(36.0 / 37.0), 0, 0.0); a.audit_only = true;
        record(&a).unwrap();
        let row = table().unwrap().into_iter().find(|r| r["planner"] == "claude-code:sonnet").unwrap();
        assert_eq!((row["n"].as_i64(), row["audit"].as_f64().map(|x| (x * 100.0).round())), (Some(1), Some(97.0)), "{row}");
        assert!(record(&{ let mut b = a.clone(); b.planner = "nobody".into(); b }).is_err(), "an audit needs a row");
        // the table answers a job by its side once a combination has enough samples
        assert!(best_for(crate::cli::Job::Planner, "l", 2).is_none(), "one sample is not knowledge");
        for _ in 0..2 { record(&o("editor-l", "l", "claude-code:sonnet", "cursor:composer-2.5", 1, 0, 0, Some(1.0), 170_000, 0.01)).unwrap(); }
        assert_eq!(best_for(crate::cli::Job::Planner, "l", 3), Some((crate::cli::Engine::ClaudeCode, "sonnet".into())));
        assert_eq!(best_for(crate::cli::Job::EditorL, "l", 3), Some((crate::cli::Engine::Cursor, "composer-2.5".into())));
        assert_eq!(best_for(crate::cli::Job::QaGate, "l", 3), None, "no QA in that row");
    }
    #[test]
    fn the_benches_seed_once_and_the_keys_and_sizes_are_what_the_router_files_under() {
        assert!(seed().unwrap() > 10);
        assert_eq!(seed().unwrap(), 0, "seeding twice adds nothing");
        assert_eq!(seed_planner(&open().unwrap()).unwrap(), 0, "derived once");

        // THE SHIPPED SEED IS HISTORY, AND ALL OF ITS MERGES WERE ZEN (2026-09-27). Every merged row `seed()` carries
        // was measured on OpenCode's gateway, so with the Zen arms withdrawn the seed now lands in the ARCHIVE and the
        // operator's table ships with only its non-metered rows — the raw controls. That is the honest consequence of
        // the pivot and it is asserted rather than papered over: the alternative would be inventing subscription
        // numbers for merges nobody has run yet, which is the one thing this file exists to prevent.
        let archived = legacy_table().unwrap();
        assert!(archived.len() >= 4, "the seeded Zen merges belong in the archive: {}", archived.len());
        assert!(archived.iter().all(|r| r["withdrawn_because"].as_str().is_some_and(|w| !w.is_empty())),
                "every archived row must say why it was withdrawn");
        assert!(archived.iter().any(|r| r["planner"].as_str().is_some_and(|p| p.contains("kimi"))),
                "the old L winner (Kimi plan over a Flash worker) is archived, not deleted");

        let rows = table().unwrap();
        assert!(rows.iter().all(|r| r["source"].as_str().unwrap().starts_with("bench")));
        // NOTHING METERED SURVIVES INTO THE LIVE TABLE — the property that matters, whatever the seed happens to hold
        for r in &rows {
            for side in ["planner", "worker"] {
                let v = r[side].as_str().unwrap_or("");
                assert!(crate::cli::side_is_metered(v).is_none(), "a metered side reached the live table: {v} in {r}");
            }
        }
        // the sub-L seed's surviving arm is its RAW control — the gateway arm that used to sit beside it is archived
        let s5: Vec<&Value> = rows.iter().filter(|r| r["job"] == "editor-s" && r["size"] == "s").collect();
        assert_eq!((s5[0]["planner"].as_str(), s5[0]["n"].as_i64()), (Some("raw"), Some(5)), "{}", s5[0]);
        assert!(crate::cli::side_is_metered(s5[0]["worker"].as_str().unwrap()).is_none());
        assert_eq!(key("claude-code", Some("sonnet")), "claude-code:sonnet"); assert_eq!(key("opencode", None), "opencode:default");
        assert_eq!((size_of(1, true, 120), size_of(1, false, 120), size_of(1, true, 900), size_of(8, false, 900)), ("s", "l", "l", "xxl"));
    }
    #[test]
    fn a_cheaper_row_outside_the_band_does_not_move_the_scale_of_the_rows_inside_it() {
        use crate::roles::Objective::*;
        // audit F-04 (2026-10-01): `best_usd` was the cheapest row of the whole cell, band or no band. A $0.01 row at 30 % — long
        // since voted out — made the two honest rows' cost ratios 0.02 and 0.04, so the fixed quality term (0.15) outweighed the
        // bill (0.75) and the dearer, better row won. Inside the band the order is COST.
        let row = |worker: &str, audit: f64, usd: f64| json!({ "planner": "raw", "worker": worker, "n": 3, "pass_first": 3, "retries": 0, "failed": 0, "audit": audit, "s": 100.0, "usd": usd });
        let dear = row("claude-code:opus", 1.00, 0.50);
        let cheap = row("cursor:composer-2.5", 0.85, 0.25);
        let junk = row("opencode:opencode/tiny", 0.30, 0.01);
        for o in [Best, Cheaper] {
            let alone = rank(&[dear.clone(), cheap.clone()], o);
            assert!(alone[0].0.contains("composer"), "{o:?}: without the junk row the cheaper in-band row wins: {alone:?}");
            let with_junk = rank(&[dear.clone(), cheap.clone(), junk.clone()], o);
            assert!(with_junk[0].0.contains("composer"), "{o:?}: a cheaper OUT-of-band row must not change who wins inside the band: {with_junk:?}");
            assert_eq!(with_junk.last().unwrap().0.contains("tiny"), true, "{o:?}: the out-of-band row ranks last: {with_junk:?}");
            // the scores of the two in-band rows are the same with or without it
            let score = |v: &Vec<(String, f64)>, who: &str| v.iter().find(|(k, _)| k.contains(who)).unwrap().1;
            assert!((score(&alone, "composer") - score(&with_junk, "composer")).abs() < 1e-9, "{o:?} {alone:?} {with_junk:?}");
        }
    }
    #[test]
    fn a_retry_costs_time_and_money_not_quality() {
        use crate::roles::Objective::*;
        // the bug (2026-09-23, found on the frontend rows): a run sent back once by the verifier and then audited at
        // 100 % came out at 0.5 — below a run that passed first time and audited at 92 %. The verifier's rejection was
        // being counted twice: once as the retry, once as the thing the retry fixed.
        let retried = json!({ "planner": "codex:t", "worker": "opencode:opencode/qwen3.6-plus", "n": 1, "pass_first": 0, "retries": 1, "failed": 0, "audit": 1.0, "s": 620.0, "usd": 0.043 });
        // the other row is inside the band (0.96 against 1.00), so price is allowed to decide for it
        let first_time = json!({ "planner": "claude-code:sonnet", "worker": "opencode:opencode/deepseek-v4-flash", "n": 1, "pass_first": 1, "retries": 0, "failed": 0, "audit": 0.96, "s": 383.0, "usd": 0.005 });
        let rows = vec![retried, first_time];
        assert!(rank(&rows, Smarter)[0].0.contains("qwen"), "the perfect result wins on quality, retry or not: {:?}", rank(&rows, Smarter));
        // …and the retry is not free: it took longer and cost eight times as much, so `cheaper` prefers the other
        assert!(rank(&rows, Cheaper)[0].0.contains("deepseek-v4-flash"), "{:?}", rank(&rows, Cheaper));
        // WHERE THE LINE MOVED (operator 2026-09-26: "quality almost similar with a lot of price reduction is far
        // more important than speed"). The band is 80 % of the best now (it was 0.10 absolute, one instance in a
        // ten-instance bench, and 0.06 before that) — so a TEN-point gap at 0.9-1.0 is inside it and price is allowed to
        // decide. 0.06 read the bench's noise floor as a quality verdict.
        let ten_points = json!({ "planner": "claude-code:sonnet", "worker": "opencode:opencode/deepseek-v4-flash", "n": 1, "pass_first": 1, "retries": 0, "failed": 0, "audit": 0.90, "s": 10.0, "usd": 0.0001 });
        assert!(rank(&[rows[0].clone(), ten_points.clone()], Cheaper)[0].0.contains("deepseek-v4-flash"),
                "one instance of difference at a fraction of the price must win now: {:?}", rank(&[rows[0].clone(), ten_points.clone()], Cheaper));
        // …but the band is a GATE, not a discount: past it, no price buys the row back
        // THE LINE MOVED AGAIN (operator 2026-09-30): the tolerance is ONE relative number, "keep at least 80 % of the frontier", so against a perfect row 0.80 is
        // the last quality that is still for sale and anything below it is not
        let eighty = json!({ "planner": "claude-code:sonnet", "worker": "opencode:opencode/deepseek-v4-flash", "n": 1, "pass_first": 1, "retries": 0, "failed": 0, "audit": 0.80, "s": 10.0, "usd": 0.0001 });
        assert!(rank(&[rows[0].clone(), eighty], Cheaper)[0].0.contains("deepseek-v4-flash"), "exactly 80 % of the best is inside the band, so price decides");
        let far = json!({ "planner": "claude-code:sonnet", "worker": "opencode:opencode/deepseek-v4-flash", "n": 1, "pass_first": 1, "retries": 0, "failed": 0, "audit": 0.75, "s": 10.0, "usd": 0.0001 });
        assert!(rank(&[rows[0].clone(), far], Cheaper)[0].0.contains("qwen"), "a twenty-five-point gap (75 % of the best) is not for sale at any price");
        // a FAILED task is different from a retried one — it still sinks the row under every objective
        let failed = json!({ "planner": "x:1", "worker": "x:2", "n": 1, "pass_first": 0, "retries": 0, "failed": 1, "audit": 1.0, "s": 10.0, "usd": 0.001 });
        for o in crate::roles::Objective::ALL { assert!(!rank(&[failed.clone(), rows[1].clone()], o)[0].0.starts_with("x:1"), "{} ranked a failed task first", o.key()); }
    }

    #[test]
    fn three_objectives_pick_three_winners_from_the_same_rows() {
        use crate::roles::Objective::*;
        // a dear, perfect merge · a cheap one a point behind · a cheap one far behind
        let rows = vec![
            json!({ "planner": "claude-code:opus", "worker": "claude-code:opus", "n": 3, "pass_first": 3, "retries": 0, "failed": 0, "audit": 1.0, "s": 300.0, "usd": 1.20 }),
            json!({ "planner": "codex:gpt-5.6-terra@medium", "worker": "opencode:opencode/deepseek-v4-flash", "n": 3, "pass_first": 3, "retries": 0, "failed": 0, "audit": 0.97, "s": 200.0, "usd": 0.012 }),
            json!({ "planner": "opencode:opencode/deepseek-v4-flash", "worker": "opencode:opencode/deepseek-v4-flash", "n": 3, "pass_first": 2, "retries": 1, "failed": 0, "audit": 0.70, "s": 150.0, "usd": 0.004 }),
        ];
        let win = |o| rank(&rows, o).first().unwrap().0.clone();
        assert!(win(Smarter).starts_with("claude-code:opus"), "smarter takes the best answer whatever it costs: {}", win(Smarter));
        assert!(win(Cheaper).starts_with("codex:"), "cheaper takes the cheapest row still keeping 80 % of the best — not the 0.70 one: {}", win(Cheaper));
        assert!(win(Best).starts_with("codex:"), "best balances and lands on the same merge here: {}", win(Best));
        // the far-behind row is never a winner under any objective — the band keeps price from buying a real gap
        for o in crate::roles::Objective::ALL { assert!(!rank(&rows, o).first().unwrap().0.starts_with("opencode:opencode/deepseek-v4-flash · "), "{} picked the row outside the band", o.key()); }
        assert!(rank(&[], Best).is_empty());
    }

    #[test]
    fn a_role_reads_its_own_domain_and_falls_back_to_the_pool() {
        let _ = std::fs::remove_file(path());
        // `raw` is the SOLO YARDSTICK and no longer routes (routing_sql); the filler was never this test's point
        let base = Outcome { suite: String::new(), stack: "any".into(), job: "editor-l".into(), size: "l".into(), domain: "any".into(), planner: key("codex", Some("medium")),
            worker: key("claude-code", Some("sonnet")), qa: String::new(), tasks: 1, pass_first: 1, retries: 0, failed: 0,
            gate_met: 0, gate_unmet: 0, audit: Some(0.9), ms: 1000, usd: 0.5, source: "bench".into(), audit_only: false };
        record(&base).unwrap();
        // nothing backend yet → the role falls back to the pool every old row lives in
        let (_, w, n) = best_for_role("editor-l", "l", "backend", crate::roles::Objective::Best, 1).unwrap();
        assert_eq!(w, key("claude-code", Some("sonnet")));
        assert_eq!(n, 1, "one sample, from the `any` pool");
        // three backend samples of a cheaper merge → the role stops borrowing and uses its own
        for _ in 0..3 { record(&Outcome { domain: "backend".into(), worker: key("cursor", Some("cursor-grok-4.6-high")), usd: 0.01, ..base.clone() }).unwrap(); }
        let (_, w, n) = best_for_role("editor-l", "l", "backend", crate::roles::Objective::Best, 3).unwrap();
        assert_eq!(w, key("cursor", Some("cursor-grok-4.6-high")), "its own domain answers now");
        assert_eq!(n, 3);
        // and a domain nobody has measured still gets an answer, from the pool
        assert!(best_for_role("editor-l", "l", "infra", crate::roles::Objective::Best, 1).is_some());
    }

    #[test]
    fn an_audit_without_the_gates_key_joins_the_newest_row_of_its_pair() {
        let _ = std::fs::remove_file(path());
        // an audit that names no gate joins the newest row of its planner × worker (the bench cannot know the gate's key)
        let base = Outcome { suite: String::new(), stack: "any".into(), job: "editor-l".into(), size: "l".into(), planner: key("claude-code", Some("sonnet")), worker: key("cursor", Some("cursor-grok-4.6-high")), qa: key("claude-code", Some("opus")), tasks: 1, pass_first: 1, retries: 0, failed: 0, gate_met: 1, gate_unmet: 0, audit: None, ms: 1000, usd: 0.1, source: "run".into(), audit_only: false, domain: "any".into()};
        record(&base).unwrap();
        let au = Outcome { qa: String::new(), audit: Some(0.9), audit_only: true, ..base.clone() };
        record(&au).unwrap();
        let t = table().unwrap(); let r = t.iter().find(|r| r["job"] == "editor-l" && r["planner"] == base.planner && r["worker"] == base.worker && r["qa"] == base.qa).unwrap();
        assert_eq!(r["audit"].as_f64().map(|x| (x * 10.0).round() / 10.0), Some(0.9)); assert_eq!(r["source"], "run+audit");
        assert!(record(&Outcome { planner: key("codex", Some("high")), ..au }).is_err(), "no row of that pair at all → still an error");
    }

    /// II.4 — THE TWO CLASSES, COMBINED EXPLICITLY. The bench sets the score; real use nudges it and can never
    /// overturn it. The temptation this guards is the easy one: letting a noisy observed row decide a ranking because
    /// it happened to look good, or throwing observation away because it is not a benchmark.
    #[test]
    fn use_moves_a_benched_score_but_cannot_overturn_it() {
        // a pairing that benched well and keeps disappointing in real work
        let (q, n, _, _, basis) = blend(Some((0.90, 4, 0.2, 100.0)), Some((0.10, 10, 0.1, 50.0))).unwrap();
        assert_eq!(basis, "bench+use");
        assert!(q < 0.90, "ten disappointing runs must move it");
        assert!(q > 0.63, "but not past a third of the way — a bench is not overturned by observation: {q}");
        assert_eq!(n, 14, "and the sample count is all the evidence behind it");

        // one observed run barely moves anything
        let (q1, _, _, _, _) = blend(Some((0.90, 4, 0.2, 100.0)), Some((0.10, 1, 0.1, 50.0))).unwrap();
        assert!(q1 > 0.78 && q1 < 0.90, "one run is a seventh of the pull, not a verdict: {q1}");
        assert!(q1 > q, "and less than ten runs move it");

        // use can lift as well as sink — the rule is symmetric, or it is a prejudice rather than evidence
        let (up, _, _, _, _) = blend(Some((0.50, 4, 0.2, 100.0)), Some((1.00, 10, 0.1, 50.0))).unwrap();
        assert!(up > 0.50 && up < 0.70, "{up}");

        // one class alone answers as itself, and says so
        assert_eq!(blend(Some((0.8, 3, 0.2, 10.0)), None).unwrap().4, "bench");
        assert_eq!(blend(None, Some((0.8, 3, 0.2, 10.0))).unwrap().4, "use");
        assert!(blend(None, None).is_none(), "no evidence is not a score of zero");
    }

    /// …and the router actually uses it: a pairing with a perfect bench that fails every real run must lose to a
    /// steadier one, and an observed-only cell must still answer rather than going silent.
    #[test]
    fn the_role_winner_reads_both_classes_of_evidence() {
        let _ = std::fs::remove_file(path());
        let row = |worker: &str, audit: f64, src: &str, gate_met: i64, gate_unmet: i64| Outcome { suite: String::new(),
            job: "editor-l".into(), size: "l".into(), domain: "backend".into(), stack: "any".into(),
            planner: "p:1".into(), worker: worker.into(), qa: String::new(), tasks: 1, pass_first: 1, retries: 0,
            failed: 0, gate_met, gate_unmet, audit: if audit < 0.0 { None } else { Some(audit) }, ms: 100_000,
            usd: 0.2, source: src.into(), audit_only: false };
        // w:flashy benches perfectly; w:steady benches a little lower
        for _ in 0..3 { record(&row("w:flashy", 1.00, "bench", 0, 0)).unwrap(); }
        for _ in 0..3 { record(&row("w:steady", 0.80, "bench", 0, 0)).unwrap(); }
        let obj = crate::roles::Objective::Best;
        assert_eq!(best_for_role("editor-l", "l", "backend", obj, 3).unwrap().1, "w:flashy", "on benches alone it leads");

        // …then real work gates it red, over and over, while the steadier one passes
        for _ in 0..12 { record(&row("w:flashy", -1.0, "use", 0, 1)).unwrap(); }
        for _ in 0..12 { record(&row("w:steady", -1.0, "use", 1, 0)).unwrap(); }
        assert_eq!(best_for_role("editor-l", "l", "backend", obj, 3).unwrap().1, "w:steady",
            "twelve gated failures in real work must move a ranking the bench alone would not");
    }

    /// EACH BENCHMARK IS ITS OWN EVIDENCE (operator 2026-09-26: "keep each bench separated on memory so we can
    /// compare"). Two suites measuring the same job must not average into one number — they measure different
    /// abilities on different work, and the average is neither.
    #[test]
    fn two_benchmarks_on_the_same_cell_stay_two_rows() {
        let _ = std::fs::remove_file(path());
        let row = |suite: &str, audit: f64, usd: f64| Outcome {
            job: "editor-l".into(), size: "l".into(), domain: "backend".into(), stack: "python".into(),
            suite: suite.into(), planner: "raw".into(), worker: "w:1".into(), qa: String::new(), tasks: 1,
            pass_first: 1, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(audit), ms: 60_000,
            usd, source: "bench".into(), audit_only: false };
        record(&row("ministore", 0.95, 0.20)).unwrap();
        record(&row("fea", 0.55, 0.90)).unwrap();
        let t = table().unwrap();
        assert_eq!(t.len(), 2, "one row per suite");
        let m = t.iter().find(|r| r["suite"] == "ministore").unwrap();
        let f = t.iter().find(|r| r["suite"] == "fea").unwrap();
        assert_eq!((m["n"].as_i64(), f["n"].as_i64()), (Some(1), Some(1)), "neither absorbed the other");
        // …and each is ranked against ITS OWN peers: both are the only row of their suite, so both lead theirs
        assert!(m["weight"].as_f64().unwrap() > 0.5 && f["weight"].as_f64().unwrap() > 0.5);
    }

    /// THE MARGIN, AND THE MOMENT IT MATTERS MOST (2026-09-26).
    #[test]
    fn a_single_perfect_run_is_never_reported_as_certain() {
        // THE WHOLE REASON WILSON IS HERE. The textbook interval is p ± 1.96·√(p(1−p)/n), which at p = 1.0 is
        // 1.0 ± 0 — it would call ONE lucky run a certainty, at exactly the moment the record knows least.
        let (lo, hi) = margin(1.0, 1);
        assert!(lo < 0.30 && hi >= 0.999, "one perfect run must stay wide open: {lo:.2}–{hi:.2}");
        // and it narrows as evidence arrives, which is the only thing a margin is for
        let (lo10, _) = margin(1.0, 10);
        let (lo100, _) = margin(1.0, 100);
        assert!(lo10 > lo, "ten runs know more than one");
        assert!(lo100 > lo10, "a hundred know more than ten");
        // a zero is not certain either
        let (zlo, zhi) = margin(0.0, 1);
        assert!(zlo <= 1e-9 && zhi > 0.70, "one failure is not proof of failure: {zlo:.2}–{zhi:.2}");
        // no evidence at all admits everything rather than claiming the middle
        assert_eq!(margin(0.9, 0), (0.0, 1.0));
    }

    /// THE HONEST HEADLINE OF THE FIRST REAL COMPARISON: at ten instances, 8/10 and 9/10 are the same result.
    #[test]
    fn the_ranking_says_when_it_is_ordering_noise() {
        assert!(!separated((0.80, 10), (0.90, 10)), "one instance in ten does not separate two arms");
        // …which is exactly why the price-first rule is the right tie-break, not a compromise
        assert!(separated((0.30, 40), (0.95, 40)), "a real gap with real evidence does separate");
        // more evidence can turn the same difference into a real one
        assert!(separated((0.80, 4000), (0.90, 4000)), "at four thousand runs, ten points is a fact");
    }

    #[test]
    fn the_winner_says_why_it_won_and_admits_when_it_cannot_tell() {
        let row = |worker: &str, audit: f64, n: i64, usd: f64| json!({
            "planner": "raw", "worker": worker, "audit": audit, "n": n, "usd": usd, "s": 100.0 });

        // the cell with one row says so rather than implying a contest
        let solo = why_won(&[row("a", 0.9, 3, 0.10)]);
        assert!(solo.contains("the only pairing measured here"), "{solo}");
        assert!(solo.contains("95%"), "and still carries its margin: {solo}");

        // THE CASE THE RULE EXISTS FOR: cheaper, inside the band, and honest that it cannot out-rank on quality
        let cheap = why_won(&[row("qwen", 0.80, 10, 0.056), row("sonnet", 0.90, 10, 0.374)]);
        assert!(cheap.contains("cheapest keeping at least 80% of the best measured quality"), "{cheap}");
        assert!(cheap.contains("NOT separated from the runner-up"), "eight and nine of ten is noise: {cheap}");

        // and when a row is both best and cheapest there is nothing to explain away
        let both = why_won(&[row("a", 0.95, 30, 0.01), row("b", 0.60, 30, 0.90)]);
        assert!(both.contains("nothing to trade"), "{both}");
        assert!(!both.contains("NOT separated"), "a real gap is not called noise: {both}");
    }

    /// A CONTROL SUITE IS READ BY PEOPLE, NOT BY THE ROUTER. SWE-bench measures three of antiloki's eight claims and
    /// penalises a fourth; its rows stay on disk and out of the table that routes work.
    #[test]
    fn a_control_suite_cannot_win_a_job() {
        let _ = std::fs::remove_file(path());
        let row = |suite: &str, worker: &str, audit: f64| Outcome {
            job: "editor-l".into(), size: "l".into(), domain: "backend".into(), stack: "any".into(),
            // `raw` is the SOLO YARDSTICK and no longer routes (routing_sql); the filler was never this test's point
            suite: suite.into(), planner: key("claude-code", Some("sonnet")), worker: worker.into(), qa: String::new(), tasks: 1,
            pass_first: 1, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(audit), ms: 60_000,
            usd: 0.1, source: "bench".into(), audit_only: false };
        for _ in 0..3 { record(&row("swebench", "w:control", 1.00)).unwrap(); }
        for _ in 0..3 { record(&row("ministore", "w:measured", 0.70)).unwrap(); }
        let (_, w, _) = best_for_role("editor-l", "l", "backend", crate::roles::Objective::Best, 3).unwrap();
        assert_eq!(w, "w:measured", "a perfect score on a control suite must not take the job");
        assert!(suite_routes("ministore") && !suite_routes("swebench"));
        // and the control's rows are still THERE — removed from routing, not from the record
        assert!(table().unwrap().iter().any(|r| r["suite"] == "swebench"));

        // THE HOLE THIS TEST DID NOT COVER UNTIL 2026-09-26: `best_for_role` went through `best_in`, which filtered;
        // `best` — the lookup the ROUTER reads through `best_for` — did not, so the guard was half-applied. The
        // control row above outweighs the measured one, so an unfiltered `best` returns it and this fails.
        let b = best("editor-l", "l", 1).expect("a winner");
        assert_eq!(b["worker"], "w:measured", "the router's own lookup must refuse a control row too");

        // and the two spellings of the rule are one list: every control name is rejected by both
        for name in CONTROL_SUITES {
            assert!(!suite_routes(name), "{name}");
            assert!(routing_sql().contains(&format!("'{name}'")), "{name} is missing from the SQL spelling");
        }
    }

    /// THE TECHNOLOGY IS PART OF THE KEY (II.2, 2026-09-25). Adopting a polyglot benchmark is pointless if the record
    /// cannot tell Rust from Python: the same merge, measured on two languages, must be two rows.
    #[test]
    fn a_merge_measured_on_two_languages_is_two_rows_not_one() {
        let _ = std::fs::remove_file(path());
        let base = |stack: &str| Outcome { suite: String::new(), job: "editor-l".into(), size: "l".into(), domain: "backend".into(), stack: stack.into(),
            planner: key("claude-code", Some("sonnet")), worker: key("cursor", Some("cursor-grok-4.6-high")), qa: String::new(),
            tasks: 1, pass_first: 1, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(0.9), ms: 100_000, usd: 0.2,
            source: "bench".into(), audit_only: false };
        record(&base("rust")).unwrap();
        record(&base("python")).unwrap();
        record(&base("rust")).unwrap();
        let t = table().unwrap();
        let rust = t.iter().find(|r| r["stack"] == "rust").expect("the rust row");
        let py = t.iter().find(|r| r["stack"] == "python").expect("the python row");
        assert_eq!(rust["n"], 2, "two rust runs land on one rust row");
        assert_eq!(py["n"], 1, "and the python run is NOT added to it");
    }

    /// A row's peers are its own language. A merge that is cheapest in Python must not take the crown in Rust, where it
    /// has never run — that is exactly the conclusion a polyglot benchmark exists to prevent.
    #[test]
    fn the_weight_ranks_within_a_language_not_across_them() {
        let _ = std::fs::remove_file(path());
        let row = |stack: &str, worker: &str, audit: f64, usd: f64| Outcome { suite: String::new(),
            job: "editor-l".into(), size: "l".into(), domain: "backend".into(), stack: stack.into(),
            planner: key("claude-code", Some("sonnet")), worker: worker.into(), qa: String::new(), tasks: 1,
            pass_first: 1, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(audit), ms: 100_000, usd,
            source: "bench".into(), audit_only: false };
        for _ in 0..3 { record(&row("rust", "w:careful", 0.95, 1.00)).unwrap(); }
        for _ in 0..3 { record(&row("python", "w:cheap", 0.95, 0.01)).unwrap(); }
        let t = table().unwrap();
        let rust = t.iter().find(|r| r["stack"] == "rust").unwrap();
        let py = t.iter().find(|r| r["stack"] == "python").unwrap();
        // each is the best row of ITS language, so each carries a top weight — the 100× price gap between them is
        // not a comparison anyone made
        assert!(rust["weight"].as_f64().unwrap() > 0.6, "the only rust row is rust's winner: {}", rust["weight"]);
        assert!(py["weight"].as_f64().unwrap() > 0.6, "and the only python row is python's: {}", py["weight"]);
    }

    /// WATCHING IS NOT MEASURING (2026-09-28). This is the bug behind the operator's "why is it always choosing
    /// sonnet, even though the task is simple and there are others AIs capable above the ranking cheaper". The record
    /// held ONE row under `editor-s/s/backend/web` — `claude-code:sonnet × cursor:composer-2.5`, 13 samples watched in
    /// the cockpit — and 29 BENCHED arms one rung along under `*`. The ladder stopped at the first pool with enough
    /// samples, so the 29 were never read and Sonnet answered every frontend ask. Observation moves a benched score
    /// (`blend`); it does not set one, and it must not end the search while a bench is still unread.
    #[test]
    fn a_pool_that_was_only_watched_defers_to_one_that_was_benched() {
        let _ = std::fs::remove_file(path());
        // A BENCH AND A RUN ARE DIFFERENT ROWS because `suite` is part of the unique key — which is exactly how the
        // same pairing can carry both kinds of evidence for `blend` to combine. A bench with no suite would merge
        // into the observed row and its source would never be seen.
        let row = |stack: &str, worker: &str, source: &str, audit: f64, usd: f64| Outcome {
            suite: if source == "bench" { "ministore".into() } else { String::new() }, job: "editor-s".into(), size: "s".into(), domain: "backend".into(), stack: stack.into(),
            planner: key("claude-code", Some("sonnet")), worker: worker.into(), qa: String::new(),
            tasks: 1, pass_first: 1, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(audit),
            ms: 100_000, usd, source: source.into(), audit_only: false };
        let obj = crate::roles::Objective::Best;

        // the real shape: web watched only, and a benched arm one rung along that is both better and far cheaper —
        // exactly the arm the operator kept asking why the router would not use
        for _ in 0..13 { record(&row("web", "cursor:composer-2.5", "run", 0.62, 0.50)).unwrap(); }
        for _ in 0..3 { record(&row("any", "cursor:cursor-grok-4.6-high", "bench", 0.97, 0.01)).unwrap(); }
        let (_, w, _) = best_for_stack("editor-s", "s", "backend", "web", obj, 3).unwrap();
        assert_eq!(w, "cursor:cursor-grok-4.6-high",
                   "the benched arm must at least be READ; 13 watched runs cannot end the search on their own");

        // …and the moment web itself is benched, the language comes first again — with an arm that earns it
        for _ in 0..6 { record(&row("web", "cursor:composer-2.5", "bench", 0.99, 0.001)).unwrap(); }
        let (_, w, _) = best_for_stack("editor-s", "s", "backend", "web", obj, 3).unwrap();
        assert_eq!(w, "cursor:composer-2.5", "its own language, now that its own language was measured");

        // and where NOTHING was ever benched, being watched is still evidence and still gets the work
        let _ = std::fs::remove_file(path());
        for _ in 0..4 { record(&row("web", "cursor:composer-2.5", "run", 0.62, 0.50)).unwrap(); }
        assert_eq!(best_for_stack("editor-s", "s", "backend", "web", obj, 3).unwrap().1, "cursor:composer-2.5",
                   "an unmeasured role still gets the work — the second pass exists for exactly this");
    }

    /// ONE MODEL, ONE SPELLING (audit 2026-09-28). A model reaches the record twice: under the alias the binding
    /// asked for (`sonnet`) and under the full id the CLI reported back (`claude-sonnet-5`). Both became `key` sides,
    /// so one model's spend sat in two rows and neither total was the real one — the operator's "the sonnet
    /// benchmark is the same and should be measured as one, but the origins must be explicitly".
    ///
    /// The fold is Claude-only on purpose: its aliases are unambiguous, while `gpt-5.6-terra`, `-luna` and `-sol`
    /// all appear at `@medium` and folding those to `medium` would merge three models into one row.
    #[test]
    fn one_model_is_one_row_however_the_cli_spelled_it_back() {
        assert_eq!(key("claude-code", Some("claude-sonnet-5")), key("claude-code", Some("sonnet")));
        assert_eq!(key("claude-code", Some("claude-opus-5[1m]")), "claude-code:opus", "a wider window is the same model");
        assert_eq!(key("claude-code", Some("claude-haiku-4-5-20251001")), "claude-code:haiku");
        // the PROVIDER half is never folded — that is the half the operator needs to see
        assert_ne!(key("claude-code", Some("sonnet")), key("cursor", Some("sonnet")));

        // and the engines whose ids cannot be folded safely keep every character
        assert_eq!(key("codex", Some("gpt-5.6-terra@medium")), "codex:gpt-5.6-terra@medium");
        assert_eq!(key("codex", Some("gpt-5.6-luna@medium")), "codex:gpt-5.6-luna@medium");
        assert_ne!(key("codex", Some("gpt-5.6-terra@medium")), key("codex", Some("gpt-5.6-luna@medium")),
                   "terra and luna are different models and must not be merged by an effort suffix");
        assert_eq!(key("gemini", Some("gemini-3.8-flash-low")), "gemini:gemini-3.8-flash-low");
        assert_eq!(key("cursor", Some("cursor-grok-4.6-high")), "cursor:cursor-grok-4.6-high");
        // an unknown engine is passed through rather than guessed at
        assert_eq!(key("nobody", Some("claude-sonnet-5")), "nobody:claude-sonnet-5");
    }

    /// A PROVIDER WITH NOTHING AFTER THE COLON (audit 2026-09-28). `key` caught `None` and let `Some("")` through, so
    /// `gemini-composer` — whose brain deliberately names no model, leaving the record free to choose one — wrote its
    /// rows as `gemini:`. The operator's rule is that a side is always `provider:model`, because "Antropic-sonnet is
    /// different from cursor-sonnet and also from opencode-sonnet".
    #[test]
    fn a_side_is_named_provider_and_model_even_when_no_model_was_chosen() {
        assert_eq!(key("claude-code", Some("sonnet")), "claude-code:sonnet");
        assert_eq!(key("gemini", None), "gemini:default");
        assert_eq!(key("gemini", Some("")), "gemini:default", "a blank is an absent model, not a nameless one");
        assert_eq!(key("gemini", Some("   ")), "gemini:default");
        assert_eq!(key("cursor", Some(" composer-2.5 ")), "cursor:composer-2.5", "and the name is trimmed, not pasted");
        for k in [key("gemini", Some("")), key("gemini", None)] {
            assert!(!k.ends_with(':'), "no side may be a provider with nothing after it: {k:?}");
        }
    }


    /// THE BASELINE IS NOT A CANDIDATE (audit 2026-09-28) — this is what "it is always choosing sonnet" actually was.
    ///
    /// `planner = 'raw'` marks the SOLO yardstick: one model, no pipeline, no brain at all. 77 such rows were
    /// routable on the live record, and they WIN on weight because a raw arm carries none of the pipeline's cost or
    /// time — so `best("editor-l", "l")` answered with a row whose planner is `raw`. `best_for` then hit its own
    /// guard, `if key == "raw" … return None`, and answered nothing; `job_binding_learned` fell through to
    /// `job_binding_any`, the static default, which is Claude at its tier. Every measured arm in the cell was
    /// skipped, every time, and nothing in the table or the logs looked wrong.
    ///
    /// Discarding a winner after choosing it is not the same as not choosing it.
    #[test]
    fn the_raw_baseline_never_wins_a_route_however_well_it_scores() {
        let _ = std::fs::remove_file(path());
        let row = |planner: &str, audit: f64, usd: f64| Outcome {
            suite: "ministore".into(), job: "editor-l".into(), size: "l".into(), domain: "backend".into(),
            stack: "web".into(), planner: planner.into(), worker: key("cursor", Some("composer-2.5")),
            qa: String::new(), tasks: 1, pass_first: 1, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0,
            audit: Some(audit), ms: 100_000, usd, source: "bench".into(), audit_only: false };

        // the baseline is better AND cheaper than the merge — exactly the shape that made it win
        for _ in 0..5 { record(&row("raw", 1.0, 0.01)).unwrap(); }
        for _ in 0..5 { record(&row(&key("gemini", None), 0.8, 0.20)).unwrap(); }

        let w = best("editor-l", "l", 1).expect("a winner");
        assert_eq!(w["planner"], "gemini:default", "the baseline must not be offered as a brain: {w}");

        // and the ladder agrees — a route that answers `None` here is what fell through to the static default
        let (p, _, _) = best_for_role("editor-l", "l", "backend", crate::roles::Objective::Best, 1).expect("a route");
        assert_eq!(p, "gemini:default");
        assert!(best_for(crate::cli::Job::Planner, "l", 1).is_some(),
                "the planner binding must be ANSWERED, not discarded into the default");

        // with nothing but the baseline measured, the record says nothing rather than naming the baseline
        let _ = std::fs::remove_file(path());
        for _ in 0..5 { record(&row("raw", 1.0, 0.01)).unwrap(); }
        assert!(best("editor-l", "l", 1).is_none(), "a table of yardsticks holds no candidates");
    }


    /// THE LADDER: this language first, the pool second, and the sample count says which one answered.
    #[test]
    fn a_role_reads_its_own_language_and_falls_back_to_the_pool() {
        let _ = std::fs::remove_file(path());
        let row = |stack: &str, worker: &str| Outcome { suite: String::new(), job: "editor-l".into(), size: "l".into(), domain: "backend".into(),
            stack: stack.into(), planner: key("claude-code", Some("sonnet")), worker: worker.into(), qa: String::new(),
            tasks: 1, pass_first: 1, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(0.95), ms: 100_000,
            usd: 0.2, source: "bench".into(), audit_only: false };
        for _ in 0..3 { record(&row("any", "w:pooled")).unwrap(); }
        let obj = crate::roles::Objective::Best;
        // nothing measured on rust yet → the pool answers, and says so with the pool's sample count
        let (_, w, n) = best_for_stack("editor-l", "l", "backend", "rust", obj, 3).unwrap();
        assert_eq!(w, "w:pooled");
        assert_eq!(n, 3);
        // …then rust gets its own evidence, and it wins for rust while python still reads the pool
        for _ in 0..3 { record(&row("rust", "w:rusty")).unwrap(); }
        assert_eq!(best_for_stack("editor-l", "l", "backend", "rust", obj, 3).unwrap().1, "w:rusty");
        assert_eq!(best_for_stack("editor-l", "l", "backend", "python", obj, 3).unwrap().1, "w:pooled");
        // AND THE OLD CALLER IS UNCHANGED: no stack asked for = everything pooled, exactly as before the column existed
        assert!(best_for_role("editor-l", "l", "backend", obj, 3).is_some());
    }

    /// A RECORD WRITTEN BEFORE THE COLUMN EXISTED MUST SURVIVE THE MIGRATION — 158 rows of real evidence were on disk
    /// when this shipped, and a rebuild that lost or re-split them would be worse than never adding the column.
    #[test]
    fn the_migration_keeps_every_old_row_and_puts_it_in_the_pool() {
        let p = path();
        let _ = std::fs::remove_file(&p);
        if let Some(d) = p.parent() { std::fs::create_dir_all(d).unwrap(); }
        {
            // the table exactly as the build before this one wrote it: domain and reasked, no stack
            let c = Connection::open(&p).unwrap();
            c.execute_batch("CREATE TABLE merge_scores (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                job TEXT NOT NULL, size TEXT NOT NULL, domain TEXT NOT NULL DEFAULT 'any',
                planner TEXT NOT NULL, worker TEXT NOT NULL, qa TEXT NOT NULL DEFAULT '',
                n INTEGER NOT NULL DEFAULT 0, pass_first INTEGER NOT NULL DEFAULT 0, retries INTEGER NOT NULL DEFAULT 0,
                failed INTEGER NOT NULL DEFAULT 0, gate_met INTEGER NOT NULL DEFAULT 0, gate_unmet INTEGER NOT NULL DEFAULT 0,
                audit_sum REAL NOT NULL DEFAULT 0, audit_n INTEGER NOT NULL DEFAULT 0,
                ms_total INTEGER NOT NULL DEFAULT 0, usd_total REAL NOT NULL DEFAULT 0,
                weight REAL NOT NULL DEFAULT 0, source TEXT NOT NULL DEFAULT 'run', updated TEXT NOT NULL DEFAULT (datetime('now')),
                reasked INTEGER NOT NULL DEFAULT 0,
                UNIQUE (job, size, domain, planner, worker, qa));
                INSERT INTO merge_scores (job, size, domain, planner, worker, qa, n, pass_first, audit_sum, audit_n, ms_total, usd_total, weight, source, reasked)
                VALUES ('editor-l','l','backend','p:1','w:1','',7,6,6.3,7,700000,1.4,0.83,'bench',2);").unwrap();
        }
        let t = table().unwrap();     // opening runs the migration
        assert_eq!(t.len(), 1, "the row is still there");
        assert_eq!(t[0]["stack"], "any", "and it is in the POOL, not guessed into a language");
        assert_eq!(t[0]["domain"], "backend", "its kind of work survives");
        assert_eq!(t[0]["n"], 7);
        assert_eq!(t[0]["weight"].as_f64().unwrap(), 0.83, "and its weight is not recomputed by the move");
        // the re-ask count, which the rebuild has to carry across by hand, is not silently dropped
        let c = open().unwrap();
        let reasked: i64 = c.query_row("SELECT reasked FROM merge_scores", [], |r| r.get(0)).unwrap();
        assert_eq!(reasked, 2);
    }

    /// THE SCREEN IS TOLD WHAT THE NUMBERS WERE MEASURED ON (plan II.7). The two axes must arrive as two fields: a
    /// row's `domain` (the kind of work) and the repo's `stack` (what it is built in). The bug this guards is the
    /// tempting one — deriving either from the other, or letting an unknown stack print as a technology.
    #[test]
    fn the_payload_carries_both_axes_and_invents_neither() {
        let r = |job: &str, size: &str, domain: &str, weight: f64| json!({ "job": job, "size": size, "domain": domain, "weight": weight });
        let rows = vec![r("editor-l", "l", "backend", 0.81), r("editor-l", "l", "frontend", 0.74), r("planner", "s", "any", 0.5)];
        let p = table_payload(rows.clone(), "rust", Some(json!({ "controlled": { "rows": 2, "runs": 9 } })));
        assert_eq!(p["stack"], "rust", "the repo's technology travels as itself");
        assert_eq!(p["rows"][0]["domain"], "backend", "and a row keeps the kind of work it was measured on");
        assert_eq!(p["best"]["editor-l/l"]["weight"].as_f64(), Some(0.81), "the best of a cell is its first row");
        assert_eq!(p["best"].as_object().unwrap().len(), 2, "one best per job × size, pooled over domains on purpose");
        assert_eq!(p["evidence"]["controlled"]["runs"], 9);
        // AN UNKNOWN STACK SAYS NOTHING rather than borrowing a row's domain, and an unreadable record is null, not zero
        let q = table_payload(rows, "", None);
        assert_eq!(q["stack"], "", "no manifest read → no claim about the technology");
        assert!(q["evidence"].is_null(), "a record that cannot be read is not a record of zero");
    }


    /// THE BRAIN JOBS READ THEIR OWN CELL (HANDOFF §6.1, "the gap is in which table it reads"). The verifier was bound to the planner half
    /// of the best EDITING pairing — a proxy — while the judges' own measured cell sat unread. The job's cell answers when it has the
    /// samples, by the one rule (a claimed-only seat cannot win it by being cheap); the editing pairing's brain stays the fallback;
    /// a seat this box lacks gives way to the next one down the SAME order; a provider with no model is never bound.
    #[test]
    fn the_brain_jobs_read_their_own_cell_before_the_editing_pairings_planner() {
        use crate::cli::{Engine, Job};
        let _ = std::fs::remove_file(path());
        put(3, obs("editor-l", "l", "backend", "fix", "codex:medium", "cursor:composer-2.5", Some(0.95), 0.20, "bench"));
        put(3, obs("verifier", "l", "backend", "judges", "cursor:composer-2.5", "-", Some(0.90), 0.013, "bench"));
        put(3, obs("verifier", "l", "backend", "judges", "codex:medium", "-", Some(0.92), 0.030, "bench"));
        put(3, obs("verifier", "l", "backend", "judges", "gemini:gemini-3.8-flash-medium", "-", None, 0.003, "bench"));   // cheap, but only CLAIMED
        let all = |_: Engine| true;
        assert_eq!(best_for_with(Job::Verifier, "l", 3, all), Some((Engine::Cursor, "composer-2.5".to_string())), "the judges' own cell, cheapest inside the band, measured");
        assert_eq!(best_for_with(Job::Verifier, "l", 3, |e| e != Engine::Cursor), Some((Engine::Codex, "medium".to_string())), "a box without Cursor gets the next seat of the same order");
        assert_eq!(best_for_with(Job::Verifier, "s", 3, all).map(|t| t.1), Some("composer-2.5".to_string()), "a small ask reads the L cell");
        assert_eq!(best_for_with(Job::Verifier, "xxl", 3, all), None, "a wave reads only its own cell, and falls back to an editing pairing that does not exist either");
        assert_eq!(best_model_within(Job::Verifier, "l", Engine::Codex, 3).as_deref(), Some("medium"), "under a pair the side's engine still rules");
        // no cell of its own → the editing pairing's brain, exactly as before
        assert_eq!(best_for_with(Job::Planner, "l", 3, all), Some((Engine::Codex, "medium".to_string())));
        // a provider with no model after the colon is the cheapest, perfect seat of the cell — and is never bound
        put(3, obs("verifier", "l", "backend", "judges2", "gemini:", "-", Some(1.0), 0.001, "bench"));
        assert_eq!(best_for_with(Job::Verifier, "l", 3, all).map(|t| t.1), Some("composer-2.5".to_string()));
    }

    /// ITEM 4's PROBE — not a check, a measurement: what the router answers on a COPY of the live record, per size and domain, with
    /// the solo cells in the contest and without, and what the pooled reads answer now.
    /// `PROBE_DB=<copy of merges.sqlite> cargo test -p antiloki-serve --lib probe_item4 -- --ignored --nocapture`
    #[test]
    #[ignore = "reads a copy of the operator's live record; prints, asserts nothing about it"]
    fn probe_item4_the_router_on_a_copy_of_the_live_record() {
        let src = std::env::var("PROBE_DB").expect("PROBE_DB=<path to a COPY of merges.sqlite>");
        let dst = path(); std::fs::create_dir_all(dst.parent().unwrap()).unwrap(); std::fs::copy(&src, &dst).unwrap();
        let obj = crate::roles::Objective::Best;
        for (job, size) in [("editor-s", "s"), ("editor-l", "l")] {
            println!("── {job}/{size}");
            for d in ["backend", "frontend", "design", "docs", "infra", "mixed", "tests", "unknown", "*"] {
                let pair = best_for_role(job, size, d, obj, 1).map(|(p, w, n)| format!("{p} × {w} (n={n})"));
                let cell = best_cell(job, size, d, obj, 1);
                println!("   {d:9} pair-only: {}\n             with solo: {}", pair.unwrap_or("-".into()),
                         cell.map(|c| format!("{} × {} (n={}){}", c.planner, c.worker, c.n, if c.note.is_empty() { String::new() } else { format!("\n             · {}", c.note) })).unwrap_or("-".into()));
            }
            println!("   pooled best(): {:?}", best(job, size, 3).map(|b| format!("{} × {} qa {} n={}", b["planner"], b["worker"], b["qa"], b["n"])));
        }
        // WHY a cell answered what it did: the candidates of one pool, as `rank` sees them (PROBE_POOL=job/size/domain)
        if let Some(spec) = std::env::var("PROBE_POOL").ok().filter(|v| !v.is_empty()) {
            let mut it = spec.split('/'); let (j, sz, d) = (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().unwrap_or("*"));
            let c = open().unwrap();
            let rows = pool_rows(&c, j, sz, d, "*", j.starts_with("editor") );
            let cands = contest(&rows);
            println!("── pool {spec}: {} pairings, {} compete", rows.len(), cands.len());
            for (k, score) in rank(&cands, obj) {
                let r = cands.iter().find(|r| format!("{} · {}", s_of(r, "planner"), s_of(r, "worker")) == k).unwrap();
                println!("   {score:.3}  q={:.2} ${:.3} {:>4.0}s n={:<3} {} {k}", r["audit"].as_f64().unwrap_or(0.0), r["usd"].as_f64().unwrap_or(0.0), r["s"].as_f64().unwrap_or(0.0), r["n"].as_i64().unwrap_or(0), r["basis"].as_str().unwrap_or(""));
            }
        }
        for (job, size) in [("planner", "l"), ("planner", "s"), ("verifier", "l"), ("test-writer", "l"), ("qa-gate", "l")] {
            println!("── {job}/{size} pooled best(): {:?}", best(job, size, 1).map(|b| format!("{} × {} n={}", b["planner"], b["worker"], b["n"])));
        }
        println!("── screening/call ranked: {:?}", ranked_seats("screening", "call", 1));
        let rows = table().unwrap();
        let mut cells: Vec<String> = rows.iter().map(|r| format!("{}/{}", r["job"].as_str().unwrap_or(""), r["size"].as_str().unwrap_or(""))).collect();
        cells.sort(); cells.dedup();
        for c in cells {
            let cell: Vec<Value> = rows.iter().filter(|r| format!("{}/{}", r["job"].as_str().unwrap_or(""), r["size"].as_str().unwrap_or("")) == c).cloned().collect();
            if let Some(w) = rule_winner(&cell) { println!("   rule_winner {c:18} {} × {}", w["planner"], w["worker"]); }
        }
    }

    // ── ITEM 4 (2026-10-01): SOLO CELLS COMPETE, AND THE POOLED READS FOLLOW THE RULE ────────────────────────────────
    /// one observation of a pairing in a pool of the record — `raw` as the planner is a solo arm
    fn obs(job: &str, size: &str, domain: &str, suite: &str, planner: &str, worker: &str, audit: Option<f64>, usd: f64, source: &str) -> Outcome {
        Outcome { suite: suite.into(), stack: "any".into(), job: job.into(), size: size.into(), domain: domain.into(), planner: planner.into(), worker: worker.into(),
                  qa: String::new(), tasks: 1, pass_first: 1, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit, ms: 60_000, usd, source: source.into(), audit_only: false }
    }
    fn put(times: usize, o: Outcome) { for _ in 0..times { record(&o).unwrap(); } }

    /// A SOLO CELL INSIDE THE BAND THAT IS CHEAPER WINS (operator 2026-09-30: "tasks L and smaller doesnt need a planner").
    /// Until now the single AI was only ever the hands of the best pair: every `raw` row was kept out of the contest, so a seat
    /// that does the whole job for a seventh of the price at 84 % of the pair's quality could never be named.
    #[test]
    fn a_solo_cell_inside_the_band_that_is_cheaper_wins_the_small_sizes() {
        let _ = std::fs::remove_file(path());
        put(3, obs("editor-l", "l", "backend", "fix", "p:brain", "w:hands", Some(0.95), 0.21, "bench"));
        put(3, obs("editor-l", "l", "backend", "fix", "raw", "s:alone", Some(0.80), 0.03, "bench"));   // 0.80 / 0.95 = 84 %: inside the 80 % band
        let obj = crate::roles::Objective::Best;
        let pick = best_cell("editor-l", "l", "backend", obj, 3).expect("a winner");
        assert!(pick.solo && pick.planner == "raw" && pick.worker == "s:alone", "{pick:?}");
        assert!(pick.note.contains("solo wins") && pick.note.contains("no planner is paid for"), "the run says why: {}", pick.note);
        // …and the same at size s
        put(2, obs("editor-s", "s", "backend", "s2", "p:brain", "w:hands", Some(1.0), 0.14, "bench"));
        put(2, obs("editor-s", "s", "backend", "s2", "raw", "s:alone", Some(0.9), 0.02, "bench"));
        assert_eq!(best_cell("editor-s", "s", "backend", obj, 1).unwrap().worker, "s:alone");
        // what offers a MERGE to bind (the Add-agent proposals, `best_for_role`) still answers a pair: a solo seat is not a brain × hands
        assert_eq!(best_for_role("editor-l", "l", "backend", obj, 3).unwrap(), ("p:brain".into(), "w:hands".into(), 3));
    }

    /// A SOLO CELL OUTSIDE THE BAND LOSES, however cheap it is: the band is a gate, not a discount.
    #[test]
    fn a_solo_cell_outside_the_band_loses_whatever_it_costs() {
        let _ = std::fs::remove_file(path());
        put(3, obs("editor-l", "l", "backend", "fix", "p:brain", "w:hands", Some(0.95), 0.21, "bench"));
        put(3, obs("editor-l", "l", "backend", "fix", "raw", "s:alone", Some(0.70), 0.001, "bench"));   // 0.70 / 0.95 = 74 %: not for sale
        let pick = best_cell("editor-l", "l", "backend", crate::roles::Objective::Best, 3).unwrap();
        assert!(!pick.solo && pick.worker == "w:hands", "{pick:?}");
        assert!(pick.note.contains("a solo cell was measured") && pick.note.contains("falls outside the 80% band"), "{}", pick.note);
        // INSIDE the band but DEARER is no better: price decides inside it
        let _ = std::fs::remove_file(path());
        put(3, obs("editor-l", "l", "backend", "fix", "p:brain", "w:hands", Some(0.95), 0.21, "bench"));
        put(3, obs("editor-l", "l", "backend", "fix", "raw", "s:alone", Some(0.90), 0.60, "bench"));
        let pick = best_cell("editor-l", "l", "backend", crate::roles::Objective::Best, 3).unwrap();
        assert!(!pick.solo && pick.note.contains("keeps the band but is not cheaper"), "{pick:?}");
    }

    /// NO DATA FALLS BACK, AND SAYS SO: nothing is invented for a shape the record has not measured. A solo seat counts only
    /// on `SOLO_MIN_N` AUDITED instances; one run, an unaudited run, or no run leaves today's answer — a pair's hands.
    #[test]
    fn where_the_solo_cells_are_not_measured_enough_the_pair_answers_as_before_and_the_note_says_so() {
        let obj = crate::roles::Objective::Best;
        let pair = || put(3, obs("editor-l", "l", "backend", "fix", "p:brain", "w:hands", Some(0.95), 0.21, "bench"));
        // 1. no solo cell at all
        let _ = std::fs::remove_file(path());
        pair();
        let pick = best_cell("editor-l", "l", "backend", obj, 3).unwrap();
        assert!(!pick.solo && pick.worker == "w:hands" && pick.note.contains("no solo cell is measured here"), "{pick:?}");
        assert_eq!(best_for_role("editor-l", "l", "backend", obj, 3).map(|t| t.1), Some("w:hands".into()), "and it is exactly what the pair-only read answers");
        // 2. one audited run of a very cheap solo seat is an anecdote
        let _ = std::fs::remove_file(path());
        pair();
        put(1, obs("editor-l", "l", "backend", "fix", "raw", "s:alone", Some(1.0), 0.001, "bench"));
        let pick = best_cell("editor-l", "l", "backend", obj, 3).unwrap();
        assert!(!pick.solo && pick.note.contains("no solo cell is measured here"), "{pick:?}");
        // 3. many runs, none of them audited (a claim is not a measurement)
        let _ = std::fs::remove_file(path());
        pair();
        put(5, obs("editor-l", "l", "backend", "fix", "raw", "s:alone", None, 0.001, "bench"));
        let pick = best_cell("editor-l", "l", "backend", obj, 3).unwrap();
        assert!(!pick.solo && pick.note.contains("no solo cell is measured here"), "{pick:?}");
        // 4. solo cells only compete where the rule applies: not above L, not for a brain job
        assert!(solo_competes("editor-s", "s") && solo_competes("editor-l", "l"));
        assert!(!solo_competes("editor-l", "xxl") && !solo_competes("planner", "l") && !solo_competes("verifier", "s"));
        let _ = std::fs::remove_file(path());
        put(3, obs("editor-l", "xxl", "backend", "fix", "p:brain", "w:hands", Some(0.95), 0.21, "bench"));
        put(3, obs("editor-l", "xxl", "backend", "fix", "raw", "s:alone", Some(0.95), 0.01, "bench"));
        let pick = best_cell("editor-l", "xxl", "backend", obj, 3).unwrap();
        assert!(!pick.solo && pick.note.is_empty(), "a wave keeps its planner: {pick:?}");
        // 5. a pool made only of solo cells still answers (the baseline is measured, not invented)
        let _ = std::fs::remove_file(path());
        put(3, obs("editor-l", "l", "docs", "docs", "raw", "s:alone", Some(0.9), 0.01, "bench"));
        assert!(best_cell("editor-l", "l", "docs", obj, 3).unwrap().solo);
        assert!(best_for_role("editor-l", "l", "docs", obj, 3).is_none(), "…while the pair-only read has nothing to say");
    }

    /// A SWITCHED-OFF SEAT IS NOT A CANDIDATE (the Claude-off audit, `cli::model_blocked`). With Claude off the best pairing of a
    /// domain must be the best pairing that can be CALLED — and the bar the band is drawn from must be drawn among the seats
    /// that remain, not from a Claude row nobody can run.
    #[test]
    fn a_switched_off_seat_is_skipped_by_the_ladder_and_the_solo_cells_too() {
        let _ = std::fs::remove_file(path());
        let obj = crate::roles::Objective::Best;
        put(3, obs("editor-l", "l", "backend", "fix", "claude-code:sonnet", "cursor:composer-2.5", Some(1.0), 0.10, "bench"));
        put(3, obs("editor-l", "l", "backend", "fix", "codex:medium", "cursor:composer-2.5", Some(0.90), 0.15, "bench"));
        put(3, obs("editor-l", "l", "backend", "fix", "raw", "claude-code:haiku", Some(0.92), 0.01, "bench"));
        put(3, obs("editor-l", "l", "backend", "fix", "raw", "cursor:composer-2.5", Some(0.88), 0.02, "bench"));
        assert_eq!(best_cell("editor-l", "l", "backend", obj, 3).unwrap().worker, "claude-code:haiku", "Claude on: the cheapest solo seat inside the band");
        crate::cli::set_provider_flag("claude-code", false).expect("the switch is writable");
        let off = best_cell("editor-l", "l", "backend", obj, 3).unwrap();
        assert_eq!((off.planner.as_str(), off.worker.as_str()), ("raw", "cursor:composer-2.5"), "Claude off: its solo row and its pair are both skipped: {off:?}");
        let pair = best_for_role("editor-l", "l", "backend", obj, 3).unwrap();
        assert!(!pair.0.starts_with("claude-code") && !pair.1.starts_with("claude-code"), "the pair-only ladder skips it too: {pair:?}");
        assert_eq!(pair.0, "codex:medium");
        assert!(best("editor-l", "l", 1).unwrap()["planner"] != "claude-code:sonnet", "and so does the pooled read");
        crate::cli::set_provider_flag("claude-code", true).expect("and back on");
        assert_eq!(best_for_role("editor-l", "l", "backend", obj, 3).unwrap().0, "claude-code:sonnet");
        // with EVERY candidate blocked there is no answer: the declared binding stands, nothing is invented
        let _ = std::fs::remove_file(path());
        put(3, obs("editor-l", "l", "backend", "fix", "claude-code:sonnet", "claude-code:haiku", Some(1.0), 0.10, "bench"));
        crate::cli::set_provider_flag("claude-code", false).unwrap();
        assert!(best_cell("editor-l", "l", "backend", obj, 1).is_none());
        crate::cli::set_provider_flag("claude-code", true).unwrap();
    }

    /// A BRAIN JOB READS ONLY ITS OWN SEAT (review 2026-10-01). A test-writer or planner row's `worker` is the editing hands it was benched with
    /// (every test-writer/l row in the live record has `cursor:composer-2.5`) - a seat the brain job never calls. With Cursor switched off the
    /// Sonnet test-writer vanished from the ladder; an EDITING pair, which runs as a unit, still ends when either side is off.
    #[test]
    fn a_brain_job_is_not_dropped_for_the_hands_it_was_benched_with() {
        let _ = std::fs::remove_file(path());
        put(3, obs("test-writer", "l", "backend", "swt", "claude-code:sonnet", "cursor:composer-2.5", Some(0.90), 0.10, "bench"));
        put(3, obs("test-writer", "l", "backend", "swt", "codex:medium", "cursor:composer-2.5", Some(0.85), 0.04, "bench"));
        put(3, obs("planner", "l", "backend", "plan", "claude-code:sonnet", "cursor:composer-2.5", Some(0.90), 0.10, "bench"));
        put(3, obs("editor-l", "l", "backend", "fix", "claude-code:sonnet", "cursor:composer-2.5", Some(0.95), 0.20, "bench"));
        put(3, obs("editor-l", "l", "backend", "fix", "codex:medium", "gemini:gemini-3.8-flash-low", Some(0.90), 0.25, "bench"));
        crate::cli::set_provider_flag("cursor", false).expect("the switch is writable");
        let tw = ranked_seats("test-writer", "l", 1);
        let pl = ranked_seats("planner", "l", 1);
        let ed = ranked_seats("editor-l", "l", 1);
        crate::cli::set_provider_flag("cursor", true).expect("and back on");
        assert_eq!(tw, vec!["codex:medium".to_string(), "claude-code:sonnet".to_string()], "both test-writers stay: only the planner side is called: {tw:?}");
        assert_eq!(pl, vec!["claude-code:sonnet".to_string()], "{pl:?}");
        assert_eq!(ed, vec!["codex:medium".to_string()], "the editing pair that needs Cursor's hands is gone, the other stays: {ed:?}");
        // …and a brain seat that is itself switched off is still skipped
        crate::cli::set_provider_flag("codex", false).expect("the switch is writable");
        let tw = ranked_seats("test-writer", "l", 1);
        crate::cli::set_provider_flag("codex", true).expect("and back on");
        assert_eq!(tw, vec!["claude-code:sonnet".to_string()], "{tw:?}");
    }

    /// A SOLO CELL IS BELIEVED ON MEASURED INSTANCES, NOT ON SAMPLES (review 2026-10-01). A watched solo run is filed as `raw` too and an audit
    /// lands on only some of them: ten samples with ONE audit passed `n >= SOLO_MIN_N`, and that single audit then set the cell's quality.
    #[test]
    fn a_solo_row_with_one_audit_among_many_samples_does_not_compete() {
        let obj = crate::roles::Objective::Best;
        let _ = std::fs::remove_file(path());
        put(3, obs("editor-l", "l", "backend", "fix", "p:brain", "w:hands", Some(0.95), 0.21, "bench"));
        put(4, obs("editor-l", "l", "backend", "fix", "raw", "s:alone", None, 0.01, "bench"));
        put(1, obs("editor-l", "l", "backend", "fix", "raw", "s:alone", Some(1.0), 0.01, "bench"));
        let pick = best_cell("editor-l", "l", "backend", obj, 3).unwrap();
        assert!(!pick.solo && pick.worker == "w:hands", "five samples, one audit: an anecdote, not a cell: {pick:?}");
        assert!(pick.note.contains("no solo cell is measured here"), "{}", pick.note);
        // a second audit makes it a cell
        put(1, obs("editor-l", "l", "backend", "fix", "raw", "s:alone", Some(1.0), 0.01, "bench"));
        let pick = best_cell("editor-l", "l", "backend", obj, 3).unwrap();
        assert!(pick.solo, "two audited instances are enough: {pick:?}");
        // the screen's winner agrees with the router's
        let _ = std::fs::remove_file(path());
        put(3, obs("editor-l", "l", "backend", "fix", "p:brain", "w:hands", Some(0.95), 0.21, "bench"));
        put(4, obs("editor-l", "l", "backend", "fix", "raw", "s:alone", None, 0.01, "bench"));
        put(1, obs("editor-l", "l", "backend", "fix", "raw", "s:alone", Some(1.0), 0.01, "bench"));
        let cell: Vec<Value> = table().unwrap().into_iter().filter(|r| r["job"] == "editor-l" && r["size"] == "l").collect();
        assert_eq!(rule_winner(&cell).unwrap()["planner"], "p:brain");
    }

    /// THE POOLED, DOMAIN-BLIND READ FOLLOWS THE RULE (HANDOFF §6.1, the open audit findings). A row's `weight` is only comparable
    /// inside ITS pool (job × size × domain × stack × suite), and shrinks toward 0.5 with few samples — so a pairing that is the
    /// cheapest of a suite where everything scored 0.76 carried a higher weight than a pairing that was the cheapest of a suite where
    /// everything scored 0.97 but had been run once. `best()` took the highest weight and named the 0.76 pairing; the band, drawn
    /// across the pairings, says it is outside 80 % of the best measured quality.
    #[test]
    fn the_pooled_read_names_the_cheapest_inside_the_band_not_the_highest_weight_of_a_small_pool() {
        let _ = std::fs::remove_file(path());
        // suite a: a mediocre suite, three runs each
        put(3, obs("editor-l", "l", "backend", "a", "p:one", "w:x", Some(0.76), 0.14, "bench"));
        put(3, obs("editor-l", "l", "backend", "a", "p:two", "w:x", Some(0.74), 0.30, "bench"));
        // suite b: a strong suite, one run each — three pairings at 0.97 with prices 0.12 / 0.06 / 0.03
        put(1, obs("editor-l", "l", "backend", "b", "p:dear", "w:x", Some(0.97), 0.12, "bench"));
        put(1, obs("editor-l", "l", "backend", "b", "p:mid", "w:x", Some(0.97), 0.06, "bench"));
        put(1, obs("editor-l", "l", "backend", "b", "p:cheap", "w:x", Some(0.97), 0.03, "bench"));
        let rows = table().unwrap();
        let w = |p: &str| rows.iter().find(|r| r["planner"] == p).unwrap()["weight"].as_f64().unwrap();
        assert!(w("p:one") > w("p:cheap"), "the trap: the 0.76 pairing out-WEIGHS the cheapest 0.97 one ({} vs {})", w("p:one"), w("p:cheap"));
        let b = best("editor-l", "l", 1).expect("a winner");
        assert_eq!(b["planner"], "p:cheap", "the cheapest inside 80 % of the best measured quality (0.97), not the highest weight: {b}");
        // the screen and the audit read the same winner the router does
        let cell: Vec<Value> = rows.into_iter().filter(|r| r["job"] == "editor-l" && r["size"] == "l").collect();
        assert_eq!(rule_winner(&cell).unwrap()["planner"], "p:cheap");
        let p = table_payload(cell, "", None);
        assert_eq!(p["best"]["editor-l/l"]["planner"], "p:cheap", "`best` in the payload is the router's winner");
        assert!(p["why"]["editor-l/l"].as_str().unwrap().starts_with("best measured quality AND the cheapest"), "the explanation reads the router's winner first: {}", p["why"]);
    }

    /// THE SCREEN'S WINNER IS THE ROUTER'S ALSO WHERE THE ROUTER IS DIFFERENT: the solo cells compete in the small editing cells, the gate is
    /// ranked on quality alone, and the row that stands for a pairing is its BENCHED one (the audit reads one row: a watched run with
    /// a quality of 0.75 must not stand for a pairing whose bench says 0.97).
    #[test]
    fn the_tables_winner_is_never_a_seat_that_is_switched_off() {
        let _ = std::fs::remove_file(path());
        put(3, obs("editor-l", "l", "backend", "fix", "claude-code:sonnet", "cursor:composer-2.5", Some(0.97), 0.05, "bench"));
        put(3, obs("editor-l", "l", "backend", "fix", "cursor:cursor-grok-4.6-high", "cursor:composer-2.5", Some(0.90), 0.06, "bench"));
        let cell = || -> Vec<Value> { table().unwrap().into_iter().filter(|r| r["job"] == "editor-l" && r["size"] == "l").collect() };
        assert_eq!(rule_winner(&cell()).unwrap()["planner"], "claude-code:sonnet", "Claude on: the better pairing");
        crate::cli::set_provider_flag("claude-code", false).unwrap();
        let off = rule_winner(&cell()).map(|w| w["planner"].clone());
        crate::cli::set_provider_flag("claude-code", true).unwrap();
        assert_eq!(off, Some(json!("cursor:cursor-grok-4.6-high")), "Claude off: the screen names a pairing the router can take");
        let _ = std::fs::remove_file(path());
    }

    #[test]
    #[test]
    fn the_tables_winner_includes_the_solo_cells_ranks_the_gate_on_quality_and_shows_the_benched_row() {
        let _ = std::fs::remove_file(path());
        put(3, obs("editor-l", "l", "backend", "fix", "p:brain", "w:hands", Some(0.95), 0.21, "bench"));
        put(3, obs("editor-l", "l", "backend", "fix", "raw", "s:alone", Some(0.90), 0.02, "bench"));
        put(12, { let mut o = obs("editor-l", "l", "backend", "", "p:brain", "w:hands", None, 0.28, "run"); o.gate_met = 0; o.gate_unmet = 1; o.pass_first = 0; o.retries = 1; o });
        let cell = |job: &str, size: &str| -> Vec<Value> { table().unwrap().into_iter().filter(|r| r["job"] == job && r["size"] == size).collect() };
        let w = rule_winner(&cell("editor-l", "l")).unwrap();
        assert_eq!((w["planner"].as_str(), w["worker"].as_str()), (Some("raw"), Some("s:alone")), "the solo seat wins the small editing cell, as for the router");
        // above L the same rows are not offered a solo contest
        let _ = std::fs::remove_file(path());
        put(3, obs("editor-l", "xxl", "backend", "fix", "p:brain", "w:hands", Some(0.95), 0.21, "bench"));
        put(3, obs("editor-l", "xxl", "backend", "fix", "raw", "s:alone", Some(0.95), 0.01, "bench"));
        assert_eq!(rule_winner(&cell("editor-l", "xxl")).unwrap()["planner"], "p:brain");
        // the benched row stands for its pairing, not the more-sampled watched one
        let _ = std::fs::remove_file(path());
        put(3, obs("editor-l", "l", "backend", "fix", "p:brain", "w:hands", Some(0.97), 0.05, "bench"));
        put(12, { let mut o = obs("editor-l", "l", "backend", "", "p:brain", "w:hands", Some(0.70), 0.28, "run+audit"); o.qa = "q:judge".into(); o });
        let w = rule_winner(&cell("editor-l", "l")).unwrap();
        assert_eq!(w["source"], "bench", "{w}");
        // THE GATE IS NOT A PRICE DECISION: the cheaper seat inside the band loses to the better one
        let _ = std::fs::remove_file(path());
        put(3, obs("qa-gate", "call", "any", "gate", "g:better", "-", Some(0.96), 0.09, "bench"));
        put(3, obs("qa-gate", "call", "any", "gate", "g:cheaper", "-", Some(0.88), 0.01, "bench"));
        assert_eq!(rule_winner(&cell("qa-gate", "call")).unwrap()["planner"], "g:better");
        put(3, obs("verifier", "call", "any", "judges", "v:better", "-", Some(0.96), 0.09, "bench"));
        put(3, obs("verifier", "call", "any", "judges", "v:cheaper", "-", Some(0.88), 0.01, "bench"));
        assert_eq!(rule_winner(&cell("verifier", "call")).unwrap()["planner"], "v:cheaper", "…while any other job is a price decision inside the band");
    }

    /// THE SAME FOR A JOB'S OWN SEATS (screening reads `ranked_seats`): ordered by the band, then price, then time, pooled over the
    /// suites — not by the weight each seat earned inside its own suite.
    #[test]
    fn the_seats_of_a_job_are_ordered_by_the_rule_across_suites() {
        let _ = std::fs::remove_file(path());
        // seat A is alone in its suite (so its weight is the best a row can have) and 80× dearer than the other two, which share one
        put(3, obs("screening", "call", "any", "x", "seat:dear", "-", Some(0.90), 0.50, "bench"));
        put(3, obs("screening", "call", "any", "y", "seat:cheap", "-", Some(0.90), 0.002, "bench"));
        put(3, obs("screening", "call", "any", "y", "seat:mid", "-", Some(0.90), 0.006, "bench"));
        let rows = table().unwrap();
        let w = |p: &str| rows.iter().find(|r| r["planner"] == p).unwrap()["weight"].as_f64().unwrap();
        assert!(w("seat:dear") > w("seat:mid"), "the trap: the dearest seat out-weighs a cheaper one ({} vs {})", w("seat:dear"), w("seat:mid"));
        assert_eq!(ranked_seats("screening", "call", 1), vec!["seat:cheap", "seat:mid", "seat:dear"]);
    }

    /// MEASURED QUALITY OUTRANKS CLAIMED, and a claim cannot set the bar. A pairing whose only evidence is "no task errored"
    /// scores a perfect first-pass ratio for free; ranking it beside audited pairings let it be the cheapest "perfect" one.
    #[test]
    fn a_claimed_pairing_cannot_win_over_a_measured_one_by_being_cheap() {
        let _ = std::fs::remove_file(path());
        put(3, obs("editor-l", "l", "backend", "fix", "m:audited", "w:x", Some(0.90), 0.30, "bench"));
        put(3, obs("editor-l", "l", "backend", "fix", "c:claimed", "w:x", None, 0.01, "bench"));   // no audit, no gate: claimed
        let (p, _, _) = best_for_role("editor-l", "l", "backend", crate::roles::Objective::Best, 3).unwrap();
        assert_eq!(p, "m:audited");
        assert_eq!(best("editor-l", "l", 1).unwrap()["planner"], "m:audited");
        // with nothing measured anywhere the claim is still evidence, and still answers
        let _ = std::fs::remove_file(path());
        put(3, obs("editor-l", "l", "backend", "fix", "c:claimed", "w:x", None, 0.01, "bench"));
        assert_eq!(best_for_role("editor-l", "l", "backend", crate::roles::Objective::Best, 3).unwrap().0, "c:claimed");
    }

    /// CONTROLLED OUTRANKS OBSERVED INSIDE THE LADDER TOO: a pairing that was only ever watched does not compete with benched ones
    /// (observation moves a benched score, it does not set one), and answers alone when there is no bench.
    #[test]
    fn a_pairing_that_was_only_watched_does_not_out_rank_a_benched_one_in_the_ladder() {
        let _ = std::fs::remove_file(path());
        put(3, obs("editor-l", "l", "backend", "fix", "b:benched", "w:x", Some(0.90), 0.30, "bench"));
        put(3, obs("editor-l", "l", "backend", "", "u:watched", "w:x", Some(1.00), 0.01, "run+audit"));
        assert_eq!(best_for_role("editor-l", "l", "backend", crate::roles::Objective::Best, 3).unwrap().0, "b:benched");
        let _ = std::fs::remove_file(path());
        put(3, obs("editor-l", "l", "backend", "", "u:watched", "w:x", Some(1.00), 0.01, "run+audit"));
        assert_eq!(best_for_role("editor-l", "l", "backend", crate::roles::Objective::Best, 3).unwrap().0, "u:watched");
    }
    // ── PLAN-THE-DESIGN S4 (2026-10-01): THE ROUTER READS HOW A CELL LOOKS ────────────────────────────────────────────
    /// a candidate as `pool_seen` / `table()` carry it: behaviour (`audit`), price, and - when a judge compared its pages - `look` / `look_n`
    fn cand(worker: &str, audit: f64, usd: f64, look: Option<(f64, i64)>) -> Value {
        json!({ "planner": "raw", "worker": worker, "n": 3, "pass_first": 3, "retries": 0, "failed": 0, "audit": audit, "s": 100.0, "usd": usd, "basis": "bench", "measured": true,
                "look": look.map(|l| l.0), "look_n": look.map(|l| l.1).unwrap_or(0) })
    }
    fn winner_of(rows: &[Value], looks: bool) -> String { rank_with(rows, crate::roles::Objective::Best, looks)[0].0.clone() }

    /// A CELL IS ELIGIBLE ONLY IF IT KEEPS 80 % OF THE BEST ON BOTH AXES, THEN PRICE (plan D6): the cheapest row of the behaviour band loses when it is out of the looks band.
    #[test]
    fn a_cell_must_keep_eighty_percent_of_the_best_on_both_axes_then_price_decides() {
        let rows = [cand("s:plain", 0.90, 0.04, Some((0.45, 200))),   // cheapest, behaviour inside the band, looks 0.45 < 0.8 x 0.70, on 200 matches: clearly worse
                    cand("s:fancy", 0.95, 0.12, Some((0.70, 200))),   // best on both
                    cand("s:mid",   0.92, 0.10, Some((0.60, 200)))];  // 0.60 >= 0.56: inside both bands, and cheaper than the best-looking
        assert!(winner_of(&rows, false).contains("s:plain"), "looks ignored: the cheapest row of the behaviour band wins: {:?}", rank_with(&rows, crate::roles::Objective::Best, false));
        let r = rank_with(&rows, crate::roles::Objective::Best, true);
        assert!(r[0].0.contains("s:mid"), "looks counted: the cheapest row inside BOTH bands wins: {r:?}");
        assert!(r[1].0.contains("s:fancy") && r[2].0.contains("s:plain"), "…and the row outside the looks band ranks last, however cheap: {r:?}");
        // exactly 80 % of the best look is on the edge, and inside it (the same edge as the behaviour band)
        let edge = [cand("s:a", 0.9, 0.05, Some((0.56, 10_000))), cand("s:b", 0.9, 0.20, Some((0.70, 10_000)))];
        assert!(winner_of(&edge, true).contains("s:a"), "0.56 is exactly 80 % of 0.70: {:?}", rank_with(&edge, crate::roles::Objective::Best, true));
        let past = [cand("s:a", 0.9, 0.05, Some((0.50, 10_000))), cand("s:b", 0.9, 0.20, Some((0.70, 10_000)))];
        assert!(winner_of(&past, true).contains("s:b"), "0.50 is past it");
    }

    /// ONE AXIS IS NOT ENOUGH: the best look does not rescue a behaviour failure, and the best behaviour does not rescue a look failure.
    #[test]
    fn the_best_look_does_not_rescue_a_behaviour_failure_nor_the_reverse() {
        let pretty_but_broken = cand("s:pretty", 0.50, 0.02, Some((0.90, 12)));   // behaviour 0.50 < 0.8 x 0.95
        let solid = cand("s:solid", 0.95, 0.20, Some((0.60, 12)));                 // 0.60 >= 0.8 x 0.90
        let r = rank_with(&[pretty_but_broken.clone(), solid.clone()], crate::roles::Objective::Best, true);
        assert!(r[0].0.contains("s:solid"), "the best-looking row is out on behaviour: {r:?}");
        let plain_but_right = cand("s:plain", 1.00, 0.02, Some((0.30, 200)));       // behaviour best, looks 0.30 < 0.8 x 0.90
        let r = rank_with(&[plain_but_right, pretty_but_broken.clone(), solid.clone()], crate::roles::Objective::Best, true);
        assert!(r[0].0.contains("s:solid"), "the best behaviour is out on looks - only the row inside both bands is eligible: {r:?}");
        // `Smarter` gives up nothing on either axis
        let r = rank_with(&[solid, cand("s:other", 0.99, 0.01, Some((0.89, 12)))], crate::roles::Objective::Smarter, true);
        assert!(r[0].0.contains("s:other"), "Smarter keeps no band: the best on both axes: {r:?}");
    }

    /// NO DATA IS NOT A BAD LOOK: a cell nobody compared stays eligible on its behaviour, is never ranked up for its looks, and with no looks anywhere the ranking is `rank`'s, exactly.
    #[test]
    fn a_cell_with_no_looks_is_not_penalised_and_never_preferred_on_looks() {
        let unseen_cheap = cand("s:unseen", 0.90, 0.03, None);
        let seen_good = cand("s:seen", 0.95, 0.12, Some((0.70, 12)));
        let seen_bad = cand("s:bad", 0.95, 0.04, Some((0.30, 200)));
        let rows = [seen_good.clone(), seen_bad.clone(), unseen_cheap.clone()];
        let r = rank_with(&rows, crate::roles::Objective::Best, true);
        assert!(r[0].0.contains("s:unseen"), "not penalised: the cheap unmeasured row still wins on price: {r:?}");
        assert!(r[1].0.contains("s:seen") && r[2].0.contains("s:bad"), "a measured bad look is out; the measured good look keeps its place: {r:?}");
        // never preferred: giving the unmeasured row the SAME price as the good look leaves them tied on score - a look is never a bonus
        let tie = [cand("s:unseen", 0.95, 0.12, None), seen_good.clone()];
        let (a, b) = (rank_with(&tie, crate::roles::Objective::Best, true), rank_with(&tie, crate::roles::Objective::Best, false));
        assert_eq!(a, b, "looks add no points to anyone: the scores are identical with and without them when nobody falls out of a band");
        // a look on fewer than LOOK_MIN_N matches is an anecdote and gates nothing
        let thin = [cand("s:thin", 0.90, 0.02, Some((0.02, LOOK_MIN_N - 1))), seen_good.clone()];
        assert!(winner_of(&thin, true).contains("s:thin"), "three matches do not close a gate");
        let enough = [cand("s:thin", 0.90, 0.02, Some((0.02, LOOK_MIN_N))), seen_good];
        assert!(winner_of(&enough, true).contains("s:seen"), "four do");
        // no looks anywhere: rank_with(true) IS rank
        let plain = [cand("s:a", 0.9, 0.05, None), cand("s:b", 0.8, 0.01, None), cand("s:c", 1.0, 0.5, None)];
        for o in crate::roles::Objective::ALL { assert_eq!(rank_with(&plain, o, true), rank(&plain, o), "{o:?}"); }
    }

    /// THE BAND ORDER, EXPLICITLY (S4 changed it): every row inside both bands ranks above every row outside either, however thin the in-band row's evidence - the `n / (n + 1)` shrink alone used to be able to put a dear
    /// one-sample row below a cheap out-of-band one.
    #[test]
    fn a_thin_row_inside_the_bands_still_ranks_above_a_well_measured_row_outside_them() {
        let thin_in = json!({ "planner": "raw", "worker": "s:in", "n": 1, "pass_first": 1, "retries": 0, "failed": 0, "audit": 0.90, "s": 100.0, "usd": 0.50, "basis": "bench", "measured": true });
        let solid_out = json!({ "planner": "raw", "worker": "s:out", "n": 10, "pass_first": 10, "retries": 0, "failed": 0, "audit": 0.40, "s": 100.0, "usd": 0.01, "basis": "bench", "measured": true });
        let best = json!({ "planner": "raw", "worker": "s:best", "n": 10, "pass_first": 10, "retries": 0, "failed": 0, "audit": 0.95, "s": 100.0, "usd": 0.60, "basis": "bench", "measured": true });
        let r = rank(&[solid_out, thin_in, best], crate::roles::Objective::Best);
        assert_eq!(r.last().unwrap().0, "raw · s:out", "outside the behaviour band ranks last: {r:?}");
    }

    /// A SPLIT THAT IS NOISE TAKES NO PLACE AWAY: two seats judged 8-4 look 0.59 and 0.41 by the fit (70 % of the best, under the 80 % bar), but twelve matches cannot tell the pages apart. Only a clear gap ejects.
    #[test]
    fn a_modest_split_on_few_matches_does_not_eject_the_loser_but_a_clear_one_does() {
        let fit = |wins: usize, losses: usize| crate::looks::rank(&[vec![crate::looks::Match { x: "w".into(), y: "l".into(), score: 1.0 }; wins], vec![crate::looks::Match { x: "w".into(), y: "l".into(), score: 0.0 }; losses]].concat(), 50, 3);
        let look = |r: &crate::looks::Ranking, c: &str| r.cells.iter().find(|l| l.cell == c).unwrap().looks;
        let rows = |r: &crate::looks::Ranking, n: i64| [cand("s:cheap", 0.9, 0.02, Some((look(r, "l"), n))), cand("s:good", 0.9, 0.10, Some((look(r, "w"), n)))];
        let noisy = fit(8, 4);
        assert!(look(&noisy, "l") < 0.8 * look(&noisy, "w"), "the point estimates alone would eject: {} vs {}", look(&noisy, "l"), look(&noisy, "w"));
        assert!(winner_of(&rows(&noisy, 12), true).contains("s:cheap"), "8-4 is inside the noise of 12 matches: the cheap row keeps its place");
        let clear = fit(80, 8);
        assert!(winner_of(&rows(&clear, 88), true).contains("s:good"), "80-8 is a clear gap: the cheap row is out");
        assert!(look_upper(0.5, 12) > 0.5 && look_upper(0.5, 10_000) < 0.52 && look_upper(0.3, 0) == 1.0);
    }

    /// THE HANDS ARE THE SAME WITH OR WITHOUT A PLANNER: a seat whose pages look clearly worse is out as a solo row AND as the worker behind a planner, so a pairing is no way round the gate.
    #[test]
    fn a_bad_looking_seat_is_out_behind_a_planner_too() {
        let pair = |planner: &str, worker: &str, audit: f64, usd: f64, look: (f64, i64)| json!({ "planner": planner, "worker": worker, "n": 3, "pass_first": 3, "retries": 0, "failed": 0, "audit": audit, "s": 100.0, "usd": usd, "basis": "bench",
            "measured": true, "look": look.0, "look_n": look.1 });
        let rows = [cand("s:bad", 0.95, 0.01, Some((0.30, 200))), pair("p:brain", "s:bad", 0.95, 0.02, (0.30, 200)), cand("s:good", 0.95, 0.20, Some((0.70, 200)))];
        let r = rank_with(&rows, crate::roles::Objective::Best, true);
        assert!(r[0].0.contains("s:good"), "both rows of the bad-looking seat are out, the cheap pairing included: {r:?}");
    }

    #[test]
    fn two_readings_of_one_pairing_pool_by_matches_and_never_sum_them() {
        assert_eq!(pool_look(None, None), None);
        assert_eq!(pool_look(Some((0.6, 12)), None), Some((0.6, 12)));
        assert_eq!(pool_look(None, Some((0.6, 12))), Some((0.6, 12)));
        let (l, n) = pool_look(Some((0.6, 12)), Some((0.6, 12))).unwrap();
        assert!((l - 0.6).abs() < 1e-12 && n == 12, "the same ranking written to two rows is ONE reading, not 24 matches: {n}");
        let (l, n) = pool_look(Some((0.8, 30)), Some((0.2, 10))).unwrap();
        assert!((l - 0.65).abs() < 1e-12 && n == 30, "{l} {n}");
    }

    fn calibration_file(tag: &str, body: Value) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("antiloki-cal-{}-{tag}", std::process::id())); std::fs::create_dir_all(&d).unwrap();
        let p = d.join("design-calibration.json"); std::fs::write(&p, body.to_string()).unwrap(); p
    }
    fn cal_json(agree: u32, n: u32) -> Value { json!({ "schema": 1, "operator": true, "judge": "j", "judge_version": crate::looks::JUDGE_VERSION, "n": n, "agree": agree }) }
    fn standing_from(p: &std::path::Path) -> crate::looks::Standing { crate::looks::standing_with(crate::looks::load_calibration_at(p).as_ref(), crate::looks::JUDGE_VERSION) }
    fn set_look(worker: &str, looks: f64, n: i64) {
        open().unwrap().execute("UPDATE merge_scores SET look_sum = ?1, look_n = ?2 WHERE worker = ?3", params![looks * n as f64, n, worker]).unwrap();
    }
    fn design_pool() {
        let _ = std::fs::remove_file(path());
        put(3, obs("editor-l", "l", "design", "design", "raw", "s:plain", Some(0.90), 0.04, "bench"));
        put(3, obs("editor-l", "l", "design", "design", "raw", "s:fancy", Some(0.95), 0.12, "bench"));
        put(3, obs("editor-l", "l", "design", "design", "raw", "s:mid", Some(0.92), 0.10, "bench"));
        set_look("s:plain", 0.45, 200); set_look("s:fancy", 0.70, 200); set_look("s:mid", 0.60, 200);
    }
    fn pick(standing: crate::looks::Standing) -> Pick { pick_in_with("editor-l", "l", "design", "*", crate::roles::Objective::Best, 1, false, true, standing).expect("a winner") }

    /// THE JUDGE DECIDES NOTHING UNTIL THE OPERATOR HAS SAID IT AGREES (plan D3 -> D6): flipping the calibration file changes the winner ONLY in the allowed case.
    #[test]
    fn flipping_the_calibration_file_changes_the_winner_only_where_looks_are_allowed() {
        design_pool();
        let none = std::env::temp_dir().join(format!("antiloki-cal-{}-absent", std::process::id())).join("design-calibration.json");
        let behaviour_only = pick(crate::looks::Standing::Advisory);
        assert_eq!(behaviour_only.worker, "s:plain", "the cheapest row of the behaviour band: {behaviour_only:?}");
        let cases: Vec<(&str, std::path::PathBuf, bool)> = vec![
            ("no file at all", none, false),
            ("18 of 24 = 75 %: advisory", calibration_file("75", cal_json(18, 24)), false),
            ("10 of 24 = 42 %: not trusted", calibration_file("42", cal_json(10, 24)), false),
            ("20 of 24 = 83 %: routes", calibration_file("83", cal_json(20, 24)), true),
            ("exactly 80 % (16 of 20): routes", calibration_file("80", cal_json(16, 20)), true),
            ("100 % of ten pairs: too few labels", calibration_file("few", cal_json(10, 10)), false),
            ("measured for another judge prompt", calibration_file("ver", json!({ "schema": 1, "operator": true, "judge_version": "design.judge/0", "n": 30, "agree": 30 })), false),
            ("not the operator's labels", calibration_file("op", json!({ "schema": 1, "operator": false, "judge_version": crate::looks::JUDGE_VERSION, "n": 30, "agree": 30 })), false),
            ("a file we cannot read", calibration_file("junk", json!("agree")), false),
        ];
        for (what, file, allowed) in cases {
            let st = standing_from(&file);
            assert_eq!(st == crate::looks::Standing::Routes, allowed, "{what}: {st:?}");
            let p = pick(st);
            if allowed { assert_eq!(p.worker, "s:mid", "{what}: looks routed - the cheapest row inside both bands: {p:?}"); }
            else { assert_eq!(p.worker, behaviour_only.worker, "{what}: the winner must not move: {p:?}"); }
            // the looks are ALWAYS in the sentence; only the routed case drops the advisory mark
            assert!(p.note.contains("looks 0.60 vs 0.70") || p.note.contains("looks 0.45 vs 0.70"), "{what}: {}", p.note);
            assert_eq!(p.note.contains("advisory"), !allowed, "{what}: {}", p.note);
        }
    }

    /// THE DOOR THE ROUTER REALLY USES: `best_cell` (what `route_role_in` and the run read) carries the looks in its note, and with no calibration file on this machine
    /// they are advisory - the winner is the behaviour-and-price one.
    #[test]
    fn the_router_door_names_the_looks_as_advisory_where_no_calibration_exists() {
        design_pool();
        let p = best_cell("editor-l", "l", "design", crate::roles::Objective::Best, 1).expect("a winner");
        assert_eq!(p.worker, "s:plain", "{p:?}");
        assert!(p.note.contains("(advisory - not yet calibrated to your taste)") && p.note.contains("looks 0.45 vs 0.70 for s:fancy"), "{}", p.note);
        // the frontend pool is the design pool's neighbour: asked for frontend work, the ladder reaches the design rows and says the same
        let f = best_cell("editor-l", "l", "frontend", crate::roles::Objective::Best, 1).expect("a winner by the ladder");
        assert_eq!(f.worker, "s:plain"); assert!(f.note.contains("advisory"), "{}", f.note);
    }

    /// THE SENTENCE, IN WORDS (operator wording: "looks 0.71 vs 0.48 (advisory - not yet calibrated to your taste), same behaviour, $0.04 dearer").
    #[test]
    fn the_route_sentence_names_the_looks_the_behaviour_and_the_bill_in_words() {
        use crate::looks::Standing::*;
        use crate::roles::Objective::Best;
        let rows = [cand("s:claude", 0.90, 0.16, Some((0.71, 12))), cand("s:composer", 0.90, 0.12, Some((0.48, 12)))];
        let adv = look_note(&rows, "raw", "s:claude", Best, Advisory);
        assert_eq!(adv, "s:claude: looks 0.71 vs 0.48 for s:composer (advisory - not yet calibrated to your taste), same behaviour, $0.04 dearer");
        let routed = look_note(&rows, "raw", "s:claude", Best, Routes);
        assert_eq!(routed, "s:claude: looks 0.71 vs 0.48 for s:composer, same behaviour, $0.04 dearer - kept inside the 80% looks band");
        assert!(look_note(&rows, "raw", "s:claude", Best, NotTrusted).contains("disagree too often"), "a judge the operator contradicted says so");
        // different behaviour and a cheaper winner read the other way round
        let rows = [cand("s:a", 0.85, 0.05, Some((0.62, 12))), cand("s:b", 0.95, 0.09, Some((0.70, 12)))];
        assert_eq!(look_note(&rows, "raw", "s:a", Best, Routes), "s:a: looks 0.62 vs 0.70 for s:b, behaviour 85% vs 95%, $0.04 cheaper - kept inside the 80% looks band");
        // a winner nobody compared says that, and that looks neither help nor hurt it
        let rows = [cand("s:a", 0.90, 0.05, None), cand("s:b", 0.90, 0.09, Some((0.70, 12)))];
        assert_eq!(look_note(&rows, "raw", "s:a", Best, Advisory), "s:a: no judge has compared its pages, while 1 cell is measured on looks (advisory - not yet calibrated to your taste) - so looks neither help nor hurt it");
        // nothing compared anywhere: no sentence at all (a non-design route reads exactly as before)
        assert_eq!(look_note(&[cand("s:a", 0.9, 0.05, None)], "raw", "s:a", Best, Advisory), "");
        assert_eq!(look_note(&[cand("s:a", 0.9, 0.05, Some((0.5, LOOK_MIN_N - 1)))], "raw", "s:a", Best, Advisory), "", "a thin look is not worth a sentence");
        // looks describe screens: not a planner job, not a backend pool
        assert!(looks_apply("editor-l", "design") && looks_apply("editor-s", "frontend"));
        assert!(!looks_apply("planner", "design") && !looks_apply("editor-l", "backend") && !looks_apply("editor-l", "any"));
    }

    /// A POOL WITHOUT LOOKS READS EXACTLY AS IT DID: no note, the same winner.
    #[test]
    fn a_pool_nobody_looked_at_routes_and_reads_as_before() {
        let _ = std::fs::remove_file(path());
        put(3, obs("editor-l", "l", "design", "design", "raw", "s:plain", Some(0.90), 0.04, "bench"));
        put(3, obs("editor-l", "l", "design", "design", "raw", "s:fancy", Some(0.95), 0.12, "bench"));
        for st in [crate::looks::Standing::Advisory, crate::looks::Standing::Routes, crate::looks::Standing::NotTrusted] {
            let p = pick(st);
            assert_eq!(p.worker, "s:plain"); assert!(!p.note.contains("looks"), "{}", p.note);
        }
        // and a backend pool carrying a stray look ignores it
        let _ = std::fs::remove_file(path());
        put(3, obs("editor-l", "l", "backend", "fix", "raw", "s:cheap", Some(0.90), 0.02, "bench"));
        put(3, obs("editor-l", "l", "backend", "fix", "raw", "s:dear", Some(0.95), 0.20, "bench"));
        set_look("s:cheap", 0.1, 12); set_look("s:dear", 0.9, 12);
        let p = pick_in_with("editor-l", "l", "backend", "*", crate::roles::Objective::Best, 1, false, true, crate::looks::Standing::Routes).unwrap();
        assert_eq!(p.worker, "s:cheap", "a backend cell is not judged on how it looks: {p:?}"); assert!(!p.note.contains("looks"));
    }

    /// THE SCREEN GETS THE LOOKS PER CELL AND THE FLAG THAT MAKES THEM HONEST; its winner is the router's.
    #[test]
    fn the_payload_carries_looks_per_cell_and_the_advisory_flag_and_its_winner_follows_the_router() {
        use crate::looks::Standing::*;
        design_pool();
        let rows = table().unwrap();
        let by = |w: &str| rows.iter().find(|r| r["worker"] == w).unwrap().clone();
        assert_eq!((by("s:fancy")["look"].as_f64(), by("s:fancy")["look_n"].as_i64()), (Some(0.7), Some(200)));
        let adv = table_payload_with(rows.clone(), "web", None, Advisory);
        assert_eq!(adv["looks"]["advisory"], true); assert_eq!(adv["looks"]["standing"], "advisory");
        assert!(adv["looks"]["text"].as_str().unwrap().contains("not yet calibrated"));
        assert_eq!(adv["best"]["editor-l/l"]["worker"], "s:plain", "advisory: the ranking is behaviour and price only");
        assert!(adv["look_why"]["editor-l/l"].as_str().unwrap().contains("advisory"), "{}", adv["look_why"]);
        let routed = table_payload_with(rows.clone(), "web", None, Routes);
        assert_eq!(routed["looks"]["advisory"], false);
        assert_eq!(routed["best"]["editor-l/l"]["worker"], "s:mid", "calibrated: the screen's winner is the router's");
        assert!(!routed["look_why"]["editor-l/l"].as_str().unwrap().contains("advisory"));
        // a cell with no looks has no sentence, and its rows say null rather than 0
        let _ = std::fs::remove_file(path());
        put(3, obs("editor-l", "l", "design", "design", "raw", "s:plain", Some(0.90), 0.04, "bench"));
        let p = table_payload_with(table().unwrap(), "web", None, Advisory);
        assert!(p["rows"][0]["look"].is_null() && p["rows"][0]["look_n"] == 0, "{}", p["rows"][0]);
        assert!(p["look_why"].as_object().unwrap().is_empty());
    }

    /// AN OLD FILE GETS THE COLUMNS AND KEEPS EVERY ROW; opening again is a no-op that keeps what was written.
    #[test]
    fn an_old_file_gets_the_look_columns_and_keeps_every_row() {
        let p = path(); let _ = std::fs::remove_file(&p);
        if let Some(d) = p.parent() { std::fs::create_dir_all(d).unwrap(); }
        {
            // the table as the build before this one wrote it: every column but the looks (the stack/suite rebuild must not drop what it does not know about, and cannot lose these)
            let c = Connection::open(&p).unwrap();
            c.execute_batch("CREATE TABLE merge_scores (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                job TEXT NOT NULL, size TEXT NOT NULL, domain TEXT NOT NULL DEFAULT 'any',
                planner TEXT NOT NULL, worker TEXT NOT NULL, qa TEXT NOT NULL DEFAULT '',
                n INTEGER NOT NULL DEFAULT 0, pass_first INTEGER NOT NULL DEFAULT 0, retries INTEGER NOT NULL DEFAULT 0,
                failed INTEGER NOT NULL DEFAULT 0, gate_met INTEGER NOT NULL DEFAULT 0, gate_unmet INTEGER NOT NULL DEFAULT 0,
                audit_sum REAL NOT NULL DEFAULT 0, audit_n INTEGER NOT NULL DEFAULT 0,
                ms_total INTEGER NOT NULL DEFAULT 0, usd_total REAL NOT NULL DEFAULT 0,
                weight REAL NOT NULL DEFAULT 0, source TEXT NOT NULL DEFAULT 'run', updated TEXT NOT NULL DEFAULT (datetime('now')),
                reasked INTEGER NOT NULL DEFAULT 0,
                UNIQUE (job, size, domain, planner, worker, qa));
                INSERT INTO merge_scores (job, size, domain, planner, worker, qa, n, pass_first, audit_sum, audit_n, ms_total, usd_total, weight, source, reasked)
                VALUES ('editor-l','l','design','raw','w:1','',7,6,6.3,7,700000,1.4,0.83,'bench',2);").unwrap();
        }
        let t = table().unwrap();
        assert_eq!(t.len(), 1); assert_eq!(t[0]["n"], 7); assert_eq!(t[0]["domain"], "design");
        assert!(t[0]["look"].is_null() && t[0]["look_n"] == 0, "an old row has no look: {}", t[0]);
        set_look("w:1", 0.66, 9);
        let t = table().unwrap();     // opened again: the migration is a no-op and the looks survive
        assert_eq!((t[0]["look"].as_f64(), t[0]["look_n"].as_i64()), (Some(0.66), Some(9)));
        assert_eq!(t[0]["weight"].as_f64().unwrap(), 0.83);
        let c = open().unwrap();
        let cols: Vec<String> = c.prepare("SELECT name FROM pragma_table_info('merge_scores')").unwrap().query_map([], |r| r.get(0)).unwrap().filter_map(Result::ok).collect();
        for want in ["look_sum", "look_n", "reasked", "stack", "suite", "legacy"] { assert!(cols.iter().any(|c| c == want), "{want} in {cols:?}"); }
    }

    /// THE BENCH -> ROUTER DOOR: the judged matches become each seat's looks on the rows it BUILDS of the screen domains, by REWRITING (twice is once), touching nothing else.
    #[test]
    fn the_import_writes_each_seats_looks_onto_every_row_it_builds_and_nothing_else_and_twice_is_once() {
        let _ = std::fs::remove_file(path());
        // behaviour rows: the two benched seats in design AND frontend, a pair row, a backend row, a brain-job row
        for d in ["design", "frontend"] {
            put(3, obs("editor-l", "l", d, "x", "raw", "claude-code:sonnet", Some(0.95), 0.12, "bench"));
            put(3, obs("editor-l", "l", d, "x", "raw", "cursor:composer-2.5", Some(0.90), 0.04, "bench"));
        }
        put(3, obs("editor-l", "l", "design", "x", "claude-code:sonnet", "cursor:composer-2.5", Some(0.92), 0.10, "bench"));
        put(3, obs("editor-l", "l", "backend", "x", "raw", "cursor:composer-2.5", Some(0.90), 0.04, "bench"));
        put(3, obs("planner", "l", "design", "x", "raw", "cursor:composer-2.5", Some(0.90), 0.04, "bench"));
        put(3, obs("editor-l", "l", "design", "x", "raw", "cursor:cursor-grok-4.6-medium", Some(0.9), 0.1, "bench"));   // a seat with a row but never compared
        let c = open_db().unwrap();
        // the bench spells a seat its own way; the runs say which engine:model it was
        let runs = vec![json!({ "seat": "claude-code", "engine": "claude-code", "model": "sonnet" }), json!({ "seat": "cursor:composer-2.5", "engine": "cursor", "model": "composer-2.5" }),
                        json!({ "seat": "cursor:grok-4.6-medium", "engine": "cursor", "model": "cursor-grok-4.6-medium" })];
        let km = keymap_from_runs(&runs);
        assert_eq!(km["claude-code"], "claude-code:sonnet"); assert_eq!(km["cursor:grok-4.6-medium"], "cursor:cursor-grok-4.6-medium");
        // eight asks: Claude beats Composer on six, Composer takes one, one tie
        for i in 0..8 {
            let w = if i < 6 { "claude-code" } else if i == 6 { "cursor:composer-2.5" } else { "tie" };
            let row = crate::looks::PairRow { domain: "design".into(), ask: format!("ask{i}"), x: "claude-code".into(), y: "cursor:composer-2.5".into(), winner: w.into(), consistent: true, order_ab: "".into(), order_ba: "".into(),
                judge: "j".into(), prompt_ver: crate::looks::JUDGE_VERSION.into(), img_hash: format!("h{i}"), per_axis: Value::Null, reason: String::new(), tokens: 0, usd: None, t: 0 };
            assert!(crate::looks::record_pair(&c, &row).unwrap());
        }
        // a SECOND domain's matches must not leak into this one
        let other = crate::looks::PairRow { domain: "other".into(), ask: "z".into(), x: "claude-code".into(), y: "cursor:composer-2.5".into(), winner: "cursor:composer-2.5".into(), consistent: true, order_ab: "".into(), order_ba: "".into(),
            judge: "j".into(), prompt_ver: crate::looks::JUDGE_VERSION.into(), img_hash: "hz".into(), per_axis: Value::Null, reason: String::new(), tokens: 0, usd: None, t: 0 };
        crate::looks::record_pair(&c, &other).unwrap();
        let imp = import_looks(&c, "design", &km, &["design", "frontend"], 50, 7, None).unwrap();
        assert_eq!(imp.matches, 8);
        let cell = |k: &str| imp.cells.iter().find(|c| c.key == k).unwrap().clone();
        assert!(cell("claude-code:sonnet").looks > cell("cursor:composer-2.5").looks, "{imp:?}");
        assert_eq!((cell("claude-code:sonnet").rows, cell("cursor:composer-2.5").rows), (2, 3), "one solo row per screen domain, and Composer's hands also behind Claude's planning in design");
        let snapshot = |c: &Connection| -> Vec<(String, String, String, String, f64, i64)> {
            c.prepare("SELECT job, domain, planner, worker, look_sum, look_n FROM merge_scores ORDER BY id").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))).unwrap().filter_map(Result::ok).collect()
        };
        let first = snapshot(&c);
        for (job, dom, pl, wk, ls, ln) in &first {
            let attached = job == "editor-l" && matches!(dom.as_str(), "design" | "frontend") && matches!(wk.as_str(), "claude-code:sonnet" | "cursor:composer-2.5");
            assert_eq!(*ln > 0, attached, "{job} {dom} {pl} {wk}: looks on every row of an editing job in a screen domain whose WORKER is a judged seat (a planner in front does not walk round the gate)");
            if attached { assert_eq!(*ln, 8); assert!(*ls > 0.0 && *ls < 8.0); }
        }
        let claude = first.iter().find(|r| r.3 == "claude-code:sonnet").unwrap();
        assert!((claude.4 / claude.5 as f64 - cell("claude-code:sonnet").looks).abs() < 1e-9, "look_sum / look_n IS the looks score");
        // twice is once, and the table says what the router reads
        import_looks(&c, "design", &km, &["design", "frontend"], 50, 7, None).unwrap();
        assert_eq!(snapshot(&c), first, "an import rewrites; it does not add");
        let t = table().unwrap();
        let tr = t.iter().find(|r| r["worker"] == "claude-code:sonnet" && r["domain"] == "design" && r["planner"] == "raw").unwrap();
        assert_eq!(tr["look_n"], 8); assert!(tr["look"].as_f64().unwrap() > 0.5);
        // a match removed from the record moves the ranking next time, and a seat that falls out of it loses its look (the fit is of ALL matches, not a running sum)
        c.execute("DELETE FROM look_pairs WHERE domain = 'design'", []).unwrap();
        let empty = import_looks(&c, "design", &km, &["design", "frontend"], 10, 7, None).unwrap();
        assert!(empty.cells.is_empty() && snapshot(&c).iter().all(|r| r.5 == 0), "nothing judged -> no look anywhere, not a stale one");
    }
}

/// EVERY CELL'S WEIGHT, RECOMPUTED FROM THE ROWS AS THEY STAND NOW. A weight is not a measurement — it is a
/// ranking derived from one, under whatever scoring and pricing was in force when the row was last touched. So when
/// the scoring changes (`Objective::weights`) or the PRICING changes, every stored weight is stale until its cell is
/// written to again, and a cell nobody records into keeps the old answer indefinitely.
///
/// 2026-09-27 is why this exists: the recorded `usd` dropped every task whose engine reports no cost, so all 40
/// frontier merge runs sat at $0.0000. That is not a wrong ranking — `reweigh_one` scores an unpriced row at the
/// neutral 0.5 rather than as the cheapest — it is worse: the cost term is 0.55 of the weight, and it carried NO
/// information for any merge row. Fixing the recording repaired new rows and left 25 already on the record.
pub fn reweigh_every_cell() -> Result<i64, String> {
    let c = open().map_err(|e| e.to_string())?;
    let cells: Vec<(String, String)> = {
        let mut st = c.prepare("SELECT DISTINCT job, size FROM merge_scores").map_err(|e| e.to_string())?;
        let it = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(|e| e.to_string())?;
        it.filter_map(Result::ok).collect()
    };
    let n = cells.len() as i64;
    for (job, size) in cells { reweigh_in(&c, &job, &size, "", "").map_err(|e| e.to_string())?; }
    Ok(n)
}

/// `POST /api/v1/settings/merges/reweigh` — recompute every cell's weight. Changes no measurement; only the ranking
/// derived from them. Safe to call at any time, and the right thing to call after pricing or scoring changes.
pub async fn http_reweigh() -> axum::response::Response {
    use axum::response::IntoResponse;
    match tokio::task::spawn_blocking(reweigh_every_cell).await.unwrap_or_else(|e| Err(e.to_string())) {
        Ok(cells) => axum::Json(json!({ "ok": true, "cells": cells, "note": "weights recomputed from the rows as they stand; no measurement was altered" })).into_response(),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, axum::Json(json!({ "ok": false, "error": e }))).into_response(),
    }
}

/// `GET /api/v1/settings/merges/legacy` — THE WITHDRAWN ARMS, for an audit only.
///
/// Every row here was measured on a metered path (OpenCode Zen, or a Cursor third-party model) and is excluded from the
/// operator's table and from routing. It is kept because deleting a measurement destroys evidence that cannot be
/// re-created: the `source='use'` rows in particular are observed production usage, not a bench that can simply be run
/// again. Each row carries WHY it was withdrawn, so the archive explains itself years later.
pub fn legacy_table() -> Result<Vec<Value>, String> {
    let c = open().map_err(|e| e.to_string())?;
    let mut st = c.prepare("SELECT job, size, planner, worker, domain, suite, n, audit_sum, audit_n, ms_total, usd_total, weight, source, updated \
                            FROM merge_scores WHERE legacy = 1 ORDER BY n DESC, job, size").map_err(|e| e.to_string())?;
    let rows = st.query_map([], |r| {
        let planner: String = r.get(2)?; let worker: String = r.get(3)?;
        let why = crate::cli::side_is_metered(&planner).or_else(|| crate::cli::side_is_metered(&worker))
            .unwrap_or_else(|| "withdrawn".into());
        let (asum, an): (f64, i64) = (r.get(7)?, r.get(8)?);
        Ok(json!({
            "job": r.get::<_, String>(0)?, "size": r.get::<_, String>(1)?,
            "planner": planner, "worker": worker,
            "domain": r.get::<_, String>(4)?, "suite": r.get::<_, String>(5)?,
            "n": r.get::<_, i64>(6)?,
            "quality": if an > 0 { json!((asum / an as f64 * 1000.0).round() / 1000.0) } else { Value::Null },
            "audits": an,
            "ms_total": r.get::<_, i64>(9)?, "usd_total": r.get::<_, f64>(10)?,
            "weight": r.get::<_, f64>(11)?, "source": r.get::<_, String>(12)?,
            "updated": r.get::<_, String>(13)?,
            "withdrawn_because": why,
        }))
    }).map_err(|e| e.to_string())?.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| e.to_string())?;
    Ok(rows)
}

/// `GET /api/v1/settings/merges/legacy` — the archive, with its own count so a caller need not sum it.
pub async fn http_legacy() -> axum::response::Response {
    use axum::response::IntoResponse;
    match tokio::task::spawn_blocking(legacy_table).await.unwrap_or_else(|e| Err(e.to_string())) {
        Ok(rows) => axum::Json(json!({
            "ok": true,
            "discontinued": true,
            "why": "these arms ran on metered credit (OpenCode Zen, or a Cursor third-party model). antiloki merges \
                    SUBSCRIPTIONS; a metered arm cannot be part of that claim, so these are kept for audit only and \
                    are shown to no one and used for no route.",
            "samples": rows.iter().filter_map(|r| r.get("n").and_then(Value::as_i64)).sum::<i64>(),
            "rows": rows,
        })).into_response(),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, axum::Json(json!({ "ok": false, "error": e }))).into_response(),
    }
}

#[cfg(test)]
mod override_tests {
    use super::*;
    #[test]
    fn an_undone_accept_is_counted_against_the_judge_that_accepted_it() {
        let o = Outcome { suite: "override-probe".into(), stack: "any".into(), domain: "any".into(), job: "qa-gate".into(), size: "l".into(), planner: "judge:probe".into(), worker: "-".into(), qa: String::new(),
            tasks: 1, pass_first: 1, retries: 0, failed: 0, gate_met: 1, gate_unmet: 0, audit: Some(1.0), ms: 1000, usd: 0.01, source: "bench".into(), audit_only: false };
        record(&o).unwrap();
        let reasked = |c: &Connection| -> i64 { c.query_row("SELECT reasked FROM merge_scores WHERE suite='override-probe' AND planner='judge:probe'", [], |r| r.get(0)).unwrap() };
        let c = open().unwrap();
        assert_eq!(reasked(&c), 0);
        assert!(record_override("qa-gate", "judge:probe").unwrap(), "the judge's row takes the mark");
        assert_eq!(reasked(&c), 1, "one false accept");
        assert!(!record_override("qa-gate", "judge:nobody").unwrap(), "a judge with no row takes nothing and nothing breaks");
        assert!(!record_override("planner", "judge:probe").unwrap(), "and the mark stays inside its job");
    }
}

#[cfg(test)]
mod in_band_tests {
    use super::*;
    const B: f64 = 0.20;   // keep >= 80 % of the best (operator 2026-09-30)
    #[test]
    fn the_band_keeps_eighty_percent_of_the_best_and_ignores_the_sample_count() {
        assert!(in_band_of(0.72, 10, 0.9, B), "exactly 80 % of 0.9 is on the edge, and inside it");
        assert!(!in_band_of(0.71, 14, 0.9, B), "…and 0.71 is outside by a hundredth — a hard edge, arbitrary, and stated as such");
        assert!(in_band_of(0.80, 10, 1.0, B) && !in_band_of(0.79, 10, 1.0, B), "against a perfect frontier the floor is 0.80");
        assert!(in_band_of(0.95, 1, 0.9, B), "above the bar is never excluded");
        // the two rules that were tried and failed, asserted as the reasons they failed
        assert!(!in_band_of(0.21, 8, 0.9, B), "a row that fails four times in five is never 'close to the frontier'");
        for n in [1, 2, 5, 20, 400] { assert_eq!(in_band_of(0.65, n, 0.9, B), in_band_of(0.65, 1, 0.9, B),
            "the verdict must not move with n: the readers rows carry n=2 for twenty graded questions"); }
    }
}

#[cfg(test)]
mod shipped_tests {
    use super::*;
    fn row(planner: &str, n: i64, pass: i64) -> Shipped {
        Shipped { job: "editor-l".into(), size: "l".into(), domain: "any".into(), stack: "any".into(), suite: "".into(), planner: planner.into(), worker: "w".into(), qa: "".into(),
                  n, pass_first: pass, usd_total: n as f64 * 0.1, ms_total: n * 1000, ..Default::default() }
    }
    fn fresh() -> Connection { let _ = std::fs::remove_file(path()); open().unwrap() }
    fn n_of(c: &Connection, planner: &str) -> Option<i64> { c.query_row("SELECT n FROM merge_scores WHERE planner = ?1", [planner], |r| r.get(0)).ok() }
    fn defaults(v: &str, rows: Vec<Shipped>) -> Defaults { Defaults { version: v.into(), rows } }

    #[test]
    fn a_fresh_record_gets_the_defaults_and_knows_they_are_ours() {
        let c = fresh();
        assert_eq!(apply_shipped(&c, &defaults("v1", vec![row("a", 5, 4), row("b", 3, 3)])).unwrap(), 2);
        assert_eq!(n_of(&c, "a"), Some(5));
        let mut rows = table().unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r["origin"] == json!("ours") && r["own_n"] == json!(0)), "{rows:?}");
        let s = defaults_summary(&rows);
        assert_eq!((s["ours_rows"].clone(), s["yours_rows"].clone(), s["ours_samples"].clone()), (json!(2), json!(0), json!(8)));
        rows.clear();
    }
    #[test]
    fn applying_the_same_version_twice_adds_nothing() {
        let c = fresh();
        let d = defaults("v1", vec![row("a", 5, 4)]);
        apply_shipped(&c, &d).unwrap();
        assert_eq!(apply_shipped(&c, &d).unwrap(), 0);
        assert_eq!(n_of(&c, "a"), Some(5));
    }
    #[test]
    fn the_users_runs_add_on_top_and_the_row_says_both() {
        let c = fresh();
        apply_shipped(&c, &defaults("v1", vec![row("a", 5, 4)])).unwrap();
        c.execute("UPDATE merge_scores SET n = n + 2 WHERE planner = 'a'", []).unwrap();   // what two of the user's own runs do
        let r = &table().unwrap()[0];
        assert_eq!((r["origin"].clone(), r["shipped_n"].clone(), r["own_n"].clone(), r["n"].clone()), (json!("both"), json!(5), json!(2), json!(7)));
    }
    #[test]
    fn a_new_version_swaps_our_share_and_keeps_the_users() {
        let c = fresh();
        apply_shipped(&c, &defaults("v1", vec![row("a", 5, 4), row("gone", 2, 2)])).unwrap();
        c.execute("UPDATE merge_scores SET n = n + 3 WHERE planner = 'a'", []).unwrap();   // the user's three
        c.execute("INSERT INTO merge_scores (job, size, domain, stack, suite, planner, worker, qa, n, source) VALUES ('editor-l','l','any','any','','mine','w','',4,'run')", []).unwrap();
        apply_shipped(&c, &defaults("v2", vec![row("a", 9, 8), row("new", 1, 1)])).unwrap();
        assert_eq!(n_of(&c, "a"), Some(12), "9 of ours + the user's 3");
        assert_eq!(n_of(&c, "gone"), None, "a row we stopped shipping and nobody used is gone");
        assert_eq!(n_of(&c, "new"), Some(1));
        assert_eq!(n_of(&c, "mine"), Some(4), "what the user measured is never touched");
        let rows = table().unwrap();
        let o = |p: &str| rows.iter().find(|r| r["planner"] == json!(p)).unwrap()["origin"].clone();
        assert_eq!((o("a"), o("mine"), o("new")), (json!("both"), json!("yours"), json!("ours")));
    }
    #[test]
    fn origin_is_decided_by_the_ledger_not_the_source_column() {
        assert_eq!(origin_of(5, 5), "ours"); assert_eq!(origin_of(6, 5), "both"); assert_eq!(origin_of(4, 0), "yours"); assert_eq!(origin_of(0, 0), "yours");
    }
    #[test]
    fn the_file_that_ships_parses_and_is_all_routable_benchmark_evidence() {
        let d: Defaults = serde_json::from_str(include_str!("merges-defaults.json")).expect("merges-defaults.json must parse");
        assert!(!d.version.is_empty() && !d.rows.is_empty());
        assert!(d.rows.iter().all(|r| r.n > 0 && !r.planner.is_empty() && !r.worker.is_empty()));
        assert!(d.rows.iter().all(|r| crate::cli::side_is_metered(&r.planner).is_none() && crate::cli::side_is_metered(&r.worker).is_none()), "a metered arm must not ship as a default");
    }
}
