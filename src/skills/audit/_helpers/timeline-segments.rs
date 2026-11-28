//! timeline-segments.rs — twin of skills/audit/_helpers/timeline-segments.mjs. NAMES ARE
//! CONTRACT: `computeDigitalPrint` · `timelinePathFromDb` · `legacyDirFor` · `chainLayout` ·
//! `chainTableRef` · `canonicalRowsDigest` · `gzipSegmentFile` · `gunzipToTemp` · `deviceId` ·
//! `discoverSegments` · `nextSegmentSeq`. `SEGMENT_BOUNDARY_OP`/`ROWS_DIGEST_ALGO` are plain data
//! consts (not units — census counts functions only), transcribed here as Rust consts. `dirname`/
//! `join` are bare node:path re-exports (not functions defined by this module — census excludes
//! them too).
//!
//! `computeDigitalPrint` duplicates the sha256-hex formula that `with-audit.rs` and
//! `verify-chain.rs` ALSO duplicate locally (the JS source's own comment: "Duplicated
//! deliberately so the hot write path (with-audit) keeps its own copy untouched" — each Rust twin
//! follows the same precedent rather than sharing one fn across modules).
//!
//! `computeDigitalPrint` joins its 9 fields with JS `Array.prototype.join('|')`, which — UNLIKE a
//! template literal — collapses BOTH `null` and `undefined` elements to `''` (not the words
//! "null"/"undefined"). `str_qq` already has that exact "None|Null → ''" behavior, so every field
//! here uses `str_qq`, not `tmpl` (contrast `canonicalRowsDigest` below, which the JS source
//! builds via template-literal interpolation for id/ts/actor/operation/entity_type/entity_id/
//! prev_print/digital_print — those use `tmpl` — and only wraps before_json/after_json/payload in
//! `?? ''`, i.e. `str_qq`).
//!
//! `chainLayout`/`chainTableRef`/`timelinePathFromDb` are db-first helpers whose OUTPUT depends on
//! which schemas/tables exist on the connection — but the crossing fixture format applies one
//! `schema`+`attach` pair to EVERY case in a unit's file (conform-db.mjs: `attachDbs`+`seedDb` run
//! once per case from the same top-level fixture fields). So each of these 3 units' fixture can
//! only exercise the ONE topology it was authored with; the other real-world layouts (verified
//! by inspection against the JS source, matching the JS module's own doc comment table) are not
//! independently reachable through this harness. `chainTableRef`'s fixture picks the 'both'
//! topology (its most-branchy path: shadow-empty-favors-main vs shadow-nonempty-is-ambiguous);
//! `chainLayout`/`timelinePathFromDb` pick the canonical 'timeline' topology (what every real
//! caller — timeline-rotate.mjs — actually runs against).
//!
//! `gzipSegmentFile`/`gunzipToTemp` are BESPOKE-NATIVE (no fixture/frontier arm): both do
//! mutating real fs I/O (create+delete a real file; mkdtemp a real scratch dir) with a
//! caller-supplied path, not a hardcoded root or a DB-derived one — conform-db's fixture model
//! (db seed → db_after diff) has no notion of filesystem side effects, and gzipSegmentFile's own
//! effect (deleting its source) makes it non-idempotent against a committed testdata file across
//! repeated CI runs. Proven instead by `#[cfg(test)]` native tests against real temp dirs, same
//! precedent as `god-driver-halt.rs`. Needs real gzip — `flate2` (miniz_oxide backend, pure Rust,
//! no system dep, same zero-system-dep ethos as rusqlite's `bundled` feature) added to Cargo.toml.
//!
//! `deviceId` shells out to the `hostname` command (std only, no new crate) rather than reading
//! `%COMPUTERNAME%`/`$HOSTNAME` — verified empirically on the dev box that `COMPUTERNAME` differs
//! in CASE from `os.hostname()` (`DANI` vs `dani`), while the `hostname` command's stdout matches
//! `os.hostname()` exactly. conform-db is a DIFFERENTIAL comparison (JS output vs Rust output on
//! the SAME machine, not a pinned expected value), so as long as both sides query the actual OS
//! hostname mechanism they agree regardless of what the real value is.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::OnceLock;

fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }

