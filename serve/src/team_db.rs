//! THE DB NODE (RFC 0011 §13.1): `sqlite` (a file in the workspace), `csv` (loaded into an in-memory SQLite, so it is queried with the same SQL), and the team's own
//! `bench.sqlite` (read-only, never writable). SAFE BY DEFAULT: read-only; a write needs the person's consent bound to the team; DDL always needs the person's yes;
//! parameterized statements only (no literal values in the SQL); a row limit and a timeout on every call; a write can be run dry first. Postgres is a later cargo feature.
use rusqlite::{params_from_iter, types::Value as Sql, Connection, OpenFlags};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limits { pub max_rows: usize, pub timeout_ms: u64 }
impl Default for Limits { fn default() -> Limits { Limits { max_rows: 1000, timeout_ms: 5000 } } }

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Table { pub columns: Vec<String>, pub rows: Vec<Vec<Value>>, pub truncated: bool }

// ── opening ───────────────────────────────────────────────────────────────────────────────────────────────────────────────

/// `kind` is `sqlite`, `csv` or `bench`. `name` is the file (inside the workspace) or, for `bench`, ignored. `write` asks for a writable sqlite (the caller has checked consent).
pub fn open_source(root: &str, team: &str, kind: &str, name: &str, write: bool) -> Result<Connection, String> {
    match kind {
        "sqlite" => {
            let p = in_workspace(root, name, &["sqlite", "sqlite3", "db"])?;
            let flags = if write { OpenFlags::SQLITE_OPEN_READ_WRITE } else { OpenFlags::SQLITE_OPEN_READ_ONLY } | OpenFlags::SQLITE_OPEN_NO_MUTEX;
            let c = Connection::open_with_flags(&p, flags).map_err(|e| format!("{name}: {e}"))?;
            if !write { let _ = c.execute_batch("PRAGMA query_only = ON;"); }
            Ok(c)
        }
        "csv" => {
            let p = in_workspace(root, name, &["csv"])?;
            let text = std::fs::read_to_string(&p).map_err(|e| format!("{name}: {e}"))?;
            let stem = p.file_stem().and_then(|s| s.to_str()).map(ident).unwrap_or_else(|| "data".into());
            load_csv(&text, &stem)
        }
        "bench" => {
            // the team's own dataset: readable so a team and the Improver can ask it questions, never writable
            let _ = crate::ledger::with_db(root, team, |_| Ok(()));
            let p = crate::ledger::bench_path(root, team);
            let c = Connection::open_with_flags(&p, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(|e| format!("bench.sqlite: {e}"))?;
            let _ = c.execute_batch("PRAGMA query_only = ON;");
            Ok(c)
        }
        other => Err(format!("a db is `sqlite`, `csv` or `bench` (not `{other}`; postgres comes later)")),
    }
}

fn in_workspace(root: &str, name: &str, exts: &[&str]) -> Result<PathBuf, String> {
    let p = Path::new(name.trim());
    if name.trim().is_empty() || p.is_absolute() || p.components().any(|c| !matches!(c, std::path::Component::Normal(_) | std::path::Component::CurDir)) { return Err(format!("`{name}`: a db file is a relative path inside the workspace")); }
    let ext = p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
    if !exts.contains(&ext.as_str()) { return Err(format!("`{name}`: expected a file ending {}", exts.join(", "))); }
    let base = Path::new(root).canonicalize().map_err(|e| e.to_string())?;
    let full = base.join(p);
    let real = full.canonicalize().map_err(|_| format!("there is no file `{name}` in the workspace"))?;
    if !real.starts_with(&base) { return Err(format!("`{name}` leads outside the workspace")); }
    Ok(real)
}

fn ident(s: &str) -> String {
    let mut t: String = s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c.to_ascii_lowercase() } else { '_' }).collect();
    if t.is_empty() || t.chars().next().map_or(false, |c| c.is_ascii_digit()) { t.insert(0, 'c'); }
    t
}

// ── csv ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────

