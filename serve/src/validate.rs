//! Port of the BE's global ValidationPipe for routes whose @Body() is a DTO class.
//!
//! main.ts configures it as `{ whitelist: true, forbidNonWhitelisted: true, transform: true }`,
//! which is stricter than it looks:
//!   - `forbidNonWhitelisted` turns an undeclared property into a 400, not a silent strip;
//!   - failures use NEST's envelope `{message: string[], error, statusCode}` — never the engine's;
//!   - `transform` only instantiates the class. Implicit conversion is OFF, so "5" does NOT become
//!     5 for an @IsInt field; it fails. No coercion is performed here either.
//!
//! Message strings are class-validator's defaults and must match verbatim — the FE renders them.
//! Verified against the W0 capture: `POST /api/v1/blueprint/get` with an empty body produces
//! "contract_slug must be longer than or equal to 1 characters".
//!
//! Message ORDER is part of the contract and is reproduced, not approximated: TypeScript
//! decorators evaluate bottom-up, so the decorator written LAST registers its validator FIRST.
//! `BlueprintGetDto` declares @IsString() then @MinLength(1) and the BE answers the MinLength
//! message first — see the byte-for-byte test below. Rules arrive in source order and are
//! iterated `.rev()`.
//!
//! STILL UNPROVEN: ordering when MULTIPLE properties fail at once (this emits unknown-property
//! messages first, then DTO declaration order). No captured response exercises that case.