pub const SEGMENT_BOUNDARY_OP: &str = "sacred_timeline.segment.boundary";
pub const ROWS_DIGEST_ALGO: &str = "rows-v1-sha256";

fn segment_file_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"^timeline-(\d+)-.*\.sqlite(\.gz)?$").unwrap())
}

/// `export function computeDigitalPrint(prevPrint, event)` — sha256 hex of 9 fields joined by
/// `'|'` (Array.join semantics: null/undefined both collapse to `''`).
pub fn computeDigitalPrint(prev_print: &Value, event: &Value) -> Value {
    let content = [
        str_qq(Some(prev_print)),
        str_qq(event.get("ts")),
        str_qq(event.get("actor")),
        str_qq(event.get("operation")),
        str_qq(event.get("entity_type")),
        str_qq(event.get("entity_id")),
        str_qq(event.get("before_json")),
        str_qq(event.get("after_json")),
        str_qq(event.get("payload")),
    ]
    .join("|");
    json!(hex::encode(Sha256::digest(content.as_bytes())))
}

/// `export function timelinePathFromDb(db)` — the on-disk path of the attached 'timeline' db, or
/// null for an in-memory timeline (or when no 'timeline' schema is attached at all).
pub fn timelinePathFromDb(db: &Connection) -> Value {
    let rows = crate::db_json::query_rows(db, "SELECT file FROM pragma_database_list WHERE name = 'timeline'", &[]);
    let file = rows
        .ok()
        .and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
        .and_then(|row| row.get("file").and_then(|f| f.as_str().map(String::from)));
    match file {
        Some(f) if !f.is_empty() => json!(f),
        _ => Value::Null,
    }
}

// join(dbDir, 'legacy_timeline') on this Windows dev box: Node's path.win32.join normalizes ALL
// separators to '\' (verified: join('C:/a/b','legacy_timeline') → 'C:\\a\\b\\legacy_timeline',
// even though the base used '/'). std::path::Path::join does NOT do that normalization (it only
// inserts a Windows '\' at the join boundary, leaving the base's own '/' chars untouched) — so a
// plain PathBuf::join would silently diverge from Node's output string. This replicates Node's
// join(base, seg) exactly for the 2-segment case this module needs.
#[cfg(windows)]
fn js_path_join2(base: &str, seg: &str) -> String {
    let b = base.replace('/', "\\");
    let b = b.trim_end_matches('\\');
    if b.is_empty() { seg.to_string() } else { format!("{b}\\{seg}") }
}
#[cfg(not(windows))]
fn js_path_join2(base: &str, seg: &str) -> String {
    let b = base.trim_end_matches('/');
    if b.is_empty() { format!("/{seg}") } else { format!("{b}/{seg}") }
}

/// `export function legacyDirFor(dbDir)` — `join(dbDir, 'legacy_timeline')`.
pub fn legacyDirFor(db_dir: &Value) -> Value {
    json!(js_path_join2(&str_qq(Some(db_dir)), "legacy_timeline"))
}

fn has_table(db: &Connection, schema: &str) -> bool {
    let sql = format!("SELECT 1 FROM {schema}.sqlite_master WHERE type='table' AND name='sacred_timeline'");
    match crate::db_json::query_rows(db, &sql, &[]) {
        Ok(v) => v.as_array().map_or(false, |a| !a.is_empty()),
        Err(_) => false, // schema not attached on this connection
    }
}

/// `export function chainLayout(db)` — where the chain ACTUALLY lives on this connection.
pub fn chainLayout(db: &Connection) -> Value {
    let in_timeline = has_table(db, "timeline");
    let in_main = has_table(db, "main");
    json!(if in_timeline && in_main { "both" } else if in_timeline { "timeline" } else if in_main { "legacy-main" } else { "none" })
}

/// `export function chainTableRef(db)` — qualified table reference for the chain's ACTIVE home.
pub fn chainTableRef(db: &Connection) -> Value {
    match chainLayout(db).as_str().unwrap_or("none") {
        "timeline" => json!("timeline.sacred_timeline"),
        "legacy-main" => json!("main.sacred_timeline"),
        "both" => {
            let shadow = crate::db_json::query_rows(db, "SELECT COUNT(*) n FROM timeline.sacred_timeline", &[])
                .ok()
                .and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
                .and_then(|row| row.get("n").and_then(|n| n.as_i64()));
            if shadow == Some(0) { json!("main.sacred_timeline") } else { Value::Null }
        }
        _ => Value::Null,
    }
}