/// A small CSV reader: quoted fields, `""` for a quote, commas and line breaks inside quotes, CRLF.
pub fn parse_csv(text: &str) -> Result<Vec<Vec<String>>, String> {
    let (mut rows, mut row, mut field, mut quoted, mut was_quoted) = (vec![], vec![], String::new(), false, false);
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        if quoted { if c == '"' { if it.peek() == Some(&'"') { field.push('"'); it.next(); } else { quoted = false; } } else { field.push(c); } continue; }
        match c {
            '"' if field.is_empty() && !was_quoted => { quoted = true; was_quoted = true; }
            ',' => { row.push(std::mem::take(&mut field)); was_quoted = false; }
            '\r' => {}
            '\n' => { row.push(std::mem::take(&mut field)); was_quoted = false; if !(row.len() == 1 && row[0].is_empty()) { rows.push(std::mem::take(&mut row)); } else { row.clear(); } }
            _ => field.push(c),
        }
    }
    if quoted { return Err("a quote was never closed".into()); }
    if !field.is_empty() || !row.is_empty() { row.push(field); rows.push(row); }
    Ok(rows)
}

/// A CSV into an in-memory SQLite table: the first row names the columns, each column is INTEGER, REAL or TEXT by what its values are.
pub fn load_csv(text: &str, table: &str) -> Result<Connection, String> {
    let rows = parse_csv(text)?; let head = rows.first().ok_or("the csv is empty")?;
    let w = head.len(); if w == 0 { return Err("the csv has no columns".into()); }
    if rows.iter().any(|r| r.len() != w) { return Err(format!("the csv rows do not all have {w} columns")); }
    let mut names: Vec<String> = vec![]; for h in head { let mut n = ident(h); while names.contains(&n) { n.push('_'); } names.push(n); }
    let kind = |i: usize| -> &'static str { let vals = rows[1..].iter().map(|r| r[i].trim()).filter(|v| !v.is_empty()); let v: Vec<&str> = vals.collect(); if v.is_empty() { "TEXT" } else if v.iter().all(|x| x.parse::<i64>().is_ok()) { "INTEGER" } else if v.iter().all(|x| x.parse::<f64>().is_ok()) { "REAL" } else { "TEXT" } };
    let kinds: Vec<&str> = (0..w).map(kind).collect();
    let c = Connection::open_in_memory().map_err(|e| e.to_string())?;
    c.execute_batch(&format!("CREATE TABLE {table} ({});", names.iter().zip(&kinds).map(|(n, k)| format!("{n} {k}")).collect::<Vec<_>>().join(", "))).map_err(|e| e.to_string())?;
    { let mut st = c.prepare(&format!("INSERT INTO {table} VALUES ({})", vec!["?"; w].join(", "))).map_err(|e| e.to_string())?;
      for r in &rows[1..] { let vals: Vec<Sql> = r.iter().zip(&kinds).map(|(v, k)| { let t = v.trim(); if t.is_empty() && *k != "TEXT" { Sql::Null } else { match *k { "INTEGER" => Sql::Integer(t.parse().unwrap_or(0)), "REAL" => Sql::Real(t.parse().unwrap_or(0.0)), _ => Sql::Text(v.clone()) } } }).collect(); st.execute(params_from_iter(vals)).map_err(|e| e.to_string())?; } }
    let _ = c.execute_batch("PRAGMA query_only = ON;");
    Ok(c)
}

/// A result table as CSV (`export` is the explicit way back out; it writes through the file type, never to an arbitrary path).
pub fn to_csv(t: &Table) -> String {
    let q = |s: &str| if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() };
    let cell = |v: &Value| match v { Value::Null => String::new(), Value::String(s) => q(s), other => other.to_string() };
    let mut out = t.columns.iter().map(|c| q(c)).collect::<Vec<_>>().join(",") + "\n";
    for r in &t.rows { out.push_str(&r.iter().map(cell).collect::<Vec<_>>().join(",")); out.push('\n'); }
    out
}

// ── what a statement is ───────────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stmt { Select, Write, Ddl }

