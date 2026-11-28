//! consistency-scan.rs — twin of skills/audit/consistency-scan.mjs. NAMES ARE CONTRACT:
//! `consistencyScan` (default, standard db lane).
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: this skill walks REAL directories on disk
//! (antiloki-BE/src, antiloki-ui/src, antiloki-core-rust/src/skills), reads REAL files
//! (AGENTS.md, CLAUDE.md, .mcp.json, napi/src/lib.rs, catalog/vault files referenced by an
//! absolute path) and sha256-hashes them — none of
//! that is fixture-seedable through conform-db's two-lane db-only harness (same class as
//! terminal-sandbox.rs / load-skill.rs / auth-sign.rs). Proven below via `#[cfg(test)]` against
//! a real temp directory tree mimicking the repo layout, reusing the `substrate_root()` +
//! `ANTILOKI_SUBSTRATE_ROOT` test-seam convention from tl-sandbox-create.rs, guarded by the
//! shared `crate::test_env_lock::ENV_LOCK` (process-global env var).
//!
//! 16 dimensions run, numbered D1-D18: D5 (MIGRATION_PENDING) and D15 (TUI_PRIVATE_INTERPRETATION)
//! are removed in place, each leaving the reasoning at its old slot. The numbering is deliberately
//! NOT closed up — D-numbers appear in findings people have already read, and renumbering would
//! silently repoint them. `lawsMd` is imported by the JS source but never
//! referenced in the function body (dead import) — not ported. `chainLayout`/`verifyChain`/
//! `listSkills` are cross-module calls into already-converted twins
//! (`crate::audit__helpers_timeline_segments::chainLayout`, `crate::audit_verify_chain::verifyChain`,
//! `crate::engine_load_skill::listSkills`), per the crossing playbook's reuse convention.
//! `BOOTSTRAP_ALLOWLIST` (D16) is NOT exported `pub` by law-hook-no-lone-skill-dispatch.rs, so
//! this twin carries its own verbatim copy (403 entries, transcribed from the same JS source
//! `skills/audit/law-hook-no-lone-skill-dispatch.mjs` both twins derive from) rather than
//! reaching into that module's private const — same "duplicated deliberately" precedent
//! documented in verify-chain.rs / timeline-segments.rs for `computeDigitalPrint`.
//!
//! `SUBSTRATE_ROOT` (roots.mjs: `resolve($ANTILOKI_SUBSTRATE_ROOT)` else `resolve(ENGINE_ROOT,
//! '..')`) has no Rust `import.meta.url` analogue — production default mirrors
//! tl-sandbox-create.rs's sibling-directories assumption: the substrate root is the PARENT of
//! `antiloki-core-rust`, which is where antiloki-BE, antiloki-ui and .antiloki all sit.
//! `KNOWLEDGE_DIR` = `<substrate_root>/.antiloki/knowledge` (both derived exactly as roots.mjs
//! derives them from `SUBSTRATE_ROOT` when `ANTILOKI_DATA_DIR`/`ANTILOKI_SUBSTRATE_ROOT` aren't
//! independently overridden — this twin doesn't need the `ANTILOKI_DATA_DIR` override since no
//! caller of this skill supplies it separately from `ANTILOKI_SUBSTRATE_ROOT`).
// QUALITY: 75/100 — all 16 live dimensions run against real fs; D6/D7 catalog/vault
// integrity and D16's allowlist-drift string-literal scan get lighter test coverage than the
// pure fs-walk dimensions (D13/D14/D17) given the surface area of this one file.

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn file_sha(abs: &Path) -> Option<String> {
    let bytes = std::fs::read(abs).ok()?;
    Some(hex::encode(Sha256::digest(&bytes)))
}

fn add(findings: &mut Vec<Value>, severity: &str, dimension: &str, message: String, evidence: Value) {
    findings.push(json!({
        "severity": severity,
        "dimension": dimension,
        "message": message,
        "evidence": evidence,
    }));
}

// roots.mjs: SUBSTRATE_ROOT = resolve($ANTILOKI_SUBSTRATE_ROOT) if set+truthy, else
// resolve(ENGINE_ROOT, '..') (antiloki-core's parent). CARGO_MANIFEST_DIR (antiloki-core-rust)
// shares the same parent as antiloki-core on this checkout layout — tl-sandbox-create.rs precedent.
fn substrate_root() -> PathBuf {
    if let Ok(v) = std::env::var("ANTILOKI_SUBSTRATE_ROOT") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .to_path_buf()
}
fn knowledge_dir() -> PathBuf { substrate_root().join(".antiloki").join("knowledge") }