use serde_json::{json, Value};

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Validate {
    pub props: Vec<Prop>,
    #[serde(default)]
    pub forbid_unknown: bool,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Prop {
    pub name: String,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Rule {
    pub t: String,
    #[serde(default)]
    pub n: Option<f64>,
    #[serde(default)]
    pub each: bool,
    #[serde(default)]
    pub values: Vec<String>,
}

fn num(n: f64) -> String {
    if n.fract() == 0.0 { format!("{}", n as i64) } else { format!("{n}") }
}

/// One constraint against one value. `None` = passed.
fn check(rule: &Rule, prop: &str, v: &Value, each_prefix: bool) -> Option<String> {
    let label = if each_prefix { format!("each value in {prop}") } else { prop.to_string() };
    let fail = |msg: String| Some(msg);
    match rule.t.as_str() {
        "string" => (!v.is_string()).then(|| format!("{label} must be a string")),
        "bool" => (!v.is_boolean()).then(|| format!("{label} must be a boolean value")),
        "array" => (!v.is_array()).then(|| format!("{label} must be an array")),
        "object" => (!v.is_object() || v.is_array())
            .then(|| format!("{label} must be an object")),
        "int" => (!v.as_i64().is_some_and(|_| v.as_f64().is_some_and(|f| f.fract() == 0.0)))
            .then(|| format!("{label} must be an integer number")),
        "number" => (!v.is_number())
            .then(|| format!("{label} must be a number conforming to the specified constraints")),
        // Each length/range validator checks the TYPE first, so a non-string fails MinLength on
        // its own — it does not merely defer to @IsString. That is why a missing required field
        // produces two messages, not one.
        "min_length" => {
            let ok = v.as_str().is_some_and(|s| (s.chars().count() as f64) >= rule.n.unwrap_or(0.0));
            (!ok).then(|| {
                format!(
                    "{label} must be longer than or equal to {} characters",
                    num(rule.n.unwrap_or(0.0))
                )
            })
        }
        "max_length" => {
            let ok = v.as_str().is_some_and(|s| (s.chars().count() as f64) <= rule.n.unwrap_or(0.0));
            (!ok).then(|| {
                format!(
                    "{label} must be shorter than or equal to {} characters",
                    num(rule.n.unwrap_or(0.0))
                )
            })
        }
        "min" => {
            let ok = v.as_f64().is_some_and(|f| f >= rule.n.unwrap_or(0.0));
            (!ok).then(|| format!("{label} must not be less than {}", num(rule.n.unwrap_or(0.0))))
        }
        "max" => {
            let ok = v.as_f64().is_some_and(|f| f <= rule.n.unwrap_or(0.0));
            (!ok).then(|| format!("{label} must not be greater than {}", num(rule.n.unwrap_or(0.0))))
        }
        "is_in" => {
            let ok = v.as_str().map(|s| rule.values.iter().any(|x| x == s)).unwrap_or(false);
            (!ok).then(|| {
                format!("{label} must be one of the following values: {}", rule.values.join(", "))
            })
        }
        _ => None,
    }
}

/// Returns the Nest 400 body when validation fails, or None when the body passes.
pub fn validate(v: &Validate, body: &Value) -> Option<Value> {
    let mut messages: Vec<String> = Vec::new();
    let empty = serde_json::Map::new();
    let map = body.as_object().unwrap_or(&empty);

    if v.forbid_unknown {
        for key in map.keys() {
            if !v.props.iter().any(|p| &p.name == key) {
                messages.push(format!("property {key} should not exist"));
            }
        }
    }

    for prop in &v.props {
        let present = map.get(&prop.name).filter(|x| !x.is_null());
        let Some(value) = present else {
            // @IsOptional short-circuits every other constraint on that property.
            if prop.optional {
                continue;
            }
            // A required-but-absent property fails EVERY one of its constraints against
            // `undefined`. Reported in reverse source order — see the .rev() note below.
            for rule in prop.rules.iter().rev() {
                if let Some(m) = check(rule, &prop.name, &Value::Null, false) {
                    messages.push(m);
                }
            }
            continue;
        };

        // .rev() because TypeScript decorators evaluate bottom-up: the decorator written LAST
        // registers its validator FIRST, and class-validator emits messages in registration
        // order. The extractor stores rules in source order, so reversing here reproduces the
        // BE's message sequence exactly (proven against blueprint/get in the W0 golden).
        for rule in prop.rules.iter().rev() {
            if rule.each {
                if let Some(items) = value.as_array() {
                    for item in items {
                        if let Some(m) = check(rule, &prop.name, item, true) {
                            messages.push(m);
                            break; // class-validator reports the constraint once per property
                        }
                    }
                }
            } else if let Some(m) = check(rule, &prop.name, value, false) {
                messages.push(m);
            }
        }
    }

    if messages.is_empty() {
        return None;
    }
    Some(json!({ "message": messages, "error": "Bad Request", "statusCode": 400 }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// BlueprintGetDto: `@IsString()` then `@MinLength(1)` on a required contract_slug.
    /// Rules are stored in SOURCE order; the validator reverses them.
    fn blueprint_dto() -> Validate {
        serde_json::from_value(json!({
            "forbid_unknown": true,
            "props": [{
                "name": "contract_slug",
                "optional": false,
                "rules": [{"t":"string"}, {"t":"min_length","n":1}]
            }]
        }))
        .unwrap()
    }

    #[test]
    fn missing_required_matches_the_be_byte_for_byte() {
        // Captured from the live BE in W0. Order is REVERSED from the decorator source order
        // because TypeScript decorators evaluate bottom-up.
        let out = validate(&blueprint_dto(), &json!({})).expect("should fail");
        assert_eq!(
            out.to_string(),
            r#"{"message":["contract_slug must be longer than or equal to 1 characters","contract_slug must be a string"],"error":"Bad Request","statusCode":400}"#
        );
    }

    #[test]
    fn forbid_non_whitelisted_rejects_unknown_property() {
        let out = validate(&blueprint_dto(), &json!({"contract_slug":"x","bogus":1})).expect("should fail");
        assert_eq!(out["message"][0], json!("property bogus should not exist"));
    }

    #[test]
    fn valid_body_passes() {
        assert!(validate(&blueprint_dto(), &json!({"contract_slug":"x"})).is_none());
    }

    #[test]
    fn optional_absent_property_skips_all_its_constraints() {
        let v: Validate = serde_json::from_value(json!({
            "forbid_unknown": false,
            "props": [{"name":"field","optional":true,"rules":[{"t":"string"},{"t":"max_length","n":3}]}]
        }))
        .unwrap();
        assert!(validate(&v, &json!({})).is_none());
        // ...but a PRESENT value is still checked
        assert!(validate(&v, &json!({"field":"toolong"})).is_some());
    }

    #[test]
    fn length_validators_check_type_themselves() {
        // MinLength does not defer to @IsString: a non-string fails BOTH, which is why one
        // missing field yields two messages rather than one.
        let v: Validate = serde_json::from_value(json!({
            "forbid_unknown": false,
            "props": [{"name":"n","optional":false,"rules":[{"t":"string"},{"t":"min_length","n":1}]}]
        }))
        .unwrap();
        let out = validate(&v, &json!({"n": 5})).expect("should fail");
        assert_eq!(out["message"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn is_in_lists_allowed_values() {
        let v: Validate = serde_json::from_value(json!({
            "forbid_unknown": false,
            "props": [{"name":"operation","optional":false,
                       "rules":[{"t":"is_in","values":["expand","summarize"]}]}]
        }))
        .unwrap();
        let out = validate(&v, &json!({"operation":"nope"})).expect("should fail");
        assert_eq!(
            out["message"][0],
            json!("operation must be one of the following values: expand, summarize")
        );
    }
}