/// Parameterized statements only: no quoted text, no number in the SQL (except after LIMIT/OFFSET), one statement, a kind we know. Values go in as `?`.
pub fn classify(sql: &str) -> Result<Stmt, String> {
    // comments are dropped first (a comment is not a way to hide a second statement)
    let mut clean = String::new(); let b: Vec<char> = sql.chars().collect(); let mut i = 0;
    while i < b.len() {
        if b[i] == '-' && b.get(i + 1) == Some(&'-') { while i < b.len() && b[i] != '\n' { i += 1; } continue; }
        if b[i] == '/' && b.get(i + 1) == Some(&'*') { i += 2; while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') { i += 1; } i += 2; continue; }
        clean.push(b[i]); i += 1;
    }
    let s = clean.trim().trim_end_matches(';').trim().to_string();
    if s.is_empty() { return Err("the statement is empty".into()); }
    if s.contains(';') { return Err("one statement at a time".into()); }
    if s.contains('\'') || s.contains('"') || s.contains('`') { return Err("parameterized statements only: no quoted text in the SQL; give values as `?` with `params`".into()); }
    let toks: Vec<String> = s.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.')).filter(|t| !t.is_empty()).map(|t| t.to_ascii_lowercase()).collect();
    for (i, t) in toks.iter().enumerate() {
        if t.chars().next().map_or(false, |c| c.is_ascii_digit() || c == '.') && !t.chars().all(|c| c.is_ascii_alphabetic() || c == '_') {
            let after_limit = i > 0 && matches!(toks[i - 1].as_str(), "limit" | "offset");
            if !after_limit { return Err(format!("parameterized statements only: `{t}` is a literal number in the SQL; give it as `?` with `params`")); }
        }
    }
    let first = toks.first().map(String::as_str).unwrap_or("");
    let has = |w: &str| toks.iter().any(|t| t == w);
    let writes = ["insert", "update", "delete", "replace"].iter().any(|w| has(w)); let ddl = ["create", "drop", "alter", "attach", "detach", "vacuum", "reindex", "pragma"].iter().any(|w| has(w));
    match first {
        "select" | "with" if !writes && !ddl => Ok(Stmt::Select),
        "select" | "with" => Err("a query that writes or changes the schema is not a query".into()),
        "insert" | "update" | "delete" | "replace" => if ddl { Err("not a plain write".into()) } else { Ok(Stmt::Write) },
        "create" | "drop" | "alter" | "attach" | "detach" | "vacuum" | "reindex" => Ok(Stmt::Ddl),
        "pragma" => Err("pragma is not allowed".into()),
        other => Err(format!("`{other}` is not a statement a db node runs (select, insert, update, delete, create, drop, alter)")),
    }
}

fn to_sql(v: &Value) -> Result<Sql, String> {
    Ok(match v { Value::Null => Sql::Null, Value::Bool(b) => Sql::Integer(*b as i64), Value::Number(n) => if let Some(i) = n.as_i64() { Sql::Integer(i) } else { Sql::Real(n.as_f64().unwrap_or(0.0)) }, Value::String(s) => Sql::Text(s.clone()), _ => return Err("a parameter is a text, a number, a bool or null".into()) })
}
fn from_sql(v: rusqlite::types::ValueRef) -> Value {
    match v { rusqlite::types::ValueRef::Null => Value::Null, rusqlite::types::ValueRef::Integer(i) => json!(i), rusqlite::types::ValueRef::Real(f) => json!(f), rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)), rusqlite::types::ValueRef::Blob(b) => json!(format!("<blob {} bytes>", b.len())) }
}

