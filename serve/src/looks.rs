//! THE LOOKS - PLAN-THE-DESIGN S2 (2026-10-01): which seat's result LOOKS best, measured by a vision judge that compares two pages and never grades one alone.
//!
//! Operator 2026-10-01: Claude and Composer both met the same frontend goal and Claude's looked clearly better, while the router (which scores behaviour only) could not tell them apart.
//! This module is the measuring half of the answer, all of it pure or over one small sqlite so it can be tested without a model:
//!   * `judge_seat_with` - who may judge a pair: a seat that is on, signed in, not limited, VISION-MEASURED, and of a different model family from every builder in the pair;
//!   * `prompt` / `parse_verdict` / `combine` - the pairwise prompt (`design.judge`), the judge's JSON, and the both-orders rule (a split or a tie in either order is a tie);
//!   * `pair_hash` / `Budget` - the cache key (the content of the four PNGs + the ask + the prompt version) and the image-token bound;
//!   * `look_pairs` - the matches, in merges.sqlite (CREATE IF NOT EXISTS, the way `merge_scores` grows columns), unique on the cache key;
//!   * `rank` - Bradley-Terry (Elo below `MIN_BT_MATCHES` matches) -> a `looks` score in 0..1 per cell = the probability of beating the fixed reference the fit is anchored on (no cell is the reference), with a bootstrap interval.
//! Nothing here routes. A look reaches the router only after the judge agrees with the operator at >= 80 % on 30 labelled pairs (D3 / S3), so the numbers are advisory until then.
use crate::cli::{Engine, QA_GATE_ORDER};
use crate::prompts;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

/// v2 (2026-10-05): the S2 exit bar was missed at v1 (swap-order agreement 13/18 = 72 %, every split purely positional, the judge never said tie): the prompt says the order is random, asks for evidence per axis BEFORE
/// the verdict and defines `tie`; a split between the two orders is settled by a third call in a recorded random order (`combine_with_third`). Rows and calibrations of v1 are advisory (`standing_with`).
/// Bump when the judge prompt or the both-orders rule changes: it is part of the cache key, so an old verdict is never reused for a new question.
pub const JUDGE_VERSION: &str = "design.judge/2";
/// The five rubric axes, the keys of `per_axis`, in the order of the rubric.
pub const AXES: [&str; 5] = ["hierarchy", "spacing", "colour", "typography", "polish"];
/// Below this many matches the ranking is Elo, not Bradley-Terry: a handful of games cannot pin a likelihood fit, and an Elo pass says exactly that much and no more.
pub const MIN_BT_MATCHES: usize = 8;
/// A cell is judged against at most this many opponents x asks; the bench never asks the judge for more.
pub const MAX_MATCHES_PER_CELL: usize = 24;

// ------------------------------------------------------------------------------------------------------------------------------ who may judge
/// THE SEATS WHOSE EYES ARE MEASURED (bench/vision, 2026-09-30/10-01): they were given a real screenshot and read it. Composer 2.5 8/8, Sonnet 8/8 (Haiku 8/8 too, but it is not in the gate walk), Gemini flash-medium 8/8, Codex low 8/8,
/// and Cursor's Grok-high on the S2 probe (bench/design/out/vision-probe.json: both of two images read, and the right one named). Opus and native Grok are NOT here: nothing was measured for them.
pub const VISION_SEATS: &[(Engine, &str)] = &[
    (Engine::Cursor, "cursor-grok-4.6-high"), (Engine::Cursor, "composer-2.5"), (Engine::ClaudeCode, "sonnet"),
    (Engine::Gemini, "gemini-3.8-flash-medium"), (Engine::Codex, "gpt-5.6-terra@medium"),
];

/// Two seats are the same MODEL FAMILY when a judge could prefer its own kind's style. The family is the model's VENDOR LINEAGE, read from the model name whatever engine hosts it: Sonnet through Cursor is
/// Claude's family just as Sonnet through Claude Code is, native Grok and Cursor's `grok-4.6-high` are one, a GPT or Codex model is one wherever it runs. A name that says nothing falls back to the engine's own
/// vendor (a Claude Code, Codex, Gemini or Grok seat) and, for Cursor and OpenCode, to the model's own name with the effort suffix off (`composer-2.5` is its own family).
pub fn family(engine: Engine, model: &str) -> String {
    let lower = model.to_ascii_lowercase();
    let m = lower.rsplit('/').next().unwrap_or(&lower).trim_start_matches("cursor-");
    let has = |needles: &[&str]| needles.iter().any(|n| m.contains(n));
    if has(&["claude", "sonnet", "opus", "haiku", "fable"]) { return "claude".into() }
    if has(&["grok"]) { return "grok".into() }
    if has(&["gpt", "codex"]) || m.starts_with("o3") || m.starts_with("o4") { return "gpt".into() }
    if has(&["gemini"]) { return "gemini".into() }
    if has(&["composer"]) { return "composer".into() }
    match engine {
        Engine::ClaudeCode => "claude".into(), Engine::Codex => "gpt".into(), Engine::Gemini => "gemini".into(), Engine::Grok => "grok".into(),
        Engine::Cursor | Engine::Opencode => {
            for suffix in ["-xhigh", "-high", "-medium", "-low", "-fast", "-max"] { if let Some(f) = m.strip_suffix(suffix) { return f.to_string() } }
            m.to_string()
        }
    }
}

