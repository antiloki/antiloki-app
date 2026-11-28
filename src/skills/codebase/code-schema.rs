//! code-schema.rs — twin of skills/codebase/code-schema.mjs. NAMES ARE CONTRACT: `codeSchemaSql`
//! (no-db) · `ensureCodeSchema` (db-first) · `codeSchema` (default, db). The Blocks + Bindings code
//! graph DDL (EPIC codebase-atlas): code_nodes/code_edges plus 20 sibling tables (onboard jobs,
//! signals, digests, decorators, descriptions, design system, entry-points, user-stories, …).
// QUALITY: 80/100 — a 22-table ${schema}-prefixed DDL template (byte-verified against the JS
// template-literal output for schema='', 'code.', 'main.') + digestColumnsDdl() embedded exactly
// where the JS interpolates ${digestColumnsDdl()} inside the code_digests CREATE. ensureCodeSchema
// freezes the EXECUTED copy's datetime('now') to the frozen instant (cockpit/notebook/sources-schema
// precedent — the conform JS lane's db proxy does the same, so a later twin's INSERT sees a matching
// frozen created_at DEFAULT); the returned DDL text from codeSchemaSql stays verbatim. The idempotent
// column-adds (codeColumnMigrations in JS, not itself exported) are inlined as a swallow-errors loop.
use rusqlite::Connection;
use serde_json::{json, Value};