/// Stops the statement after the timeout: the guard interrupts the connection when it expires, and does nothing once the call is over.
struct Timeout { done: std::sync::Arc<std::sync::atomic::AtomicBool>, fired: std::sync::Arc<std::sync::atomic::AtomicBool> }
impl Timeout {
    fn start(c: &Connection, ms: u64) -> Timeout {
        use std::sync::{atomic::AtomicBool, Arc};
        let (done, fired) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let (h, d, f) = (c.get_interrupt_handle(), done.clone(), fired.clone());
        std::thread::spawn(move || { let end = std::time::Instant::now() + Duration::from_millis(ms); while std::time::Instant::now() < end { if d.load(std::sync::atomic::Ordering::SeqCst) { return; } std::thread::sleep(Duration::from_millis(5)); } if !d.load(std::sync::atomic::Ordering::SeqCst) { f.store(true, std::sync::atomic::Ordering::SeqCst); h.interrupt(); } });
        Timeout { done, fired }
    }
    fn finish(self) -> bool { self.done.store(true, std::sync::atomic::Ordering::SeqCst); self.fired.load(std::sync::atomic::Ordering::SeqCst) }
}

pub fn schema(c: &Connection) -> Result<Value, String> {
    let mut st = c.prepare("SELECT name FROM sqlite_master WHERE type IN ('table','view') AND name NOT LIKE 'sqlite_%' ORDER BY name").map_err(|e| e.to_string())?;
    let names: Vec<String> = st.query_map([], |r| r.get(0)).map_err(|e| e.to_string())?.collect::<Result<_, _>>().map_err(|e| e.to_string())?;
    let mut tables = vec![];
    for n in names {
        let mut p = c.prepare(&format!("PRAGMA table_info(\"{}\")", n.replace('"', ""))).map_err(|e| e.to_string())?;
        let cols: Vec<Value> = p.query_map([], |r| Ok(json!({ "name": r.get::<_, String>(1)?, "type": r.get::<_, String>(2)? }))).map_err(|e| e.to_string())?.collect::<Result<_, _>>().map_err(|e| e.to_string())?;
        tables.push(json!({ "name": n, "columns": cols }));
    }
    Ok(json!({ "tables": tables }))
}

/// A read: `select` only, with a row limit (one past it is read to say `truncated`) and a timeout.
pub fn query(c: &Connection, sql: &str, params: &[Value], lim: Limits) -> Result<Table, String> {
    if classify(sql)? != Stmt::Select { return Err("this is a write: a query is a select".into()); }
    let ps: Vec<Sql> = params.iter().map(to_sql).collect::<Result<_, _>>()?;
    let t = Timeout::start(c, lim.timeout_ms);
    let res = (|| -> Result<Table, String> {
        let mut st = c.prepare(sql).map_err(|e| e.to_string())?;
        let columns: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
        let mut rows = st.query(params_from_iter(ps)).map_err(|e| e.to_string())?;
        let (mut out, mut truncated) = (vec![], false);
        while let Some(r) = rows.next().map_err(|e| e.to_string())? { if out.len() >= lim.max_rows { truncated = true; break; } out.push((0..columns.len()).map(|i| from_sql(r.get_ref_unwrap(i))).collect()); }
        Ok(Table { columns, rows: out, truncated })
    })();
    if t.finish() { return Err(format!("timeout: the query ran over {} ms and was stopped", lim.timeout_ms)); }
    res
}

// ── consent (a write is the person's, bound to this team and this db) ────────────────────────────────────────────────────

