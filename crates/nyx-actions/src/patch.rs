//! RFC 7396 JSON Merge Patches, as topics' changes travel.

use serde_json::{Map, Value};

/// The patch that turns `old` into `new` (a `null` inside an object reads as a removed key).
pub fn diff(old: &Value, new: &Value) -> Value {
    match (old, new) {
        (Value::Object(o), Value::Object(n)) => {
            let mut patch = Map::new();
            for k in o.keys().filter(|k| !n.contains_key(*k)) {
                patch.insert(k.clone(), Value::Null);
            }
            for (k, v) in n {
                match o.get(k) {
                    Some(old) if old == v => {}
                    Some(old @ Value::Object(_)) if v.is_object() => {
                        patch.insert(k.clone(), diff(old, v));
                    }
                    _ => {
                        patch.insert(k.clone(), v.clone());
                    }
                }
            }
            Value::Object(patch)
        }
        _ => new.clone(),
    }
}

/// `patch` merged into `target`.
pub fn apply(target: &mut Value, patch: &Value) {
    let Value::Object(p) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Default::default());
    }
    let t = target.as_object_mut().expect("an object");
    for (k, v) in p {
        if v.is_null() {
            t.remove(k);
        } else {
            apply(t.entry(k.clone()).or_insert(Value::Null), v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn diffs_apply_back() {
        let old = json!({"title": "A", "art": "x", "volume": {"level": 0.5, "muted": false}});
        let new = json!({"title": "B", "volume": {"level": 0.5, "muted": true}, "queue": [1]});
        let p = diff(&old, &new);
        assert_eq!(p, json!({"title": "B", "art": null, "volume": {"muted": true}, "queue": [1]}));
        let mut back = old.clone();
        apply(&mut back, &p);
        assert_eq!(back, new);
        apply(&mut back, &json!([2]));
        assert_eq!(back, json!([2]));
    }
}