/// `export function canonicalRowsDigest(conn, tableRef = 'sacred_timeline', batch = 500)` —
/// sha256 over every row in id order (id-ordered pagination — see FIX-VERIFY-CHAIN-OOM in
/// verify-chain.rs for why batching matters). A fixture-omitted trailing arg arrives here as
/// `Value::Null` (the harness's `arg()` default), which this treats the same way JS treats an
/// omitted/`undefined` positional arg: fall back to the JS default.
pub fn canonicalRowsDigest(db: &Connection, table_ref: &Value, batch: &Value) -> Value {
    let table = table_ref.as_str().filter(|s| !s.is_empty()).unwrap_or("sacred_timeline");
    let batch_n = batch.as_i64().unwrap_or(500);
    let mut hasher = Sha256::new();
    hasher.update(b"rows-v1");
    let sql = format!("SELECT * FROM {table} WHERE id > ? ORDER BY id ASC LIMIT ?");
    let mut last_id: i64 = 0;
    loop {
        let rows = match crate::db_json::query_rows(db, &sql, &[json!(last_id), json!(batch_n)]) {
            Ok(v) => v,
            Err(_) => break,
        };
        let arr = rows.as_array().cloned().unwrap_or_default();
        if arr.is_empty() {
            break;
        }
        for row in &arr {
            let piece = format!(
                "\n{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
                tmpl(row.get("id")),
                tmpl(row.get("ts")),
                tmpl(row.get("actor")),
                tmpl(row.get("operation")),
                tmpl(row.get("entity_type")),
                tmpl(row.get("entity_id")),
                str_qq(row.get("before_json")),
                str_qq(row.get("after_json")),
                str_qq(row.get("payload")),
                tmpl(row.get("prev_print")),
                tmpl(row.get("digital_print")),
            );
            hasher.update(piece.as_bytes());
            last_id = row.get("id").and_then(|v| v.as_i64()).unwrap_or(last_id);
        }
    }
    json!(hex::encode(hasher.finalize()))
}

/// `export function deviceId()` — hostname, or 'unknown-device' on any failure. See module doc
/// for why this shells out to `hostname` rather than reading an env var.
pub fn deviceId() -> Value {
    match crate::proc::cmd("hostname").output() {
        Ok(out) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if s.is_empty() { json!("unknown-device") } else { json!(s) }
        }
        _ => json!("unknown-device"),
    }
}

/// `export function discoverSegments(dbDir)` — archived segments oldest → newest, sorted by the
/// PARSED numeric seq (not lexically — '...-10000-...' must sort after '...-9999-...').
pub fn discoverSegments(db_dir: &Value) -> Value {
    let dir_str = match legacyDirFor(db_dir) {
        Value::String(s) => s,
        _ => String::new(),
    };
    let dir = PathBuf::from(&dir_str);
    if !dir.is_dir() {
        return json!([]);
    }
    let re = segment_file_re();
    let mut segs: Vec<(String, i64, bool)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(caps) = re.captures(&name) {
                let seq: i64 = caps[1].parse().unwrap_or(0);
                let gz = name.ends_with(".gz");
                segs.push((name, seq, gz));
            }
        }
    }
    segs.sort_by(|a, b| a.1.cmp(&b.1)); // stable — ties keep readdir order, like JS .sort()
    let arr: Vec<Value> = segs
        .into_iter()
        .enumerate()
        .map(|(idx, (file, seq, gz))| {
            let mut o = Map::new();
            o.insert("file".into(), json!(file));
            o.insert("seq".into(), json!(seq));
            o.insert("gz".into(), json!(gz));
            o.insert("idx".into(), json!(idx as i64));
            Value::Object(o)
        })
        .collect();
    json!(arr)
}