/// The judge for a pair of builders: walk `QA_GATE_ORDER` (the order a gate is chosen in), keep the vision-measured seats that can take a call today and are not of any builder's family. `None` when no seat qualifies -
/// the match is then NOT run (an unjudged pair is not a tie). Pure over the availability question, like `binding_when_unavailable`.
pub fn judge_seat_with(builders: &[(Engine, String)], ok: impl Fn(Engine, Option<&str>) -> bool) -> Option<(Engine, &'static str)> { judge_seats_with(builders, ok).into_iter().next() }
/// EVERY seat that may judge the pair, in the gate's order. One judge per pair makes the judge and the pair perfectly confounded (a seat's looks then blend its builds with its judge's taste); judging a pair with
/// more than one of these is how the bench crosses the two.
pub fn judge_seats_with(builders: &[(Engine, String)], ok: impl Fn(Engine, Option<&str>) -> bool) -> Vec<(Engine, &'static str)> {
    let fams: Vec<String> = builders.iter().map(|(e, m)| family(*e, m)).collect();
    QA_GATE_ORDER.iter().copied().filter(|(e, m)| VISION_SEATS.contains(&(*e, *m)) && ok(*e, Some(m)) && !fams.contains(&family(*e, m))).collect()
}
/// The same, against this machine's real availability (switched on, signed in, not out of quota).
pub fn judge_seat(builders: &[(Engine, String)]) -> Option<(Engine, &'static str)> { judge_seat_with(builders, crate::cli::seat_ok) }

// ------------------------------------------------------------------------------------------------------------------------------ the prompt, the verdict
/// The pairwise prompt, rendered from the registered `design.judge` text with the registered rubric spliced in. The paths must already be neutral (no seat, model or ask id in them): `neutral_names` says what they look like.
pub fn prompt(ask: &str, a_desktop: &Path, a_phone: &Path, b_desktop: &Path, b_phone: &Path) -> String {
    let mut v = Map::new();
    v.insert("ask".into(), json!(ask)); v.insert("rubric".into(), json!(prompts::design_rubric()));
    for (k, p) in [("a_desktop", a_desktop), ("a_phone", a_phone), ("b_desktop", b_desktop), ("b_phone", b_phone)] { v.insert(k.into(), json!(p.to_string_lossy())); }
    prompts::render_base("design.judge", &v)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side { A, B, Tie }
impl Side {
    fn parse(s: &str) -> Option<Side> { match s.trim().to_ascii_lowercase().as_str() { "a" => Some(Side::A), "b" => Some(Side::B), "tie" | "draw" | "equal" => Some(Side::Tie), _ => None } }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Verdict { pub winner: Side, pub per_axis: BTreeMap<String, Side>, pub reason: String }

/// An axis answer is a bare "A"/"B"/"tie" (v1) or `{"evidence": "...", "winner": "A"}` (v2: the evidence comes first, then the verdict).
fn axis_side(v: &Value) -> Option<Side> { match v { Value::String(s) => Side::parse(s), Value::Object(o) => o.get("winner").and_then(Value::as_str).and_then(Side::parse), _ => None } }

/// The LAST JSON object in the judge's answer that names a winner (A, B or tie). A judge that thinks aloud before answering is read by its answer, not its preamble; no winner -> `None` (an UNREAD, never a tie).
pub fn parse_verdict(text: &str) -> Option<Verdict> {
    let mut best = None; let mut covered = 0;
    for (i, _) in text.match_indices('{') {
        if i < covered { continue }                      // inside an object already read (v2 axes are objects with their own "winner")
        let mut stream = serde_json::Deserializer::from_str(&text[i..]).into_iter::<Value>();
        let Some(Ok(Value::Object(o))) = stream.next() else { continue };
        let Some(winner) = o.get("winner").and_then(Value::as_str).and_then(Side::parse) else { continue };
        covered = i + stream.byte_offset();
        let mut per_axis = BTreeMap::new();
        if let Some(Value::Object(ax)) = o.get("per_axis") { for (k, v) in ax { if let Some(s) = axis_side(v) { per_axis.insert(k.to_ascii_lowercase(), s); } } }
        best = Some(Verdict { winner, per_axis, reason: o.get("reason").and_then(Value::as_str).unwrap_or("").to_string() });
    }
    best
}

/// Who won, in the pair's own terms: X is the first builder named in the pair, Y the second.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Who { X, Y, Tie }
fn who(side: Side, a_is_x: bool) -> Who { match (side, a_is_x) { (Side::Tie, _) => Who::Tie, (Side::A, true) | (Side::B, false) => Who::X, _ => Who::Y } }
/// The two orders of one pair, resolved. `winner` is a win only when BOTH orders name the same build; any disagreement (X in one order, Y or a tie in the other) is a tie - position bias and indecision look the same here
/// and both mean "the judge cannot separate them". `consistent` is the judge's self-consistency: the two orders gave the same answer, ties included.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Outcome { pub winner: Who, pub consistent: bool, pub ab: Who, pub ba: Who, pub axes: BTreeMap<String, Who>,
    /// The third call, run only when the two orders disagreed (`combine_with_third`); it is recorded whatever it said.
    #[serde(default)] pub third: Option<Who>,
    /// The winner was decided by the 2-of-3 majority and not by the two orders agreeing.
    #[serde(default)] pub by_third: bool }
/// `ab` = the verdict of the call with A = X, B = Y; `ba` = the call with A = Y, B = X.
pub fn combine(ab: &Verdict, ba: &Verdict) -> Outcome { combine_with_third(ab, ba, None) }
/// Two of three calls name the same answer -> that answer; three different answers (x, y, tie) -> tie.
pub fn majority(votes: [Who; 3]) -> Who {
    for w in [Who::X, Who::Y, Who::Tie] { if votes.iter().filter(|v| **v == w).count() >= 2 { return w } }
    Who::Tie
}
/// Which order the third call shows the pictures in: true = A is X. Derived from the match's cache key, so it looks random (it is not "the order nobody has tried") and is the same on a re-run and recordable.
pub fn third_a_is_x(hash: &str) -> bool { hash.bytes().last().map_or(true, |b| b.is_ascii_hexdigit() && (b.to_ascii_lowercase() as char).to_digit(16).unwrap_or(0) % 2 == 0) }
/// `combine` with the v2 third call: when the first two orders disagree on the overall winner, `third` = (that call's verdict, whether its A was X) settles it by majority of the three (else tie). When the two orders
/// agree the third is not needed and is ignored. `consistent` stays the SWAP-ORDER AGREEMENT of the first two calls (the number S2 reports); `third` and `by_third` record the rest.
pub fn combine_with_third(ab: &Verdict, ba: &Verdict, third: Option<(&Verdict, bool)>) -> Outcome {
    let (w1, w2) = (who(ab.winner, true), who(ba.winner, false));
    let mut axes = BTreeMap::new();
    for k in AXES {
        let a1 = ab.per_axis.get(k).map(|s| who(*s, true)).unwrap_or(Who::Tie);
        let a2 = ba.per_axis.get(k).map(|s| who(*s, false)).unwrap_or(Who::Tie);
        axes.insert(k.to_string(), if a1 == a2 { a1 } else { Who::Tie });
    }
    if w1 == w2 { return Outcome { winner: w1, consistent: true, ab: w1, ba: w2, axes, third: None, by_third: false } }
    let w3 = third.map(|(v, a_is_x)| who(v.winner, a_is_x));
    Outcome { winner: w3.map_or(Who::Tie, |w3| majority([w1, w2, w3])), consistent: false, ab: w1, ba: w2, axes, third: w3, by_third: w3.is_some() }
}

// ------------------------------------------------------------------------------------------------------------------------------ the cache key, the budget
/// The cache key of a match: the CONTENT of the four pictures (in A/B-canonical order: X desktop, X phone, Y desktop, Y phone), the ask, the judge seat and the prompt version. Names and paths are not in it, so a rebuilt
/// page that looks the same pixel for pixel costs nothing, and a changed prompt never reuses an old verdict.
pub fn pair_hash(ask: &str, images: &[Vec<u8>], judge: &str) -> String {
    let mut h = Sha256::new();
    h.update(JUDGE_VERSION.as_bytes()); h.update([0]); h.update(judge.as_bytes()); h.update([0]); h.update(ask.as_bytes());
    for img in images { h.update([0]); h.update(Sha256::digest(img)); }
    hex::encode(h.finalize())
}
/// The input tokens one match costs the judge: every picture is read once per order, so twice.
pub fn match_tokens(images: &[&Path]) -> u64 { 2 * images.iter().map(|p| crate::qa_evidence::image_tokens(p)).sum::<u64>() }
/// A cap on the picture tokens a bench may spend. `try_spend` refuses (and spends nothing) when the next match would pass the cap. NOT YET ENFORCED by the bench driver: `bench/design/run.py` bounds the number of calls
/// (`--max-calls`), not the picture tokens, and `antiloki-looks tokens` is how a driver would ask what a match costs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Budget { pub cap: u64, pub spent: u64 }
impl Budget {
    pub fn try_spend(&mut self, n: u64) -> bool { if self.spent + n > self.cap { return false } self.spent += n; true }
}
/// The names a judge may see: a fresh folder per match and a-/b- file names. Nothing in them says who built what.
pub fn neutral_names() -> [&'static str; 4] { ["a-desktop.png", "a-phone.png", "b-desktop.png", "b-phone.png"] }

// ------------------------------------------------------------------------------------------------------------------------------ the matches, in merges.sqlite
/// merges.sqlite itself (the file `merge_scores` lives in), with the look_pairs table made. Opened here and not through `merges::open` so this module adds nothing to that file's schema code.
pub fn open_db() -> rusqlite::Result<Connection> {
    let p = crate::merges::path(); if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
    let c = Connection::open(p)?; c.execute_batch("PRAGMA journal_mode=WAL;")?; ensure_schema(&c)?; Ok(c)
}
pub fn ensure_schema(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS look_pairs (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        domain TEXT NOT NULL, ask TEXT NOT NULL, x_cell TEXT NOT NULL, y_cell TEXT NOT NULL,
        winner TEXT NOT NULL,                                -- the winning cell's name, or 'tie'
        consistent INTEGER NOT NULL DEFAULT 1, order_ab TEXT NOT NULL DEFAULT '', order_ba TEXT NOT NULL DEFAULT '',
        judge TEXT NOT NULL, prompt_ver TEXT NOT NULL, img_hash TEXT NOT NULL,
        per_axis TEXT NOT NULL DEFAULT '{}', reason TEXT NOT NULL DEFAULT '',
        tokens INTEGER NOT NULL DEFAULT 0, usd REAL, t INTEGER NOT NULL DEFAULT 0,
        UNIQUE(img_hash));
        CREATE INDEX IF NOT EXISTS look_pairs_domain ON look_pairs(domain);")
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PairRow {
    pub domain: String, pub ask: String, pub x: String, pub y: String, pub winner: String, pub consistent: bool,
    #[serde(default)] pub order_ab: String, #[serde(default)] pub order_ba: String,
    pub judge: String, #[serde(default = "default_ver")] pub prompt_ver: String, pub img_hash: String,
    #[serde(default)] pub per_axis: Value, #[serde(default)] pub reason: String, #[serde(default)] pub tokens: i64, #[serde(default)] pub usd: Option<f64>, #[serde(default)] pub t: i64,
}
fn default_ver() -> String { JUDGE_VERSION.to_string() }
/// Record one match. The cache key is unique, so the same pictures judged by the same seat under the same prompt are stored once; returns whether a row was added.
pub fn record_pair(c: &Connection, r: &PairRow) -> rusqlite::Result<bool> {
    ensure_schema(c)?;
    let n = c.execute("INSERT OR IGNORE INTO look_pairs (domain, ask, x_cell, y_cell, winner, consistent, order_ab, order_ba, judge, prompt_ver, img_hash, per_axis, reason, tokens, usd, t)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
        params![r.domain, r.ask, r.x, r.y, r.winner, r.consistent as i64, r.order_ab, r.order_ba, r.judge, r.prompt_ver, r.img_hash, r.per_axis.to_string(), r.reason, r.tokens, r.usd, r.t])?;
    Ok(n > 0)
}
/// A match already judged under this key, if any - the cache.
pub fn cached(c: &Connection, img_hash: &str) -> Option<PairRow> {
    ensure_schema(c).ok()?;
    c.query_row("SELECT domain, ask, x_cell, y_cell, winner, consistent, order_ab, order_ba, judge, prompt_ver, img_hash, per_axis, reason, tokens, usd, t FROM look_pairs WHERE img_hash = ?1", params![img_hash], |r| {
        Ok(PairRow { domain: r.get(0)?, ask: r.get(1)?, x: r.get(2)?, y: r.get(3)?, winner: r.get(4)?, consistent: r.get::<_, i64>(5)? != 0, order_ab: r.get(6)?, order_ba: r.get(7)?, judge: r.get(8)?, prompt_ver: r.get(9)?, img_hash: r.get(10)?,
            per_axis: serde_json::from_str(&r.get::<_, String>(11)?).unwrap_or(Value::Null), reason: r.get(12)?, tokens: r.get(13)?, usd: r.get(14)?, t: r.get(15)? })
    }).ok()
}
/// Every stored match of a domain judged under the CURRENT prompt (`JUDGE_VERSION`), as ranking input, oldest first. A match judged under an older prompt stays in the table (the cache key keeps it apart) but is not
/// a measurement of this prompt, and the operator's calibration vouches only for this one - so it never reaches a ranking.
/// `judge`: the calibration vouches for ONE judge seat, so a ranking that may steer the router passes that seat and only its matches are read; `None` reads every judge (advisory display only).
/// ONE verdict per pair: a second verdict on the same ask and the same two cells (another judge, or the same pictures rebuilt) is not an independent match, so only the latest stands.
pub fn matches_of(c: &Connection, domain: &str, judge: Option<&str>) -> Vec<Match> {
    if ensure_schema(c).is_err() { return vec![] }
    let Ok(mut st) = c.prepare("SELECT id, ask, x_cell, y_cell, winner FROM look_pairs WHERE domain = ?1 AND prompt_ver = ?2 AND (?3 IS NULL OR judge = ?3) ORDER BY id") else { return vec![] };
    let rows: Vec<(i64, String, String, String, String)> = st.query_map(params![domain, JUDGE_VERSION, judge], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        .map(|rows| rows.flatten().collect()).unwrap_or_default();
    let pair_key = |ask: &str, x: &str, y: &str| { let (a, b) = if x <= y { (x, y) } else { (y, x) }; (ask.to_string(), a.to_string(), b.to_string()) };
    let mut latest: std::collections::HashMap<(String, String, String), i64> = std::collections::HashMap::new();
    for (id, ask, x, y, _) in &rows { latest.insert(pair_key(ask, x, y), *id); }
    rows.iter().filter(|(id, ask, x, y, _)| latest.get(&pair_key(ask, x, y)) == Some(id)).map(|(_, _, x, y, w)| Match::from_winner(x, y, w)).collect()
}

// ------------------------------------------------------------------------------------------------------------------------------ ranking
/// One match between two cells (a seat, or a pair of seats). `score` is X's share: 1 a win, 0 a loss, 0.5 a tie.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Match { pub x: String, pub y: String, pub score: f64 }
impl Match {
    pub fn from_winner(x: &str, y: &str, winner: &str) -> Match { Match { x: x.into(), y: y.into(), score: if winner == x { 1.0 } else if winner == y { 0.0 } else { 0.5 } } }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Look { pub cell: String, pub looks: f64, pub lo: f64, pub hi: f64, pub n: usize, pub wins: f64, pub losses: f64, pub ties: usize }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ranking { pub method: String, pub matches: usize, pub boots: usize, pub cells: Vec<Look> }

/// Bradley-Terry strengths by the minorization-maximization update, with ONE virtual tie against a fixed opponent of strength 1 per cell. The prior keeps a cell that never won (or never lost) finite - its strength is small,
/// not zero or infinite - and anchors the scale, so no renormalising is needed. A tie counts half a win to each side.
fn bt(cells: &[String], ms: &[Match]) -> Vec<f64> {
    let idx = |n: &str| cells.iter().position(|c| c == n);
    let games: Vec<(usize, usize, f64)> = ms.iter().filter_map(|m| Some((idx(&m.x)?, idx(&m.y)?, m.score))).filter(|(a, b, _)| a != b).collect();
    let mut s = vec![1.0f64; cells.len()];
    for _ in 0..1000 {
        let mut w = vec![0.5f64; cells.len()];       // the virtual tie is half a win
        let mut d: Vec<f64> = s.iter().map(|si| 1.0 / (si + 1.0)).collect();
        for &(a, b, sc) in &games { w[a] += sc; w[b] += 1.0 - sc; let k = 1.0 / (s[a] + s[b]); d[a] += k; d[b] += k; }
        let next: Vec<f64> = w.iter().zip(&d).map(|(w, d)| w / d).collect();
        let moved = next.iter().zip(&s).map(|(a, b)| (a.ln() - b.ln()).abs()).fold(0.0, f64::max);
        s = next; if moved < 1e-10 { break }
    }
    s
}
/// Elo: everyone starts at 1500, K = 32, matches in the order given; the strength is 10^((rating - 1500) / 400) so the two methods read through the same `looks_of`. The fallback for a handful of matches.
fn elo(cells: &[String], ms: &[Match]) -> Vec<f64> {
    let mut r = vec![1500.0f64; cells.len()];
    for m in ms {
        let (Some(a), Some(b)) = (cells.iter().position(|c| *c == m.x), cells.iter().position(|c| *c == m.y)) else { continue };
        if a == b { continue }
        let e = 1.0 / (1.0 + 10f64.powf((r[b] - r[a]) / 400.0));
        r[a] += 32.0 * (m.score - e); r[b] -= 32.0 * (m.score - e);
    }
    r.iter().map(|x| 10f64.powf((x - 1500.0) / 400.0)).collect()
}
/// `looks` = the probability that a cell beats the FIXED REFERENCE opponent the fit is anchored on (strength 1: the virtual tie `bt` gives every cell, and Elo's 1500): s / (s + 1). A cell is never the reference,
/// so no seat is pinned to 0.5 by construction, and the number means the same thing for any set of cells and in any run (a median cell moved with the set, and with three cells it WAS the middle seat at exactly 0.5).
/// An average cell sits near 0.5, the best above, the worst below.
pub fn looks_of(strengths: &[f64]) -> Vec<f64> { strengths.iter().map(|s| s / (s + 1.0)).collect() }
fn strengths(method_bt: bool, cells: &[String], ms: &[Match]) -> Vec<f64> { if method_bt { bt(cells, ms) } else { elo(cells, ms) } }

/// A tiny deterministic generator (xorshift64*), so a bootstrap is reproducible from its seed.
struct Rng(u64);
impl Rng { fn next(&mut self) -> u64 { self.0 ^= self.0 >> 12; self.0 ^= self.0 << 25; self.0 ^= self.0 >> 27; self.0.wrapping_mul(0x2545F4914F6CDD1D) } }
fn quantile(sorted: &[f64], q: f64) -> f64 { if sorted.is_empty() { return 0.5 } sorted[((sorted.len() - 1) as f64 * q).round() as usize] }

/// Rank the cells of a set of matches. The point estimate is the fit of ALL matches; the interval is the 2.5 %-97.5 % range of the same fit over `boots` resamples of the matches (with replacement, seeded). Sorted best first.
/// A cell with a few games has a wide interval - that width is the honest part of the number.
pub fn rank(ms: &[Match], boots: usize, seed: u64) -> Ranking {
    let mut cells: Vec<String> = Vec::new();
    for m in ms { for c in [&m.x, &m.y] { if !cells.contains(c) { cells.push(c.clone()) } } }
    cells.sort();
    let use_bt = ms.len() >= MIN_BT_MATCHES;
    let point = looks_of(&strengths(use_bt, &cells, ms));
    let mut rng = Rng(seed.max(1));
    let mut draws: Vec<Vec<f64>> = vec![Vec::with_capacity(boots); cells.len()];
    if !ms.is_empty() {
        for _ in 0..boots {
            let re: Vec<Match> = (0..ms.len()).map(|_| ms[(rng.next() % ms.len() as u64) as usize].clone()).collect();
            for (i, l) in looks_of(&strengths(use_bt, &cells, &re)).into_iter().enumerate() { draws[i].push(l) }
        }
    }
    let mut out: Vec<Look> = cells.iter().enumerate().map(|(i, c)| {
        let mine: Vec<&Match> = ms.iter().filter(|m| m.x == *c || m.y == *c).collect();
        let score = |m: &Match| if m.x == *c { m.score } else { 1.0 - m.score };
        let mut d = draws[i].clone(); d.sort_by(|a, b| a.total_cmp(b));
        Look { cell: c.clone(), looks: point[i], lo: if d.is_empty() { point[i] } else { quantile(&d, 0.025).min(point[i]) }, hi: if d.is_empty() { point[i] } else { quantile(&d, 0.975).max(point[i]) },
            n: mine.len(), wins: mine.iter().map(|m| score(m)).filter(|s| *s == 1.0).count() as f64, losses: mine.iter().map(|m| score(m)).filter(|s| *s == 0.0).count() as f64, ties: mine.iter().filter(|m| m.score == 0.5).count() }
    }).collect();
    out.sort_by(|a, b| b.looks.total_cmp(&a.looks).then(a.cell.cmp(&b.cell)));
    Ranking { method: if use_bt { "bradley-terry".into() } else { "elo".into() }, matches: ms.len(), boots, cells: out }
}

// ------------------------------------------------------------------------------------------------------------------------------ the operator's calibration (D3)
/// PLAN-THE-DESIGN D3 (2026-10-01): taste is subjective, so a look may steer the router only after the judge has agreed with the OPERATOR on enough real pairs. `bench/design/agreement.py --operator` writes the file
/// this reads, from the picks the operator made on the contact sheet; nothing else writes it, and an absent or odd file means ADVISORY - the looks are shown, never ranked. The thresholds are the plan's, on integers (no float edge at 80 %).
pub const ROUTE_PCT: u32 = 80;
pub const ADVISORY_PCT: u32 = 65;
/// Fewer labelled pairs than this can never allow routing, whatever the rate: 15 of 18 and 18 of 18 are both inside the noise of a coin that is 70 % right.
pub const MIN_LABELS: u32 = 20;
const CALIBRATION_SCHEMA: u32 = 1;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing { Routes, Advisory, NotTrusted }
/// The plan's rule: >= 80 % routes, 65-80 % is advisory, under 65 % is not trusted; under `MIN_LABELS` pairs it is advisory (too few to allow routing, too few to condemn).
pub fn standing_of(agree: u32, n: u32) -> Standing {
    if n < MIN_LABELS || agree > n { return Standing::Advisory }
    // u64: the counts come from a file on disk, and `agree * 100` overflows u32 from n = 43 million (a panic in debug, a wrap to a nonsense verdict in release)
    let (a, n) = (agree as u64 * 100, n as u64);
    if a >= ROUTE_PCT as u64 * n { Standing::Routes } else if a >= ADVISORY_PCT as u64 * n { Standing::Advisory } else { Standing::NotTrusted }
}
/// What `agreement.py --operator` wrote. Only the fields the router needs; the file carries more (intervals, per-axis rates) for people.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Calibration { pub schema: u32, pub operator: bool, #[serde(default)] pub judge: String, pub judge_version: String, pub n: u32, pub agree: u32, #[serde(default)] pub sheet: String, #[serde(default)] pub measured_at: String }
pub fn parse_calibration(text: &str) -> Option<Calibration> { serde_json::from_str(text).ok() }
/// The standing of the looks against a calibration made (or not) for THIS judge prompt AND naming the one judge seat it vouches for (`judge`; an older file without one is advisory until re-measured). Advisory when: there is no file, it is not the operator's, it is a schema we do not know, it was measured for another prompt version
/// (the plan: "re-measure after every judge or rubric change" - bumping `JUDGE_VERSION` retires the old calibration at once), or it counts fewer than `MIN_LABELS` pairs. The rate is recomputed from agree/n, never read from a stored verdict.
pub fn standing_with(cal: Option<&Calibration>, current_version: &str) -> Standing {
    match cal { Some(c) if c.operator && c.schema == CALIBRATION_SCHEMA && c.judge_version == current_version && !c.judge.is_empty() => standing_of(c.agree, c.n), _ => Standing::Advisory }
}
/// `$ANTILOKI_HOME/design-calibration.json`, beside merges.sqlite and eta.json.
pub fn calibration_path() -> std::path::PathBuf { crate::eta::History::path().with_file_name("design-calibration.json") }
pub fn load_calibration_at(p: &Path) -> Option<Calibration> { parse_calibration(&std::fs::read_to_string(p).ok()?) }
/// The standing of the looks on this machine now.
pub fn looks_standing() -> Standing { standing_with(load_calibration_at(&calibration_path()).as_ref(), JUDGE_VERSION) }
/// The judge seat the calibration vouches for: the only one whose matches may be ranked into routing. None without a usable calibration.
pub fn calibrated_judge() -> Option<String> { load_calibration_at(&calibration_path()).filter(|c| !c.judge.is_empty()).map(|c| c.judge) }
/// May the router rank on looks? Only on `Routes`; the D6 router change (S4) asks this before it lets a look into `contest`/`rank`.
pub fn looks_may_route() -> bool { looks_standing() == Standing::Routes }

#[cfg(test)]
mod tests {
    use super::*;
    fn m(x: &str, y: &str, score: f64) -> Match { Match { x: x.into(), y: y.into(), score } }
    fn rep(x: &str, y: &str, score: f64, n: usize) -> Vec<Match> { (0..n).map(|_| m(x, y, score)).collect() }
    fn looks(r: &Ranking, c: &str) -> f64 { r.cells.iter().find(|l| l.cell == c).unwrap().looks }
    fn v(w: Side, axes: &[(&str, Side)]) -> Verdict { Verdict { winner: w, per_axis: axes.iter().map(|(k, s)| (k.to_string(), *s)).collect(), reason: String::new() } }

    // ----- Bradley-Terry
    #[test]
    fn a_seat_that_always_loses_is_near_zero_and_the_one_that_always_wins_is_near_one() {
        let mut ms = rep("claude", "composer", 1.0, 12); ms.extend(rep("claude", "grok", 1.0, 12)); ms.extend(rep("grok", "composer", 1.0, 12));
        let r = rank(&ms, 50, 7);
        assert_eq!(r.method, "bradley-terry");
        assert!(looks(&r, "composer") < 0.1, "a seat that lost every game: {}", looks(&r, "composer"));
        assert!(looks(&r, "claude") > 0.9, "a seat that won every game: {}", looks(&r, "claude"));
        assert!(looks(&r, "grok") > 0.4 && looks(&r, "grok") < 0.6, "the middle one is the median: {}", looks(&r, "grok"));
        assert_eq!(r.cells.iter().map(|l| l.cell.as_str()).collect::<Vec<_>>(), ["claude", "grok", "composer"]);
    }
    #[test]
    fn no_seat_is_pinned_to_one_half_by_being_the_middle_one() {
        // a beat b 9-3 and c 10-2, b and c split 6-6: b and c are both worse than the average cell. With looks measured against the MEDIAN cell the median one of them was exactly 0.5 whatever the matches said.
        let ms = [rep("a", "b", 1.0, 9), rep("a", "b", 0.0, 3), rep("a", "c", 1.0, 10), rep("a", "c", 0.0, 2), rep("b", "c", 1.0, 6), rep("b", "c", 0.0, 6)].concat();
        let r = rank(&ms, 50, 11);
        assert_eq!(r.method, "bradley-terry");
        assert!(looks(&r, "b") < 0.45 && looks(&r, "c") < 0.45, "b {} c {}: the two losers are both below the reference", looks(&r, "b"), looks(&r, "c"));
        assert!(looks(&r, "a") > 0.6, "a {}", looks(&r, "a"));
        // the number does not depend on which other cells were in the set: a alone against b is the same fit as in the three-cell run's a-b matches only
        let two = rank(&[rep("a", "b", 1.0, 9), rep("a", "b", 0.0, 3)].concat(), 20, 1);
        assert!(looks(&two, "a") > 0.5 && looks(&two, "b") < 0.5);
    }
    #[test]
    fn equal_seats_sit_at_one_half() {
        let mut ms = Vec::new();
        for i in 0..24 { ms.push(m("a", "b", if i % 2 == 0 { 1.0 } else { 0.0 })); ms.push(m("b", "c", if i % 2 == 0 { 1.0 } else { 0.0 })); ms.push(m("a", "c", if i % 2 == 0 { 0.0 } else { 1.0 })); }
        let r = rank(&ms, 50, 3);
        for c in ["a", "b", "c"] { assert!((looks(&r, c) - 0.5).abs() < 0.02, "{c} = {}", looks(&r, c)); }
    }
    #[test]
    fn all_ties_are_all_one_half_and_the_interval_is_still_a_range() {
        let ms = [rep("a", "b", 0.5, 10), rep("b", "c", 0.5, 10)].concat();
        let r = rank(&ms, 50, 3);
        for l in &r.cells { assert!((l.looks - 0.5).abs() < 1e-6 && l.lo <= l.looks && l.looks <= l.hi, "{l:?}"); assert_eq!(l.ties, l.n); }
    }
    #[test]
    fn strength_is_transitive_even_between_seats_that_never_met() {
        // a beat b 8 times, b beat c 8 times, a and c never played: a > b > c all the same
        let ms = [rep("a", "b", 1.0, 8), rep("b", "c", 1.0, 8)].concat();
        let r = rank(&ms, 50, 1);
        assert!(looks(&r, "a") > looks(&r, "b") && looks(&r, "b") > looks(&r, "c"), "{:?}", r.cells);
        assert!(r.cells[0].cell == "a" && r.cells[2].cell == "c");
    }
    #[test]
    fn a_win_counts_for_more_than_a_tie_and_a_tie_for_more_than_a_loss() {
        let ms = [rep("a", "z", 1.0, 6), rep("b", "z", 0.5, 6), rep("c", "z", 0.0, 6)].concat();
        let r = rank(&ms, 20, 1);
        assert!(looks(&r, "a") > looks(&r, "b") && looks(&r, "b") > looks(&r, "c"));
    }
    #[test]
    fn a_handful_of_matches_is_ranked_by_elo_and_still_orders_the_winner_first() {
        let ms = [rep("a", "b", 1.0, 3), rep("b", "c", 1.0, 2)].concat();
        let r = rank(&ms, 30, 5);
        assert_eq!(r.method, "elo");
        assert!(looks(&r, "a") > looks(&r, "b") && looks(&r, "b") > looks(&r, "c"), "{:?}", r.cells);
        assert!(rank(&[], 10, 1).cells.is_empty());
    }
    #[test]
    fn the_interval_is_wider_with_fewer_games_and_always_holds_the_estimate() {
        let few = [rep("a", "b", 1.0, 5), rep("a", "b", 0.0, 3), rep("b", "c", 1.0, 4)].concat();
        let many: Vec<Match> = (0..8).flat_map(|_| few.clone()).collect();
        let (rf, rm) = (rank(&few, 200, 9), rank(&many, 200, 9));
        let w = |r: &Ranking| r.cells.iter().find(|l| l.cell == "a").map(|l| l.hi - l.lo).unwrap();
        assert!(w(&rf) > w(&rm), "few {} many {}", w(&rf), w(&rm));
        for l in rf.cells.iter().chain(&rm.cells) { assert!(l.lo <= l.looks && l.looks <= l.hi && l.lo >= 0.0 && l.hi <= 1.0, "{l:?}"); }
        assert_eq!(rank(&few, 200, 9), rf, "the same seed gives the same interval");
    }

    // ----- the both-orders rule
    #[test]
    fn a_win_needs_both_orders_to_name_the_same_build() {
        // order 1: A = x, B = y  -> "A" means x;  order 2: A = y, B = x -> "B" means x
        let o = combine(&v(Side::A, &[("hierarchy", Side::A), ("polish", Side::A)]), &v(Side::B, &[("hierarchy", Side::B), ("polish", Side::A)]));
        assert_eq!((o.winner, o.consistent), (Who::X, true));
        assert_eq!(o.axes["hierarchy"], Who::X);
        assert_eq!(o.axes["polish"], Who::Tie, "order 1 said x, order 2 said y: the axis is a tie");
        assert_eq!(o.axes["colour"], Who::Tie, "an axis nobody judged is a tie, not a win");
    }
    #[test]
    fn a_split_is_settled_by_a_third_call_two_of_three_else_tie() {
        use Who::*;
        assert_eq!([majority([X, Y, X]), majority([X, Y, Y]), majority([Tie, X, X]), majority([X, X, X])], [X, Y, X, X]);
        assert_eq!(majority([X, Y, Tie]), Tie, "three different answers: no majority");
        assert_eq!(majority([X, Tie, Tie]), Tie);
        // orders: ab said A (= x), ba said A (= y): a split. The third call shows A = y and says B (= x): x wins 2 of 3.
        let o = combine_with_third(&v(Side::A, &[]), &v(Side::A, &[]), Some((&v(Side::B, &[]), false)));
        assert_eq!((o.winner, o.consistent, o.third, o.by_third), (X, false, Some(X), true), "consistent is still the first two orders' agreement");
        // the third sides with the other: y wins
        let o = combine_with_third(&v(Side::A, &[]), &v(Side::A, &[]), Some((&v(Side::A, &[]), false)));
        assert_eq!((o.winner, o.third), (Y, Some(Y)));
        // the third says tie: x, y, tie -> tie, recorded
        let o = combine_with_third(&v(Side::A, &[]), &v(Side::A, &[]), Some((&v(Side::Tie, &[]), true)));
        assert_eq!((o.winner, o.third, o.by_third), (Tie, Some(Tie), true));
        // no third given: a split is a tie, as in v1
        let o = combine_with_third(&v(Side::A, &[]), &v(Side::A, &[]), None);
        assert_eq!((o.winner, o.third, o.by_third), (Tie, None, false));
        // the two orders agree: a third call, if one was made anyway, changes nothing
        let o = combine_with_third(&v(Side::A, &[]), &v(Side::B, &[]), Some((&v(Side::A, &[]), false)));
        assert_eq!((o.winner, o.consistent, o.third, o.by_third), (X, true, None, false));
        // a split where one order said tie: x, tie + third x -> x
        let o = combine_with_third(&v(Side::A, &[]), &v(Side::Tie, &[]), Some((&v(Side::A, &[]), true)));
        assert_eq!(o.winner, X);
    }
    #[test]
    fn the_third_order_comes_from_the_hash_and_both_orders_occur() {
        assert_eq!(third_a_is_x("ab12"), third_a_is_x("ab12"));
        let hs: Vec<bool> = ["00", "01", "02", "03", "0a", "0f"].iter().map(|h| third_a_is_x(h)).collect();
        assert!(hs.contains(&true) && hs.contains(&false));
    }
    #[test]
    fn v2_axes_carry_evidence_before_the_verdict_and_the_old_bare_form_still_reads() {
        let t = r#"{"per_axis": {"hierarchy": {"evidence": "the h1 is 40px, the body 14px", "winner": "A"}, "polish": "B", "colour": {"evidence": "same", "winner": "tie"}}, "winner": "A", "reason": "r"}"#;
        let p = parse_verdict(t).unwrap();
        assert_eq!((p.winner, p.per_axis["hierarchy"], p.per_axis["polish"], p.per_axis["colour"]), (Side::A, Side::A, Side::B, Side::Tie));
    }
    #[test]
    fn the_prompt_version_was_bumped_so_v1_rows_and_calibrations_do_not_mix_with_v2() {
        assert_eq!(JUDGE_VERSION, "design.judge/2");
        assert_ne!(pair_hash("a", &[vec![1]], "j"), { let mut h = Sha256::new(); h.update(b"design.judge/1"); h.update([0]); h.update(b"j"); h.update([0]); h.update(b"a"); h.update([0]); h.update(Sha256::digest([1u8])); hex::encode(h.finalize()) });
        assert_eq!(standing_with(Some(&Calibration { schema: 1, operator: true, judge: "j".into(), judge_version: "design.judge/1".into(), n: 30, agree: 30, sheet: "s".into(), measured_at: String::new() }), JUDGE_VERSION), Standing::Advisory);
    }
    #[test]
    fn the_registered_prompt_tells_the_judge_the_order_is_random_and_defines_tie() {
        let p = prompt("an ask", Path::new("/a/d.png"), Path::new("/a/p.png"), Path::new("/b/d.png"), Path::new("/b/p.png"));
        assert!(p.contains("RANDOM") && p.contains("carries no meaning"), "positional bias is named");
        assert!(p.contains("evidence") && p.contains("\"tie\""), "evidence per axis, tie defined");
        assert!(p.find("per_axis").unwrap() < p.rfind("\"winner\"").unwrap(), "the axes come before the verdict in the answer shape");
    }
    #[test]
    fn a_judge_that_always_says_a_is_position_bias_and_every_match_is_a_tie() {
        let o = combine(&v(Side::A, &[]), &v(Side::A, &[]));
        assert_eq!((o.winner, o.consistent, o.ab, o.ba), (Who::Tie, false, Who::X, Who::Y));
    }
    #[test]
    fn a_tie_in_one_order_only_is_a_tie_and_not_consistent_but_a_tie_in_both_is_consistent() {
        assert_eq!(combine(&v(Side::A, &[]), &v(Side::Tie, &[])).winner, Who::Tie);
        assert!(!combine(&v(Side::A, &[]), &v(Side::Tie, &[])).consistent);
        assert!(combine(&v(Side::Tie, &[]), &v(Side::Tie, &[])).consistent);
        let y = combine(&v(Side::B, &[]), &v(Side::A, &[]));
        assert_eq!((y.winner, y.consistent), (Who::Y, true));
    }
    #[test]
    fn the_verdict_is_read_from_the_last_json_object_and_an_answer_without_a_winner_is_unread() {
        let t = "Looking at both... {\"note\": 1}\nFinal:\n```json\n{\"winner\": \"b\", \"per_axis\": {\"Hierarchy\": \"A\", \"spacing\": \"tie\", \"colour\": \"nonsense\"}, \"reason\": \"B has a clear headline\"}\n```";
        let p = parse_verdict(t).expect("read");
        assert_eq!((p.winner, p.per_axis["hierarchy"], p.per_axis["spacing"]), (Side::B, Side::A, Side::Tie));
        assert!(!p.per_axis.contains_key("colour") && p.reason.contains("headline"));
        assert!(parse_verdict("I prefer the first one.").is_none());
        assert!(parse_verdict("{\"winner\": \"C\"}").is_none(), "a winner that is none of A, B, tie is unread");
        assert!(parse_verdict("{\"fixes\": []}").is_none());
    }

    // ----- who may judge
    fn seats(b: &[(Engine, &str)]) -> Vec<(Engine, String)> { b.iter().map(|(e, m)| (*e, m.to_string())).collect() }
    #[test]
    fn claude_against_composer_is_judged_by_cursors_grok_and_composer_against_grok_by_claude() {
        let all = |_: Engine, _: Option<&str>| true;
        assert_eq!(judge_seat_with(&seats(&[(Engine::ClaudeCode, "sonnet"), (Engine::Cursor, "composer-2.5")]), all), Some((Engine::Cursor, "cursor-grok-4.6-high")));
        assert_eq!(judge_seat_with(&seats(&[(Engine::Cursor, "composer-2.5"), (Engine::Cursor, "cursor-grok-4.6-medium")]), all), Some((Engine::ClaudeCode, "sonnet")), "grok-4.6-high is the builder's own family");
        assert_eq!(judge_seat_with(&seats(&[(Engine::ClaudeCode, "sonnet"), (Engine::Cursor, "cursor-grok-4.6-medium")]), all), Some((Engine::Cursor, "composer-2.5")));
    }
    #[test]
    fn a_judge_is_never_a_seat_that_is_off_limited_unmeasured_or_a_builders_own_family() {
        let b = seats(&[(Engine::ClaudeCode, "sonnet"), (Engine::Cursor, "composer-2.5")]);
        // Cursor's grok is out of quota: next in the walk that is vision-measured and not Claude/Composer is Gemini
        assert_eq!(judge_seat_with(&b, |e, m| !(e == Engine::Cursor && m == Some("cursor-grok-4.6-high"))), Some((Engine::Gemini, "gemini-3.8-flash-medium")));
        // Claude off, Gemini out of quota -> Codex is what is left
        assert_eq!(judge_seat_with(&b, |e, m| e == Engine::Codex || (e == Engine::Cursor && m == Some("composer-2.5"))), Some((Engine::Codex, "gpt-5.6-terra@medium")));
        // opus is in QA_GATE_ORDER but nothing measured its eyes: never chosen even when it is the only one on
        assert_eq!(judge_seat_with(&seats(&[(Engine::Cursor, "composer-2.5"), (Engine::Cursor, "cursor-grok-4.6-medium")]), |e, m| e == Engine::ClaudeCode && m == Some("opus")), None);
        assert_eq!(judge_seat_with(&b, |_, _| false), None, "nothing on -> the match is not run");
        // every Claude tier is Claude's family
        assert_eq!(family(Engine::ClaudeCode, "haiku"), family(Engine::ClaudeCode, "opus"));
        assert_eq!(family(Engine::Cursor, "cursor-grok-4.6-high"), family(Engine::Cursor, "grok-4.6-medium"));
        assert_ne!(family(Engine::Cursor, "composer-2.5"), family(Engine::Cursor, "cursor-grok-4.6-high"));
    }
    #[test]
    fn every_vision_seat_is_a_member_of_the_gate_order_so_a_judge_is_always_chosen_by_the_same_walk() {
        for s in VISION_SEATS { assert!(QA_GATE_ORDER.contains(s), "{s:?} is not in QA_GATE_ORDER"); }
    }

    // ----- the prompt, the cache, the budget
    #[test]
    fn the_prompt_quotes_the_registered_rubric_and_names_only_neutral_paths() {
        let d = Path::new("/j/1/a-desktop.png");
        let p = prompt("add a search box above the products", d, Path::new("/j/1/a-phone.png"), Path::new("/j/1/b-desktop.png"), Path::new("/j/1/b-phone.png"));
        assert!(p.contains(prompts::design_rubric().trim()), "the rubric is spliced whole");
        for f in neutral_names() { assert!(p.contains(&format!("/j/1/{f}")), "{f} missing"); }
        assert!(p.contains("add a search box above the products") && p.contains("\"winner\"") && !p.contains("{{"), "{p}");
        let low = p.to_lowercase();
        for banned in ["claude", "composer", "grok", "cursor", "sonnet", "opus", "gemini", "codex"] { assert!(!low.contains(banned), "the prompt names {banned}"); }
        assert!(low.contains("do not reward length"), "length-neutral");
    }
    #[test]
    fn the_cache_key_is_the_pixels_the_ask_the_judge_and_the_prompt_version_and_nothing_else() {
        let imgs = vec![vec![1u8, 2, 3], vec![4], vec![5, 6], vec![7]];
        let k = pair_hash("ask", &imgs, "cursor:grok");
        assert_eq!(k, pair_hash("ask", &imgs.clone(), "cursor:grok"));
        assert_ne!(k, pair_hash("ask2", &imgs, "cursor:grok"));
        assert_ne!(k, pair_hash("ask", &imgs, "claude:sonnet"));
        let mut swapped = imgs.clone(); swapped.swap(0, 2);
        assert_ne!(k, pair_hash("ask", &swapped, "cursor:grok"), "which page is first matters to the key");
        let mut moved = imgs.clone(); moved[3] = vec![8];
        assert_ne!(k, pair_hash("ask", &moved, "cursor:grok"), "one changed pixel is a new match");
        assert_eq!(k.len(), 64);
    }
    #[test]
    fn the_budget_refuses_a_match_that_would_pass_the_cap_and_spends_nothing_then() {
        let mut b = Budget { cap: 10_000, spent: 0 };
        assert!(b.try_spend(6_000) && b.try_spend(4_000));
        assert!(!b.try_spend(1) && b.spent == 10_000);
        let mut c = Budget { cap: 5_000, spent: 3_000 };
        assert!(!c.try_spend(2_001) && c.spent == 3_000 && c.try_spend(2_000));
    }
    #[test]
    fn a_match_costs_the_pictures_twice_one_read_per_order() {
        let dir = std::env::temp_dir().join(format!("antiloki-looks-tokens-{}", std::process::id())); std::fs::create_dir_all(&dir).unwrap();
        // a PNG header claiming 1280x800 is all image_tokens reads
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec(); png.extend_from_slice(&[0, 0, 0, 13]); png.extend_from_slice(b"IHDR"); png.extend_from_slice(&1280u32.to_be_bytes()); png.extend_from_slice(&800u32.to_be_bytes());
        let p = dir.join("a.png"); std::fs::write(&p, &png).unwrap();
        assert_eq!(match_tokens(&[p.as_path(), p.as_path()]), 2 * 2 * 1366);
        assert_eq!(match_tokens(&[Path::new("/no/such.png")]), 0);
    }

    // ----- look_pairs
    #[test]
    fn a_family_is_the_models_vendor_whichever_engine_hosts_it() {
        let all = |_: Engine, _: Option<&str>| true;
        // a NATIVE Grok builder excludes Cursor's Grok judge (and the walk goes on to Composer)
        assert_eq!(family(Engine::Grok, "grok-4.6"), family(Engine::Cursor, "cursor-grok-4.6-high"));
        assert_eq!(judge_seat_with(&seats(&[(Engine::Grok, "grok-4.6"), (Engine::ClaudeCode, "sonnet")]), all), Some((Engine::Cursor, "composer-2.5")));
        // a Claude model run THROUGH Cursor is Claude's family: Sonnet does not judge it
        assert_eq!(family(Engine::Cursor, "sonnet-4.5"), family(Engine::ClaudeCode, "sonnet"));
        assert_eq!(judge_seat_with(&seats(&[(Engine::Cursor, "sonnet-4.5"), (Engine::Cursor, "composer-2.5")]), all), Some((Engine::Cursor, "cursor-grok-4.6-high")));
        let only_claude_and_codex = |e: Engine, _: Option<&str>| matches!(e, Engine::ClaudeCode | Engine::Codex);
        assert_eq!(judge_seat_with(&seats(&[(Engine::Cursor, "claude-opus-4.1"), (Engine::Cursor, "composer-2.5")]), only_claude_and_codex), Some((Engine::Codex, "gpt-5.6-terra@medium")));
        // a GPT model through Cursor excludes the Codex judge; a Gemini one the Gemini judge
        assert_eq!(family(Engine::Cursor, "gpt-5.5-high"), family(Engine::Codex, "gpt-5.6-terra@medium"));
        assert_eq!(family(Engine::Cursor, "gemini-3.8-pro"), family(Engine::Gemini, "gemini-3.8-flash-medium"));
        // and the unrelated ones stay apart
        assert_ne!(family(Engine::Cursor, "composer-2.5"), family(Engine::ClaudeCode, "sonnet"));
        assert_ne!(family(Engine::Grok, "grok-4.6"), family(Engine::Codex, "gpt-5.6-terra@medium"));
        // every seat that may judge, in the gate's order (the bench crosses judges with pairs by using more than one)
        assert_eq!(judge_seats_with(&seats(&[(Engine::ClaudeCode, "sonnet"), (Engine::Cursor, "composer-2.5")]), all), vec![(Engine::Cursor, "cursor-grok-4.6-high"), (Engine::Gemini, "gemini-3.8-flash-medium"), (Engine::Codex, "gpt-5.6-terra@medium")]);
    }
    fn row(winner: &str, hash: &str) -> PairRow {
        PairRow { domain: "design".into(), ask: "sort-search".into(), x: "claude-code:sonnet".into(), y: "cursor:composer-2.5".into(), winner: winner.into(), consistent: true, order_ab: "x".into(), order_ba: "x".into(),
            judge: "cursor:cursor-grok-4.6-high".into(), prompt_ver: JUDGE_VERSION.into(), img_hash: hash.into(), per_axis: json!({"hierarchy": "x"}), reason: "r".into(), tokens: 5_500, usd: None, t: 1 }
    }
    #[test]
    fn a_match_is_stored_once_per_cache_key_and_read_back_whole() {
        let c = Connection::open_in_memory().unwrap();
        assert!(record_pair(&c, &row("claude-code:sonnet", "h1")).unwrap());
        assert!(!record_pair(&c, &row("cursor:composer-2.5", "h1")).unwrap(), "the same key is a cache hit, not a second match");
        assert!(record_pair(&c, &PairRow { ask: "cart-drawer".into(), ..row("tie", "h2") }).unwrap());
        let got = cached(&c, "h1").expect("cached");
        assert_eq!((got.winner.as_str(), got.tokens, got.usd, got.per_axis["hierarchy"].as_str()), ("claude-code:sonnet", 5_500, None, Some("x")));
        assert!(cached(&c, "nope").is_none());
        let ms = matches_of(&c, "design", None);
        assert_eq!(ms.iter().map(|m| m.score).collect::<Vec<_>>(), [1.0, 0.5]);
        assert!(matches_of(&c, "frontend", None).is_empty());
    }
    #[test]
    fn a_match_judged_under_an_older_prompt_is_stored_but_never_ranked() {
        let c = Connection::open_in_memory().unwrap();
        record_pair(&c, &row("claude-code:sonnet", "h1")).unwrap();
        record_pair(&c, &PairRow { prompt_ver: "design.judge/0".into(), ..row("cursor:composer-2.5", "h0") }).unwrap();
        assert_eq!(matches_of(&c, "design", None).len(), 1, "the calibration vouches for the current prompt only");
        assert!(cached(&c, "h0").is_some(), "the old verdict is still there");
    }
    #[test]
    fn only_the_calibrated_judge_is_ranked_and_a_pair_counts_once() {
        let c = Connection::open_in_memory().unwrap();
        let by = |j: &str, ask: &str, w: &str, h: &str| PairRow { judge: j.into(), ask: ask.into(), ..row(w, h) };
        record_pair(&c, &by("grok", "a1", "claude-code:sonnet", "h1")).unwrap();
        record_pair(&c, &by("grok", "a2", "tie", "h2")).unwrap();
        // a crossed judge the operator never calibrated against, on the same pair, and a second verdict of the primary on a1 (rebuilt pictures)
        record_pair(&c, &by("codex", "a1", "cursor:composer-2.5", "h3")).unwrap();
        record_pair(&c, &by("codex", "a3", "cursor:composer-2.5", "h4")).unwrap();
        record_pair(&c, &by("grok", "a1", "cursor:composer-2.5", "h5")).unwrap();
        let g = matches_of(&c, "design", Some("grok"));
        assert_eq!(g.iter().map(|m| m.score).collect::<Vec<_>>(), [0.5, 0.0], "grok only, one verdict per ask and pair (the latest on a1 stands)");
        assert_eq!(matches_of(&c, "design", Some("codex")).len(), 2);
        assert_eq!(matches_of(&c, "design", None).len(), 3, "advisory display: every judge, still one per ask and pair");
        assert!(matches_of(&c, "design", Some("nobody")).is_empty());
    }
    #[test]
    fn a_calibration_that_names_no_judge_does_not_route() {
        let mut c = cal(30, 30);
        assert_eq!(standing_with(Some(&c), JUDGE_VERSION), Standing::Routes);
        c.judge = String::new();
        assert_eq!(standing_with(Some(&c), JUDGE_VERSION), Standing::Advisory, "an older file, or one for no particular judge, vouches for nobody");
    }
    #[test]
    fn the_table_lives_beside_merge_scores_and_is_made_on_first_use() {
        let c = Connection::open_in_memory().unwrap();
        assert!(c.prepare("SELECT 1 FROM look_pairs").is_err());
        ensure_schema(&c).unwrap(); ensure_schema(&c).unwrap();
        assert!(c.prepare("SELECT domain, ask, x_cell, y_cell, winner, consistent, judge, prompt_ver, img_hash FROM look_pairs").is_ok());
    }

    // ----- the operator's calibration (D3)
    fn cal(agree: u32, n: u32) -> Calibration { Calibration { schema: 1, operator: true, judge: "j".into(), judge_version: JUDGE_VERSION.into(), n, agree, sheet: "s".into(), measured_at: "2026-10-01".into() } }
    #[test]
    fn the_plans_thresholds_are_80_and_65_percent_with_no_float_edge() {
        for (agree, n, want) in [(16, 20, Standing::Routes), (15, 20, Standing::Advisory), (13, 20, Standing::Advisory), (12, 20, Standing::NotTrusted), (20, 20, Standing::Routes), (0, 20, Standing::NotTrusted),
                                 (80, 100, Standing::Routes), (79, 100, Standing::Advisory), (65, 100, Standing::Advisory), (64, 100, Standing::NotTrusted), (24, 30, Standing::Routes), (19, 30, Standing::NotTrusted), (20, 30, Standing::Advisory)] {
            assert_eq!(standing_of(agree, n), want, "{agree}/{n}");
        }
    }
    #[test]
    fn fewer_than_twenty_labelled_pairs_never_allow_routing_however_well_they_agree() {
        assert_eq!(MIN_LABELS, 20, "the number the operator was told");
        for n in 0..20 { assert_eq!(standing_of(n, n), Standing::Advisory, "{n} of {n}"); assert_eq!(standing_of(0, n), Standing::Advisory, "0 of {n}: too few to condemn either"); }
        assert_eq!(standing_of(20, 20), Standing::Routes);
        // a corrupt file with a huge count must not overflow into a verdict (u32 `agree * 100` overflows from n = 43 million)
        assert_eq!(standing_of(u32::MAX, u32::MAX), Standing::Routes);
        assert_eq!(standing_of(u32::MAX / 2, u32::MAX), Standing::NotTrusted);
        assert_eq!(standing_of(60_000_000, 70_000_000), Standing::Routes, "85.7 %");
        assert_eq!(standing_of(21, 20), Standing::Advisory, "more agreements than pairs is a broken file, not a perfect judge");
    }
    #[test]
    fn no_file_is_advisory_and_a_good_one_routes() {
        assert_eq!(standing_with(None, JUDGE_VERSION), Standing::Advisory);
        assert_eq!(standing_with(Some(&cal(24, 30)), JUDGE_VERSION), Standing::Routes);
        assert_eq!(standing_with(Some(&cal(21, 30)), JUDGE_VERSION), Standing::Advisory);
        assert_eq!(standing_with(Some(&cal(15, 30)), JUDGE_VERSION), Standing::NotTrusted);
    }
    #[test]
    fn a_calibration_for_another_prompt_or_not_the_operators_or_an_unknown_schema_counts_for_nothing() {
        let mut c = cal(30, 30);
        assert_eq!(standing_with(Some(&c), "design.judge/3"), Standing::Advisory, "a changed judge prompt must be re-measured");
        c.operator = false; assert_eq!(standing_with(Some(&c), JUDGE_VERSION), Standing::Advisory);
        c.operator = true; c.schema = 2; assert_eq!(standing_with(Some(&c), JUDGE_VERSION), Standing::Advisory);
    }
    #[test]
    fn the_file_agreement_py_writes_is_read_and_a_stored_verdict_is_not_trusted_over_the_counts() {
        // the shape bench/design/agreement.py --operator writes (its --self-test prints the same keys); the extra keys are ignored
        let text = r#"{"schema":1,"operator":true,"measured_at":"2026-10-01T13:49:51","sheet":"SYN","picks_sha256":"x","judge_version":"design.judge/1","n":20,"agree":16,"agreement":0.8,"lo":0.58,"hi":0.92,
            "decision":"route","routing_allowed":true,"lint":{"n":20,"agree":20,"agreement":1.0},"axes":{"hierarchy":{"n":18,"agree":18,"agreement":1.0}},"judge":"j","per_judge":{"j":{"n":20,"agree":16}}}"#;
        let text = text.replace("design.judge/1", JUDGE_VERSION);
        let c = parse_calibration(&text).expect("parsed");
        assert_eq!((c.n, c.agree, c.judge_version.as_str(), c.operator), (20, 16, JUDGE_VERSION, true));
        assert_eq!(standing_with(Some(&c), JUDGE_VERSION), Standing::Routes);
        // a hand-edited verdict on a poor count does not route: the standing is recomputed from agree/n
        let edited = text.replace("\"agree\":16", "\"agree\":10");
        assert_eq!(standing_with(parse_calibration(&edited).as_ref(), JUDGE_VERSION), Standing::NotTrusted);
        assert!(parse_calibration("{\"schema\":1}").is_none() && parse_calibration("not json").is_none());
    }
    #[test]
    fn load_reads_the_file_and_an_absent_or_broken_one_is_none() {
        let d = std::env::temp_dir().join(format!("antiloki-calibration-{}", std::process::id())); std::fs::create_dir_all(&d).unwrap();
        let f = d.join("design-calibration.json");
        assert!(load_calibration_at(&f).is_none());
        std::fs::write(&f, "{ broken").unwrap(); assert!(load_calibration_at(&f).is_none());
        std::fs::write(&f, serde_json::to_string(&json!({"schema":1,"operator":true,"judge_version":JUDGE_VERSION,"n":22,"agree":18})).unwrap()).unwrap();
        assert_eq!(standing_with(load_calibration_at(&f).as_ref(), JUDGE_VERSION), Standing::Advisory, "no judge named");
        std::fs::write(&f, serde_json::to_string(&json!({"schema":1,"operator":true,"judge":"j","judge_version":JUDGE_VERSION,"n":22,"agree":18})).unwrap()).unwrap();
        assert_eq!(standing_with(load_calibration_at(&f).as_ref(), JUDGE_VERSION), Standing::Routes);
        assert!(calibration_path().ends_with("design-calibration.json"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
