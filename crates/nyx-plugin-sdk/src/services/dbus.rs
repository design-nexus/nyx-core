//! Small D-Bus helpers over zbus's blocking API: read all properties of an object,
//! pick values out of them, and listen for a service's signals.

use std::collections::HashMap;
use std::sync::mpsc::Sender;
pub use zbus::blocking::Connection;
use zbus::zvariant::{OwnedValue, Value};

pub type Props = HashMap<String, OwnedValue>;

pub fn get_all(conn: &Connection, dest: &str, path: &str, iface: &str) -> Option<Props> {
    let reply = conn.call_method(Some(dest), path, Some("org.freedesktop.DBus.Properties"), "GetAll", &(iface,)).ok()?;
    reply.body().deserialize::<Props>().ok()
}

pub fn set(conn: &Connection, dest: &str, path: &str, iface: &str, prop: &str, value: Value<'_>) -> bool {
    conn.call_method(Some(dest), path, Some("org.freedesktop.DBus.Properties"), "Set", &(iface, prop, value)).is_ok()
}

/// A variant inside a variant, unwrapped.
pub fn plain<'a>(v: &'a Value<'a>) -> &'a Value<'a> {
    match v {
        Value::Value(inner) => plain(inner),
        other => other,
    }
}

pub fn str_of(v: &Value) -> Option<String> {
    match plain(v) {
        Value::Str(s) => Some(s.to_string()),
        Value::ObjectPath(p) => Some(p.to_string()),
        _ => None,
    }
}

pub fn bool_of(v: &Value) -> Option<bool> {
    match plain(v) {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

pub fn num_of(v: &Value) -> Option<f64> {
    Some(match plain(v) {
        Value::U8(n) => *n as f64,
        Value::U16(n) => *n as f64,
        Value::I16(n) => *n as f64,
        Value::U32(n) => *n as f64,
        Value::I32(n) => *n as f64,
        Value::U64(n) => *n as f64,
        Value::I64(n) => *n as f64,
        Value::F64(n) => *n,
        _ => return None,
    })
}

pub fn strs_of(v: &Value) -> Vec<String> {
    match plain(v) {
        Value::Array(a) => a.iter().filter_map(str_of).collect(),
        _ => vec![],
    }
}

pub fn prop_str(p: &Props, k: &str) -> Option<String> {
    p.get(k).and_then(|v| str_of(v))
}

pub fn prop_bool(p: &Props, k: &str) -> Option<bool> {
    p.get(k).and_then(|v| bool_of(v))
}

pub fn prop_num(p: &Props, k: &str) -> Option<f64> {
    p.get(k).and_then(|v| num_of(v))
}

pub fn prop_strs(p: &Props, k: &str) -> Vec<String> {
    p.get(k).map(|v| strs_of(v)).unwrap_or_default()
}

/// Entries of an `a{sv}` value (MPRIS metadata).
pub fn dict_of(v: &Value) -> HashMap<String, OwnedValue> {
    let mut out = HashMap::new();
    if let Value::Dict(d) = plain(v) {
        for (k, val) in d.iter() {
            if let (Some(k), Ok(val)) = (str_of(k), OwnedValue::try_from(plain(val).try_clone().unwrap_or(Value::Bool(false)))) {
                out.insert(k, val);
            }
        }
    }
    out
}

/// Every signal from `sender` (and, optionally, for `path`) nudges `trigger`.
pub fn listen(conn: &Connection, sender: Option<&str>, path_namespace: Option<&str>, trigger: Sender<()>) {
    let mut b = zbus::MatchRule::builder().msg_type(zbus::message::Type::Signal);
    if let Some(s) = sender {
        b = match b.sender(s.to_string()) {
            Ok(b) => b,
            Err(_) => return,
        };
    }
    if let Some(p) = path_namespace {
        b = match b.path_namespace(p.to_string()) {
            Ok(b) => b,
            Err(_) => return,
        };
    }
    let rule = b.build();
    let conn = conn.clone();
    std::thread::spawn(move || {
        let Ok(iter) = zbus::blocking::MessageIterator::for_match_rule(rule, &conn, Some(64)) else { return };
        for _ in iter {
            if trigger.send(()).is_err() {
                return;
            }
        }
    });
}