const SEG0: &str = "\nCREATE TABLE IF NOT EXISTS ";
const SEG1: &str = "code_nodes (\n  id             TEXT PRIMARY KEY,                 -- stable id (file node: path@origin)\n  origin         TEXT NOT NULL,                    -- 'atlas' | 'blueprint:<slug>@<v>'\n  path           TEXT NOT NULL,\n  type           TEXT NOT NULL,                    -- code vocab (NODE_TYPES)\n  name           TEXT,\n  signature      TEXT,                             -- real for atlas/authored; declared|null for blueprint intent\n  intent         TEXT,                             -- words: what it does (lazy AI for atlas; required for blueprint)\n  parent_id      TEXT,                             -- recursion: a Block inside a Block\n  span_json      TEXT,                             -- {file,start_line,end_line} — POINTER to the body, never the body\n  content_hash   TEXT,                             -- incremental-sync basis (atlas)\n  exported       INTEGER,                          -- 1 = part of the module public API (parse-time flag; the census basis)\n  version        INTEGER NOT NULL DEFAULT 1,\n  superseded_by  TEXT,                             -- history within an origin\n  became         TEXT,                             -- blueprint node -> the atlas node it became (forward link)\n  from_blueprint TEXT,                             -- atlas node -> the blueprint that created/changed it (reverse = change history)\n  created_at     TEXT NOT NULL DEFAULT (datetime('now'))\n);\nCREATE TABLE IF NOT EXISTS ";
const SEG2: &str = "code_edges (\n  id        INTEGER PRIMARY KEY AUTOINCREMENT,\n  origin    TEXT NOT NULL,\n  from_node TEXT NOT NULL,\n  to_node   TEXT NOT NULL,                         -- a blueprint edge MAY reference an atlas node id\n  type      TEXT NOT NULL,                         -- EDGE_TYPES\n  output    TEXT,\n  input     TEXT,\n  kind      TEXT                                   -- imports only: value | type | dynamic | reexport (NULL = value)\n);\nCREATE INDEX IF NOT EXISTS ";
const SEG3: &str = "idx_code_nodes_origin_path ON code_nodes (origin, path);\nCREATE INDEX IF NOT EXISTS ";
const SEG4: &str = "idx_code_edges_to          ON code_edges (to_node);\nCREATE INDEX IF NOT EXISTS ";
const SEG5: &str = "idx_code_edges_from        ON code_edges (from_node);          -- fan-OUT queries (metrics) were a full scan per file → 8s on React; indexed → 2.4s\nCREATE INDEX IF NOT EXISTS ";
const SEG6: &str = "idx_code_nodes_parent      ON code_nodes (origin, parent_id);  -- per-file child count + complexity spans (metrics) — same 3.3× win\nCREATE INDEX IF NOT EXISTS ";
const SEG7: &str = "idx_code_nodes_from_bp     ON code_nodes (from_blueprint);\nCREATE TABLE IF NOT EXISTS ";
const SEG8: &str = "code_onboard_jobs (\n  job_id       TEXT PRIMARY KEY,\n  root         TEXT NOT NULL,\n  phase        TEXT NOT NULL DEFAULT 'queued',   -- queued|scope|ingest|metrics|testmap|digest|memory|report|done\n  status       TEXT NOT NULL DEFAULT 'running',  -- running|done|error\n  profile_json TEXT,                             -- the final OnboardProfile (when done)\n  error        TEXT,\n  detail       TEXT,                             -- live sub-phase progress JSON (during ingest: the per-folder-family map-reduce — {families_total, family_index, folder, files_done, files_total, nodes})\n  created_at   TEXT NOT NULL DEFAULT (datetime('now')),\n  updated_at   TEXT NOT NULL DEFAULT (datetime('now'))\n);\n-- REBUILD-WIZARD: per-folder-family checkpoints of the partitioned rebuild. Each completed family\n-- persists its nodes (code_nodes, same savepoint) AND its pooled stitch inputs (pool_json: light\n-- symbol defs + imports + calls + flow extracts + health) — so a crashed/paused rebuild RESUMES\n-- from the last completed family instead of starting over. family_key = hash of the family's file\n-- list + sizes: a checkpoint is reused only when the family's files are bit-for-bit unchanged.\n-- SIGNALS (waves B/C/D, operator 2026-07-23): every DETERMINISTIC per-file/graph finding that isn't\n-- surface or smell — safety (secrets, swallowed errors, .only tests, debug leftovers, regex risk,\n-- a11y), debt (todos, unused imports, dead exports, clone pairs), history (bus factor, co-change),\n-- i18n. Written PROGRESSIVELY per family (lane categories) + at x-ray time (graph categories).\nCREATE TABLE IF NOT EXISTS ";
const SEG9: &str = "code_signals (\n  id         INTEGER PRIMARY KEY AUTOINCREMENT,\n  origin     TEXT NOT NULL,\n  category   TEXT NOT NULL,   -- safety | debt | history | i18n\n  kind       TEXT NOT NULL,   -- secret | swallowed-error | test-only | debug-leftover | regex-risk | a11y | todo | unused-import | dead-export | orphan-file | clone-pair | bus-factor | co-change | hardcoded-strings\n  severity   TEXT NOT NULL DEFAULT 'low',  -- high | medium | low\n  path       TEXT NOT NULL,\n  line       INTEGER,\n  detail     TEXT,\n  created_at TEXT NOT NULL DEFAULT (datetime('now'))\n);\n-- PUBLIC-API census + drift (Signals-A remainder): the exported surface (name+signature per module),\n-- snapshotted per rebuild with a diff vs the prior one — \"your public API changed: 3 signatures\".\nCREATE TABLE IF NOT EXISTS ";
const SEG10: &str = "code_api_snapshots (\n  id         INTEGER PRIMARY KEY AUTOINCREMENT,\n  origin     TEXT NOT NULL,\n  counts_json TEXT NOT NULL,\n  api_json   TEXT NOT NULL,\n  diff_json  TEXT,\n  created_at TEXT NOT NULL DEFAULT (datetime('now'))\n);\nCREATE TABLE IF NOT EXISTS ";
const SEG11: &str = "code_family_checkpoints (\n  origin      TEXT NOT NULL,\n  family_ix   INTEGER NOT NULL,\n  family_key  TEXT NOT NULL,\n  pool_json   TEXT NOT NULL,\n  status      TEXT NOT NULL DEFAULT 'done',\n  updated_at  TEXT NOT NULL DEFAULT (datetime('now')),\n  PRIMARY KEY (origin, family_ix)\n);\n-- EPIC-TYPECHECK P2: per-file parse health from the rebuild — files that SHOULD parse (have a language\n-- adapter) but didn't. status: 'fail' (unreadable / acorn threw — definitely broken) | 'degraded'\n-- (tree-sitter reported ERROR nodes — likely broken, though a few fragile grammars flag valid code).\n-- Rewritten on every full rebuild (atlas-ingest), so it always reflects the current tree.\nCREATE TABLE IF NOT EXISTS ";
const SEG12: &str = "code_parse_health (\n  origin     TEXT NOT NULL,\n  path       TEXT NOT NULL,\n  lang       TEXT,\n  status     TEXT NOT NULL,                    -- 'fail' | 'degraded'\n  detail     TEXT,\n  created_at TEXT NOT NULL DEFAULT (datetime('now')),\n  PRIMARY KEY (origin, path)\n);\n-- EPIC atlas-insights: a snapshot of the graded x-ray (score + smells, later + vulns + AI review),\n-- one row per run → the dashboard reads the latest; the time-series gives score/smell TRENDS.\nCREATE TABLE IF NOT EXISTS ";
const SEG13: &str = "code_reports (\n  id           INTEGER PRIMARY KEY AUTOINCREMENT,\n  origin       TEXT NOT NULL,\n  health_score REAL,\n  risk_score   REAL,\n  scores_json  TEXT,                             -- the full atlas-score result {project,by_file,top_risk}\n  smells_json  TEXT,                             -- [{kind,severity,nodes,detail}]\n  vulns_json   TEXT,                             -- Wave C (OSV) — nullable for now\n  review_md    TEXT,                             -- Wave D (AI review) — nullable for now\n  counts_json  TEXT,                             -- {files, health, risk, smells:{…}}\n  created_at   TEXT NOT NULL DEFAULT (datetime('now'))\n);\nCREATE INDEX IF NOT EXISTS ";
const SEG14: &str = "idx_code_reports_created ON code_reports (origin, id);\n-- EPIC atlas-insights: a persisted AI \"intelligence zoom\" of ONE file — a fixed-template JSON\n-- analysis (description / score / insights / vulnerabilities / suggestions). One row per generation;\n-- the dashboard reads the latest, and content_hash detects staleness vs the current atlas file node.\nCREATE TABLE IF NOT EXISTS ";
const SEG15: &str = "code_file_snapshots (\n  id            INTEGER PRIMARY KEY AUTOINCREMENT,\n  origin        TEXT NOT NULL,\n  path          TEXT NOT NULL,\n  content_hash  TEXT,                          -- file hash at analysis time (staleness basis)\n  snapshot_json TEXT NOT NULL,                 -- the validated template object\n  model_id      TEXT,\n  created_at    TEXT NOT NULL DEFAULT (datetime('now'))\n);\nCREATE INDEX IF NOT EXISTS ";
const SEG16: &str = "idx_code_file_snapshots_path ON code_file_snapshots (origin, path, id);\n-- EPIC data-stories: an animated E2E \"data journey\" played as a movie on the atlas graph (user\n-- clicks search → handler → service → db → …). ONE table + a direction (forward=from-user,\n-- backward=from-db). hops = the path as a small DAG: [{node_id,label?,sample_data?,branch?}].\n-- linked_test ties it to a real e2e test (level 3). status is computed at read-time (verified vs the\n-- live call graph), not stored, so it never goes stale. Levels: 1 authored · 2 atlas-verified ·\n-- 3 test-linked · 4 execution-traced (per-language, later).\nCREATE TABLE IF NOT EXISTS ";
const SEG17: &str = "code_data_stories (\n  id           INTEGER PRIMARY KEY AUTOINCREMENT,\n  origin       TEXT NOT NULL,\n  name         TEXT NOT NULL,\n  description  TEXT,\n  direction    TEXT NOT NULL DEFAULT 'forward',   -- forward=from-user | backward=from-db\n  hops_json    TEXT NOT NULL DEFAULT '[]',         -- [{node_id,label?,sample_data?,branch?}]\n  linked_test  TEXT,                               -- a test file path (level 3)\n  created_at   TEXT NOT NULL DEFAULT (datetime('now')),\n  updated_at   TEXT NOT NULL DEFAULT (datetime('now'))\n);\nCREATE INDEX IF NOT EXISTS ";
const SEG18: &str = "idx_code_data_stories ON code_data_stories (origin, id);\n-- EPIC atlas-scope: the per-workspace SCOPE config — which folders/extensions are in the atlas (the\n-- workspace IS the codebase). exclude/include are discover() globs; a rebuild applies the saved scope,\n-- so scope is part of the codebase DEFINITION (the driver / sync / entry-points all honor the same set).\n-- One row per origin — UPSERT on set.\nCREATE TABLE IF NOT EXISTS ";
const SEG19: &str = "code_scope (\n  origin       TEXT PRIMARY KEY,                  -- 'atlas'\n  exclude_json TEXT NOT NULL DEFAULT '[]',        -- folder/file globs to skip (ON TOP OF the built-in SKIP_DIRS)\n  include_json TEXT NOT NULL DEFAULT '[]',        -- if non-empty, a file must match at least one (the extension allowlist, as globs)\n  updated_at   TEXT NOT NULL DEFAULT (datetime('now'))\n);\n-- EPIC entry-points: the codebase's EXTERNAL SURFACE — where users / the network / storage touch the\n-- code. The complement to the internal call graph (the seams), and the seed layer for data-stories\n-- (a journey starts at a route/UI entry and ends at a db entry). Recomputed every ingest (live set).\n-- kind = ui|route|data; subtype = the concrete form (http-route|page-route|click|form-field|db-read|…).\n-- Heuristic + framework-specific, so each row carries a confidence. node_id ties it to its file node.\nCREATE TABLE IF NOT EXISTS ";
const SEG20: &str = "code_entry_points (\n  id          INTEGER PRIMARY KEY AUTOINCREMENT,\n  origin      TEXT NOT NULL,\n  kind        TEXT NOT NULL,                      -- 'ui' | 'route' | 'data'\n  subtype     TEXT NOT NULL,                      -- http-route | page-route | click | form-field | form-submit | db-read | db-write\n  node_id     TEXT,                               -- the code_nodes id it lives in (the file node)\n  path        TEXT NOT NULL,                      -- source file (POSIX rel)\n  label       TEXT NOT NULL,                      -- the route path / field name / table+verb — the human handle\n  http_method TEXT,                               -- GET|POST|… for routes; null otherwise\n  detail      TEXT,                               -- handler name / SQL summary / element — extra context\n  confidence  REAL NOT NULL DEFAULT 0.7,          -- heuristic match strength (0..1)\n  line        INTEGER,                            -- 1-based source line of the match\n  framework   TEXT,                               -- nestjs | express | tanstack | react | sqlite | …\n  created_at  TEXT NOT NULL DEFAULT (datetime('now'))\n);\nCREATE INDEX IF NOT EXISTS ";
const SEG21: &str = "idx_code_entry_points ON code_entry_points (origin, kind, path);\n-- EPIC entry-points: a point-in-time CAPTURE of the whole entry-point set (code history — the external\n-- surface over time). One row per rebuild → drift is visible (\"3 new routes\", \"this DB write vanished\").\n-- Mirrors code_file_snapshots / code_data_stories: the live table is current, this is the timeline.\nCREATE TABLE IF NOT EXISTS ";
const SEG22: &str = "code_entry_point_snapshots (\n  id                INTEGER PRIMARY KEY AUTOINCREMENT,\n  origin            TEXT NOT NULL,\n  captured_at       TEXT NOT NULL DEFAULT (datetime('now')),\n  counts_json       TEXT NOT NULL DEFAULT '{}',   -- {route:n, ui:n, data:n, total:n}\n  entry_points_json TEXT NOT NULL DEFAULT '[]',   -- the full set at capture time\n  diff_json         TEXT                          -- {added:[…], removed:[…]} vs the previous snapshot\n);\nCREATE INDEX IF NOT EXISTS ";
const SEG23: &str = "idx_code_ep_snapshots ON code_entry_point_snapshots (origin, id);\n-- EPIC codebase-digest: a DETERMINISTIC full code-review SNAPSHOT — one row per rebuild. Each column\n-- is ONE section (the column-name suffix encodes the render kind: _num/_pct/_json/_md), and the on-screen\n-- JSON is reflected back out of these columns (see digest-spec.mjs). Adding a column to the spec adds a\n-- section everywhere. The column list is GENERATED from the spec so the table and the spec never drift.\nCREATE TABLE IF NOT EXISTS ";
const SEG24A: &str = "code_digests (\n  id          INTEGER PRIMARY KEY AUTOINCREMENT,\n  origin      TEXT NOT NULL,\n  root        TEXT,\n";
const SEG24B: &str = ",\n  created_at  TEXT NOT NULL DEFAULT (datetime('now'))\n);\nCREATE INDEX IF NOT EXISTS ";
const SEG25: &str = "idx_code_digests ON code_digests (origin, id);\n-- EPIC codebase-decorators: VIRTUAL doc-cards for every code block, derived deterministically from the\n-- AST (NOT stored in the source). One row per block; wipe+repopulate per origin on rebuild (a current\n-- mirror, not a timeline). Overlays the read-only file view + the diagram, and composes into the docs.\nCREATE TABLE IF NOT EXISTS ";
const SEG26: &str = "code_decorators (\n  id           INTEGER PRIMARY KEY AUTOINCREMENT,\n  origin       TEXT NOT NULL,\n  path         TEXT NOT NULL,\n  name         TEXT,\n  kind         TEXT,                              -- function|component|hook|class|const\n  role         TEXT,                              -- ui|state|data|logic|external\n  line         INTEGER,\n  end_line     INTEGER,\n  signature    TEXT,\n  inputs_json  TEXT,                              -- [{name,kind}]\n  output_json  TEXT,                              -- {kind,label}\n  effects_json TEXT,                              -- [\"async\",\"dispatches\",\"iterates\",…]\n  calls_json   TEXT,                              -- the bindings it invokes\n  uses_json    TEXT,                              -- which imports it references\n  complexity   INTEGER,\n  flags_json   TEXT,                              -- [\"exported\",\"async\",\"component\",\"pure\",…]\n  summary      TEXT,                              -- the deterministic one-line \"what it does\"\n  steps_json   TEXT,                              -- the logic trace: [{depth,kind,text}] (branches/loops/sets/returns)\n  created_at   TEXT NOT NULL DEFAULT (datetime('now'))\n);\nCREATE INDEX IF NOT EXISTS ";
const SEG27: &str = "idx_code_decorators ON code_decorators (origin, path, line);\n-- EPIC code-descriptions: a per-FILE conceptual NARRATIVE, generated in 2 steps. Step 1 = a DETERMINISTIC\n-- gather (facts_json: the file's symbols / imports / dependents / role, from code_nodes/edges + decorators)\n-- run for EVERY file on rebuild — free + always ready. Step 2 = a LOW-AI essay (summary_md) that NARRATES\n-- those facts, on-demand + cached (or eagerly for files the driver touches). content_hash detects staleness\n-- vs the file node; one row per (origin, path), UPSERT.\nCREATE TABLE IF NOT EXISTS ";
const SEG28: &str = "code_descriptions (\n  origin       TEXT NOT NULL,\n  path         TEXT NOT NULL,\n  facts_json   TEXT NOT NULL DEFAULT '{}',         -- the deterministic gather (the grounding for the essay)\n  summary_md   TEXT,                                -- the LOW-AI conceptual narrative (null until generated)\n  model_id     TEXT,                                -- the model that wrote the essay\n  content_hash TEXT,                                -- the file's hash at gather time (staleness basis)\n  gathered_at  TEXT,\n  generated_at TEXT,                                -- when the essay was written\n  PRIMARY KEY (origin, path)\n);\n-- EPIC design-system: the de-facto DESIGN PALETTE harvested from the real UI — the DECLARED theme tokens\n-- (theme.ts: const C = {…}) UNION every style LITERAL used in a style={{…}} object across the FE, via a\n-- tree-sitter CST walk (parse-treesitter.parseToTree). declared=1 is a named theme token; declared=0 is a\n-- raw literal (the audit's drift candidate — a hardcoded value bypassing the palette). usage_count credits\n-- declared tokens by their member-expression references (C.brand). Wipe+repopulate per origin on rebuild.\nCREATE TABLE IF NOT EXISTS ";
const SEG29: &str = "code_design_tokens (\n  id           INTEGER PRIMARY KEY AUTOINCREMENT,\n  origin       TEXT NOT NULL,\n  token        TEXT,                                -- declared name ('C.brand', 'nav') or NULL for a raw literal\n  value        TEXT NOT NULL,                       -- resolved/literal value ('oklch(…)', '13', '11px')\n  category     TEXT NOT NULL,                       -- color|spacing|radius|font_size|font_family|font_weight|z|opacity|motion|other\n  declared     INTEGER NOT NULL DEFAULT 0,          -- 1 = a named theme token; 0 = a harvested literal\n  usage_count  INTEGER NOT NULL DEFAULT 0,\n  sources_json TEXT NOT NULL DEFAULT '[]',          -- [{path,line}] (capped)\n  created_at   TEXT NOT NULL DEFAULT (datetime('now'))\n);\nCREATE INDEX IF NOT EXISTS ";
const SEG30: &str = "idx_code_design_tokens ON code_design_tokens (origin, category, declared);\n-- EPIC design-system P1: the COMPONENT INVENTORY — every UI component (a Capitalized function/const that\n-- returns JSX), with its props, the child components it renders, and the design tokens it consumes. A\n-- faithful tree-sitter CST walk (NOT the atlas's React-awareness, which the generic tsx profile lacks).\n-- Wipe+repopulate per origin on rebuild. The component gallery's data.\nCREATE TABLE IF NOT EXISTS ";
const SEG31: &str = "code_design_components (\n  id           INTEGER PRIMARY KEY AUTOINCREMENT,\n  origin       TEXT NOT NULL,\n  name         TEXT NOT NULL,\n  path         TEXT NOT NULL,\n  line         INTEGER,\n  props_json   TEXT NOT NULL DEFAULT '[]',         -- prop names (destructured param)\n  renders_json TEXT NOT NULL DEFAULT '[]',         -- child component names rendered in its JSX\n  tokens_json  TEXT NOT NULL DEFAULT '[]',         -- design tokens it consumes (C.brand, …)\n  created_at   TEXT NOT NULL DEFAULT (datetime('now'))\n);\nCREATE INDEX IF NOT EXISTS ";
const SEG32: &str = "idx_code_design_components ON code_design_components (origin, path);\n-- EPIC design-system (snapshots): per-component/screen HTML+CSS SNAPSHOTS — the project's UI CONVERTED to\n-- static HTML (deterministic, no render) so the Design screen can show a Storybook-style gallery. kind:\n-- 'component'|'screen' carry html; one 'stylesheet' row carries the project's COMPILED css (shared by all\n-- iframes). Multi-framework: html comes from the per-framework converter (jsx-to-html for React; the HTML/SFC\n-- converters for Vue/Angular/Svelte are additive). Wipe+repopulate per origin on rebuild.\nCREATE TABLE IF NOT EXISTS ";
const SEG33: &str = "code_design_snapshots (\n  id          INTEGER PRIMARY KEY AUTOINCREMENT,\n  origin      TEXT NOT NULL,\n  kind        TEXT NOT NULL,                        -- 'component' | 'screen' | 'stylesheet'\n  name        TEXT,\n  path        TEXT,\n  framework   TEXT,                                 -- react | vue | angular | svelte\n  html        TEXT,                                 -- converted static HTML (null for the stylesheet row)\n  css         TEXT,                                 -- the project's compiled CSS (only on the stylesheet row)\n  created_at  TEXT NOT NULL DEFAULT (datetime('now'))\n);\nCREATE INDEX IF NOT EXISTS ";
const SEG34: &str = "idx_code_design_snapshots ON code_design_snapshots (origin, kind);\n-- UI-STORIES P2: recorded user stories — the UI-interaction sibling of code_data_stories (data-stories trace\n-- data through CODE; user-stories trace a user through the UI). Managed by antiloki, OUTSIDE the codebase.\n-- steps_json = ordered [{action:'navigate|click|type|submit', selector, value, route}]; source = record|scan.\nCREATE TABLE IF NOT EXISTS ";
const SEG35: &str = "code_user_stories (\n  id          INTEGER PRIMARY KEY AUTOINCREMENT,\n  origin      TEXT NOT NULL,\n  name        TEXT NOT NULL,\n  url         TEXT,                                 -- the base URL the story was recorded against\n  source      TEXT NOT NULL DEFAULT 'record',       -- 'record' | 'scan'\n  steps_json  TEXT NOT NULL,                        -- the ordered interaction steps\n  routes_json TEXT,                                 -- the distinct route trail (quick display)\n  step_count  INTEGER NOT NULL DEFAULT 0,\n  baseline_json TEXT,                               -- P5: last replay signature (per-step ok + routes + snap hashes) → cross-version diff\n  created_at  TEXT NOT NULL DEFAULT (datetime('now'))\n);\nCREATE INDEX IF NOT EXISTS ";
const SEG36: &str = "idx_code_user_stories ON code_user_stories (origin, created_at);\n-- RECORD & REPLAY: real API responses captured while the backend works, replayed as a TRUSTWORTHY stub (faithful\n-- real data, offline, deterministic — not synthesized). Keyed by (origin, method, url); upsert on record.\nCREATE TABLE IF NOT EXISTS ";
const SEG37: &str = "code_captured_responses (\n  origin       TEXT NOT NULL,\n  method       TEXT NOT NULL DEFAULT 'GET',\n  url          TEXT NOT NULL,\n  status       INTEGER NOT NULL DEFAULT 200,\n  content_type TEXT,\n  body         TEXT,\n  created_at   TEXT NOT NULL DEFAULT (datetime('now')),\n  PRIMARY KEY (origin, method, url)\n);\n-- RECORD CONTROL: a single-row out-of-band channel so the antiloki FE can stop the live (detached) record session\n-- WITHOUT relying on the in-page overlay button (which a page CSP can block). The record skill polls the stop column\n-- each tick; the FE flips it via ui.record {stop:true}. The active column lets the FE show a stop button only while a session runs.\nCREATE TABLE IF NOT EXISTS ";
const SEG38: &str = "code_record_control (\n  id         INTEGER PRIMARY KEY CHECK (id = 1),\n  stop       INTEGER NOT NULL DEFAULT 0,\n  active     INTEGER NOT NULL DEFAULT 0,\n  url        TEXT,\n  updated_at TEXT\n);\n";