// Bootstrap allowlist — verbatim copy of BOOTSTRAP_ALLOWLIST from
// law-hook-no-lone-skill-dispatch.mjs (see module doc: the sibling Rust twin's const isn't `pub`).
// `system.migrate.run`/`system.migrate.list` dropped with the migration runner, matching the trim
// the sibling twin took: D16 uses this as an EXCLUSION set, so an entry for an action that no
// longer exists can only suppress a warning that should be heard.
const BOOTSTRAP_ALLOWLIST: [&str; 401] = [
    "system.action.find", "system.agent.find", "system.skill.find",
    "system.heimdall.classify", "system.heimdall.validate", "system.heimdall.attribute",
    "system.heimdall.ledger.rebuild", "system.heimdall.overview", "system.heimdall.flipflops",
    "system.heimdall.ceiling.set", "system.heimdall.tag.add", "system.heimdall.tag.queue", "system.heimdall.aitag.run",
    "system.heimdall.aitag.batch", "system.heimdall.aitag.report", "system.heimdall.trust.set",
    "system.heimdall.prompt.tags", "system.heimdall.tag.assist", "system.heimdall.prompt.patch",
    "system.stats.overview", "system.pr.test", "system.driver.forensics", "system.gate.telemetry",
    "system.failure.feed", "system.model.scoreboard", "system.switcher.mode", "system.workspace.scripts",
    "system.workspace.runtime", "system.feature.inventory", "system.feature.wizard", "system.cost.openrouter",
    "system.law.list", "system.law.find", "system.law.check", "system.law.surface", "system.law.violations.list",
    "system.law.violation.record", "system.anatomy.snapshot", "system.anatomy.latest", "system.docs.agent_stages",
    "system.docs.anatomy", "system.docs.concurrency", "system.docs.driver", "system.docs.getting_started",
    "system.docs.index", "system.docs.man_compliance", "system.docs.rag", "system.docs.rag_flow",
    "system.docs.sandbox", "system.docs.defense", "system.docs.substrate_defense", "system.docs.autonomy_score",
    "man.audit.compliance", "man.audit.drift", "man.audit.drift.spec", "man.audit.truth.cross-check",
    "man.render.compliance.html", "system.prompt.lint", "system.consistency.scan", "system.consistency.scan.html",
    "system.file-write.coverage.audit", "system.codebase.ingest", "system.codebase.blast", "system.codebase.get",
    "system.codebase.typecheck.status", "system.codebase.typecheck.install", "system.codebase.typecheck.run",
    "system.codebase.parse.health", "system.codebase.lint.status", "system.codebase.lint.install",
    "system.codebase.lint.run", "system.bugfinder.run", "system.bugfinder.report", "system.bugfinder.classify",
    "system.bugfinder.schedule", "system.ultra.run", "system.ultra.report", "system.ultra.provenance",
    "system.constitution.law", "system.constitution.business.gen", "system.constitution.law.build",
    "system.constitution.packs", "system.constitution.pack.gen", "system.constitution.patterns",
    "system.constitution.mine", "system.constitution.estimate", "system.constitution.judge",
    "system.constitution.check", "system.constitution.neighborhood", "system.constitution.agents",
    "system.constitution.agent.run", "system.constitution.research.suggest", "system.constitution.editor",
    "system.constitution.paper", "system.constitution.class", "system.constitution.soliloquy", "system.tts.speak",
    "system.constitution.file", "system.constitution.report", "system.constitution.insights",
    "system.constitution.provenance", "system.constitution.brief", "system.constitution.classify", "system.ultra.deep",
    "system.codebase.verify", "system.codebase.sync", "system.codebase.refresh", "system.codebase.summarize",
    "system.codebase.digest", "system.codebase.digest.snapshot", "system.codebase.digest.latest",
    "system.codebase.decorate", "system.codebase.decorate.all", "system.codebase.decorators.doc",
    "system.codebase.docs.build", "system.codebase.docs.page", "system.codebase.docs.context",
    "system.codebase.docs.export", "system.codebase.docs.overview", "system.widget.generate", "system.widget.save",
    "system.codebase.describe", "system.codebase.describe.get", "system.codebase.describe.gather",
    "system.codebase.design.get", "system.codebase.design.rebuild", "system.codebase.design.guide",
    "system.codebase.design.snapshots", "system.codebase.design.capture", "system.codebase.ui.stories",
    "system.codebase.ui.story.delete", "system.codebase.ui.record", "system.codebase.ui.story.spec",
    "system.codebase.ui.story.replay", "system.codebase.ui.scan", "system.codebase.test.run",
    "system.codebase.test.surface", "system.codebase.test.smoke", "system.codebase.test.flow",
    "system.cockpit.session.create", "system.cockpit.session.get", "system.cockpit.overview",
    "system.cockpit.session.close", "system.cockpit.pane.add", "system.cockpit.pane.message",
    "system.cockpit.pane.update", "system.cockpit.pane.diff", "system.cockpit.pane.remove", "system.cockpit.promote",
    "system.cockpit.pane.claim", "system.cockpit.script.run", "system.risk.tag", "cockpit.pane.file.read",
    "cockpit.pane.file.write", "system.cockpit.git.graph", "system.cockpit.blackboard.post",
    "system.cockpit.blackboard.read", "system.cockpit.integrate", "system.cockpit.pr.open", "system.cockpit.pr.sync",
    "system.cockpit.pr.list", "system.cockpit.pr.analyze", "system.cockpit.pr.merge", "system.cockpit.context.preview",
    "system.cockpit.session.audit", "system.cockpit.pane.brief", "system.cockpit.session.narrate",
    "system.cockpit.plan.generate", "system.research.magazine.observe", "system.research.magazine.press",
    "system.research.course.press", "system.research.course.get", "system.cockpit.plan.judge",
    "system.cockpit.swarm.run", "system.cockpit.formation.run", "system.card.automerge.set", "system.naming.derive",
    "system.codebase.git.init", "system.codebase.regenerate", "system.codebase.regenerate.stage",
    "system.codebase.regenerate.merge", "system.codebase.run", "system.codebase.lint", "system.codebase.file_diagram",
    "system.codebase.scope", "system.codebase.scope.get", "system.codebase.scope.set",
    "system.codebase.entrypoints.extract", "system.codebase.entrypoints.get", "system.codebase.subgraph",
    "system.codebase.find", "system.codebase.testmap", "system.codebase.metrics", "system.codebase.history",
    "system.codebase.telemetry", "system.codebase.coverage", "system.codebase.review.history",
    "system.codebase.file.review.history", "system.codebase.signals.snapshot", "system.codebase.score",
    "system.codebase.smells", "system.codebase.report", "system.codebase.report.latest", "system.codebase.smell.card",
    "system.codebase.deps.audit", "system.codebase.insights.review", "system.codebase.file.read",
    "system.codebase.file.snapshot", "system.codebase.file.snapshot.get", "system.codebase.snapshot.all",
    "system.codebase.symbol.explain", "system.codebase.explain.snippet", "system.codebase.story.add",
    "system.codebase.story.list", "system.codebase.story.get", "system.codebase.story.update",
    "system.codebase.story.delete", "system.codebase.story.propose", "system.codebase.story.trace",
    "system.codebase.story.card", "system.codebase.onboard", "system.codebase.onboard.start",
    "system.codebase.onboard.status", "system.codebase.onboard.latest", "system.codebase.onboard.control",
    "system.codebase.rebuild.settings", "system.codebase.signals", "system.codebase.signals.scan",
    "system.codebase.integrate", "system.codebase.summarize.symbol", "system.codebase.review", "system.repl.direct",
    "system.deepsearch", "system.notebook.store", "system.notebook.scope", "system.notebook.ask",
    "system.notebook.generate", "system.notebook.promote", "system.notebook.source", "system.notebook.corpus",
    "system.notebook.diagram", "system.notebook.compare", "system.notebook.ingest", "system.notebook.autoscope",
    "system.notebook.mindmap", "system.notebook.quiz", "system.notebook.flashcards", "system.notebook.bundle",
    "system.research.find", "system.source.add", "system.source.list", "system.source.get", "system.source.read",
    "system.source.remove", "system.driver.inspect", "system.driver.snippet_gate", "system.action.verify",
    "system.action.verify.coverage", "system.focus.resolve", "system.board.evaluate", "system.driver.status",
    "system.driver.queue", "system.audit.timeline", "system.runtime.events", "system.rag.sweep", "system.rag.graph",
    "system.terminal.sandbox", "system.code.git", "system.driver.ticks", "system.driver.build_tests",
    "system.driver.failures.list", "system.driver.failures.interpret", "system.driver.halt",
    "system.notifications.list", "system.notifications.add", "system.personas.dashboard", "system.laws.list",
    "system.bypasses.list", "system.cost.settings", "system.cost.aggregate", "system.cost.today",
    "system.cost.sub-skill-matrix", "system.cost.provider-mix", "system.cost.agent-mix", "system.byok.status",
    "system.byok.key.set", "system.byok.strategy.set", "system.byok.model.set", "system.byok.test",
    "system.byok.local.endpoint.set", "system.byok.local.model.add", "system.byok.local.models.list",
    "system.reconcile", "system.reconcile.apply", "system.inbox.message.add", "system.inbox.message.list",
    "system.inbox.message.update", "system.ai.call.detail", "system.heimdall.mode", "system.benchmark.status",
    "system.blueprint.get", "system.blueprint.describe", "system.ai.postmortem.run", "system.ai.quality.report",
    "contract.gate.human.approve", "contract.gate.human.reject", "contract.render.html", "system.excalidraw.hello",
    "system.excalidraw.board", "system.excalidraw.pipeline", "system.excalidraw.driver", "system.excalidraw.rag",
    "system.excalidraw.bundles", "system.excalidraw.meta", "system.excalidraw.fullflow", "system.excalidraw.lifecycle",
    "system.mcp.markdown", "system.mcp.json", "mcp.board", "mcp.card", "mcp.reasoning", "mcp.sessions", "mcp.memory",
    "god.override.verify", "interviewer.turn", "interviewer.dispatch", "interviewer.classify",
    "interviewer.regenerate.presentation", "board-keeper.card.list", "board-keeper.card.html",
    "board-keeper.log.board", "board-keeper.log.card", "system.log.timeline", "system.log.tail.html",
    "system.log.tail.ansi", "system.mcp.descriptors", "system.card.driver.play", "system.card.driver.pause",
    "system.card.driver.stop", "system.card.driver.restart", "board-keeper.card.reorder",
    "board-keeper.recommend.next.work", "board-keeper.triage.recommendation.mark",
    "board-keeper.triage.recommendation.list", "system.ai.provider.test", "system.card.lint",
    "system.card.grounding.collect", "system.card.commits", "system.timeline.verify_full",
    "system.timeline.legacy_read", "system.timeline.digest", "system.timeline.stats", "system.card.discuss",
    "system.turn.rewrite", "system.turn.widget.save", "system.session.create", "system.skilllib.manage",
    "system.skilllib.materialize", "system.skilllib.mint", "system.book.manage", "system.book.run",
    "system.book.swarm", "system.instruments.run", "system.paper.store", "system.narration.manage",
    "system.narration.render", "system.reasoning.manage", "system.artefacts.manage", "system.episode.consolidate",
    "system.pane.sentry", "system.shared.diff", "system.pane.feed", "system.pane.checkpoint",
    "system.knowledge.document.ingest", "system.card.body.enrich.one", "system.card.body.enrich.persist",
    "system.card.body.reshape.one", "system.card.phase", "system.cards.active", "system.card.metrics",
    "system.card.process.events", "system.card.runtime.state", "system.drift.list", "system.substrate.stream",
    "system.drift.questionnaire", "system.drift.resolve", "board-keeper.card.score",
    "board-keeper.card.review.blueprint", "board-keeper.card.review.blueprint.generate",
    "board-keeper.card.reviews.list", "system.code.tree", "system.code.file", "system.code.imports",
    "system.code.metadata", "system.code.file.history", "system.code.authors", "git-manager.diff.decide",
    "git-manager.diff.apply", "tl.refine.microtasks", "tl.plan.contract", "tl.author.snippets",
];

// Recursive walker: `skip_dir` decides whether to descend into a directory (by its bare name);
// `keep_file` decides whether to collect a file (by its bare name). Mirrors every JS
// walk*(dir, acc) helper in the source (all of them push files satisfying a name predicate while
// pruning specific directory names / dot-prefixed names from the recursion).
fn walk(dir: &Path, skip_dir: &dyn Fn(&str) -> bool, keep_file: &dyn Fn(&str) -> bool, acc: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            if skip_dir(&name) {
                continue;
            }
            walk(&path, skip_dir, keep_file, acc);
        } else if keep_file(&name) {
            acc.push(path);
        }
    }
}

fn rel(project_root: &Path, abs: &Path) -> String {
    abs.strip_prefix(project_root)
        .unwrap_or(abs)
        .to_string_lossy()
        .replace('\\', "/")
}

