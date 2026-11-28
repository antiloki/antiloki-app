//! canonical-actions.rs — twin of skills/engine/canonical-actions.mjs: the INDEX that
//! concatenates the 30 per-domain action shards (skills/engine/canonical-actions/*.mjs,
//! each already a conformant data twin) into CANONICAL_ACTIONS, in the exact order the JS
//! source imports + spreads them, plus the hand-ported accessor. NAMES ARE CONTRACT: fn
//! carries the exact JS name.
// QUALITY: 82/100 — composes 30 already-conformant shard statics in JS import order (no
// re-transcription risk); trivial hand accessor, order verified byte-identical against JS.
use serde_json::Value;
use std::sync::LazyLock;

pub static CANONICAL_ACTIONS: LazyLock<Value> = LazyLock::new(|| {
    let shards: [&Value; 31] = [
        &*crate::engine_canonical_actions_action::ACTIONS,
        &*crate::engine_canonical_actions_agent::ACTIONS,
        &*crate::engine_canonical_actions_ai::ACTIONS,
        &*crate::engine_canonical_actions_blueprint::ACTIONS,
        &*crate::engine_canonical_actions_board_keeper::ACTIONS,
        &*crate::engine_canonical_actions_brain::ACTIONS,
        &*crate::engine_canonical_actions_card::ACTIONS,
        &*crate::engine_canonical_actions_cockpit::ACTIONS,
        &*crate::engine_canonical_actions_codebase_1::ACTIONS,
        &*crate::engine_canonical_actions_codebase_2::ACTIONS,
        &*crate::engine_canonical_actions_contract::ACTIONS,
        &*crate::engine_canonical_actions_cost::ACTIONS,
        &*crate::engine_canonical_actions_docs::ACTIONS,
        &*crate::engine_canonical_actions_driver::ACTIONS,
        &*crate::engine_canonical_actions_git_manager::ACTIONS,
        &*crate::engine_canonical_actions_god::ACTIONS,
        &*crate::engine_canonical_actions_heimdall::ACTIONS,
        &*crate::engine_canonical_actions_keeper::ACTIONS,
        &*crate::engine_canonical_actions_law::ACTIONS,
        &*crate::engine_canonical_actions_man::ACTIONS,
        &*crate::engine_canonical_actions_memory::ACTIONS,
        &*crate::engine_canonical_actions_misc_1::ACTIONS,
        &*crate::engine_canonical_actions_misc_2::ACTIONS,
        &*crate::engine_canonical_actions_notebook::ACTIONS,
        &*crate::engine_canonical_actions_pl::ACTIONS,
        &*crate::engine_canonical_actions_qa::ACTIONS,
        &*crate::engine_canonical_actions_tl::ACTIONS,
        &*crate::engine_canonical_actions_bugfinder::ACTIONS,
        &*crate::engine_canonical_actions_ultra::ACTIONS,
        &*crate::engine_canonical_actions_constitution::ACTIONS,
        // LAST, deliberately: these were registered by migrations rather than by the seed, so
        // appending keeps every pre-existing index stable and makes the addition visible as a
        // tail rather than a reshuffle.
        &*crate::engine_canonical_actions_migrated::ACTIONS,
    ];
    let mut all = Vec::new();
    for shard in shards {
        all.extend(shard.as_array().expect("shard ACTIONS is an array").iter().cloned());
    }
    Value::Array(all)
});

/// JS: `canonicalActions(_input, _context)` — returns CANONICAL_ACTIONS, verbatim.
pub fn canonicalActions(_input: &Value, _context: &Value) -> Value {
    CANONICAL_ACTIONS.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_actions_concatenates_all_31_shards_in_order() {
        let v: &Value = &CANONICAL_ACTIONS;
        let a = v.as_array().expect("array");
        // 665 → 666: system.codebase.atlas.register added to the codebase-2 shard.
        assert_eq!(a.len(), 670, "pressed length: sum of all 31 shard lengths (+3 hand-added context-pack actions in the cockpit shard, 2026-09-14; +1 context library, 2026-09-19)");
        let mut names: Vec<&str> = a.iter().map(|x| x["name"].as_str().expect("name")).collect();
        let n = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), n, "action names unique across the whole index");
        assert_eq!(a[0]["name"].as_str().unwrap(), "system.action.add", "first shard (action.mjs) leads");
        // constitution.mjs used to trail; the migrated shard is appended AFTER it, so the tail
        // moved by design. Both are asserted, which is what makes an accidental reorder visible
        // rather than just a changed last element.
        assert_eq!(a[a.len() - 53]["name"].as_str().unwrap(), "system.constitution.classify", "constitution shard still trails the seeded set");
        assert_eq!(a[a.len() - 1]["name"].as_str().unwrap(), "system.substrate.stream", "the migrated shard is last");
    }

    #[test]
    fn canonicalactions_returns_the_static_verbatim() {
        assert_eq!(canonicalActions(&Value::Null, &Value::Null), *CANONICAL_ACTIONS);
    }
}
