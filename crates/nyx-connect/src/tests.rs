//! Against a fake fluxd on a socket in a scratch folder.

use super::*;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

#[derive(Default)]
struct Fake {
    state: Value,
    calls: Vec<(String, Value)>,
    subscribers: Vec<UnixStream>,
}

struct Server {
    fake: Arc<Mutex<Fake>>,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

fn phone(online: bool) -> Value {
    json!({ "id": "a1b2c3d4e5f60718293a4b5c6d7e8f90", "name": "Pandora", "type": "phone", "paired": true,
            "online": online, "pairState": "paired", "battery": { "charge": 80, "charging": false },
            "plugins": ["battery", "clipboard", "share", "notifications", "findmyphone"],
            "fingerprint": "0A1B2C3D4E5F6071", "app": "android", "appVersion": "0.19.1" })
}

fn state() -> Value {
    json!({ "self": { "id": "me", "name": "laptop", "type": "laptop", "version": "0.19.1", "tcpPort": 12100 },
            "devices": [phone(true), { "id": "tablet1tablet1tablet1tablet1tabl", "name": "Tab", "type": "tablet",
                                       "paired": false, "online": true, "pairState": "incoming", "pairKey": "5EE6825F974ED59A" }],
            "clipboard": [{ "id": "c1", "text": "short", "dir": "in", "device": "a1b2c3d4e5f60718293a4b5c6d7e8f90", "deviceName": "Pandora", "time": 1 },
                          { "id": "c2", "text": "long…", "truncated": true, "size": 5000, "dir": "in", "device": "a1b2c3d4e5f60718293a4b5c6d7e8f90", "time": 2 }],
            "transfers": [{ "id": "t1", "device": "a1b2c3d4e5f60718293a4b5c6d7e8f90", "deviceName": "Pandora", "name": "a.jpg",
                            "path": "/home/u/Downloads/a.jpg", "size": 10, "done": 10, "dir": "in", "state": "done", "time": 3 }],
            "settings": { "autoClipboard": true, "notifications": true, "shareHome": true, "remoteInput": false,
                          "deviceRules": null, "notificationRules": null } })
}

fn serve() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fluxd.sock");
    let fake = Arc::new(Mutex::new(Fake { state: state(), ..Default::default() }));
    listen(&socket, fake.clone());
    Server { fake, socket, _dir: dir }
}

fn listen(socket: &Path, fake: Arc<Mutex<Fake>>) {
    let listener = UnixListener::bind(socket).unwrap();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { break };
            let fake = fake.clone();
            std::thread::spawn(move || {
                let mut writer = conn.try_clone().unwrap();
                for line in BufReader::new(conn).lines() {
                    let Ok(line) = line else { break };
                    let req: Value = serde_json::from_str(&line).unwrap();
                    let (id, method, params) =
                        (req["id"].clone(), req["method"].as_str().unwrap().to_string(), req["params"].clone());
                    let mut f = fake.lock().unwrap();
                    f.calls.push((method.clone(), params.clone()));
                    let reply = match method.as_str() {
                        "state" => json!({ "id": id, "result": f.state }),
                        "subscribe" => {
                            f.subscribers.push(writer.try_clone().unwrap());
                            json!({ "id": id, "result": {} })
                        }
                        "clipboard.search" => {
                            json!({ "id": id, "result": [{ "id": "c2", "text": "x".repeat(5000), "dir": "in" }] })
                        }
                        "ring" if params["device"] == "nope" => {
                            json!({ "id": id, "error": { "code": "offline", "message": "nope is offline" } })
                        }
                        "clipboard.send" => {
                            json!({ "id": id, "error": { "code": "disabled", "message": "Clipboard access is off for this device" } })
                        }
                        "settings.set" => {
                            let key = params["key"].as_str().unwrap().to_string();
                            f.state["settings"][key] = params["value"].clone();
                            json!({ "id": id, "result": {} })
                        }
                        "device.settings.set" => {
                            let (dev, key) = (params["device"].as_str().unwrap(), params["key"].as_str().unwrap());
                            if f.state["settings"]["deviceRules"].is_null() {
                                f.state["settings"]["deviceRules"] = json!({});
                            }
                            let rules = &mut f.state["settings"]["deviceRules"];
                            if params["value"].is_null() {
                                rules[dev].as_object_mut().map(|o| o.remove(key));
                            } else {
                                rules[dev][key] = params["value"].clone();
                            }
                            json!({ "id": id, "result": {} })
                        }
                        "pair.unpair" => {
                            f.state["devices"][0]["paired"] = json!(false);
                            json!({ "id": id, "result": { "device": params["device"], "name": "Pandora", "key": "" } })
                        }
                        "pair.accept" => {
                            json!({ "id": id, "result": { "device": params["device"], "name": "Tab", "key": "5EE6825F974ED59A", "fingerprint": "FF" } })
                        }
                        _ => json!({ "id": id, "error": { "code": "unknown_method", "message": "Unknown method" } }),
                    };
                    drop(f);
                    // An event first, as fluxd may send one before a reply.
                    let _ = writeln!(writer, "{}", json!({ "event": "toast", "data": { "text": "" } }));
                    if writeln!(writer, "{reply}").is_err() {
                        break;
                    }
                }
            });
        }
    });
}