fn schema_sql(schema: &str, digest_cols: &str) -> String {
    [
        SEG0, schema, SEG1, schema, SEG2, schema, SEG3, schema, SEG4, schema, SEG5, schema, SEG6, schema, SEG7, schema, SEG8, schema, SEG9, schema, SEG10, schema, SEG11, schema, SEG12, schema, SEG13, schema, SEG14, schema, SEG15, schema, SEG16, schema, SEG17, schema, SEG18, schema, SEG19, schema, SEG20, schema, SEG21, schema, SEG22, schema, SEG23, schema, SEG24A, digest_cols, SEG24B, schema, SEG25, schema, SEG26, schema, SEG27, schema, SEG28, schema, SEG29, schema, SEG30, schema, SEG31, schema, SEG32, schema, SEG33, schema, SEG34, schema, SEG35, schema, SEG36, schema, SEG37, schema, SEG38
    ].concat()
}

// JS `ensureCodeSchema(db, schema='')` / `codeSchemaSql(schema='')`: only string prefixes reach
// these in the fixtures (notebook-schema/cockpit-schema precedent) — missing/null arg is the JS
// default param's `undefined` case, matched by unwrap_or("").
fn digest_cols_text() -> String {
    crate::codebase_digest_spec::digestColumnsDdl(&Value::Null)
        .as_str()
        .unwrap_or("")
        .to_string()
}

