//! The zbus control server on a private D-Bus, called through the registry: the same
//! rules as the GTK helper, and topics to watchers.
#![cfg(feature = "server")]

use nyx_actions::server::{self, Request};
use nyx_actions::{ErrorKind, InvokeOptions, Options, Registry};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const APP: &str = "io.github.design_nexus.nyx.Home";
const BUS: &str = "io.github.design_nexus.nyx.Home.Automation";
const PATH: &str = "/io/github/design_nexus/nyx/Home/Automation";

struct Bus {
    child: Child,
    address: String,
    _dir: tempfile::TempDir,
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn bus() -> Bus {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("bus.conf");
    std::fs::write(&config, format!(r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>unix:path={}</listen><auth>EXTERNAL</auth>
<policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>"#, dir.path().join("bus").display())).unwrap();
    let mut child = Command::new("dbus-daemon")
        .arg(format!("--config-file={}", config.display()))
        .args(["--nofork", "--print-address=1"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut address = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut address).unwrap();
    Bus { child, address: address.trim().to_string(), _dir: dir }
}

fn description() -> Value {
    json!({
        "tools": [
            {"name": "home.devices", "description": "Devices.", "x-nyx": {"risk": "read_only"}},
            {"name": "home.switch", "description": "Switch a device.", "inputSchema": {"type": "object", "properties": {"device": {"type": "string"}}, "required": ["device"]}, "x-nyx": {"risk": "writes"}},
            {"name": "home.forget", "description": "Forget a device.", "x-nyx": {"risk": "destructive"}}
        ],
        "nyx": {"api": 2, "app": APP, "name": "Home", "namespaces": ["home"], "bus_name": BUS, "object_path": PATH, "activation": "running", "topics": [{"name": "devices"}]}
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn daemons_serve_with_the_same_rules() {
    let b = bus();
    let manifests = tempfile::tempdir().unwrap();
    std::fs::write(manifests.path().join(format!("{APP}.json")), description().to_string()).unwrap();
    let conn = zbus::connection::Builder::address(b.address.as_str()).unwrap().name(BUS).unwrap().build().await.unwrap();
    let seen: Arc<Mutex<Vec<Request>>> = Arc::default();
    let record = seen.clone();
    let handler: server::Handler = Arc::new(move |r: Request| {
        record.lock().unwrap().push(r.clone());
        Box::pin(async move { Ok(json!({"ran": r.action})) })
    });
    let own = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
    let served = server::serve(&conn, PATH, BUS, description(), Some(own), handler).await.unwrap();
    let watching = Arc::new(Mutex::new(Vec::new()));
    {
        let (w, s) = (watching.clone(), served.clone());
        served.on_watch(move |topic, on| {
            w.lock().unwrap().push((topic.to_string(), on));
            if on {
                let s = s.clone();
                tokio::spawn(async move { s.publish("devices", json!({"lamp": "off"})).await });
            }
        });
    }

    let registry =
        Registry::new(Options { dirs: vec![manifests.path().to_path_buf()], bus_address: Some(b.address.clone()), live: true });
    let person = InvokeOptions { trusted: true, frontend: "test".into(), ..Default::default() };
    assert_eq!(registry.invoke("home.devices", json!({}), &person).await.unwrap()["ran"], "home.devices");
    assert_eq!(registry.invoke("home.switch", json!({"device": "lamp"}), &person).await.unwrap()["ran"], "home.switch");
    assert_eq!(registry.invoke("home.switch", json!({}), &person).await.unwrap_err().kind, ErrorKind::InvalidArguments);
    let forget = registry.invoke("home.forget", json!({}), &InvokeOptions { confirmed: true, ..person.clone() }).await.unwrap();
    assert_eq!(forget["ran"], "home.forget");
    let r = seen.lock().unwrap().last().unwrap().clone();
    assert!(r.confirmed && r.caller.trusted && r.frontend.as_deref() == Some("test"));

    // The test binary from elsewhere isn't one of the server's fellow programs.
    let strict = tempfile::tempdir().unwrap();
    let conn2 = zbus::connection::Builder::address(b.address.as_str()).unwrap().build().await.unwrap();
    let handler2: server::Handler = Arc::new(|r: Request| Box::pin(async move { Ok(json!({"ran": r.action})) }));
    server::serve(&conn2, "/strict", BUS, description(), Some(strict.path().to_path_buf()), handler2).await.unwrap();
    let call = |method: &'static str, body: (String, String, String)| {
        let conn = conn.clone();
        let dest = conn2.unique_name().unwrap().to_string();
        async move {
            let reply = conn
                .call_method(Some(dest.as_str()), "/strict", Some("io.github.design_nexus.nyx.Control"), method, &body)
                .await
                .unwrap();
            serde_json::from_str::<Value>(&reply.body().deserialize::<String>().unwrap()).unwrap()
        }
    };
    let refused = call("Invoke", ("home.switch".into(), r#"{"device":"lamp"}"#.into(), r#"{"confirmed":true}"#.into())).await;
    assert_eq!(refused["error"]["kind"], "permission_denied");
    let read = call("Invoke", ("home.devices".into(), "{}".into(), "{}".into())).await;
    assert_eq!(read["ok"]["ran"], "home.devices");

    let mut live = registry.watch(APP, "devices").await.unwrap();
    let mut first = live.next().await.unwrap().unwrap();
    if first.is_null() {
        first = tokio::time::timeout(Duration::from_secs(3), live.next()).await.unwrap().unwrap().unwrap();
    }
    assert_eq!(first, json!({"lamp": "off"}));
    served.publish("devices", json!({"lamp": "on"})).await;
    let changed = tokio::time::timeout(Duration::from_secs(3), live.next()).await.unwrap().unwrap().unwrap();
    assert_eq!(changed, json!({"lamp": "on"}));
    drop(live);
    for _ in 0..100 {
        if watching.lock().unwrap().contains(&("devices".to_string(), false)) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(*watching.lock().unwrap(), vec![("devices".to_string(), true), ("devices".to_string(), false)]);
}
