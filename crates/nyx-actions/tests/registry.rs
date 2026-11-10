//! The registry against apps on a private D-Bus: an API 2 app, an API 1 app, a closed
//! app D-Bus starts (this test binary again, as `startable_app`), and one that answers
//! only while open.

use nyx_actions::{ErrorKind, InvokeOptions, Options, Registry, State};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const NOTES: &str = "io.github.design_nexus.nyx.Notes";
const CHAT: &str = "io.github.design_nexus.nyx.Chat";
const WEATHER: &str = "io.github.design_nexus.nyx.Weather";
const MAIL: &str = "io.github.design_nexus.nyx.Mail";
const FAKE: &str = "NYX_ACTIONS_TEST_FAKE";

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

fn bus(dir: tempfile::TempDir, services: &Path) -> Bus {
    let config = dir.path().join("bus.conf");
    std::fs::write(
        &config,
        format!(
            r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>unix:path={}</listen><servicedir>{}</servicedir><auth>EXTERNAL</auth>
<policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>"#,
            dir.path().join("bus").display(),
            services.display()
        ),
    )
    .unwrap();
    let mut child = Command::new("dbus-daemon")
        .arg(format!("--config-file={}", config.display()))
        .args(["--nofork", "--print-address=1"])
        .env(FAKE, "1")
        .stdout(Stdio::piped())
        .spawn()
        .expect("dbus-daemon");
    let mut address = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut address).unwrap();
    Bus { child, address: address.trim().to_string(), _dir: dir }
}