pub fn codeSchemaSql(schema: &Value) -> Value {
    json!(schema_sql(schema.as_str().unwrap_or(""), &digest_cols_text()))
}

// JS `codeColumnMigrations(db, schema)`: idempotent ALTERs for columns added to the DDL after a
// table first existed on disk — each swallowed independently (column already present -> harmless).
fn column_migrations(db: &Connection, s: &str) {
    for sql in [
        format!("ALTER TABLE {s}code_decorators ADD COLUMN steps_json TEXT"),
        format!("ALTER TABLE {s}code_user_stories ADD COLUMN baseline_json TEXT"),
        format!("ALTER TABLE {s}code_onboard_jobs ADD COLUMN detail TEXT"),
        format!("ALTER TABLE {s}code_nodes ADD COLUMN exported INTEGER"),
        format!("ALTER TABLE {s}code_edges ADD COLUMN kind TEXT"),
    ] {
        let _ = db.execute_batch(&sql);
    }
}

// JS `ensureCodeSchema(db, schema='')` execs the DDL, runs the column migrations, then `return db;`
// (unlike notebook/cockpit-schema, which fall off the end -> undefined). The conform JS lane's
// normalizeResult walks the returned node:sqlite DatabaseSync's own enumerable properties
// (Object.keys(db) === ['isOpen','isTransaction','limits']) — a FIXED shape for a freshly-opened
// connection (isOpen=true, isTransaction=false, limits = SQLite's compile-time SQLITE_LIMIT_*
// defaults), independent of the schema/prefix executed. Transcribed as a literal so the twin
// matches byte-for-byte without a live rusqlite equivalent of that introspection surface.
// FROZEN-CLOCK MIRROR: only the EXECUTED DDL copy pins datetime('now') to the frozen instant —
// codeSchemaSql's returned text (above) stays verbatim (it is emitted, not executed).
fn db_handle_shape() -> Value {
    json!({
        "isOpen": true,
        "isTransaction": false,
        "limits": {
            "length": 1000000000,
            "sqlLength": 1000000000,
            "column": 2000,
            "exprDepth": 1000,
            "compoundSelect": 500,
            "vdbeOp": 250000000,
            "functionArg": 1000,
            "attach": 10,
            "likePatternLength": 50000,
            "variableNumber": 32766,
            "triggerDepth": 1000
        }
    })
}

