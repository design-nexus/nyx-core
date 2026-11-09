//! A backend with one topic, `counter`, that counts up every 200 ms, and an `Add`
//! operation. For trying an interface against: `NYX_COUNTER_UI_SOCKET=/tmp/c.sock
//! cargo run -p nyx-ui-link --example counter`.

use serde_json::json;
use std::sync::{Arc, Mutex};

fn main() {
    let n = Arc::new(Mutex::new(0i64));
    let link = {
        let n = n.clone();
        nyx_ui_link::serve(nyx_ui_link::Options::new("nyx-counter"), move |r| match r.op.as_str() {
            "Add" => {
                let by = r.body.get("by").and_then(|v| v.as_i64()).unwrap_or(1);
                *n.lock().unwrap() += by;
                r.ok(json!({}))
            }
            _ => {
                let op = r.op.clone();
                r.err("unknown_op", op)
            }
        })
        .expect("serve")
    };
    println!("{}", link.path().display());
    loop {
        let v = {
            let mut g = n.lock().unwrap();
            *g += 1;
            *g
        };
        link.publish("counter", json!({"value": v, "label": "count", "list": [v % 3, 1]}));
        if v % 5 == 0 {
            link.emit("Tick", json!({"value": v}));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}