/// `export default function consistencyScan(input, context)`.
pub fn consistencyScan(_input: &Value, db: &Connection) -> Result<Value, String> {
    let project_root = substrate_root();
    let mut findings: Vec<Value> = Vec::new();

    // Build skill index once (used by D1/D3).
    let skill_list = crate::engine_load_skill::listSkills();
    let skill_names: std::collections::HashSet<String> = skill_list
        .as_array()
        .map(|a| a.iter().filter_map(|e| e.get("name").and_then(Value::as_str).map(String::from)).collect())
        .unwrap_or_default();

    // ── D1: ACTION_SKILL_RESOLUTION ──────────────────────────────────
    let actions = crate::db_json::query_rows(db, "SELECT name, steps_json FROM actions", &[])?;
    let actions_arr = actions.as_array().cloned().unwrap_or_default();
    for sk in &actions_arr {
        let name = sk.get("name").and_then(Value::as_str).unwrap_or("");
        let steps_json = sk.get("steps_json").and_then(Value::as_str).unwrap_or("");
        let steps: Value = match serde_json::from_str(steps_json) {
            Ok(v) => v,
            Err(e) => {
                add(&mut findings, "critical", "SKILL_STEPS_JSON",
                    format!("action '{name}' has invalid steps_json: {e}"),
                    json!({ "action": name }));
                continue;
            }
        };
        for step in steps.as_array().cloned().unwrap_or_default() {
            if let Some(s) = step.get("skill").and_then(Value::as_str) {
                if !skill_names.contains(s) {
                    add(&mut findings, "critical", "ACTION_SKILL_RESOLUTION",
                        format!("action '{name}' references missing skill '{s}'"),
                        json!({ "action": name, "missing_skill": s }));
                }
            }
            if let Some(s) = step.get("input").and_then(|i| i.get("skill")).and_then(Value::as_str) {
                if !skill_names.contains(s) {
                    add(&mut findings, "critical", "ACTION_SKILL_RESOLUTION",
                        format!("action '{name}' wraps missing skill '{s}'"),
                        json!({ "action": name, "missing_skill": s }));
                }
            }
        }
    }

    // ── D2: AGENT_SKILL_INTEGRITY ──────────────────────────────────────
    let orphan_grants = crate::db_json::query_rows(db, "
        SELECT ag.agent_name, ag.action_name, ag.id
        FROM agent_actions ag
        LEFT JOIN agents a ON a.name = ag.agent_name
        LEFT JOIN actions s ON s.name = ag.action_name
        WHERE ag.revoked_at IS NULL AND (a.name IS NULL OR s.name IS NULL)
    ", &[])?;
    for g in orphan_grants.as_array().cloned().unwrap_or_default() {
        let agent_name = g.get("agent_name").and_then(Value::as_str).unwrap_or("");
        let action_name = g.get("action_name").and_then(Value::as_str).unwrap_or("");
        let id = g.get("id").cloned().unwrap_or(Value::Null);
        add(&mut findings, "critical", "AGENT_SKILL_INTEGRITY",
            format!("grant {id}: agent='{agent_name}' action='{action_name}' — one or both missing"),
            g);
    }

    // ── D3: LAW_HOOK_RESOLUTION ────────────────────────────────────────
    let hard_laws = crate::db_json::query_rows(db, "
        SELECT slug, enforcement_hook FROM laws.laws
        WHERE superseded_by IS NULL AND enforcement = 'hard' AND enforcement_hook IS NOT NULL
    ", &[])?;
    for l in hard_laws.as_array().cloned().unwrap_or_default() {
        let slug = l.get("slug").and_then(Value::as_str).unwrap_or("");
        let hook = l.get("enforcement_hook").and_then(Value::as_str).unwrap_or("");
        if !skill_names.contains(hook) {
            add(&mut findings, "critical", "LAW_HOOK_RESOLUTION",
                format!("law '{slug}' enforcement_hook='{hook}' skill not found — enforcement is fail-closed, all dispatch is blocked until the hook lands (or ANTILOKI_LAW_FAILOPEN=1)"),
                l);
        }
    }

    // ── D4: CHAIN_INTEGRITY ────────────────────────────────────────────
    match crate::audit_verify_chain::verifyChain(&Value::Null, db) {
        Ok(chain) => {
            let valid = chain.get("valid").and_then(Value::as_bool).unwrap_or(false);
            if !valid {
                let broken_at = chain.get("brokenAt").cloned().unwrap_or(Value::Null);
                let reason = chain.get("reason").and_then(Value::as_str).unwrap_or("");
                add(&mut findings, "critical", "CHAIN_INTEGRITY",
                    format!("verify-chain failed at event {broken_at}: {reason}"), chain);
            } else {
                let count = chain.get("count").cloned().unwrap_or(json!(0));
                add(&mut findings, "info", "CHAIN_INTEGRITY",
                    format!("chain valid over {count} events"), json!({ "count": count }));
            }
        }
        Err(e) => add(&mut findings, "warning", "CHAIN_INTEGRITY", format!("verify-chain threw: {e}"), json!({})),
    }

    // ── D5: MIGRATION_PENDING — REMOVED ────────────────────────────────
    //
    // Migrations are retired: the 196 .mjs files are deleted with antiloki-core, and the actions
    // they registered ship in the canonical catalog (canonical-actions/migrated.rs), so a fresh
    // substrate reaches a working action set without applying one.
    //
    // The dimension is DELETED rather than left to report "all migrations applied", which is what
    // it would have said forever — against a directory that no longer exists. A check that can
    // only return one answer is not a check; it is a green light with no wiring behind it, and
    // this scan has exactly one job: not to say healthy when it does not know.
    //
    // (They were never runnable on this engine in any case: migrate-run executed them through boa,
    // which parses script rather than ESM, so all 196 failed at `export`.)

    // ── D6: CATALOG_DRIFT ──────────────────────────────────────────────
    match crate::db_json::query_rows(db, "SELECT name, path, sha256 FROM memory.skills", &[]) {
        Ok(catalog) => {
            let rows = catalog.as_array().cloned().unwrap_or_default();
            let mut drifted = 0i64;
            for r in &rows {
                let name = r.get("name").and_then(Value::as_str).unwrap_or("");
                let path_str = r.get("path").and_then(Value::as_str).unwrap_or("");
                let sha256 = r.get("sha256").and_then(Value::as_str).unwrap_or("");
                let abs = PathBuf::from(path_str.replace('/', "\\"));
                match file_sha(&abs) {
                    None => {
                        add(&mut findings, "warning", "CATALOG_DRIFT",
                            format!("catalog row for '{name}' references missing file '{path_str}'"),
                            json!({ "name": name, "path": path_str }));
                        drifted += 1;
                    }
                    Some(disk_sha) if disk_sha != sha256 => {
                        add(&mut findings, "info", "CATALOG_DRIFT",
                            format!("skill '{name}' edited since last catalog-refresh (sha diff)"),
                            json!({ "name": name, "catalog_sha": &sha256[..12.min(sha256.len())], "disk_sha": &disk_sha[..12.min(disk_sha.len())] }));
                        drifted += 1;
                    }
                    _ => {}
                }
            }
            if rows.is_empty() {
                add(&mut findings, "warning", "CATALOG_DRIFT", "memory.skills catalog is empty — run system.catalog.refresh".to_string(), json!({}));
            } else if drifted > 0 {
                add(&mut findings, "info", "CATALOG_DRIFT",
                    format!("{drifted} of {} skills have drift — run system.catalog.refresh to sync", rows.len()),
                    json!({ "drifted": drifted, "total": rows.len() }));
            }
        }
        Err(e) => add(&mut findings, "warning", "CATALOG_DRIFT", format!("catalog scan failed: {e}"), json!({})),
    }

    // ── D7: KNOWLEDGE_VAULT_INTEGRITY ──────────────────────────────────
    match crate::db_json::query_rows(db, "SELECT id, vault_path, sha256 FROM knowledge.documents WHERE superseded_by IS NULL", &[]) {
        Ok(docs) => {
            let rows = docs.as_array().cloned().unwrap_or_default();
            let mut mismatched = 0i64;
            for d in &rows {
                let id = d.get("id").cloned().unwrap_or(Value::Null);
                let vault_path = d.get("vault_path").and_then(Value::as_str).unwrap_or("");
                let sha256 = d.get("sha256").and_then(Value::as_str).unwrap_or("");
                let abs = knowledge_dir().join(vault_path);
                match file_sha(&abs) {
                    None => {
                        add(&mut findings, "critical", "KNOWLEDGE_VAULT_INTEGRITY",
                            format!("document #{id} '{vault_path}' missing from vault"), d.clone());
                        mismatched += 1;
                    }
                    Some(disk_sha) if disk_sha != sha256 => {
                        add(&mut findings, "warning", "KNOWLEDGE_VAULT_INTEGRITY",
                            format!("document #{id} '{vault_path}' content changed since ingest"),
                            json!({ "id": id, "vault_path": vault_path, "ingested_sha": &sha256[..12.min(sha256.len())], "current_sha": &disk_sha[..12.min(disk_sha.len())] }));
                        mismatched += 1;
                    }
                    _ => {}
                }
            }
            if !rows.is_empty() && mismatched == 0 {
                add(&mut findings, "info", "KNOWLEDGE_VAULT_INTEGRITY",
                    format!("all {} vault documents match their stored sha256", rows.len()),
                    json!({ "count": rows.len() }));
            }
        }
        Err(e) => {
            if !e.contains("no such table") {
                add(&mut findings, "warning", "KNOWLEDGE_VAULT_INTEGRITY", format!("vault scan error: {e}"), json!({}));
            }
        }
    }

    // ── D8: LAW_TRIGGER_INSTALLED ──────────────────────────────────────
    let laws_with_triggers = crate::db_json::query_rows(db, "
        SELECT slug, trigger_sql FROM laws.laws
        WHERE superseded_by IS NULL AND trigger_sql IS NOT NULL AND trigger_sql != ''
    ", &[])?;
    let mut installed_by_schema: std::collections::HashMap<String, std::collections::HashSet<String>> = std::collections::HashMap::new();
    let schemas = crate::db_json::query_rows(db, "SELECT name FROM pragma_database_list", &[])?;
    for s in schemas.as_array().cloned().unwrap_or_default() {
        let sch = s.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        let sql = format!("SELECT name FROM {sch}.sqlite_master WHERE type='trigger'");
        let names: std::collections::HashSet<String> = crate::db_json::query_rows(db, &sql, &[])
            .ok()
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|r| r.get("name").and_then(Value::as_str).map(String::from))
            .collect();
        installed_by_schema.insert(sch, names);
    }
    let trigger_installed = |schema: Option<&str>, name: &str| -> bool {
        match schema {
            Some(s) => installed_by_schema.get(s).map(|set| set.contains(name)).unwrap_or(false),
            None => installed_by_schema.values().any(|set| set.contains(name)),
        }
    };
    let trigger_decl_re = Regex::new(r"(?i)CREATE\s+TRIGGER\s+(?:IF\s+NOT\s+EXISTS\s+)?(?:(\w+)\.)?(\w+)").unwrap();
    for l in laws_with_triggers.as_array().cloned().unwrap_or_default() {
        let slug = l.get("slug").and_then(Value::as_str).unwrap_or("").to_string();
        let trigger_sql = l.get("trigger_sql").and_then(Value::as_str).unwrap_or("");
        for caps in trigger_decl_re.captures_iter(trigger_sql) {
            let schema = caps.get(1).map(|m| m.as_str().to_string());
            let name = caps.get(2).unwrap().as_str().to_string();
            if !trigger_installed(schema.as_deref(), &name) {
                let prefix = schema.as_deref().map(|s| format!("{s}.")).unwrap_or_default();
                add(&mut findings, "critical", "LAW_TRIGGER_INSTALLED",
                    format!("law '{slug}' declares trigger '{prefix}{name}' but it's not installed — re-open db or call applyCanonicalLawTriggers"),
                    json!({ "law_slug": slug, "trigger_name": name, "schema": schema }));
            }
        }
    }

    // ── D9: SACRED_TIMELINE_FILE_EVENTS ────────────────────────────────
    if let Ok(file_events) = crate::db_json::query_rows(db, "
        SELECT DISTINCT entity_id FROM sacred_timeline
        WHERE entity_type = 'file' AND operation IN ('file.write', 'god.file.write')
    ", &[]) {
        let rows = file_events.as_array().cloned().unwrap_or_default();
        let mut missing = 0i64;
        for fe in &rows {
            let entity_id = fe.get("entity_id").and_then(Value::as_str).unwrap_or("");
            let abs = project_root.join(entity_id);
            if !abs.exists() {
                missing += 1;
            }
        }
        if missing > 0 {
            add(&mut findings, "info", "SACRED_TIMELINE_FILE_EVENTS",
                format!("{missing} of {} audited files no longer exist (likely deleted intentionally)", rows.len()),
                json!({ "missing": missing, "total": rows.len() }));
        }
    }

    // ── D10: MCP_CONFIG_INTEGRITY ──────────────────────────────────────
    {
        let mcp_root = project_root.join(".mcp.json");
        let mcp_claude_wrong = project_root.join(".claude").join("mcp.json");
        if !mcp_root.exists() {
            if mcp_claude_wrong.exists() {
                add(&mut findings, "critical", "MCP_CONFIG_INTEGRITY",
                    "MCP config at .claude/mcp.json — Claude Code reads from .mcp.json at REPO ROOT. Move it.".to_string(),
                    json!({ "wrong_path": ".claude/mcp.json", "correct_path": ".mcp.json" }));
            } else {
                add(&mut findings, "info", "MCP_CONFIG_INTEGRITY",
                    "No .mcp.json at repo root — Claude Code will not auto-discover an antiloki MCP server.".to_string(), json!({}));
            }
        } else {
            match std::fs::read_to_string(&mcp_root) {
                Ok(content) => match serde_json::from_str::<Value>(&content) {
                    Ok(cfg) => {
                        let servers = cfg.get("mcpServers").and_then(Value::as_object).cloned();
                        match servers {
                            None => add(&mut findings, "warning", "MCP_CONFIG_INTEGRITY",
                                ".mcp.json exists but has no mcpServers entries".to_string(), json!({ "path": ".mcp.json" })),
                            Some(servers) if servers.is_empty() => add(&mut findings, "warning", "MCP_CONFIG_INTEGRITY",
                                ".mcp.json exists but has no mcpServers entries".to_string(), json!({ "path": ".mcp.json" })),
                            Some(servers) => {
                                let arg_path_re = Regex::new(r"^[A-Za-z]:|^/").unwrap();
                                for (name, conf) in servers.iter() {
                                    if conf.get("command").is_none() {
                                        add(&mut findings, "warning", "MCP_CONFIG_INTEGRITY",
                                            format!("mcpServer '{name}' missing 'command' field"), json!({ "name": name }));
                                        continue;
                                    }
                                    if let Some(args) = conf.get("args").and_then(Value::as_array) {
                                        for arg in args {
                                            if let Some(a) = arg.as_str() {
                                                if arg_path_re.is_match(a) && a.ends_with(".mjs") && !Path::new(a).exists() {
                                                    add(&mut findings, "critical", "MCP_CONFIG_INTEGRITY",
                                                        format!("mcpServer '{name}' references missing file: {a}"),
                                                        json!({ "name": name, "missing_file": a }));
                                                }
                                            }
                                        }
                                    }
                                }
                                let names: Vec<&String> = servers.keys().collect();
                                let names_joined = names.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
                                add(&mut findings, "info", "MCP_CONFIG_INTEGRITY",
                                    format!("{} MCP server(s) in .mcp.json: {names_joined}", names.len()),
                                    json!({ "names": names }));
                            }
                        }
                        if mcp_claude_wrong.exists() {
                            add(&mut findings, "warning", "MCP_CONFIG_INTEGRITY",
                                ".claude/mcp.json exists alongside .mcp.json — Claude Code ignores the former. Remove the stale file.".to_string(),
                                json!({ "stale_path": ".claude/mcp.json" }));
                        }
                    }
                    Err(e) => add(&mut findings, "critical", "MCP_CONFIG_INTEGRITY",
                        format!(".mcp.json is not valid JSON: {e}"), json!({ "error": e.to_string() })),
                },
                Err(e) => add(&mut findings, "critical", "MCP_CONFIG_INTEGRITY",
                    format!(".mcp.json is not valid JSON: {e}"), json!({ "error": e.to_string() })),
            }
        }
    }

    // ── D11: INSTRUCTION_DOCS_SYNC ─────────────────────────────────────
    {
        let claude_root = project_root.join("CLAUDE.md");
        let agents_root = project_root.join("AGENTS.md");
        let law_slugs: Vec<String> = crate::db_json::query_rows(db,
            "SELECT slug FROM laws.laws WHERE superseded_by IS NULL AND enforcement != 'archived'", &[])?
            .as_array()
            .map(|a| a.iter().filter_map(|r| r.get("slug").and_then(Value::as_str).map(String::from)).collect())
            .unwrap_or_default();

        if !agents_root.exists() {
            add(&mut findings, "critical", "INSTRUCTION_DOCS_SYNC",
                "AGENTS.md missing at REPO ROOT — laws are not loaded by ANY AI agent (Claude reads it via @AGENTS.md import)".to_string(),
                json!({ "expected": "AGENTS.md", "regen_cmd": "system.laws.render.all" }));
        } else {
            let on_disk = std::fs::read_to_string(&agents_root).unwrap_or_default();
            let missing: Vec<&String> = law_slugs.iter().filter(|s| !on_disk.contains(s.as_str())).collect();
            if !missing.is_empty() {
                add(&mut findings, "warning", "INSTRUCTION_DOCS_SYNC",
                    format!("AGENTS.md is stale: {} law(s) added after last render missing from doc", missing.len()),
                    json!({ "missing_slugs": missing.iter().take(5).collect::<Vec<_>>(), "total_missing": missing.len(), "regen_cmd": "system.laws.render.all" }));
            } else {
                add(&mut findings, "info", "INSTRUCTION_DOCS_SYNC",
                    format!("AGENTS.md at repo root contains all {} active law slugs", law_slugs.len()), json!({}));
            }
        }

        if !claude_root.exists() {
            add(&mut findings, "critical", "INSTRUCTION_DOCS_SYNC",
                "CLAUDE.md missing at REPO ROOT — Claude Code will not auto-load project laws".to_string(),
                json!({ "expected": "CLAUDE.md", "regen_cmd": "system.laws.render.all" }));
        } else {
            let claude_on_disk = std::fs::read_to_string(&claude_root).unwrap_or_default();
            if !claude_on_disk.contains("@AGENTS.md") {
                add(&mut findings, "critical", "INSTRUCTION_DOCS_SYNC",
                    "CLAUDE.md exists but is missing the `@AGENTS.md` import — Claude Code will load CLAUDE.md but never see the laws".to_string(),
                    json!({ "regen_cmd": "system.laws.render.all" }));
            } else {
                add(&mut findings, "info", "INSTRUCTION_DOCS_SYNC",
                    "CLAUDE.md at repo root imports @AGENTS.md correctly".to_string(), json!({}));
            }
        }
    }

    // ── D12: CARD_STATUS_COVERAGE ──────────────────────────────────────
    let done_cards = crate::db_json::query_rows(db, "SELECT slug FROM cards WHERE status = 'done'", &[])?;
    let done_rows = done_cards.as_array().cloned().unwrap_or_default();
    let mut undocumented = 0i64;
    for c in &done_rows {
        let slug = c.get("slug").and_then(Value::as_str).unwrap_or("");
        let ev = crate::db_json::query_rows(db, "SELECT COUNT(*) AS c FROM sacred_timeline WHERE entity_type='card' AND entity_id = ?", &[json!(slug)])?;
        let count = ev.as_array().and_then(|a| a.first()).and_then(|r| r.get("c")).and_then(Value::as_i64).unwrap_or(0);
        if count == 0 {
            add(&mut findings, "warning", "CARD_STATUS_COVERAGE",
                format!("card '{slug}' is status=done but has no audit-trail events"), json!({ "slug": slug }));
            undocumented += 1;
        }
    }
    if !done_rows.is_empty() && undocumented == 0 {
        add(&mut findings, "info", "CARD_STATUS_COVERAGE",
            format!("all {} done cards have audit-trail events", done_rows.len()), json!({ "count": done_rows.len() }));
    }

    // D13 (SKILL_DIRECT_IMPORT) watched the Node bridge `napi/`, the one way into the crate from outside; the bridge was removed 2026-11-26, so there is no such surface to watch.

    // ── D14: CUSTOMER_LAYER_SKILL_IMPORT ──────────────────────────────
    {
        // THE IMPORT IS IMPOSSIBLE; THE CONCERN IS NOT. This caught customer-layer TypeScript
        // importing `.antiloki/skills/*.mjs` — unreachable now that the customer layer talks to
        // the substrate over FFI and there is no JS skill tree to import. What it was really
        // defending is intact and, per the operator rule of 2026-08-10, wider than it was: the
        // Rust skills own the databases, so every read and write is one auditable surface instead
        // of something any layer can reach for.
        //
        // So the dimension now looks for the layer reaching AROUND the engine — opening SQLite
        // itself, or resolving a path into the deleted JS core. It is the substrate-side twin of
        // antiloki-BE/src/engine/db-ownership.spec.ts, and deliberately covers the FE too, which
        // that spec cannot see.
        //
        // Comments are stripped first, exactly as the BE ratchet does: a dozen files explain why
        // the JS lane was removed, and deleting that history to satisfy a grep is the wrong trade.
        let reach_around = [".db_readonly", ".db_write", "better-sqlite3", "node:sqlite", "new Database(", "antiloki-core/skills", "antiloki-core/bin"];
        let block_comment_re = Regex::new(r"(?s)/\*.*?\*/").unwrap();
        let roots: [(&str, &[&str]); 2] = [("antiloki-BE/src", &["ts"]), ("antiloki-ui/src", &["ts", "tsx"])];
        for (dir, exts) in roots {
            let abs_root = project_root.join(dir);
            if !abs_root.exists() {
                continue;
            }
            let mut files = Vec::new();
            walk(&abs_root, &|n| n == "node_modules" || n == "dist" || n.starts_with('.'), &|n| exts.iter().any(|e| n.ends_with(&format!(".{e}"))), &mut files);
            files.sort();
            for abs in files {
                // The ratchet spec names the paths it forbids, in live code. Excluding it is the
                // same self-exemption it applies to itself, not a hole in the rule.
                if abs.file_name().map(|n| n == "db-ownership.spec.ts").unwrap_or(false) {
                    continue;
                }
                let rel_path = rel(&project_root, &abs);
                let raw = std::fs::read_to_string(&abs).unwrap_or_default();
                let code: String = block_comment_re
                    .replace_all(&raw, "")
                    .split('\n')
                    .filter(|l| { let t = l.trim(); !t.starts_with("//") && !t.starts_with('*') })
                    .collect::<Vec<_>>()
                    .join("\n");
                let offenders: Vec<&str> = reach_around.iter().copied().filter(|p| code.contains(p)).collect();
                if !offenders.is_empty() {
                    add(&mut findings, "critical", "CUSTOMER_LAYER_SKILL_IMPORT",
                        format!("{rel_path} reaches past the engine ({}) — the Rust skills own the databases and the JS core is gone, so this either dispatches an action or is dead code", offenders.join(", ")),
                        json!({ "file": rel_path, "patterns": offenders }));
                }
            }
        }
    }

    // ── D15: TUI_PRIVATE_INTERPRETATION — REMOVED ─────────────────────
    //
    // It walked `FE-TUI/src`. There is no FE-TUI: the terminal client is gone and the surviving
    // front end is antiloki-ui, a different codebase with different idioms. Its remediation
    // also named `bin/audit-tui-interpretations.mjs`, in a `bin/` that no longer exists — the
    // dimension was dead at both ends, and had been reporting a clean tree from an empty walk.
    //
    // NOT retargeted at antiloki-ui, deliberately. The rule was that the TUI must not re-derive
    // a state meaning the substrate already owns, and it was enforceable because the TUI rendered
    // substrate rows almost directly. Pointing the same switch/ternary heuristic at a React app
    // would flag ordinary view logic and teach everyone to ignore the dimension, which costs more
    // than the check is worth. If the concern comes back for the web FE it deserves its own rule
    // written against how that code is actually shaped — not this one aimed at a new target.


    // ── D16: ALLOWLIST_DISPATCH_DRIFT ─────────────────────────────────
    {
        let action_name_set: std::collections::HashSet<&str> = actions_arr.iter().filter_map(|a| a.get("name").and_then(Value::as_str)).collect();
        let bootstrap_set: std::collections::HashSet<&str> = BOOTSTRAP_ALLOWLIST.iter().copied().collect();
        let exclude_re = Regex::new(r"(\.spec\.ts$)|([\\/]god\.(controller|service)\.ts$)|([\\/]engine\.service\.ts$)").unwrap();
        let string_lit_re = Regex::new("['\"]([a-z][a-z0-9-]*(?:\\.[a-z0-9_-]+){1,4})['\"]").unwrap();
        let mut files = Vec::new();
        let be_root = project_root.join("antiloki-BE").join("src");
        walk(&be_root, &|n| n == "node_modules" || n == "dist" || n.starts_with('.'), &|n| n.ends_with(".ts"), &mut files);
        let mut offenders_by_action: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
        for abs in &files {
            let abs_str = abs.to_string_lossy();
            if exclude_re.is_match(&abs_str) {
                continue;
            }
            let rel_path = rel(&project_root, abs);
            let Ok(src) = std::fs::read_to_string(abs) else { continue };
            for caps in string_lit_re.captures_iter(&src) {
                let name = &caps[1];
                if !action_name_set.contains(name) || bootstrap_set.contains(name) {
                    continue;
                }
                let entry = offenders_by_action.entry(name.to_string()).or_default();
                if !entry.contains(&rel_path) {
                    entry.push(rel_path.clone());
                }
            }
        }
        for (name, rel_files) in offenders_by_action {
            add(&mut findings, "warning", "ALLOWLIST_DISPATCH_DRIFT",
                format!("BE references action '{name}' which is NOT in the lone-dispatch BOOTSTRAP_ALLOWLIST — a web-fe dispatch of it will 403 in block mode (unless god-token or driver-signed at runtime)"),
                json!({ "action": name, "files": rel_files.iter().take(5).collect::<Vec<_>>() }));
        }
    }

    // ── D17: HEAD_SLICE_ON_SIGNAL ─────────────────────────────────────
    {
        // THE RULE IS LANGUAGE-AGNOSTIC, THE IDIOM IS NOT. The concern is unchanged: truncating
        // signal-bearing text — feedback, evidence, a verdict, a remedy — throws away the very
        // thing a reader needs, and a 500+ char cap says the author knew the value was long.
        //
        // What changed is how you spell it. JavaScript truncates with `.slice(0, N)`; Rust does it
        // with `.chars().take(N)`, `.truncate(N)`, or a `[..N]` range. All three are matched, so
        // the dimension keeps working rather than reporting a clean tree because it was looking
        // for syntax the tree no longer contains.
        let signal_re = Regex::new(r"(?i)feedback|evidence|verdict|defect|error_m|remedy").unwrap();
        let head_slice_re = Regex::new(r"\.take\(\s*(\d{3,})\s*\)|\.truncate\(\s*(\d{3,})\s*\)|\[\s*\.\.\s*(\d{3,})\s*\]|\.slice\(\s*0\s*,\s*(\d{3,})\s*\)").unwrap();
        let mut files = Vec::new();
        let skills_dir = project_root.join("antiloki-core-rust").join("src").join("skills");
        walk(&skills_dir, &|n| n == "target" || n.starts_with('.'), &|n| n.ends_with(".rs"), &mut files);
        for abs in &files {
            let rel_path = rel(&project_root, abs);
            let Ok(src) = std::fs::read_to_string(abs) else { continue };
            let lines: Vec<&str> = src.split('\n').collect();
            for i in 0..lines.len() {
                let line = lines[i];
                let trimmed = line.trim();
                if trimmed.starts_with("//") || trimmed.starts_with('*') {
                    continue;
                }
                let Some(caps) = head_slice_re.captures(line) else { continue };
                // One alternation per idiom, so exactly one group captured.
                let n: i64 = (1..=4)
                    .find_map(|g| caps.get(g).and_then(|m| m.as_str().parse::<i64>().ok()))
                    .unwrap_or(0);
                if n < 500 || !signal_re.is_match(line) {
                    continue;
                }
                let prev = if i > 0 { lines[i - 1] } else { "" };
                if line.contains("head-ok") || prev.contains("head-ok") {
                    continue;
                }
                add(&mut findings, "warning", "HEAD_SLICE_ON_SIGNAL",
                    "head slice on a signal-carrying string — the verdict usually lives at the END (4 incidents of this class). Tail-bias it (slice(-N)) or justify with a 'head-ok:' comment.".to_string(),
                    json!({ "file": rel_path, "line": i + 1, "code": trimmed.chars().take(160).collect::<String>() }));
            }
        }
    }

    // ── D18: TIMELINE_LAYOUT_DRIFT ────────────────────────────────────
    {
        let layout = crate::audit__helpers_timeline_segments::chainLayout(db);
        let layout_str = layout.as_str().unwrap_or("none");
        match layout_str {
            "both" => {
                let shadow_rows = crate::db_json::query_rows(db, "SELECT COUNT(*) n FROM timeline.sacred_timeline", &[])
                    .ok()
                    .and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
                    .and_then(|r| r.get("n").and_then(Value::as_i64));
                if shadow_rows == Some(0) {
                    add(&mut findings, "warning", "TIMELINE_LAYOUT_DRIFT",
                        "sacred_timeline exists in BOTH main and timeline, but the timeline-side table is an EMPTY shadow — main is the active record. Finish the relocation: apply migration 20260613-138-relocate-sacred-timeline with the BE stopped.".to_string(),
                        json!({ "layout": layout, "shadow_rows": 0 }));
                } else {
                    add(&mut findings, "critical", "TIMELINE_LAYOUT_DRIFT",
                        "sacred_timeline carries rows in BOTH main and timeline — two tables claim to be the chain; unqualified writes land in main while rotate targets timeline. Resolve immediately.".to_string(),
                        json!({ "layout": layout, "shadow_rows": shadow_rows }));
                }
            }
            "legacy-main" => add(&mut findings, "warning", "TIMELINE_LAYOUT_DRIFT",
                "sacred_timeline still lives in state.sqlite (pre-relocation layout): timeline-rotate refuses to run here. Heal: apply migration 20260613-138-relocate-sacred-timeline with the BE stopped, then VACUUM state to reclaim.".to_string(),
                json!({ "layout": layout })),
            "none" => add(&mut findings, "info", "TIMELINE_LAYOUT_DRIFT",
                "no sacred_timeline table on this connection (fresh DB before the first audited write, or a partial attach set).".to_string(),
                json!({ "layout": layout })),
            _ => {}
        }
    }

    // ── Summary ──────────────────────────────────────────────────────
    let critical = findings.iter().filter(|f| f.get("severity").and_then(Value::as_str) == Some("critical")).count();
    let warning = findings.iter().filter(|f| f.get("severity").and_then(Value::as_str) == Some("warning")).count();
    let info = findings.iter().filter(|f| f.get("severity").and_then(Value::as_str) == Some("info")).count();
    let total = findings.len();

    let mut summary = Map::new();
    summary.insert("critical".into(), json!(critical as i64));
    summary.insert("warning".into(), json!(warning as i64));
    summary.insert("info".into(), json!(info as i64));
    summary.insert("total".into(), json!(total as i64));

    let mut out = Map::new();
    out.insert("summary".into(), Value::Object(summary));
    out.insert("findings".into(), Value::Array(findings));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // SAFETY: ANTILOKI_SUBSTRATE_ROOT is process-global; serialize via the shared crate-wide lock
    // (same convention as tl-sandbox-create.rs's `with_substrate_root`).
    fn with_substrate_root<T>(root: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", root.to_str().unwrap()); }
        let result = f();
        unsafe { std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT"); }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-consistency-scan-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn base_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE actions (name TEXT PRIMARY KEY, steps_json TEXT NOT NULL);
             CREATE TABLE agents (name TEXT PRIMARY KEY);
             CREATE TABLE agent_actions (id INTEGER PRIMARY KEY AUTOINCREMENT, agent_name TEXT, action_name TEXT, revoked_at TEXT);
             CREATE TABLE cards (slug TEXT PRIMARY KEY, status TEXT NOT NULL);
             CREATE TABLE sacred_timeline (
               id INTEGER PRIMARY KEY, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT,
               entity_id TEXT, before_json TEXT, after_json TEXT, payload TEXT, prev_print TEXT, digital_print TEXT
             );
             CREATE TABLE schema_migrations (name TEXT PRIMARY KEY);
             ATTACH ':memory:' AS laws;
             CREATE TABLE laws.laws (slug TEXT PRIMARY KEY, enforcement TEXT, enforcement_hook TEXT, trigger_sql TEXT, superseded_by TEXT);
             ATTACH ':memory:' AS memory;
             CREATE TABLE memory.skills (name TEXT, path TEXT, sha256 TEXT);
             ATTACH ':memory:' AS knowledge;
             CREATE TABLE knowledge.documents (id INTEGER PRIMARY KEY, vault_path TEXT, sha256 TEXT, superseded_by TEXT);",
        )
        .unwrap();
        c
    }

    fn scaffold_repo(root: &Path) {
        fs::create_dir_all(root.join("antiloki-core-rust").join("src").join("skills").join("audit")).unwrap();
        fs::write(root.join("AGENTS.md"), "no laws here").unwrap();
        fs::write(root.join("CLAUDE.md"), "@AGENTS.md\n").unwrap();
        // A CLEAN FFI BRIDGE, in every fixture. D13 warns when it cannot read one, so a scaffold
        // without these two files would put that warning in the output of every other test — and
        // worse, would let a test assert "no SKILL_DIRECT_IMPORT finding" while proving only that
        // the check never ran. `entities_card_add` is declared but not called: it is the skill
        // module a violating bridge would reach for, so the negative tests have a real target.
        fs::write(
            root.join("antiloki-core-rust").join("src").join("lib.rs"),
            "#[path = \"skills/engine/action-run.rs\"]\npub mod engine_action_run;\n\
             #[path = \"skills/engine/open-db.rs\"]\npub mod engine_open_db;\n\
             #[path = \"skills/entities/card-add.rs\"]\npub mod entities_card_add;\n\
             pub mod clock;\n",
        )
        .unwrap();
    }

    #[test]
    fn empty_repo_all_dimensions_run_without_panicking() {
        let dir = temp_dir("empty");
        scaffold_repo(&dir);
        let c = base_db();
        let r = with_substrate_root(&dir, || consistencyScan(&Value::Null, &c).unwrap());
        assert!(r["summary"]["total"].as_i64().unwrap() > 0);
        assert!(r["findings"].as_array().unwrap().iter().any(|f| f["dimension"] == json!("CHAIN_INTEGRITY")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn d1_action_skill_resolution_flags_missing_skill() {
        let dir = temp_dir("d1");
        scaffold_repo(&dir);
        let c = base_db();
        c.execute(
            "INSERT INTO actions (name, steps_json) VALUES ('sys.foo', ?1)",
            rusqlite::params![json!([{ "skill": "no-such-skill" }]).to_string()],
        )
        .unwrap();
        let r = with_substrate_root(&dir, || consistencyScan(&Value::Null, &c).unwrap());
        let hits: Vec<&Value> = r["findings"].as_array().unwrap().iter()
            .filter(|f| f["dimension"] == json!("ACTION_SKILL_RESOLUTION")).collect();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["evidence"]["missing_skill"], json!("no-such-skill"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn d1_invalid_steps_json_reports_critical() {
        let dir = temp_dir("d1-bad-json");
        scaffold_repo(&dir);
        let c = base_db();
        c.execute("INSERT INTO actions (name, steps_json) VALUES ('sys.bad', 'not json')", []).unwrap();
        let r = with_substrate_root(&dir, || consistencyScan(&Value::Null, &c).unwrap());
        let hit = r["findings"].as_array().unwrap().iter().find(|f| f["dimension"] == json!("SKILL_STEPS_JSON")).unwrap();
        assert_eq!(hit["severity"], json!("critical"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn d2_orphan_grant_flagged() {
        let dir = temp_dir("d2");
        scaffold_repo(&dir);
        let c = base_db();
        c.execute("INSERT INTO agent_actions (agent_name, action_name, revoked_at) VALUES ('ghost', 'sys.nope', NULL)", []).unwrap();
        let r = with_substrate_root(&dir, || consistencyScan(&Value::Null, &c).unwrap());
        let hit = r["findings"].as_array().unwrap().iter().find(|f| f["dimension"] == json!("AGENT_SKILL_INTEGRITY")).unwrap();
        assert_eq!(hit["evidence"]["agent_name"], json!("ghost"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn d3_law_hook_resolution_flags_unresolved_hook() {
        let dir = temp_dir("d3");
        scaffold_repo(&dir);
        let c = base_db();
        c.execute("INSERT INTO laws.laws (slug, enforcement, enforcement_hook) VALUES ('MY-LAW', 'hard', 'no-such-hook')", []).unwrap();
        let r = with_substrate_root(&dir, || consistencyScan(&Value::Null, &c).unwrap());
        let hit = r["findings"].as_array().unwrap().iter().find(|f| f["dimension"] == json!("LAW_HOOK_RESOLUTION")).unwrap();
        assert_eq!(hit["severity"], json!("critical"));
        let _ = fs::remove_dir_all(&dir);
    }

    // The two d5 (MIGRATION_PENDING) tests are gone with the dimension. They asserted that a
    // pending .mjs migration produced a warning and that none produced an info — both statements
    // about machinery that no longer exists. Keeping them green would have required keeping a
    // dimension whose only possible answer was "all applied".

    #[test]
    fn d7_knowledge_vault_missing_document() {
        let dir = temp_dir("d7");
        scaffold_repo(&dir);
        fs::create_dir_all(dir.join(".antiloki").join("knowledge")).unwrap();
        let c = base_db();
        c.execute("INSERT INTO knowledge.documents (id, vault_path, sha256) VALUES (1, 'gone.md', 'ff00')", []).unwrap();
        let r = with_substrate_root(&dir, || consistencyScan(&Value::Null, &c).unwrap());
        let hit = r["findings"].as_array().unwrap().iter().find(|f| f["dimension"] == json!("KNOWLEDGE_VAULT_INTEGRITY")).unwrap();
        assert_eq!(hit["severity"], json!("critical"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn d8_law_trigger_not_installed() {
        let dir = temp_dir("d8");
        scaffold_repo(&dir);
        let c = base_db();
        c.execute("INSERT INTO laws.laws (slug, trigger_sql) VALUES ('TRIG-LAW', 'CREATE TRIGGER my_trig BEFORE INSERT ON foo BEGIN SELECT 1; END;')", []).unwrap();
        let r = with_substrate_root(&dir, || consistencyScan(&Value::Null, &c).unwrap());
        let hit = r["findings"].as_array().unwrap().iter().find(|f| f["dimension"] == json!("LAW_TRIGGER_INSTALLED")).unwrap();
        assert_eq!(hit["evidence"]["trigger_name"], json!("my_trig"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn d9_sacred_timeline_file_event_missing() {
        let dir = temp_dir("d9");
        scaffold_repo(&dir);
        let c = base_db();
        c.execute(
            "INSERT INTO sacred_timeline (id, entity_type, entity_id, operation, prev_print, digital_print) VALUES (1, 'file', 'no-such-file.txt', 'file.write', 'GENESIS', 'h')",
            [],
        ).unwrap();
        let r = with_substrate_root(&dir, || consistencyScan(&Value::Null, &c).unwrap());
        let hit = r["findings"].as_array().unwrap().iter().find(|f| f["dimension"] == json!("SACRED_TIMELINE_FILE_EVENTS")).unwrap();
        assert_eq!(hit["evidence"]["missing"], json!(1));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn d10_mcp_config_missing_reports_info() {
        let dir = temp_dir("d10-missing");
        scaffold_repo(&dir);
        let c = base_db();
        let r = with_substrate_root(&dir, || consistencyScan(&Value::Null, &c).unwrap());
        let hit = r["findings"].as_array().unwrap().iter().find(|f| f["dimension"] == json!("MCP_CONFIG_INTEGRITY")).unwrap();
        assert_eq!(hit["severity"], json!("info"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn d10_mcp_config_wrong_path_only() {
        let dir = temp_dir("d10-wrong");
        scaffold_repo(&dir);
        fs::create_dir_all(dir.join(".claude")).unwrap();
        fs::write(dir.join(".claude").join("mcp.json"), "{}").unwrap();
        let c = base_db();
        let r = with_substrate_root(&dir, || consistencyScan(&Value::Null, &c).unwrap());
        let hit = r["findings"].as_array().unwrap().iter().find(|f| f["dimension"] == json!("MCP_CONFIG_INTEGRITY")).unwrap();
        assert_eq!(hit["severity"], json!("critical"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn d10_mcp_config_valid_reports_server_count() {
        let dir = temp_dir("d10-valid");
        scaffold_repo(&dir);
        fs::write(dir.join(".mcp.json"), json!({ "mcpServers": { "antiloki": { "command": "node", "args": ["bin/mcp.mjs"] } } }).to_string()).unwrap();
        let c = base_db();
        let r = with_substrate_root(&dir, || consistencyScan(&Value::Null, &c).unwrap());
        let hit = r["findings"].as_array().unwrap().iter().find(|f| f["dimension"] == json!("MCP_CONFIG_INTEGRITY") && f["message"].as_str().unwrap().contains("MCP server(s)")).unwrap();
        assert_eq!(hit["evidence"]["names"], json!(["antiloki"]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn d11_missing_agents_and_claude_md() {
        let dir = temp_dir("d11-missing");
        fs::create_dir_all(dir.join("migrations")).unwrap();
        let c = base_db();
        let r = with_substrate_root(&dir, || consistencyScan(&Value::Null, &c).unwrap());
        let findings = r["findings"].as_array().unwrap();
        assert!(findings.iter().any(|f| f["dimension"] == json!("INSTRUCTION_DOCS_SYNC") && f["message"].as_str().unwrap().contains("AGENTS.md missing")));
        assert!(findings.iter().any(|f| f["dimension"] == json!("INSTRUCTION_DOCS_SYNC") && f["message"].as_str().unwrap().contains("CLAUDE.md missing")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn d11_stale_agents_md_missing_law_slug() {
        let dir = temp_dir("d11-stale");
        scaffold_repo(&dir);
        let c = base_db();
        c.execute("INSERT INTO laws.laws (slug, enforcement) VALUES ('NEW-LAW-SLUG', 'hard')", []).unwrap();
        let r = with_substrate_root(&dir, || consistencyScan(&Value::Null, &c).unwrap());
        let hit = r["findings"].as_array().unwrap().iter().find(|f| f["dimension"] == json!("INSTRUCTION_DOCS_SYNC") && f["message"].as_str().unwrap().contains("stale")).unwrap();
        assert_eq!(hit["evidence"]["total_missing"], json!(1));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn d12_done_card_without_evidence() {
        let dir = temp_dir("d12");
        scaffold_repo(&dir);
        let c = base_db();
        c.execute("INSERT INTO cards (slug, status) VALUES ('FEAT-X', 'done')", []).unwrap();
        let r = with_substrate_root(&dir, || consistencyScan(&Value::Null, &c).unwrap());
        let hit = r["findings"].as_array().unwrap().iter().find(|f| f["dimension"] == json!("CARD_STATUS_COVERAGE")).unwrap();
        assert_eq!(hit["evidence"]["slug"], json!("FEAT-X"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn d14_customer_layer_opening_the_db_itself_is_flagged() {
        let dir = temp_dir("d14");
        scaffold_repo(&dir);
        let be_src = dir.join("antiloki-BE").join("src");
        fs::create_dir_all(&be_src).unwrap();
        fs::write(be_src.join("sneaky.service.ts"),
            "import { DatabaseSync } from 'node:sqlite';\nconst rows = this.db_readonly(sql);\n").unwrap();
        let c = base_db();
        let r = with_substrate_root(&dir, || consistencyScan(&Value::Null, &c).unwrap());
        let hit = r["findings"].as_array().unwrap().iter().find(|f| f["dimension"] == json!("CUSTOMER_LAYER_SKILL_IMPORT")).unwrap();
        assert_eq!(hit["severity"], json!("critical"));
        assert_eq!(hit["evidence"]["patterns"], json!([".db_readonly", "node:sqlite"]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn d14_comments_explaining_the_removed_js_lane_are_not_violations() {
        // A dozen real BE files narrate why the JS engine went away. Flagging that history would
        // make the only way to a clean scan deleting the explanation, so comments are stripped —
        // block and line — and only live code counts.
        let dir = temp_dir("d14-comments");
        scaffold_repo(&dir);
        let be_src = dir.join("antiloki-BE").join("src");
        fs::create_dir_all(&be_src).unwrap();
        fs::write(be_src.join("history.service.ts"),
            "/* this used to hold a node:sqlite handle */\n// db_readonly is GONE, not deprecated\nexport class S {}\n").unwrap();
        let c = base_db();
        let r = with_substrate_root(&dir, || consistencyScan(&Value::Null, &c).unwrap());
        assert!(!r["findings"].as_array().unwrap().iter().any(|f| f["dimension"] == json!("CUSTOMER_LAYER_SKILL_IMPORT")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn d14_the_ratchet_spec_may_name_the_paths_it_forbids() {
        let dir = temp_dir("d14-ratchet");
        scaffold_repo(&dir);
        let be_engine = dir.join("antiloki-BE").join("src").join("engine");
        fs::create_dir_all(&be_engine).unwrap();
        fs::write(be_engine.join("db-ownership.spec.ts"),
            "expect(code.includes('antiloki-core/skills')).toBe(false);\nsrc.match(/\\.db_write\\s*[(<]/g);\n").unwrap();
        let c = base_db();
        let r = with_substrate_root(&dir, || consistencyScan(&Value::Null, &c).unwrap());
        assert!(!r["findings"].as_array().unwrap().iter().any(|f| f["dimension"] == json!("CUSTOMER_LAYER_SKILL_IMPORT")));
        let _ = fs::remove_dir_all(&dir);
    }

    // The d15 (TUI_PRIVATE_INTERPRETATION) test is gone with the dimension. It scaffolded an
    // `FE-TUI/src` that only ever existed inside the test — which is how a dead dimension keeps a
    // green test: the fixture supplies the tree the real repo lost.

    #[test]
    fn d16_allowlist_dispatch_drift_flags_unallowlisted_action_reference() {
        let dir = temp_dir("d16");
        scaffold_repo(&dir);
        let be_src = dir.join("antiloki-BE").join("src");
        fs::create_dir_all(&be_src).unwrap();
        fs::write(be_src.join("chat.controller.ts"), "dispatch('sys.custom.action.thing');\n").unwrap();
        let c = base_db();
        c.execute("INSERT INTO actions (name, steps_json) VALUES ('sys.custom.action.thing', '[]')", []).unwrap();
        let r = with_substrate_root(&dir, || consistencyScan(&Value::Null, &c).unwrap());
        let hit = r["findings"].as_array().unwrap().iter().find(|f| f["dimension"] == json!("ALLOWLIST_DISPATCH_DRIFT")).unwrap();
        assert_eq!(hit["evidence"]["action"], json!("sys.custom.action.thing"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn d16_allowlisted_action_is_not_flagged() {
        let dir = temp_dir("d16-allow");
        scaffold_repo(&dir);
        let be_src = dir.join("antiloki-BE").join("src");
        fs::create_dir_all(&be_src).unwrap();
        fs::write(be_src.join("board.controller.ts"), "dispatch('board-keeper.card.list');\n").unwrap();
        let c = base_db();
        c.execute("INSERT INTO actions (name, steps_json) VALUES ('board-keeper.card.list', '[]')", []).unwrap();
        let r = with_substrate_root(&dir, || consistencyScan(&Value::Null, &c).unwrap());
        assert!(!r["findings"].as_array().unwrap().iter().any(|f| f["dimension"] == json!("ALLOWLIST_DISPATCH_DRIFT")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn d17_head_slice_on_signal_flagged_and_head_ok_suppresses() {
        let dir = temp_dir("d17");
        scaffold_repo(&dir);
        let skills_dir = dir.join("antiloki-core-rust").join("src").join("skills").join("audit");
        // Rust truncation idiom, because the tree is Rust. `.slice(0, N)` is still matched for a
        // stray .mjs, but it is no longer the shape this dimension exists to find.
        fs::write(skills_dir.join("bad.rs"), "let msg: String = feedback.chars().take(1500).collect();\n").unwrap();
        fs::write(skills_dir.join("ok.rs"), "let msg: String = feedback.chars().take(1500).collect(); // head-ok: preview only\n").unwrap();
        let c = base_db();
        let r = with_substrate_root(&dir, || consistencyScan(&Value::Null, &c).unwrap());
        let hits: Vec<&Value> = r["findings"].as_array().unwrap().iter().filter(|f| f["dimension"] == json!("HEAD_SLICE_ON_SIGNAL")).collect();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["evidence"]["file"], json!("antiloki-core-rust/src/skills/audit/bad.rs"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn d18_timeline_layout_both_empty_shadow_is_warning() {
        let dir = temp_dir("d18-both");
        scaffold_repo(&dir);
        let c = base_db();
        c.execute_batch(
            "ATTACH ':memory:' AS timeline;
             CREATE TABLE timeline.sacred_timeline (id INTEGER PRIMARY KEY);",
        ).unwrap();
        c.execute("INSERT INTO sacred_timeline (id, prev_print, digital_print) VALUES (1, 'GENESIS', 'h')", []).unwrap();
        let r = with_substrate_root(&dir, || consistencyScan(&Value::Null, &c).unwrap());
        let hit = r["findings"].as_array().unwrap().iter().find(|f| f["dimension"] == json!("TIMELINE_LAYOUT_DRIFT")).unwrap();
        assert_eq!(hit["severity"], json!("warning"));
        assert_eq!(hit["evidence"]["shadow_rows"], json!(0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn d18_timeline_layout_legacy_main_is_warning() {
        let dir = temp_dir("d18-legacy");
        scaffold_repo(&dir);
        let c = base_db();
        c.execute("INSERT INTO sacred_timeline (id, prev_print, digital_print) VALUES (1, 'GENESIS', 'h')", []).unwrap();
        let r = with_substrate_root(&dir, || consistencyScan(&Value::Null, &c).unwrap());
        let hit = r["findings"].as_array().unwrap().iter().find(|f| f["dimension"] == json!("TIMELINE_LAYOUT_DRIFT")).unwrap();
        assert_eq!(hit["severity"], json!("warning"));
        assert_eq!(hit["evidence"]["layout"], json!("legacy-main"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn summary_counts_match_findings_by_severity() {
        let dir = temp_dir("summary");
        scaffold_repo(&dir);
        let c = base_db();
        let r = with_substrate_root(&dir, || consistencyScan(&Value::Null, &c).unwrap());
        let findings = r["findings"].as_array().unwrap();
        let critical = findings.iter().filter(|f| f["severity"] == json!("critical")).count();
        let warning = findings.iter().filter(|f| f["severity"] == json!("warning")).count();
        let info = findings.iter().filter(|f| f["severity"] == json!("info")).count();
        assert_eq!(r["summary"]["critical"], json!(critical as i64));
        assert_eq!(r["summary"]["warning"], json!(warning as i64));
        assert_eq!(r["summary"]["info"], json!(info as i64));
        assert_eq!(r["summary"]["total"], json!(findings.len() as i64));
        let _ = fs::remove_dir_all(&dir);
    }
}