pub fn ensureCodeSchema(db: &Connection, schema: &Value) -> Value {
    let s = schema.as_str().unwrap_or("").to_string();
    let sql = schema_sql(&s, &digest_cols_text()).replace("datetime('now'", &format!("datetime({}", crate::clock::now_sql_inner()));
    let _ = db.execute_batch(&sql);
    column_migrations(db, &s);
    db_handle_shape()
}

// JS default export `codeSchema(_input, context)`: `if (!db) throw err.contextMissing('code-schema',
// 'db')` — the db lane always injects a connection, so that guard is unreachable and not transcribed
// (playbook rule 9). ensureCodeSchema's return value is discarded here (JS: `ensureCodeSchema(db);`
// as a bare statement), so the db-handle-shape quirk above does not leak into this fn's own return.
pub fn codeSchema(_input: &Value, db: &Connection) -> Result<Value, String> {
    ensureCodeSchema(db, &Value::Null);
    Ok(json!({ "ok": true, "tables": ["code_nodes", "code_edges"] }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_names(c: &Connection, master: &str) -> Vec<String> {
        c.prepare(&format!(
            "SELECT name FROM {master} WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
        ))
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    }

    #[test]
    fn schema_sql_shape_and_prefix() {
        let base = codeSchemaSql(&json!(""));
        let s = base.as_str().unwrap();
        // was 22366 byte/char-exact with the JS template-literal output; +118 for code_edges.kind
        // (2026-09-12), the first DDL change since the JS lane was retired.
        assert_eq!(s.chars().count(), 22484);
        assert!(s.contains("  kind      TEXT                                   -- imports only: value | type | dynamic | reexport"));
        assert!(s.starts_with("\nCREATE TABLE IF NOT EXISTS code_nodes ("));
        assert!(s.ends_with("  updated_at TEXT\n);\n"));
        assert!(s.contains("CREATE TABLE IF NOT EXISTS code_edges ("));
        assert!(s.contains("CREATE TABLE IF NOT EXISTS code_record_control ("));
        assert!(s.contains("CREATE INDEX IF NOT EXISTS idx_code_nodes_origin_path ON code_nodes"));
        // digestColumnsDdl() embedded verbatim inside code_digests, exactly where JS interpolates it
        assert!(s.contains("  root        TEXT,\n  total_files_num REAL,\n  total_symbols_num REAL,"));
        assert!(s.contains("narrative_md TEXT,\n  created_at  TEXT NOT NULL DEFAULT (datetime('now'))\n);\nCREATE INDEX IF NOT EXISTS idx_code_digests"));
        assert!(s.contains("DEFAULT (datetime('now'))")); // literal in the RETURNED text, not frozen
        assert!(!s.contains("${schema}"));
        assert!(!s.contains('\r'));

        // ${schema} interpolates at exactly the 38 name positions (table names + index names)
        let pre = codeSchemaSql(&json!("code."));
        let p = pre.as_str().unwrap();
        assert_eq!(p.chars().count(), s.chars().count() + 5 * 38);
        assert!(p.contains("CREATE TABLE IF NOT EXISTS code.code_nodes ("));
        assert!(p.contains("CREATE INDEX IF NOT EXISTS code.idx_code_digests ON code_digests"));
        assert!(p.contains("CREATE TABLE IF NOT EXISTS code.code_digests ("));
    }

    #[test]
    fn ensure_creates_all_tables_and_freezes_created_at() {
        let c = Connection::open_in_memory().unwrap();
        assert_eq!(ensureCodeSchema(&c, &json!("")), db_handle_shape());
        let tables = table_names(&c, "sqlite_master");
        assert_eq!(tables.len(), 22);
        assert!(tables.contains(&"code_nodes".to_string()));
        assert!(tables.contains(&"code_digests".to_string()));
        assert!(tables.contains(&"code_record_control".to_string()));
        // the ALTER-added columns (self-heal path) are present even on a fresh create
        let cols: Vec<String> = c
            .prepare("SELECT name FROM pragma_table_info('code_decorators')")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert!(cols.iter().any(|x| x == "steps_json"));
        // idempotent — running again does not error
        assert_eq!(ensureCodeSchema(&c, &json!("")), db_handle_shape());
        // frozen-clock mirror: an INSERT's created_at DEFAULT fires at the pinned instant
        c.execute("INSERT INTO code_nodes (id, origin, path, type) VALUES ('n1','atlas','a.js','file')", [])
            .unwrap();
        let created_at: String = c
            .query_row("SELECT created_at FROM code_nodes WHERE id='n1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(created_at, "2026-01-01 00:00:00");
    }

    #[test]
    fn ensure_targets_attached_code_db() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS code").unwrap();
        assert_eq!(ensureCodeSchema(&c, &json!("code.")), db_handle_shape());
        assert_eq!(table_names(&c, "code.sqlite_master").len(), 22);
        assert!(table_names(&c, "sqlite_master").is_empty()); // main db untouched
    }

    #[test]
    fn skill_ensures_and_returns_ok() {
        let c = Connection::open_in_memory().unwrap();
        let r = codeSchema(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "tables": ["code_nodes", "code_edges"] }));
        assert_eq!(table_names(&c, "sqlite_master").len(), 22);
    }
}