fn push(server: &Server, event: Value) {
    let mut f = server.fake.lock().unwrap();
    f.subscribers.retain_mut(|s| writeln!(s, "{event}").is_ok());
}

#[test]
fn a_snapshot_is_normalized() {
    let s = serve();
    let snap = Client::at(&s.socket).state().unwrap();
    assert_eq!(snap.this.version, "0.19.1");
    let pandora = &snap.devices[0];
    assert_eq!(pandora.status(), Status::Connected);
    assert!(pandora.has(Feature::Ring) && !pandora.has(Feature::Sms));
    assert_eq!(pandora.battery, Some(Battery { charge: 80, charging: false }));
    let tab = &snap.devices[1];
    assert_eq!(tab.status(), Status::Pairing(PairStep::Incoming));
    assert_eq!(tab.key_groups(), "5EE6 825F 974E D59A");
    assert!(snap.settings.device_rules.is_empty(), "null rules are none");
    assert!(snap.allowed(&pandora.id, Access::Clipboard));
    assert!(!snap.allowed(&pandora.id, Access::RemoteInput));
    assert!(snap.clipboard[0].is_incoming_copy());
    assert!(snap.transfers[0].incoming() && snap.transfers[0].finished());

    let mut offline: Device = serde_json::from_value(phone(false)).unwrap();
    assert_eq!(offline.status(), Status::Disconnected);
    offline.paired = false;
    offline.pair_state = "none".into();
    offline.online = true;
    assert_eq!(offline.status(), Status::Discovered);
}

#[test]
fn errors_keep_fluxds_codes() {
    let s = serve();
    let c = Client::at(&s.socket);
    let e = c.ring("nope").unwrap_err();
    assert_eq!((e.kind, e.message.as_str()), (ErrorKind::Offline, "nope is offline"));
    assert_eq!(c.send_clipboard("x", "hi").unwrap_err().kind, ErrorKind::PermissionDenied);
    assert_eq!(c.call("made.up", json!({})).unwrap_err().kind, ErrorKind::UnknownMethod);
    let gone = Client::at(s.socket.with_file_name("missing.sock"));
    assert_eq!(gone.state().unwrap_err().kind, ErrorKind::Unavailable);
    assert!(!gone.reachable());
}

#[test]
fn a_long_clip_comes_whole() {
    let s = serve();
    let c = Client::at(&s.socket);
    let snap = c.state().unwrap();
    assert_eq!(c.clip_text(&snap.clipboard[0]).unwrap(), "short");
    assert_eq!(c.clip_text(&snap.clipboard[1]).unwrap().len(), 5000);
    let calls = &s.fake.lock().unwrap().calls;
    assert_eq!(calls.iter().filter(|(m, _)| m == "clipboard.search").count(), 1, "only the truncated one is searched");
}

#[test]
fn pairing_sends_the_compared_key() {
    let s = serve();
    let p = Client::at(&s.socket).pair_accept("tablet1tablet1tablet1tablet1tabl", "5EE6 825F 974E D59A").unwrap();
    assert_eq!(p.name, "Tab");
    let calls = &s.fake.lock().unwrap().calls;
    assert_eq!(calls.last().unwrap().1["key"], "5EE6 825F 974E D59A");
}

#[test]
fn the_watch_follows_and_survives_a_restart() {
    let s = serve();
    let (tx, rx) = mpsc::channel();
    let mut watch = Watch::at(s.socket.clone(), move |e| {
        let _ = tx.send(e);
    });
    let next = || loop {
        match rx.recv_timeout(Duration::from_secs(5)).expect("an event") {
            Event::Toast(_) => continue,
            e => return e,
        }
    };
    let Event::State(first) = next() else { panic!("the first state") };
    assert_eq!(first.devices.len(), 2);
    // Wait until the subscription is in place, then change the state.
    while s.fake.lock().unwrap().subscribers.is_empty() {
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut changed = state();
    changed["devices"][0]["online"] = json!(false);
    push(&s, json!({ "event": "state", "data": changed }));
    let Event::State(second) = next() else { panic!("the changed state") };
    assert_eq!(second.devices[0].status(), Status::Disconnected);

    // fluxd goes: one Unavailable. It comes back at the same path: state again.
    for sub in s.fake.lock().unwrap().subscribers.drain(..) {
        let _ = sub.shutdown(std::net::Shutdown::Both);
    }
    let Event::Unavailable(e) = next() else { panic!("unavailable") };
    assert_eq!(e.kind, ErrorKind::Unavailable);
    let Event::State(again) = next() else { panic!("back") };
    assert_eq!(again.devices.len(), 2);
    watch.stop();
}

#[test]
fn the_watch_says_once_that_flux_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("fluxd.sock");
    let (tx, rx) = mpsc::channel();
    let mut watch = Watch::at(socket.clone(), move |e| {
        let _ = tx.send(e);
    });
    assert!(matches!(rx.recv_timeout(Duration::from_secs(5)), Ok(Event::Unavailable(_))));
    // It starts later: the watch finds it.
    listen(&socket, Arc::new(Mutex::new(Fake { state: state(), ..Default::default() })));
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Event::State(_)) => {}
        other => panic!("expected the state, got {other:?}"),
    }
    watch.stop();
}

