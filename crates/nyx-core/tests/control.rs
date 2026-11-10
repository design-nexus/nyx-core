//! The control helper on a private D-Bus: callers identified by their program, the
//! risk rules enforced before the handler, typed errors, and state sent to watchers.
#![cfg(feature = "gtk")]

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use nyx_actions::{Error, ErrorKind};
use nyx_core::control::{Answer, INTERFACE, Request, serve_on};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

const BUS: &str = "io.github.design_nexus.nyx.Notes";

struct Bus {
    child: Child,
    address: String,
    _dir: tempfile::TempDir,
}

impl Bus {
    fn start() -> Bus {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("bus.conf");
        std::fs::write(
            &config,
            format!(
                r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>unix:path={}</listen><auth>EXTERNAL</auth>
<policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>"#,
                dir.path().join("bus").display()
            ),
        )
        .unwrap();
        let mut child = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("dbus-daemon");
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut address).unwrap();
        Bus { child, address: address.trim().to_string(), _dir: dir }
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn connect(address: &str) -> gio::DBusConnection {
    gio::DBusConnection::for_address_sync(
        address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
        None,
        gio::Cancellable::NONE,
    )
    .unwrap()
}

fn description() -> Value {
    json!({
        "tools": [
            {"name": "notes.search", "description": "Search.", "inputSchema": {"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]}, "x-nyx": {"risk": "read_only"}},
            {"name": "notes.create", "description": "Create.", "x-nyx": {"risk": "writes"}},
            {"name": "notes.delete", "description": "Delete.", "x-nyx": {"risk": "destructive"}},
            {"name": "notes.fail", "description": "Refuses.", "x-nyx": {"risk": "read_only"}}
        ],
        "nyx": {"api": 2, "app": BUS, "name": "Notes", "namespaces": ["notes"], "bus_name": BUS,
            "topics": [{"name": "recent"}, {"name": "drafts", "sensitive": true}]}
    })
}

#[test]
fn checks_callers_and_sends_state_to_watchers() {
    let bus = Bus::start();
    let server = connect(&bus.address);
    let seen: Rc<RefCell<Vec<Request>>> = Rc::default();
    let handler = {
        let seen = seen.clone();
        move |request: Request, answer: Answer| {
            seen.borrow_mut().push(request.clone());
            match request.action.as_str() {
                "notes.fail" => answer(Err(Error::new(ErrorKind::PermissionDenied, "the vault is locked"))),
                a => answer(Ok(json!({"ran": a}))),
            }
        }
    };
    let own = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
    let trusted = serve_on(&server, "/t", BUS, description(), Some(own), None, handler.clone()).unwrap();
    let _untrusted = serve_on(&server, "/u", BUS, description(), Some("/nonexistent".into()), None, handler).unwrap();
    let watching: Rc<RefCell<Vec<(String, bool)>>> = Rc::default();
    {
        let (watching, service) = (watching.clone(), trusted.clone());
        trusted.on_watch(move |topic, on| {
            watching.borrow_mut().push((topic.to_string(), on));
            if on {
                service.publish(topic, json!({"notes": ["Plans"]}));
            }
        });
    }
    let dest = server.unique_name().unwrap().to_string();
    let address = bus.address.clone();
    let client = std::thread::spawn(move || {
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let c = connect(&address);
                let call = |path: &str, method: &str, args: glib::Variant| {
                    c.call_sync(
                        Some(&dest),
                        path,
                        INTERFACE,
                        method,
                        Some(&args),
                        None,
                        gio::DBusCallFlags::NONE,
                        3000,
                        gio::Cancellable::NONE,
                    )
                };
                let text = |v: glib::Variant| -> Value { serde_json::from_str(&v.get::<(String,)>().unwrap().0).unwrap() };
                let invoke = |path: &str, action: &str, args: Value, options: Value| {
                    text(call(path, "Invoke", (action, args.to_string(), options.to_string()).to_variant()).unwrap())
                };
                let api1 = |path: &str, action: &str| text(call(path, "Call", (action, "{}").to_variant()).unwrap());
                let kind = |v: Value| v["error"]["kind"].as_str().unwrap_or("none").to_string();

                let described = text(call("/t", "Describe", ().to_variant()).unwrap());
                assert_eq!(described["nyx"]["app"], BUS);

                // A Nyx program (this test, from its own folder).
                assert_eq!(invoke("/t", "notes.search", json!({"query": "a"}), json!({}))["ok"]["ran"], "notes.search");
                assert_eq!(invoke("/t", "notes.create", json!({}), json!({}))["ok"]["ran"], "notes.create");
                assert_eq!(kind(invoke("/t", "notes.delete", json!({}), json!({}))), "confirmation_required");
                assert_eq!(
                    invoke("/t", "notes.delete", json!({}), json!({"confirmed": true, "frontend": "test"}))["ok"]["ran"],
                    "notes.delete"
                );
                assert_eq!(kind(invoke("/t", "notes.search", json!({}), json!({}))), "invalid_arguments");
                assert_eq!(kind(invoke("/t", "notes.nothing", json!({}), json!({}))), "unknown_action");
                let refused = invoke("/t", "notes.fail", json!({}), json!({}));
                assert_eq!(
                    (kind(refused.clone()), refused["error"]["message"].as_str()),
                    ("permission_denied".into(), Some("the vault is locked"))
                );
                // API 1: a Nyx program's call counts as confirmed, errors are words.
                assert_eq!(api1("/t", "notes.delete")["ok"]["ran"], "notes.delete");
                assert_eq!(api1("/t", "notes.fail")["error"], "the vault is locked");

                // Any other program.
                assert_eq!(invoke("/u", "notes.search", json!({"query": "a"}), json!({}))["ok"]["ran"], "notes.search");
                assert_eq!(kind(invoke("/u", "notes.create", json!({}), json!({"confirmed": true}))), "permission_denied");
                assert!(api1("/u", "notes.create")["error"].as_str().unwrap().contains("only Nyx programs"));
                let denied = call("/u", "State", ("drafts",).to_variant()).unwrap_err();
                assert!(denied.to_string().contains("only Nyx programs"), "{denied}");

                // State for watchers only, as a merge patch.
                let changes: Rc<RefCell<Vec<(String, u32, String)>>> = Rc::default();
                let recorded = changes.clone();
                let _sub = c.subscribe_to_signal(
                    Some(&dest),
                    Some(INTERFACE),
                    Some("StateChanged"),
                    None,
                    None,
                    gio::DBusSignalFlags::NONE,
                    move |s| {
                        recorded.borrow_mut().push(s.parameters.get::<(String, u32, String)>().unwrap());
                    },
                );
                let rev = call("/t", "Watch", ("recent",).to_variant()).unwrap().get::<(u32,)>().unwrap().0;
                let start = Instant::now();
                while changes.borrow().is_empty() && start.elapsed() < Duration::from_secs(3) {
                    glib::MainContext::ref_thread_default().iteration(false);
                    std::thread::sleep(Duration::from_millis(2));
                }
                let (topic, rev_sent, patch) = changes.borrow()[0].clone();
                assert_eq!((topic.as_str(), rev_sent), ("recent", rev + 1));
                assert_eq!(serde_json::from_str::<Value>(&patch).unwrap(), json!({"notes": ["Plans"]}));
                let state = text(call("/t", "State", ("recent",).to_variant()).unwrap());
                assert_eq!((state["rev"].as_u64(), state["state"]["notes"][0].as_str()), (Some(1), Some("Plans")));
                call("/t", "Unwatch", ("recent",).to_variant()).unwrap();
                assert!(call("/t", "Watch", ("weather",).to_variant()).is_err());
            })
            .unwrap();
    });
    let main = glib::MainContext::default();
    let start = Instant::now();
    while !client.is_finished() {
        assert!(start.elapsed() < Duration::from_secs(30), "the client took too long");
        main.iteration(false);
        std::thread::sleep(Duration::from_millis(1));
    }
    client.join().unwrap();
    let main_seen = seen.borrow();
    let delete = main_seen.iter().find(|r| r.action == "notes.delete" && r.frontend.is_some()).unwrap();
    assert!(delete.confirmed && delete.caller.trusted);
    assert!(delete.caller.program.is_some());
    assert!(!main_seen.iter().any(|r| r.action == "notes.create" && !r.caller.trusted), "refused calls never reach the handler");
    assert_eq!(*watching.borrow(), vec![("recent".to_string(), true), ("recent".to_string(), false)]);
}
