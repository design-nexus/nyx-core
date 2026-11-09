//! JSON Merge Patch (RFC 7386): what changed between two states of a topic.
//!
//! Objects are compared key by key; anything else (arrays, strings, numbers) is
//! replaced whole when it differs. A key set to `null` is removed, so a topic's state
//! treats `null` and a missing key the same.

use serde_json::{Map, Value};

/// The merge patch that turns `old` into `new`, or `None` when they are equal.
pub fn diff(old: &Value, new: &Value) -> Option<Value> {
    if old == new {
        return None;
    }
    match (old, new) {
        (Value::Object(a), Value::Object(b)) => {
            let mut out = Map::new();
            for key in a.keys() {
                if !b.contains_key(key) {
                    out.insert(key.clone(), Value::Null);
                }
            }
            for (key, value) in b {
                match a.get(key) {
                    None => {
                        if !value.is_null() {
                            out.insert(key.clone(), strip_nulls(value));
                        }
                    }
                    Some(before) => {
                        if value.is_null() {
                            if !before.is_null() {
                                out.insert(key.clone(), Value::Null);
                            }
                        } else if let Some(p) = diff(before, value) {
                            out.insert(key.clone(), p);
                        }
                    }
                }
            }
            (!out.is_empty()).then_some(Value::Object(out))
        }
        _ => Some(strip_nulls(new)),
    }
}

/// Apply a merge patch to `target`.
pub fn apply(target: &mut Value, patch: &Value) {
    let Value::Object(changes) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    let Value::Object(map) = target else { unreachable!() };
    for (key, value) in changes {
        if value.is_null() {
            map.remove(key);
        } else {
            apply(map.entry(key.clone()).or_insert(Value::Null), value);
        }
    }
}

/// Object members set to `null` dropped, since a patch can't carry them.
fn strip_nulls(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            Value::Object(m.iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k.clone(), strip_nulls(v))).collect())
        }
        _ => v.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn roundtrip(a: Value, b: Value) {
        let mut x = a.clone();
        if let Some(p) = diff(&a, &b) {
            apply(&mut x, &p);
        }
        assert_eq!(strip_nulls(&x), strip_nulls(&b), "from {a} to {b}");
    }

    #[test]
    fn equal_states_have_no_patch() {
        assert_eq!(diff(&json!({"a": [1, 2]}), &json!({"a": [1, 2]})), None);
    }

    #[test]
    fn only_changed_members_are_sent() {
        let p = diff(&json!({"a": 1, "b": {"c": 2, "d": 3}, "e": 4}), &json!({"a": 1, "b": {"c": 2, "d": 5}})).unwrap();
        assert_eq!(p, json!({"b": {"d": 5}, "e": null}));
    }

    #[test]
    fn arrays_and_scalars_are_replaced() {
        assert_eq!(diff(&json!({"l": [1, 2]}), &json!({"l": [1, 3]})).unwrap(), json!({"l": [1, 3]}));
        assert_eq!(diff(&json!(1), &json!("x")).unwrap(), json!("x"));
    }

    #[test]
    fn patches_reproduce_the_new_state() {
        roundtrip(json!({}), json!({"a": {"b": [1, {"c": null}]}}));
        roundtrip(json!({"a": {"b": 1}}), json!({"a": 2}));
        roundtrip(json!({"a": 2}), json!({"a": {"b": 1, "n": null}}));
        roundtrip(json!({"a": 1, "b": null}), json!({"a": null}));
        roundtrip(json!([1]), json!({"x": 1}));
        roundtrip(json!({"x": 1}), json!([1]));
    }
}
