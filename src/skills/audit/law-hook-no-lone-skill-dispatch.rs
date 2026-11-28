//! law-hook-no-lone-skill-dispatch.rs — twin of skills/audit/law-hook-no-lone-skill-dispatch.mjs
//! (standard db lane). NAMES ARE CONTRACT: `lawHookNoLoneSkillDispatch` (default, modeled as
//! `lawHookNoLoneSkillDispatch`), `getMode` (modeled as `getMode`), `checkGodSignature`
//! (modeled as `checkGodSignature`) — the JS module's three named exports, `BOOTSTRAP_ALLOWLIST`
//! (data, inlined as a const array — not a census unit).
//!
//! Enforces LONE_SKILL_DISPATCH_FORBIDDEN + DRIVER_DISPATCH_REQUIRED. Real (production) callers
//! pass a full action-run context object as the JS default export's SECOND positional param —
//! `ctx.source`, `ctx.god_signature_hex`, `ctx.god_challenge`, `ctx.dispatch_turn_id`,
//! `ctx.driver_ts`, `ctx.driver_hmac`, `ctx.actor`, `ctx.tui_god_token`, `ctx.db` — but per the
//! crossing playbook's "standard" db lane, this twin's fixed Rust signature is
//! `(input: &Value, db: &Connection)`: the ONLY context field the lane can ever supply is `db`.
//! Same established precedent as law-hook-no-drift-on-audit-commit.rs's `context.gitRunner` note:
//! since there is no Rust-side analog for the other ctx fields, every branch keyed on them is DEAD
//! by construction in this twin (documented per-branch below) —
//!   - `source === 'driver'` (branch 1: driver-signed HMAC dispatch, calls the already-converted
//!     driver-token.rs `readDriverToken`/`verifyDriverCall`) — never taken (`source` is always
//!     absent/null here).
//!   - `source === 'interviewer:confirmed'` (branch 2.5: plan-confirmed dispatch, reads
//!     `ctx.dispatch_turn_id` + `ctx.db` for an `actions`/`interviewer_turns` lookup) — never
//!     taken (`source` is always absent/null here).
//!   - `checkGodSignature` (branch 3, calls the fs+crypto-touching `godOverrideVerify` twin) —
//!     `ctx.god_signature_hex`/`ctx.god_challenge` are always absent here, so the JS itself
//!     short-circuits to `{signed:false}` BEFORE ever calling `godOverrideVerify`; `checkGodSignature`
//!     below models that short-circuit exactly, and only calls into the real bespoke-native
//!     `godOverrideVerify` twin when both fields ARE present (same untested-by-fixture class as
//!     `godOverrideVerify` itself — see its own module doc).
//!   - `checkTuiGodToken` (branch 3.5, calls the fs-touching tui-god-token twin) —
//!     `ctx.tui_god_token` is always absent here, so the JS itself returns
//!     `{ok:false, reason:'no tui_god_token in context'}` before any fs read; modeled the same way
//!     by calling the real (already-converted) `checkTuiGodToken` twin, which takes that identical
//!     early-return path whenever the field is absent.
//! What remains — and IS fully exercised by the fixture — is the part of the decision tree driven
//! purely by `input.action_name` + the `ANTILOKI_ENFORCE_DRIVER` env var: the bootstrap allowlist
//! check, warn-mode passthrough, and the final block-mode deny (`source` always renders as
//! `'unknown'` in those messages, matching the JS `source ?? 'unknown'` fallback since `source` is
//! always null here). Matches the mutation-via-with-audit.rs / no-direct-audit-write.rs /
//! no-drift-on-audit-commit.rs env-gated hook pattern exactly.
use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// Bootstrap allowlist — actions that MUST work from any source for the substrate to function.
// Transcribed verbatim from the JS `BOOTSTRAP_ALLOWLIST` Set (403 entries) — data, not a census
// unit (see module doc). Never edit by hand; source of truth is the JS module.
const BOOTSTRAP_ALLOWLIST: [&str; 441] = [
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
    // CONTEXT PACK (2026-09-14): bake/refresh from the FE + the watcher; tool from the MCP server a pane's CLI spawns
    "system.cockpit.pane.context.bake", "system.cockpit.pane.context.refresh", "system.cockpit.pane.context.tool",
    "system.context.library",
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
    "system.codebase.atlas.register",
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
    // The board's READ surface. These were raw db_readonly queries in the BE until the
    // db-ownership pass; dispatching them correctly subjects them to this gate, and the board is
    // the substrate's primary read surface — as bootstrap-essential as it gets.
    // Reused by /api/v1/workflows rather than minting a near-duplicate skill. It needed the
    // allowlist for the same reason the board reads did — the raw query it replaced bypassed this
    // gate entirely. Missing it made the route 403 on BOTH engines; the parity suite caught it.
    "system.workflow.list",
    "system.board.cards.list", "system.board.card.get", "system.board.agents.list",
    "system.board.actions.list", "system.board.prompt-templates.list", "system.board.pipeline.list",
    "system.contracts.pending-human-approval", "system.contracts.recent", "system.contract.detail.full",
    "system.preview.target.resolve",
    "system.memory.browse.list", "system.memory.graph.full", "system.memory.get.one",
    "system.memory.counts.by-type",
    "system.knowledge.papers.list", "system.knowledge.paper.get", "system.knowledge.concepts.list",
    "system.knowledge.evidence.list", "system.knowledge.docs.list",
    "system.chat.checkpoints.list", "system.chat.session.widgets.list", "system.chat.sessions.list",
    "system.chat.session.turns", "system.diffs.pending.list", "system.diff.get",
    "system.notebook.item.meta", "system.env.flags.dangerous", "system.enrich.proposals.list",
    "system.cockpit.pane.get", "system.cockpit.pane.live", "system.shell.target.resolve",
    "system.cockpit.pane.meter", "system.build-gate.chunk.append",
    "system.notifications.list", "system.notifications.add",
    // Dismiss joins add/list: the operator's dismiss button is the same bootstrap-essential
    // surface. It only became subject to this gate when the BE stopped UPDATEing the table
    // directly — the raw write bypassed the law entirely, which is the hole being closed.
    "system.notifications.dismiss",
    "system.personas.dashboard", "system.laws.list",
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

/// `export function getMode()` — reads `ANTILOKI_ENFORCE_DRIVER`, defaulting to `'block'`;
/// unrecognized values also fall back to `'block'`.
pub fn getMode() -> String {
    let m = std::env::var("ANTILOKI_ENFORCE_DRIVER").unwrap_or_else(|_| "block".to_string()).to_lowercase();
    if m == "block" || m == "warn" || m == "off" { m } else { "block".to_string() }
}

/// `export function checkGodSignature(ctx)` — mirrors the JS: `const { god_signature_hex,
/// god_challenge, db } = ctx ?? {};` then `if (!god_signature_hex || !god_challenge) return
/// { signed: false };`. Only when BOTH fields are present does the JS call the real (bespoke-
/// native, fs+crypto) `godOverrideVerify` twin — `godOverrideVerify`'s own `_db` param is unused
/// (see its module doc), so a throwaway in-memory connection stands in for `ctx.db` here; this
/// twin's fixture only exercises the missing-field early return (see module doc "branch 3").
pub fn checkGodSignature(ctx: &Value) -> Value {
    let god_signature_hex = ctx.get("god_signature_hex");
    let god_challenge = ctx.get("god_challenge");
    if !truthy(god_signature_hex) || !truthy(god_challenge) {
        return json!({ "signed": false });
    }
    let dummy = Connection::open_in_memory().unwrap();
    match crate::agents_god_god_override_verify::godOverrideVerify(
        &json!({ "challenge": god_challenge, "signature_hex": god_signature_hex }),
        &dummy,
    ) {
        Ok(_) => json!({ "signed": true }),
        Err(e) => json!({ "signed": false, "error": e }),
    }
}

pub fn lawHookNoLoneSkillDispatch(input: &Value, db: &Connection) -> Result<Value, String> {
    // Preserved signature + behavior for the conform fixture unit: an empty ctx means source=null,
    // which lands on exactly the branches this twin always took.
    lawHookNoLoneSkillDispatchWithCtx(input, &json!({}), db)
}

/// The production form. JS calls `hookFn(input, ctx)` with the FULL action-run context as the
/// second argument; the Rust twin originally dropped it, which left branches 1 / 2.5 / 3 / 3.5
/// permanently dead ("source is always null") and refused every action outside the 403-entry
/// bootstrap allowlist — 225 of the 581 seeded actions. Threading ctx is what makes a
/// driver-signed, interviewer-confirmed, or god-signed dispatch reachable at all.
pub fn lawHookNoLoneSkillDispatchWithCtx(input: &Value, ctx: &Value, db: &Connection) -> Result<Value, String> {
    let mode = getMode();
    if mode == "off" {
        return Ok(json!({ "allowed": true, "mode": "off" }));
    }

    let action_name = input.get("action_name");
    if !truthy(action_name) {
        return Ok(json!({ "allowed": true, "reason": "no action_name provided to hook" }));
    }
    let name = action_name.and_then(|v| v.as_str()).unwrap_or("");
    let source = ctx.get("source").and_then(Value::as_str).unwrap_or("");

    // 1. source='driver' → REQUIRE a valid HMAC over (action, input, ts, actor). Claiming the
    //    source is not enough; the signature must come from code that can read the token file.
    if source == "driver" {
        let token = crate::audit_driver_token::readDriverToken();
        let verdict = crate::audit_driver_token::verifyDriverCall(&json!({
            "action": name,
            "input": input.get("input_data").cloned()
                .filter(|v| !v.is_null())
                .or_else(|| ctx.get("input").cloned())
                .unwrap_or_else(|| json!({})),
            "ts": ctx.get("driver_ts").cloned().unwrap_or(Value::Null),
            "hmac": ctx.get("driver_hmac").cloned().unwrap_or(Value::Null),
            "token": token,
            "actor": ctx.get("actor").cloned().unwrap_or(Value::Null),
        }));
        if truthy(verdict.get("ok")) {
            return Ok(json!({ "allowed": true, "reason": "source=driver+hmac-verified", "mode": mode }));
        }
        let reason = verdict.get("reason").and_then(Value::as_str).unwrap_or("unknown");
        return Ok(json!({
            "allowed": false,
            "reason": format!("source='driver' but HMAC verification failed: {reason}"),
            "suggested_path": "invoke via bin/driver-fire.mjs which holds the token and signs each call. Direct dispatches with source=driver must include context.driver_hmac + context.driver_ts (HMAC-SHA256 of `${action}:${inputHash}:${ts}:${actor}` with the token as key, actor matching context.actor).",
            "mode": mode,
        }));
    }

    // 2. Bootstrap allowlist → allow regardless of source.
    if BOOTSTRAP_ALLOWLIST.contains(&name) {
        return Ok(json!({ "allowed": true, "reason": "bootstrap-allowlist", "mode": mode }));
    }

    // 2.5. Interviewer-confirmed dispatch → allow when the turn's proposed plan contains this
    //      exact action (the user typed the gate's expected confirmation to get here).
    if source == "interviewer:confirmed" {
        let turn_id = ctx.get("dispatch_turn_id").cloned().unwrap_or(Value::Null);
        // god-owned actions are NEVER reachable this way — user confirmation is not god credentials.
        if let Ok(owner) = db.query_row(
            "SELECT owner_agent FROM actions WHERE name = ?",
            rusqlite::params![name],
            |r| r.get::<_, Option<String>>(0),
        ) {
            if owner.as_deref() == Some("god") {
                return Ok(json!({
                    "allowed": false,
                    "reason": format!("action '{name}' is god-owned; interviewer-confirmed dispatch is not enough — needs god credentials (--as god TTY or TUI god-token)"),
                    "suggested_path": "activate god mode in the TUI (Ctrl+G) OR run via bin/antiloki.mjs --as god",
                    "mode": mode,
                }));
            }
        }
        if !turn_id.is_null() {
            let plan_json: Option<String> = db
                .query_row(
                    "SELECT proposed_actions_json FROM interviewer_turns WHERE id = ?",
                    rusqlite::params![turn_id.as_i64().unwrap_or(-1)],
                    |r| r.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten();
            if let Some(pj) = plan_json {
                let plan: Value = serde_json::from_str(&pj).unwrap_or_else(|_| json!([]));
                let in_plan = plan
                    .as_array()
                    .map(|a| a.iter().any(|p| p.get("action").and_then(Value::as_str) == Some(name)))
                    .unwrap_or(false);
                if in_plan {
                    return Ok(json!({ "allowed": true, "reason": "interviewer:confirmed+action-in-plan", "mode": mode }));
                }
                return Ok(json!({
                    "allowed": false,
                    "reason": format!("source='interviewer:confirmed' but action '{name}' not in turn {turn_id}'s proposed plan"),
                    "suggested_path": "check interviewer-turn classification produced the intended action; do not hand-craft source/turn_id from outside interviewer-dispatch",
                    "mode": mode,
                }));
            }
        }
        let shown = if turn_id.is_null() { "null".to_string() } else { turn_id.to_string() };
        return Ok(json!({
            "allowed": false,
            "reason": format!("source='interviewer:confirmed' requires ctx.dispatch_turn_id + db; got turn_id={shown}"),
            "suggested_path": "interviewer-dispatch must pass dispatch_turn_id alongside source",
            "mode": mode,
        }));
    }

    // 3. God-mode offline signature → allow (caller proved they hold the key).
    if truthy(checkGodSignature(ctx).get("signed")) {
        return Ok(json!({ "allowed": true, "reason": "god-mode-signature-verified", "severity": "info", "mode": mode }));
    }

    // 3.5. TUI god-mode token → allow (TUI activates only on a real TTY).
    if truthy(crate::audit_tui_god_token::checkTuiGodToken(ctx).get("ok")) {
        return Ok(json!({ "allowed": true, "reason": "tui-god-token-verified", "severity": "info", "mode": mode }));
    }

    // JS renders `source ?? 'unknown'`; this used to be hardcoded because source was always null.
    let shown_source = if source.is_empty() { "unknown" } else { source };

    // 4. Warn mode → allow but report.
    if mode == "warn" {
        return Ok(json!({
            "allowed": true,
            "severity": "warning",
            "reason": format!("source='{shown_source}' — would be blocked in mode=block (no driver source, not allowlisted, no god signature)"),
            "mode": mode,
        }));
    }

    // 5. Block mode → deny.
    Ok(json!({
        "allowed": false,
        "reason": format!("dispatch refused: source='{shown_source}', action '{name}' not in bootstrap allowlist, no god-mode signature. Either invoke via the driver loop, sign a god-mode challenge, or add the action to the allowlist if it is bootstrap-essential."),
        "suggested_path": "export ANTILOKI_ENFORCE_DRIVER=warn temporarily to identify offending callers, fix them to declare source, then re-enable block",
        "mode": mode,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db0() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    fn call(i: Value) -> Value {
        lawHookNoLoneSkillDispatch(&i, &db0()).unwrap()
    }

    // SAFETY: ANTILOKI_ENFORCE_DRIVER is process-global; serialize since `cargo test` runs threads
    // in parallel by default — shared crate-wide lock (test_env_lock.rs). EVERY test in this file
    // reads it (directly via getMode, or transitively via lawHookNoLoneSkillDispatch), so
    // every test — not just the ones that set a non-default value — takes the lock.
    fn with_mode<T>(mode: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        match mode {
            Some(m) => unsafe { std::env::set_var("ANTILOKI_ENFORCE_DRIVER", m) },
            None => unsafe { std::env::remove_var("ANTILOKI_ENFORCE_DRIVER") },
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_ENFORCE_DRIVER");
        }
        result
    }

    #[test]
    fn no_action_name_allows() {
        with_mode(None, || {
            assert_eq!(call(json!({})), json!({ "allowed": true, "reason": "no action_name provided to hook" }));
            assert_eq!(call(json!({ "action_name": "" })), json!({ "allowed": true, "reason": "no action_name provided to hook" }));
            assert_eq!(call(json!({ "action_name": null })), json!({ "allowed": true, "reason": "no action_name provided to hook" }));
        });
    }

    #[test]
    fn allowlisted_action_allows() {
        with_mode(None, || {
            let r = call(json!({ "action_name": "system.action.find" }));
            assert_eq!(r["allowed"], json!(true));
            assert_eq!(r["reason"], json!("bootstrap-allowlist"));
            assert_eq!(r["mode"], json!("block"));

            assert_eq!(call(json!({ "action_name": "god.override.verify" }))["allowed"], json!(true));
            assert_eq!(call(json!({ "action_name": "tl.author.snippets" }))["allowed"], json!(true));
        });
    }

    #[test]
    fn non_allowlisted_action_denied_in_default_block_mode() {
        with_mode(None, || {
            let r = call(json!({ "action_name": "board-keeper.card.add" }));
            assert_eq!(r["allowed"], json!(false));
            assert_eq!(
                r["reason"],
                json!("dispatch refused: source='unknown', action 'board-keeper.card.add' not in bootstrap allowlist, no god-mode signature. Either invoke via the driver loop, sign a god-mode challenge, or add the action to the allowlist if it is bootstrap-essential.")
            );
            assert_eq!(
                r["suggested_path"],
                json!("export ANTILOKI_ENFORCE_DRIVER=warn temporarily to identify offending callers, fix them to declare source, then re-enable block")
            );
            assert_eq!(r["mode"], json!("block"));
        });
    }

    #[test]
    fn off_mode_always_allows() {
        let r = with_mode(Some("off"), || call(json!({ "action_name": "board-keeper.card.add" })));
        assert_eq!(r, json!({ "allowed": true, "mode": "off" }));
    }

    #[test]
    fn warn_mode_allows_but_reports() {
        let r = with_mode(Some("warn"), || call(json!({ "action_name": "board-keeper.card.add" })));
        assert_eq!(r["allowed"], json!(true));
        assert_eq!(r["severity"], json!("warning"));
        assert_eq!(r["mode"], json!("warn"));
    }

    #[test]
    fn get_mode_defaults_and_normalizes() {
        assert_eq!(with_mode(Some("BLOCK"), getMode), "block");
        assert_eq!(with_mode(Some("Warn"), getMode), "warn");
        assert_eq!(with_mode(Some("garbage"), getMode), "block");
        assert_eq!(with_mode(None, getMode), "block");
    }

    #[test]
    fn check_god_signature_missing_fields_is_unsigned() {
        assert_eq!(checkGodSignature(&json!({})), json!({ "signed": false }));
        assert_eq!(checkGodSignature(&json!({ "god_signature_hex": "" })), json!({ "signed": false }));
        assert_eq!(checkGodSignature(&json!({ "god_signature_hex": "ab" })), json!({ "signed": false }));
        assert_eq!(checkGodSignature(&json!({ "god_challenge": "chal" })), json!({ "signed": false }));
    }

    // ── ctx-threaded branches (were dead while the twin dropped its ctx param) ────────────────

    fn call_ctx(i: Value, ctx: Value, db: &Connection) -> Value {
        lawHookNoLoneSkillDispatchWithCtx(&i, &ctx, db).unwrap()
    }

    fn actions_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE actions (name TEXT, owner_agent TEXT);
             CREATE TABLE interviewer_turns (id INTEGER PRIMARY KEY, proposed_actions_json TEXT);
             INSERT INTO actions VALUES ('system.ai.call','system'),('god.thing','god');
             INSERT INTO interviewer_turns VALUES (7, '[{\"action\":\"system.ai.call\"}]');",
        )
        .unwrap();
        c
    }

    #[test]
    fn empty_ctx_still_renders_the_unknown_source_message() {
        // the conform lane: lawHookNoLoneSkillDispatch delegates with {}, so nothing moved.
        with_mode(Some("block"), || {
            let r = call(json!({ "action_name": "not.allowlisted" }));
            assert_eq!(r["allowed"], json!(false));
            assert!(r["reason"].as_str().unwrap().contains("source='unknown'"), "{}", r["reason"]);
        });
    }

    #[test]
    fn driver_source_without_hmac_is_refused_by_the_driver_branch() {
        // Proves branch 1 is REACHABLE now. Previously source was dropped, so this fell through to
        // the generic allowlist refusal and the HMAC gate never ran at all.
        with_mode(Some("block"), || {
            let c = actions_db();
            let r = call_ctx(json!({ "action_name": "not.allowlisted" }), json!({ "source": "driver" }), &c);
            assert_eq!(r["allowed"], json!(false));
            let reason = r["reason"].as_str().unwrap();
            assert!(reason.contains("source='driver' but HMAC verification failed"), "{reason}");
        });
    }

    #[test]
    fn interviewer_confirmed_allows_an_action_present_in_the_turn_plan() {
        with_mode(Some("block"), || {
            let c = actions_db();
            let ctx = json!({ "source": "interviewer:confirmed", "dispatch_turn_id": 7 });
            let r = call_ctx(json!({ "action_name": "system.ai.call" }), ctx, &c);
            assert_eq!(r["allowed"], json!(true));
            assert_eq!(r["reason"], json!("interviewer:confirmed+action-in-plan"));
        });
    }

    #[test]
    fn interviewer_confirmed_refuses_an_action_not_in_the_plan() {
        with_mode(Some("block"), || {
            let c = actions_db();
            let ctx = json!({ "source": "interviewer:confirmed", "dispatch_turn_id": 7 });
            let r = call_ctx(json!({ "action_name": "some.other.action" }), ctx, &c);
            assert_eq!(r["allowed"], json!(false));
            assert!(r["reason"].as_str().unwrap().contains("not in turn"), "{}", r["reason"]);
        });
    }

    #[test]
    fn interviewer_confirmed_never_reaches_a_god_owned_action() {
        with_mode(Some("block"), || {
            let c = actions_db();
            let ctx = json!({ "source": "interviewer:confirmed", "dispatch_turn_id": 7 });
            let r = call_ctx(json!({ "action_name": "god.thing" }), ctx, &c);
            assert_eq!(r["allowed"], json!(false));
            assert!(r["reason"].as_str().unwrap().contains("god-owned"), "{}", r["reason"]);
        });
    }

    #[test]
    fn a_real_source_is_named_in_the_refusal_instead_of_unknown() {
        with_mode(Some("block"), || {
            let c = actions_db();
            let r = call_ctx(json!({ "action_name": "not.allowlisted" }), json!({ "source": "cli" }), &c);
            assert!(r["reason"].as_str().unwrap().contains("source='cli'"), "{}", r["reason"]);
        });
    }
}