fn notes_description() -> Value {
    json!({
        "tools": [
            {"name": "notes.search", "description": "Search notes.", "inputSchema": {"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]}, "x-nyx": {"risk": "read_only"}},
            {"name": "notes.create", "description": "Create a note.", "x-nyx": {"risk": "writes"}},
            {"name": "notes.delete", "description": "Delete a note.", "x-nyx": {"risk": "destructive"}},
            {"name": "notes.slow", "description": "Takes its time.", "x-nyx": {"risk": "read_only", "timeout_ms": 300}}
        ],
        "nyx": {"api": 2, "app": NOTES, "name": "Notes", "namespaces": ["notes"], "bus_name": NOTES, "activation": "dbus", "topics": [{"name": "recent"}]}
    })
}

#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<(String, Value)>>>);

struct Notes(Seen);

#[zbus::interface(name = "io.github.design_nexus.nyx.Control")]
impl Notes {
    fn describe(&self) -> String {
        notes_description().to_string()
    }

    async fn invoke(&self, action: String, arguments: String, options: String) -> String {
        let options: Value = serde_json::from_str(&options).unwrap();
        self.0.0.lock().unwrap().push((action.clone(), options));
        let args: Value = serde_json::from_str(&arguments).unwrap();
        match action.as_str() {
            "notes.slow" => {
                tokio::time::sleep(Duration::from_secs(2)).await;
                json!({"ok": null}).to_string()
            }
            "notes.search" if args["query"] == "fail" => {
                json!({"error": {"kind": "permission_denied", "message": "the vault is locked"}}).to_string()
            }
            _ => json!({"ok": {"action": action, "arguments": args}}).to_string(),
        }
    }

    fn state(&self, topic: String) -> String {
        json!({"rev": 1, "state": {"topic": topic, "notes": ["Plans"]}}).to_string()
    }
}

struct Chat;

#[zbus::interface(name = "io.github.design_nexus.nyx.Control")]
impl Chat {
    fn describe(&self) -> String {
        json!([
            {"name": "chat.list", "description": "Recent conversations.", "annotations": {"readOnlyHint": true}},
            {"name": "chat.send", "description": "Send a message."}
        ])
        .to_string()
    }

    fn call(&self, name: String, _arguments: String) -> String {
        json!({"ok": {"called": name}}).to_string()
    }
}

struct Weather;

#[zbus::interface(name = "io.github.design_nexus.nyx.Control")]
impl Weather {
    fn describe(&self) -> String {
        weather_description().to_string()
    }

    fn invoke(&self, action: String, _arguments: String, _options: String) -> String {
        json!({"ok": {"started": true, "action": action}}).to_string()
    }
}

fn weather_description() -> Value {
    json!({
        "tools": [{"name": "weather.current", "description": "Conditions now.", "x-nyx": {"risk": "read_only"}}],
        "nyx": {"api": 2, "app": WEATHER, "name": "Weather", "namespaces": ["weather"], "bus_name": WEATHER, "activation": "dbus"}
    })
}

/// Weather, as D-Bus starts it: this binary run with `startable_app` as the only test.
#[tokio::test]
async fn startable_app() {
    let Ok(address) = std::env::var("DBUS_STARTER_ADDRESS") else { return };
    if std::env::var_os(FAKE).is_none() {
        return;
    }
    let _conn = zbus::connection::Builder::address(address.as_str())
        .unwrap()
        .name(WEATHER)
        .unwrap()
        .serve_at("/io/github/design_nexus/nyx/Weather", Weather)
        .unwrap()
        .build()
        .await
        .unwrap();
    // Until the test's bus goes away.
    let socket = address.trim_start_matches("unix:path=").split(',').next().unwrap_or_default().to_string();
    for _ in 0..300 {
        if !Path::new(&socket).exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn manifest(dir: &Path, description: &Value) {
    let app = description["nyx"]["app"].as_str().unwrap();
    std::fs::write(dir.join(format!("{app}.json")), description.to_string()).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn routes_checks_and_starts_apps() {
    let dir = tempfile::tempdir().unwrap();
    let (services, actions) = (dir.path().join("services"), dir.path().join("actions"));
    std::fs::create_dir_all(&services).unwrap();
    std::fs::create_dir_all(&actions).unwrap();
    let exe = std::env::current_exe().unwrap();
    // D-Bus gives a started service no usable stdout, which the test harness writes to.
    let exec = format!("/bin/sh -c \"exec '{}' startable_app --exact --test-threads=1 >/dev/null 2>&1\"", exe.display());
    std::fs::write(services.join(format!("{WEATHER}.service")), format!("[D-BUS Service]\nName={WEATHER}\nExec={exec}\n"))
        .unwrap();
    manifest(&actions, &notes_description());
    manifest(&actions, &weather_description());
    manifest(
        &actions,
        &json!({"tools": [{"name": "mail.read", "description": "Read mail.", "x-nyx": {"risk": "read_only"}}],
            "nyx": {"api": 2, "app": MAIL, "name": "Mail", "namespaces": ["mail"], "bus_name": MAIL, "activation": "running"}}),
    );
    let bus = bus(dir, &services);

    let seen = Seen::default();
    let _notes = zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name(NOTES)
        .unwrap()
        .serve_at("/io/github/design_nexus/nyx/Notes", Notes(seen.clone()))
        .unwrap()
        .build()
        .await
        .unwrap();
    let _chat = zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name(CHAT)
        .unwrap()
        .serve_at("/io/github/design_nexus/nyx/Chat", Chat)
        .unwrap()
        .build()
        .await
        .unwrap();

    let registry = Registry::new(Options { dirs: vec![actions.clone()], bus_address: Some(bus.address.clone()), live: true });
    let catalog = registry.catalog().await;
    let state = |id: &str| catalog.find(id).map(|(e, l)| (e.running, l.state));
    assert_eq!(state("notes.search"), Some((Some(2), State::Ready)));
    assert_eq!(state("chat.list"), Some((Some(1), State::Ready)));
    assert_eq!(state("weather.current"), Some((None, State::Startable)));
    assert_eq!(state("mail.read"), Some((None, State::NotRunning)));

    let person = InvokeOptions { trusted: true, frontend: "test".into(), ..Default::default() };
    let stranger = InvokeOptions { trusted: false, frontend: "test".into(), ..Default::default() };
    let confirmed = InvokeOptions { confirmed: true, ..person.clone() };
    let err = |r: Result<Value, nyx_actions::Error>| r.unwrap_err().kind;

    let found = registry.invoke("notes.search", json!({"query": "plans"}), &stranger).await.unwrap();
    assert_eq!(found["arguments"]["query"], "plans");
    assert_eq!(seen.0.lock().unwrap()[0].1["frontend"], "test");
    assert_eq!(err(registry.invoke("notes.search", json!({}), &person).await), ErrorKind::InvalidArguments);
    let refused = registry.invoke("notes.search", json!({"query": "fail"}), &person).await.unwrap_err();
    assert_eq!((refused.kind, refused.message.as_str()), (ErrorKind::PermissionDenied, "the vault is locked"));

    assert_eq!(err(registry.invoke("notes.create", json!({}), &stranger).await), ErrorKind::PermissionDenied);
    registry.invoke("notes.create", json!({}), &person).await.unwrap();
    assert_eq!(err(registry.invoke("notes.delete", json!({}), &person).await), ErrorKind::ConfirmationRequired);
    registry.invoke("notes.delete", json!({}), &confirmed).await.unwrap();
    assert_eq!(seen.0.lock().unwrap().last().unwrap().1["confirmed"], true);
    assert_eq!(err(registry.invoke("notes.slow", json!({}), &person).await), ErrorKind::Timeout);

    assert_eq!(registry.invoke("chat.list", json!({}), &stranger).await.unwrap()["called"], "chat.list");
    assert_eq!(err(registry.invoke("chat.send", json!({}), &stranger).await), ErrorKind::PermissionDenied);
    assert_eq!(
        err(registry.invoke("chat.send", json!({}), &person).await),
        ErrorKind::ConfirmationRequired,
        "undeclared risk asks"
    );
    assert_eq!(registry.invoke("chat.send", json!({}), &confirmed).await.unwrap()["called"], "chat.send");

    let started = registry.invoke("weather.current", json!({}), &stranger).await.unwrap();
    assert_eq!(started["started"], true);
    assert_eq!(err(registry.invoke("mail.read", json!({}), &person).await), ErrorKind::NotRunning);
    assert_eq!(err(registry.invoke("notes.nothing", json!({}), &person).await), ErrorKind::UnknownAction);
    assert_eq!(err(registry.invoke("films.play", json!({}), &person).await), ErrorKind::UnknownApp);

    let recent = registry.state(NOTES, "recent").await.unwrap();
    assert_eq!((recent["rev"].as_u64(), recent["state"]["notes"][0].as_str()), (Some(1), Some("Plans")));
    assert_eq!(err(registry.state(NOTES, "drafts").await), ErrorKind::UnknownAction);

    // A changed manifest is read again.
    let mut changed = weather_description();
    changed["tools"][0]["name"] = json!("weather.forecast");
    manifest(&actions, &changed);
    assert!(registry.installed().find("weather.forecast").is_some());
}
