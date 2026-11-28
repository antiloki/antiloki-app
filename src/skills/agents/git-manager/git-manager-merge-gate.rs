//! git-manager-merge-gate.rs — twin of skills/agents/git-manager/git-manager-merge-gate.mjs (db READ).
//! NAMES ARE CONTRACT: `gitManagerMergeGate` (default) + `DEFAULT_REQUIRED` (named). Go/no-go on phases.
// QUALITY: 84/100 — required-phase subset check over DISTINCT card_phase_events; advice string branch
use rusqlite::Connection;
use serde_json::{json, Value};

pub const DEFAULT_REQUIRED: [&str; 4] = ["pl_draft", "tl_sign", "dev_impl", "qa_pass"];

pub fn gitManagerMergeGate(input: &Value, db: &Connection) -> Result<Value, String> {
    let card_slug = input.get("card_slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let card_slug = match card_slug { Some(s) => s.to_string(), None => return Err("git-manager-merge-gate: card_slug is required".into()) };
    // required defaults to DEFAULT_REQUIRED; echoed back verbatim in the result
    let required: Value = match input.get("required").filter(|v| v.is_array()) {
        Some(v) => v.clone(),
        None => json!(DEFAULT_REQUIRED),
    };

    let card = crate::db_json::query_rows(db, "SELECT slug FROM cards WHERE slug = ?", &[json!(card_slug)])?;
    if card.as_array().map(|a| a.is_empty()).unwrap_or(true) {
        return Err(format!("git-manager-merge-gate: card '{card_slug}' not found"));
    }

    let rows = crate::db_json::query_rows(db, "SELECT DISTINCT phase FROM card_phase_events WHERE card_slug = ?", &[json!(card_slug)])?;
    let present_set: std::collections::HashSet<String> = rows.as_array().unwrap().iter()
        .filter_map(|r| r.get("phase").and_then(|v| v.as_str()).map(|s| s.to_string())).collect();

    let req_strs: Vec<String> = required.as_array().unwrap().iter().map(|v| v.as_str().unwrap_or("").to_string()).collect();
    let missing: Vec<Value> = req_strs.iter().filter(|p| !present_set.contains(*p)).map(|p| json!(p)).collect();
    let ok = missing.is_empty();
    let mut present_sorted: Vec<String> = present_set.into_iter().collect();
    present_sorted.sort();

    let advice = if ok {
        "All required phases present. Caller may proceed with merge.".to_string()
    } else {
        let ms: Vec<&str> = missing.iter().map(|v| v.as_str().unwrap()).collect();
        format!("Refusing merge: missing {}. Record those events first.", ms.join(", "))
    };
    Ok(json!({ "ok": ok, "card_slug": card_slug, "required": required, "present": present_sorted, "missing": missing, "advice": advice }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gates_on_required_phases() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE cards (slug TEXT); CREATE TABLE card_phase_events (card_slug TEXT, phase TEXT); \
          INSERT INTO cards VALUES ('C'); INSERT INTO card_phase_events VALUES ('C','pl_draft'),('C','tl_sign'),('C','dev_impl');").unwrap();
        let r = gitManagerMergeGate(&json!({ "card_slug": "C" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["missing"], json!(["qa_pass"]));
        assert_eq!(r["present"], json!(["dev_impl", "pl_draft", "tl_sign"]));
        assert!(gitManagerMergeGate(&json!({ "card_slug": "NOPE" }), &c).is_err());
    }
}