/// `export function nextSegmentSeq(dbDir)` — max existing seq + 1, else 1.
pub fn nextSegmentSeq(db_dir: &Value) -> Value {
    let arr = discoverSegments(db_dir).as_array().cloned().unwrap_or_default();
    if arr.is_empty() {
        return json!(1);
    }
    let max_seq = arr.iter().filter_map(|s| s.get("seq").and_then(|v| v.as_i64())).max().unwrap_or(0);
    json!(max_seq + 1)
}

// ---------------------------------------------------------------------------------------------
// BESPOKE-NATIVE (no conform-db fixture/frontier arm — see module doc): real mutating fs I/O.
// ---------------------------------------------------------------------------------------------

/// `export function gzipSegmentFile(path)` — `<path> → <path>.gz` (gzip level 6), removing the
/// plaintext. Returns the gz path.
pub fn gzipSegmentFile(path: &str) -> std::io::Result<String> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    let gz_path = format!("{path}.gz");
    let bytes = std::fs::read(path)?;
    let file = std::fs::File::create(&gz_path)?;
    let mut encoder = GzEncoder::new(file, Compression::new(6));
    encoder.write_all(&bytes)?;
    encoder.finish()?;
    std::fs::remove_file(path)?;
    Ok(gz_path)
}

fn mkdtemp(prefix: &str) -> std::io::Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir();
    for _ in 0..1000 {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let candidate = base.join(format!("{prefix}{}-{nanos}-{n}", std::process::id()));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "could not create a unique temp dir"))
}