pub fn consent_hash(team: &str, kind: &str, name: &str) -> String { let mut h = Sha256::new(); h.update(format!("antiloki-db-write\0{}\0{kind}\0{name}", crate::team_graph::slug(team)).as_bytes()); hex::encode(h.finalize()) }
fn consent_path(root: &str, team: &str) -> PathBuf { crate::team_graph_run::team_files(root, team).join("db-consent.json") }
pub fn grant_consent(root: &str, team: &str, kind: &str, name: &str) -> Result<(), String> {
    let p = consent_path(root, team); if let Some(d) = p.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
    let mut v: Value = std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
    v[format!("{kind}:{name}")] = json!(consent_hash(team, kind, name));
    std::fs::write(p, serde_json::to_string_pretty(&v).unwrap_or_default()).map_err(|e| e.to_string())
}
pub fn has_consent(root: &str, team: &str, kind: &str, name: &str) -> bool {
    std::fs::read_to_string(consent_path(root, team)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).map_or(false, |v| v[format!("{kind}:{name}")] == json!(consent_hash(team, kind, name)))
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Wrote { pub affected: usize, pub dry_run: bool }

/// A write or a schema change. `consent`: the person's consent bound to this team and db is on file. `person_ok`: the person says yes to THIS call (DDL always needs it).
/// `dry_run` runs it inside a savepoint and rolls it back, reporting the rows it would touch.
pub fn execute(c: &Connection, sql: &str, params: &[Value], lim: Limits, consent: bool, person_ok: bool, dry_run: bool) -> Result<Wrote, String> {
    let kind = classify(sql)?;
    match kind {
        Stmt::Select => return Err("this is a query: use query".into()),
        Stmt::Write if !consent => return Err("a write needs the person's consent, bound to this team and this db: it is not on file".into()),
        Stmt::Ddl if !person_ok => return Err("a change to the schema always needs the person's yes".into()),
        Stmt::Ddl if !consent => return Err("a change to the schema needs the person's consent for this db as well".into()),
        _ => {}
    }
    let ps: Vec<Sql> = params.iter().map(to_sql).collect::<Result<_, _>>()?;
    let t = Timeout::start(c, lim.timeout_ms);
    c.execute_batch("SAVEPOINT dbnode;").map_err(|e| e.to_string())?;
    let r = c.execute(sql, params_from_iter(ps));
    let fired = t.finish();
    let n = match r { Ok(n) => n, Err(e) => { let _ = c.execute_batch("ROLLBACK TO dbnode; RELEASE dbnode;"); return Err(if fired { format!("timeout: stopped after {} ms", lim.timeout_ms) } else { e.to_string() }); } };
    if dry_run { c.execute_batch("ROLLBACK TO dbnode; RELEASE dbnode;").map_err(|e| e.to_string())?; } else { c.execute_batch("RELEASE dbnode;").map_err(|e| e.to_string())?; }
    Ok(Wrote { affected: n, dry_run })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NTH: AtomicU64 = AtomicU64::new(0);
    fn ws() -> String { let n = NTH.fetch_add(1, Ordering::SeqCst); let d = std::env::temp_dir().join(format!("antiloki-db-{}-{n}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d.to_string_lossy().into_owned() }
    fn make(root: &str, file: &str, sql: &str) { let c = Connection::open(Path::new(root).join(file)).unwrap(); c.execute_batch(sql).unwrap(); }
    const SHOP: &str = "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL, price REAL); CREATE TABLE tags (item_id INTEGER, tag TEXT); INSERT INTO items VALUES (1,'Orchard',18.0),(2,'Winter',22.5),(3,'Spring',12.0);";

    #[test]
    fn schema_lists_tables_and_columns() {
        let root = ws(); make(&root, "shop.sqlite", SHOP);
        let c = open_source(&root, "t", "sqlite", "shop.sqlite", false).unwrap();
        let s = schema(&c).unwrap(); let tables = s["tables"].as_array().unwrap();
        assert_eq!(tables.iter().map(|t| t["name"].as_str().unwrap()).collect::<Vec<_>>(), vec!["items", "tags"]);
        assert_eq!(tables[0]["columns"], json!([{ "name": "id", "type": "INTEGER" }, { "name": "name", "type": "TEXT" }, { "name": "price", "type": "REAL" }]));
        // a file outside the workspace, or of another kind, is not opened
        assert!(open_source(&root, "t", "sqlite", "../x.sqlite", false).is_err() && open_source(&root, "t", "sqlite", "/etc/x.sqlite", false).is_err() && open_source(&root, "t", "sqlite", "shop.txt", false).is_err() && open_source(&root, "t", "postgres", "x", false).is_err());
    }

    #[test]
    fn query_is_read_only_by_default() {
        let root = ws(); make(&root, "shop.sqlite", SHOP);
        let c = open_source(&root, "t", "sqlite", "shop.sqlite", false).unwrap();
        let t = query(&c, "SELECT name, price FROM items WHERE price > ? ORDER BY price", &[json!(15)], Limits::default()).unwrap();
        assert_eq!(t.columns, vec!["name", "price"]); assert_eq!(t.rows, vec![vec![json!("Orchard"), json!(18.0)], vec![json!("Winter"), json!(22.5)]]); assert!(!t.truncated);
        assert!(query(&c, "DELETE FROM items", &[], Limits::default()).unwrap_err().contains("this is a write"));
        assert!(query(&c, "DELETE FROM items WHERE id = ?", &[json!(1)], Limits::default()).is_err(), "a query is a select");
        // even a write that got past the checks would meet a read-only connection
        assert!(c.execute("DELETE FROM items WHERE id = ?1", [1]).is_err(), "the connection itself is read-only");
        assert!(execute(&c, "DELETE FROM items WHERE id = ?", &[json!(1)], Limits::default(), true, true, false).is_err());
        assert_eq!(query(&c, "SELECT count(*) FROM items", &[], Limits::default()).unwrap().rows[0][0], json!(3));
        assert!(query(&c, "SELECT * FROM items; DROP TABLE items", &[], Limits::default()).is_err());
    }

    #[test]
    fn a_write_needs_the_persons_consent_bound_to_the_team() {
        let root = ws(); make(&root, "shop.sqlite", SHOP);
        let c = open_source(&root, "shop team", "sqlite", "shop.sqlite", true).unwrap();
        let ins = "INSERT INTO items (name, price) VALUES (?, ?)"; let p = [json!("Autumn"), json!(9.5)];
        assert!(!has_consent(&root, "shop team", "sqlite", "shop.sqlite"));
        assert!(execute(&c, ins, &p, Limits::default(), false, true, false).unwrap_err().contains("consent"), "even with the person's yes on the call, no consent on file");
        grant_consent(&root, "shop team", "sqlite", "shop.sqlite").unwrap();
        assert!(has_consent(&root, "shop team", "sqlite", "shop.sqlite")); assert!(!has_consent(&root, "other team", "sqlite", "shop.sqlite"), "bound to the team");
        assert!(!has_consent(&root, "shop team", "sqlite", "other.sqlite"), "and to the db");
        // a dry run touches nothing and says how many rows it would
        let w = execute(&c, "UPDATE items SET price = price + ? WHERE price < ?", &[json!(1), json!(20)], Limits::default(), true, false, true).unwrap(); assert_eq!((w.affected, w.dry_run), (2, true));
        assert_eq!(query(&c, "SELECT price FROM items WHERE id = ?", &[json!(1)], Limits::default()).unwrap().rows[0][0], json!(18.0), "the dry run was rolled back");
        let w = execute(&c, ins, &p, Limits::default(), true, false, false).unwrap(); assert_eq!(w.affected, 1);
        assert_eq!(query(&c, "SELECT count(*) FROM items", &[], Limits::default()).unwrap().rows[0][0], json!(4));
        // a consent from a team does not carry to a graph of another name
        assert_ne!(consent_hash("shop team", "sqlite", "shop.sqlite"), consent_hash("other team", "sqlite", "shop.sqlite"));
    }

    #[test]
    fn ddl_always_needs_the_persons_yes() {
        let root = ws(); make(&root, "shop.sqlite", SHOP);
        let c = open_source(&root, "t", "sqlite", "shop.sqlite", true).unwrap(); grant_consent(&root, "t", "sqlite", "shop.sqlite").unwrap();
        for ddl in ["CREATE TABLE extra (a INTEGER)", "DROP TABLE tags", "ALTER TABLE items ADD COLUMN stock INTEGER"] {
            assert_eq!(classify(ddl).unwrap(), Stmt::Ddl, "{ddl}");
            assert!(execute(&c, ddl, &[], Limits::default(), true, false, false).unwrap_err().contains("always needs the person's yes"), "consent is not enough: {ddl}");
        }
        assert_eq!(query(&c, "SELECT count(*) FROM tags", &[], Limits::default()).unwrap().rows[0][0], json!(0), "nothing was dropped");
        execute(&c, "CREATE TABLE extra (a INTEGER)", &[], Limits::default(), true, true, false).unwrap();
        assert!(schema(&c).unwrap()["tables"].as_array().unwrap().iter().any(|t| t["name"] == "extra"));
        assert!(execute(&c, "ATTACH DATABASE ? AS x", &[json!("/tmp/x.db")], Limits::default(), true, false, false).is_err(), "attach is a schema matter too");
        assert!(classify("PRAGMA journal_mode = WAL").is_err());
    }

    #[test]
    fn statements_are_parameterized_only() {
        for bad in ["SELECT * FROM items WHERE name = 'Orchard'", "SELECT * FROM items WHERE price > 15", "SELECT * FROM items WHERE name = \"Orchard\"", "SELECT 1", "SELECT * FROM items; SELECT * FROM tags", "SELECT * FROM items -- ; DROP TABLE items\n; DROP TABLE tags", "DELETE FROM items WHERE id = 7", "INSERT INTO items VALUES (9, ?, 1.5)", "", "VACUUM; select ?"] {
            assert!(classify(bad).is_err(), "{bad:?} must be refused");
        }
        for good in ["SELECT * FROM items WHERE name = ? AND price > ?", "select name from items order by price limit 5", "SELECT * FROM items LIMIT ? OFFSET 10", "WITH t AS (SELECT * FROM items WHERE price > ?) SELECT name FROM t", "SELECT * FROM items WHERE id IN (?, ?, ?) -- a comment with 'quotes' and 42\n", "SELECT t1.name FROM items t1 WHERE t1.id = ?;"] {
            assert!(classify(good).is_ok(), "{good:?}: {:?}", classify(good));
        }
        assert_eq!(classify("insert into items (name) values (?)").unwrap(), Stmt::Write); assert_eq!(classify("WITH x AS (SELECT ?) INSERT INTO items(name) SELECT * FROM x").unwrap_err(), "a query that writes or changes the schema is not a query");
        // a value that looks like SQL stays a value
        let root = ws(); make(&root, "shop.sqlite", SHOP); let c = open_source(&root, "t", "sqlite", "shop.sqlite", false).unwrap();
        assert!(query(&c, "SELECT * FROM items WHERE name = ?", &[json!("x' OR '1'='1")], Limits::default()).unwrap().rows.is_empty());
        assert!(query(&c, "SELECT * FROM items WHERE name = ?", &[], Limits::default()).is_err(), "a missing parameter is an error, not a guess");
        assert!(query(&c, "SELECT * FROM items WHERE name = ?", &[json!({ "a": 1 })], Limits::default()).is_err());
    }

    #[test]
    fn the_row_limit_and_the_timeout_apply() {
        let root = ws(); make(&root, "big.sqlite", "CREATE TABLE n (v INTEGER); WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c LIMIT 200) INSERT INTO n SELECT x FROM c;");
        let c = open_source(&root, "t", "sqlite", "big.sqlite", false).unwrap();
        let t = query(&c, "SELECT v FROM n ORDER BY v", &[], Limits { max_rows: 10, timeout_ms: 5000 }).unwrap(); assert_eq!(t.rows.len(), 10); assert!(t.truncated);
        let t = query(&c, "SELECT v FROM n ORDER BY v", &[], Limits::default()).unwrap(); assert_eq!(t.rows.len(), 200); assert!(!t.truncated);
        // a query that would run for minutes is stopped
        let started = std::time::Instant::now();
        let e = query(&c, "WITH RECURSIVE c(x) AS (SELECT ? UNION ALL SELECT x+? FROM c LIMIT ?) SELECT count(*) FROM c", &[json!(1), json!(1), json!(2_000_000_000i64)], Limits { max_rows: 10, timeout_ms: 60 }).unwrap_err();
        assert!(e.contains("timeout"), "{e}"); assert!(started.elapsed() < Duration::from_secs(5), "it was stopped, not waited for");
        // and the connection is fine afterwards
        assert_eq!(query(&c, "SELECT count(*) FROM n", &[], Limits::default()).unwrap().rows[0][0], json!(200));
    }

    #[test]
    fn csv_is_loaded_into_memory_and_queried_with_sql() {
        let root = ws(); std::fs::write(Path::new(&root).join("Price List.csv"), "Name,Price,Note\r\n\"Orchard, Morning\",18,\"says \"\"hi\"\"\"\nWinter,22.5,\nSpring,12,ok\n").unwrap();
        let c = open_source(&root, "t", "csv", "Price List.csv", false).unwrap();
        let s = schema(&c).unwrap(); assert_eq!(s["tables"][0]["name"], json!("price_list"));
        assert_eq!(s["tables"][0]["columns"], json!([{ "name": "name", "type": "TEXT" }, { "name": "price", "type": "REAL" }, { "name": "note", "type": "TEXT" }]));
        let t = query(&c, "SELECT name, price FROM price_list WHERE price > ? ORDER BY price DESC", &[json!(15)], Limits::default()).unwrap();
        assert_eq!(t.rows, vec![vec![json!("Winter"), json!(22.5)], vec![json!("Orchard, Morning"), json!(18.0)]], "numbers compare as numbers, quoted commas stay in the field");
        assert_eq!(query(&c, "SELECT note FROM price_list WHERE name = ?", &[json!("Orchard, Morning")], Limits::default()).unwrap().rows[0][0], json!("says \"hi\""));
        assert!(execute(&c, "DELETE FROM price_list WHERE price < ?", &[json!(15)], Limits::default(), true, true, false).is_err(), "a csv is read; writing back is an explicit export");
        // export is a csv text, and it reads back as the same table
        let out = to_csv(&t); assert!(out.starts_with("name,price\n") && out.contains("\"Orchard, Morning\",18"), "{out}");
        assert_eq!(parse_csv(&out).unwrap().len(), 3);
        assert!(parse_csv("a,b\n\"x").is_err()); assert!(load_csv("", "t").is_err()); assert!(load_csv("a,b\n1\n", "t").is_err());
    }

    #[test]
    fn bench_sqlite_is_readable_and_never_writable() {
        let root = ws();
        crate::ledger::with_db(&root, "shelf", |c| { c.execute("INSERT INTO calls (ts_ms, team, caller, source, outcome, bench, prompt_hash, answer_hash) VALUES (1,'shelf','x','endpoint','ok',1,'p','a')", []).unwrap(); Ok(()) }).unwrap();
        let c = open_source(&root, "shelf", "bench", "", false).unwrap();
        assert_eq!(query(&c, "SELECT count(*) FROM calls WHERE bench = ?", &[json!(1)], Limits::default()).unwrap().rows[0][0], json!(1), "the dataset can be asked");
        assert!(schema(&c).unwrap()["tables"].as_array().unwrap().iter().any(|t| t["name"] == "calls"));
        // never writable: not by a write the person consented to, not by the person's yes, not by the raw connection, not when `write` is asked for
        grant_consent(&root, "shelf", "bench", "").unwrap();
        assert!(execute(&c, "DELETE FROM calls WHERE bench = ?", &[json!(1)], Limits::default(), true, true, false).is_err());
        assert!(execute(&c, "CREATE TABLE evil (a INTEGER)", &[], Limits::default(), true, true, false).is_err());
        assert!(c.execute("DELETE FROM calls", []).is_err());
        let again = open_source(&root, "shelf", "bench", "", true).unwrap(); assert!(execute(&again, "UPDATE calls SET outcome = ? WHERE id = ?", &[json!("x"), json!(1)], Limits::default(), true, true, false).is_err(), "asking for write on bench changes nothing");
        assert_eq!(query(&c, "SELECT outcome FROM calls", &[], Limits::default()).unwrap().rows[0][0], json!("ok"));
    }
}