#[test]
fn prefs_follow_the_layers() {
    let d = tempfile::tempdir().unwrap();
    let (defaults, user) = (d.path().join("defaults.toml"), d.path().join("user/connect.toml"));
    std::fs::write(&defaults, "clipboard_sync = true\n").unwrap();
    let layers = vec![defaults.clone(), d.path().join("missing.toml"), user.clone()];
    assert!(Prefs::load_from(&layers).clipboard_sync);
    Prefs::set_in(&user, "clipboard_sync", Some(toml::Value::Boolean(false))).unwrap();
    Prefs::set_in(&user, "flux_auto_clipboard", Some(toml::Value::Boolean(true))).unwrap();
    let p = Prefs::load_from(&layers);
    assert_eq!((p.clipboard_sync, p.flux_auto_clipboard), (false, Some(true)));
    Prefs::set_in(&user, "flux_auto_clipboard", None).unwrap();
    assert_eq!(Prefs::load_from(&layers).flux_auto_clipboard, None);
    std::fs::write(&user, "not = [toml").unwrap();
    assert!(Prefs::load_from(&layers).clipboard_sync, "a broken user file leaves the defaults");
}

#[test]
fn forget_clears_access_unpairs_and_removes_the_memory() {
    let s = serve();
    let c = Client::at(&s.socket);
    let id = "a1b2c3d4e5f60718293a4b5c6d7e8f90";
    c.set_access(id, Access::Clipboard, Some(false)).unwrap();
    let snap = c.state().unwrap();
    assert_eq!(snap.settings.device_override(id, Access::Clipboard), Some(false));
    assert!(!snap.allowed(id, Access::Clipboard));

    let d = tempfile::tempdir().unwrap();
    let mut index = Index::load(&d.path().join("devices.json"));
    remember(&mut index, &snap.devices[0]);
    assert!(orphans(&index, &snap).is_empty());
    forget(&c, &snap, &mut index, id).unwrap();
    assert!(index.get(&device_id(id)).is_none());
    let after = c.state().unwrap();
    assert_eq!(after.settings.device_override(id, Access::Clipboard), None);
    assert!(!after.devices[0].paired);
    let methods: Vec<String> = s.fake.lock().unwrap().calls.iter().map(|(m, _)| m.clone()).collect();
    let (clear, unpair) = (
        methods.iter().rposition(|m| m == "device.settings.set").unwrap(),
        methods.iter().position(|m| m == "pair.unpair").unwrap(),
    );
    assert!(clear < unpair, "access is cleared while fluxd still has the device paired");
}

#[test]
fn an_unpaired_remembered_device_is_an_orphan() {
    let s = serve();
    let c = Client::at(&s.socket);
    let snap = c.state().unwrap();
    let d = tempfile::tempdir().unwrap();
    let mut index = Index::load(&d.path().join("devices.json"));
    remember(&mut index, &snap.devices[0]);
    c.unpair(&snap.devices[0].id).unwrap();
    let after = c.state().unwrap();
    assert_eq!(orphans(&index, &after).len(), 1);
}

#[test]
fn nyx_takes_over_clipboard_sync_and_gives_it_back() {
    let s = serve();
    let c = Client::at(&s.socket);
    let d = tempfile::tempdir().unwrap();
    // SAFETY: the tests that read XDG_CONFIG_HOME are only this one.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", d.path()) };
    set_clipboard_sync(&c, true).unwrap();
    assert!(!c.state().unwrap().settings.auto_clipboard);
    let p = Prefs::load_from(&[Prefs::user_path()]);
    assert_eq!((p.clipboard_sync, p.flux_auto_clipboard), (true, Some(true)));

    // Something turns fluxd's sync back on: kept off.
    c.set("autoClipboard", json!(true)).unwrap();
    assert!(keep_flux_sync_off(&c, &c.state().unwrap(), &p));
    assert!(!c.state().unwrap().settings.auto_clipboard);

    set_clipboard_sync(&c, false).unwrap();
    assert!(c.state().unwrap().settings.auto_clipboard, "fluxd syncs again, as before");
    let p = Prefs::load_from(&[Prefs::user_path()]);
    assert_eq!((p.clipboard_sync, p.flux_auto_clipboard), (false, None));
}