/// `export function gunzipToTemp(gzPath)` — decompress to a scratch `<tmp>/segment.sqlite`.
/// JS returns `{ path, cleanup() }`; a closure isn't representable across this boundary, so this
/// returns `(segment_path, temp_dir)` — caller removes `temp_dir` (mirrors JS's `cleanup()`).
pub fn gunzipToTemp(gz_path: &str) -> std::io::Result<(PathBuf, PathBuf)> {
    use flate2::read::GzDecoder;
    let dir = mkdtemp("antiloki-seg-")?;
    let out = dir.join("segment.sqlite");
    let bytes = std::fs::read(gz_path)?;
    let mut decoder = GzDecoder::new(&bytes[..]);
    let mut decompressed = Vec::new();
    decoder.read_to_end(&mut decompressed)?;
    std::fs::write(&out, decompressed)?;
    Ok((out, dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_empty() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn compute_digital_print_is_deterministic_and_chains() {
        let event = json!({ "ts": "2026-01-01 00:00:00", "actor": "tester", "operation": "card.add", "entity_type": "card", "entity_id": "X1", "before_json": null, "after_json": "{\"a\":1}", "payload": null });
        let h1 = computeDigitalPrint(&json!("GENESIS"), &event);
        let h2 = computeDigitalPrint(&json!("GENESIS"), &event);
        assert_eq!(h1, h2);
        assert_eq!(h1.as_str().unwrap().len(), 64);
        let h3 = computeDigitalPrint(&json!("OTHER-PREV"), &event);
        assert_ne!(h1, h3);
    }

    #[test]
    fn compute_digital_print_null_and_missing_fields_both_join_as_empty() {
        let with_null = json!({ "ts": "t", "actor": "a", "operation": "o", "entity_type": "e", "entity_id": "i", "before_json": null, "after_json": null, "payload": null });
        let with_missing = json!({ "ts": "t", "actor": "a", "operation": "o", "entity_type": "e", "entity_id": "i" });
        assert_eq!(computeDigitalPrint(&json!("P"), &with_null), computeDigitalPrint(&json!("P"), &with_missing));
    }

    #[test]
    fn legacy_dir_for_normalizes_to_platform_separator() {
        let r = legacyDirFor(&json!("C:/dev/antiloki/.antiloki/db"));
        #[cfg(windows)]
        assert_eq!(r, json!("C:\\dev\\antiloki\\.antiloki\\db\\legacy_timeline"));
        #[cfg(not(windows))]
        assert_eq!(r, json!("C:/dev/antiloki/.antiloki/db/legacy_timeline"));
    }

    #[test]
    fn timeline_path_from_db_null_when_no_timeline_attached() {
        let c = db_empty();
        assert_eq!(timelinePathFromDb(&c), Value::Null);
    }

    #[test]
    fn timeline_path_from_db_null_for_in_memory_attached_timeline() {
        let c = db_empty();
        c.execute_batch("ATTACH ':memory:' AS timeline").unwrap();
        assert_eq!(timelinePathFromDb(&c), Value::Null);
    }

    #[test]
    fn chain_layout_all_four_states() {
        let none = db_empty();
        assert_eq!(chainLayout(&none), json!("none"));

        let main_only = db_empty();
        main_only.execute_batch("CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY)").unwrap();
        assert_eq!(chainLayout(&main_only), json!("legacy-main"));

        let timeline_only = db_empty();
        timeline_only.execute_batch("ATTACH ':memory:' AS timeline; CREATE TABLE timeline.sacred_timeline (id INTEGER PRIMARY KEY)").unwrap();
        assert_eq!(chainLayout(&timeline_only), json!("timeline"));

        let both = db_empty();
        both.execute_batch(
            "ATTACH ':memory:' AS timeline; \
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY); \
             CREATE TABLE timeline.sacred_timeline (id INTEGER PRIMARY KEY)",
        )
        .unwrap();
        assert_eq!(chainLayout(&both), json!("both"));
    }

    #[test]
    fn chain_table_ref_simple_layouts() {
        let none = db_empty();
        assert_eq!(chainTableRef(&none), Value::Null);

        let legacy = db_empty();
        legacy.execute_batch("CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY)").unwrap();
        assert_eq!(chainTableRef(&legacy), json!("main.sacred_timeline"));

        let canonical = db_empty();
        canonical.execute_batch("ATTACH ':memory:' AS timeline; CREATE TABLE timeline.sacred_timeline (id INTEGER PRIMARY KEY)").unwrap();
        assert_eq!(chainTableRef(&canonical), json!("timeline.sacred_timeline"));
    }

    #[test]
    fn chain_table_ref_both_empty_shadow_favors_main_nonempty_shadow_is_ambiguous() {
        let empty_shadow = db_empty();
        empty_shadow.execute_batch(
            "ATTACH ':memory:' AS timeline; \
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY); \
             INSERT INTO sacred_timeline (id) VALUES (1); \
             CREATE TABLE timeline.sacred_timeline (id INTEGER PRIMARY KEY)",
        )
        .unwrap();
        assert_eq!(chainTableRef(&empty_shadow), json!("main.sacred_timeline"));

        let both_populated = db_empty();
        both_populated.execute_batch(
            "ATTACH ':memory:' AS timeline; \
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY); \
             INSERT INTO sacred_timeline (id) VALUES (1); \
             CREATE TABLE timeline.sacred_timeline (id INTEGER PRIMARY KEY); \
             INSERT INTO timeline.sacred_timeline (id) VALUES (1)",
        )
        .unwrap();
        assert_eq!(chainTableRef(&both_populated), Value::Null);
    }

    fn digest_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT, \
             entity_id TEXT, before_json TEXT, after_json TEXT, payload TEXT, prev_print TEXT, digital_print TEXT)",
        )
        .unwrap();
        c
    }

    #[test]
    fn canonical_rows_digest_deterministic_content_sensitive_and_paginates() {
        let c = digest_db();
        for i in 1..=5 {
            c.execute(
                "INSERT INTO sacred_timeline (id, ts, actor, operation, entity_type, entity_id, before_json, after_json, payload, prev_print, digital_print) \
                 VALUES (?1, '2026-01-01 00:00:00', 'tester', 'card.add', 'card', ?2, NULL, NULL, NULL, 'PREV', ?3)",
                rusqlite::params![i, format!("card-{i}"), format!("HASH{i}")],
            )
            .unwrap();
        }
        let full_batch = canonicalRowsDigest(&c, &Value::Null, &Value::Null);
        let small_batch = canonicalRowsDigest(&c, &Value::Null, &json!(2)); // forces multiple SELECT pages
        assert_eq!(full_batch, small_batch);
        assert_eq!(full_batch.as_str().unwrap().len(), 64);

        let empty = digest_db();
        let empty_digest = canonicalRowsDigest(&empty, &Value::Null, &Value::Null);
        assert_ne!(empty_digest, full_batch);
    }

    #[test]
    fn canonical_rows_digest_qualified_table_ref() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS timeline; \
             CREATE TABLE timeline.sacred_timeline (id INTEGER PRIMARY KEY, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT, \
             entity_id TEXT, before_json TEXT, after_json TEXT, payload TEXT, prev_print TEXT, digital_print TEXT); \
             INSERT INTO timeline.sacred_timeline VALUES (1, '2026-01-01 00:00:00', 'tester', 'card.add', 'card', 'c1', NULL, NULL, NULL, 'GENESIS', 'H1')",
        )
        .unwrap();
        let r = canonicalRowsDigest(&c, &json!("timeline.sacred_timeline"), &Value::Null);
        assert_eq!(r.as_str().unwrap().len(), 64);
    }

    #[test]
    fn device_id_returns_a_nonempty_string() {
        let r = deviceId();
        assert!(r.as_str().is_some_and(|s| !s.is_empty()));
    }

    fn temp_dbdir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-timeline-segments-rs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn discover_segments_filters_sorts_numerically_and_assigns_idx() {
        let dbdir = temp_dbdir("discover");
        let legacy = dbdir.join("legacy_timeline");
        std::fs::create_dir_all(&legacy).unwrap();
        for name in ["timeline-0002-b.sqlite.gz", "timeline-0001-a.sqlite", "timeline-10000-e.sqlite.gz", "timeline-9999-d.sqlite.gz", "not-a-segment.txt", "timeline-abc-x.sqlite"] {
            std::fs::write(legacy.join(name), b"x").unwrap();
        }
        let r = discoverSegments(&json!(dbdir.to_string_lossy().into_owned()));
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 4);
        let seqs: Vec<i64> = arr.iter().map(|s| s["seq"].as_i64().unwrap()).collect();
        assert_eq!(seqs, vec![1, 2, 9999, 10000]); // numeric, not lexical, order
        let idxs: Vec<i64> = arr.iter().map(|s| s["idx"].as_i64().unwrap()).collect();
        assert_eq!(idxs, vec![0, 1, 2, 3]);
        assert_eq!(arr[0]["file"], json!("timeline-0001-a.sqlite"));
        assert_eq!(arr[0]["gz"], json!(false));
        assert_eq!(arr[3]["gz"], json!(true));
        let keys: Vec<&String> = arr[0].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["file", "seq", "gz", "idx"]);

        let _ = std::fs::remove_dir_all(&dbdir);
    }

    #[test]
    fn discover_segments_empty_when_no_legacy_dir() {
        let dbdir = temp_dbdir("no-legacy");
        let r = discoverSegments(&json!(dbdir.to_string_lossy().into_owned()));
        assert_eq!(r, json!([]));
        let _ = std::fs::remove_dir_all(&dbdir);
    }

    #[test]
    fn next_segment_seq_max_plus_one_else_one() {
        let with_segs = temp_dbdir("next-seq");
        let legacy = with_segs.join("legacy_timeline");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("timeline-0003-a.sqlite"), b"x").unwrap();
        std::fs::write(legacy.join("timeline-0007-b.sqlite.gz"), b"x").unwrap();
        assert_eq!(nextSegmentSeq(&json!(with_segs.to_string_lossy().into_owned())), json!(8));
        let _ = std::fs::remove_dir_all(&with_segs);

        let empty = temp_dbdir("next-seq-empty");
        assert_eq!(nextSegmentSeq(&json!(empty.to_string_lossy().into_owned())), json!(1));
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn gzip_then_gunzip_round_trips_content_and_removes_plaintext() {
        let dbdir = temp_dbdir("gzip-roundtrip");
        let src = dbdir.join("segment.sqlite");
        std::fs::write(&src, b"hello segment bytes").unwrap();

        let gz_path = gzipSegmentFile(src.to_str().unwrap()).unwrap();
        assert!(gz_path.ends_with(".gz"));
        assert!(!src.exists()); // plaintext removed
        assert!(PathBuf::from(&gz_path).exists());

        let (out_path, temp_dir) = gunzipToTemp(&gz_path).unwrap();
        assert_eq!(std::fs::read(&out_path).unwrap(), b"hello segment bytes");
        assert!(out_path.starts_with(&temp_dir));

        let _ = std::fs::remove_dir_all(&temp_dir);
        let _ = std::fs::remove_dir_all(&dbdir);
    }
}
