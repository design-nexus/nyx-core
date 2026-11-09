//! The link between a shell component's Rust backend and its Quickshell interface.
//!
//! A backend (the Dock's, the Control Center's, …) keeps everything that isn't drawing:
//! services, settings, launching, the plugin host. Its interface, in QML, connects to
//! one Unix socket and speaks newline-delimited JSON, the same framing as Nyx Shell
//! Core's protocol:
//!
//! - request `{"v":1,"id":7,"op":"Activate","body":{…}}`, reply `{"id":7,"ok":…}` or
//!   `{"id":7,"err":{"code":"…","msg":"…"}}`;
//! - event `{"ev":"…","seq":12,"body":{…}}`, `seq` counting from 1 per connection.
//!
//! State is published as **topics**: named JSON values. `Subscribe {topic}` replies
//! with `{seq, rev, state}`; afterwards each change arrives as
//! `Patch {topic, base, rev, patch}` (a JSON Merge Patch, [`patch`]) or, when that
//! would be bigger, `State {topic, rev, state}`. A client applies a patch only when
//! `base` is the revision it holds, and subscribes again otherwise. A client that falls
//! more than `max_queue` events behind gets one `Resync {topics}` instead of the events
//! it missed.
//!
//! Built-in operations: `Hello`, `Subscribe`, `Unsubscribe`, `Get`. Everything else
//! goes to the backend's handler as a [`Request`].
//!
//! Only processes of the same user may connect (`SO_PEERCRED`); [`Options::allow`]
//! narrows that further.

pub mod patch;
pub mod switch;

use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

pub const PROTOCOL_VERSION: u64 = 1;
/// Longest request line accepted.
pub const MAX_LINE: usize = 1 << 20;

/// The socket of `app`'s backend: `$NYX_APP_UI_SOCKET` (`nyx-dock` reads
/// `NYX_DOCK_UI_SOCKET`), else `$XDG_RUNTIME_DIR/APP/ui.sock`.
pub fn socket_path(app: &str) -> PathBuf {
    let var = format!("{}_UI_SOCKET", app.to_uppercase().replace('-', "_"));
    if let Some(p) = std::env::var_os(var).filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(format!("nyx-{}", unsafe { libc::getuid() })));
    base.join(app).join("ui.sock")
}

/// Who is on the other end of a connection.
#[derive(Clone, Debug)]
pub struct Peer {
    /// This connection's number in the backend, from 1.
    pub id: u64,
    pub pid: i32,
    pub uid: u32,
    /// The executable it runs, when readable.
    pub exe: Option<PathBuf>,
}

type Allow = Arc<dyn Fn(&Peer) -> bool + Send + Sync>;
type Disconnected = Arc<dyn Fn(&Peer) + Send + Sync>;
type Watched = Arc<dyn Fn(&str, usize) + Send + Sync>;

pub struct Options {
    /// Named in `Hello`'s reply and in log lines.
    pub name: String,
    pub path: PathBuf,
    /// Events a client may have waiting before it is sent `Resync` instead.
    pub max_queue: usize,
    /// Further limits on same-user peers (an executable check, say).
    pub allow: Option<Allow>,
    /// Called when a client goes.
    pub on_disconnect: Option<Disconnected>,
    /// Called with a topic and how many clients now subscribe to it, whenever that
    /// changes: start a service when its first viewer comes, stop it after the last.
    pub on_watch: Option<Watched>,
}

impl Options {
    /// For `app`'s backend at [`socket_path`].
    pub fn new(app: &str) -> Options {
        Options {
            name: app.to_string(),
            path: socket_path(app),
            max_queue: 512,
            allow: None,
            on_disconnect: None,
            on_watch: None,
        }
    }
}

#[derive(Debug)]
pub enum Error {
    /// Another backend answers on the socket.
    AlreadyRunning(PathBuf),
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::AlreadyRunning(p) => write!(f, "another backend is running at {}", p.display()),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::Io(e)
    }
}

struct Topic {
    rev: u64,
    value: Value,
}

struct Client {
    peer: Peer,
    tx: mpsc::Sender<String>,
    queued: Arc<AtomicUsize>,
    subs: HashSet<String>,
    seq: u64,
    /// Events were dropped; `Resync` goes out once the queue is empty.
    resync: bool,
}

#[derive(Default)]
struct State {
    topics: HashMap<String, Topic>,
    clients: HashMap<u64, Client>,
    next: u64,
}

struct Shared {
    name: String,
    path: PathBuf,
    max_queue: usize,
    allow: Option<Allow>,
    on_disconnect: Option<Disconnected>,
    on_watch: Option<Watched>,
    state: Mutex<State>,
    _lock: std::fs::File,
    stopped: AtomicBool,
}

/// A running backend socket. Cheap to clone; publishing and emitting work from any
/// thread.
#[derive(Clone)]
pub struct Link {
    shared: Arc<Shared>,
}

/// A request for the backend. Answer with [`Request::ok`] or [`Request::err`] from any
/// thread; one dropped unanswered is answered `internal`.
pub struct Request {
    pub op: String,
    pub body: Value,
    pub peer: Peer,
    id: Value,
    shared: Arc<Shared>,
    answered: bool,
}

impl Request {
    pub fn ok(mut self, value: Value) {
        self.answered = true;
        reply(&self.shared, self.peer.id, json!({"id": self.id, "ok": value}));
    }

    pub fn err(mut self, code: &str, msg: impl Into<String>) {
        self.answered = true;
        reply(&self.shared, self.peer.id, json!({"id": self.id, "err": {"code": code, "msg": msg.into()}}));
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        if !self.answered {
            reply(
                &self.shared,
                self.peer.id,
                json!({"id": self.id, "err": {"code": "internal", "msg": format!("{} not answered", self.op)}}),
            );
        }
    }
}

/// Bind the socket and start serving. `handler` is called on the connection's thread
/// for every operation that isn't built in.
pub fn serve(options: Options, handler: impl Fn(Request) + Send + Sync + 'static) -> Result<Link, Error> {
    let path = options.path.clone();
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(format!("{}.lock", file_name(&path))))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::AlreadyRunning(path));
    }
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            return Err(Error::AlreadyRunning(path));
        }
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let shared = Arc::new(Shared {
        name: options.name,
        path,
        max_queue: options.max_queue.max(1),
        allow: options.allow,
        on_disconnect: options.on_disconnect,
        on_watch: options.on_watch,
        state: Mutex::new(State::default()),
        _lock: lock,
        stopped: AtomicBool::new(false),
    });
    let handler: Arc<dyn Fn(Request) + Send + Sync> = Arc::new(handler);
    let accept = shared.clone();
    std::thread::Builder::new().name("ui-link-accept".into()).spawn(move || {
        for stream in listener.incoming() {
            if accept.stopped.load(Ordering::Relaxed) {
                break;
            }
            match stream {
                Ok(s) => connection(&accept, s, handler.clone()),
                Err(e) => eprintln!("{}: accept: {e}", accept.name),
            }
        }
    })?;
    Ok(Link { shared })
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "ui.sock".into())
}

fn peer_of(stream: &UnixStream) -> Option<(i32, u32)> {
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let r = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    (r == 0).then_some((cred.pid, cred.uid))
}

fn connection(shared: &Arc<Shared>, stream: UnixStream, handler: Arc<dyn Fn(Request) + Send + Sync>) {
    let Some((pid, uid)) = peer_of(&stream) else { return };
    if uid != unsafe { libc::getuid() } {
        return;
    }
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok();
    let (tx, rx) = mpsc::channel::<String>();
    let queued = Arc::new(AtomicUsize::new(0));
    let peer = {
        let mut st = shared.state.lock().unwrap();
        st.next += 1;
        let peer = Peer { id: st.next, pid, uid, exe };
        if let Some(allow) = &shared.allow
            && !allow(&peer)
        {
            return;
        }
        st.clients.insert(
            peer.id,
            Client { peer: peer.clone(), tx, queued: queued.clone(), subs: HashSet::new(), seq: 0, resync: false },
        );
        peer
    };
    let Ok(mut out) = stream.try_clone() else { return };
    let writer_shared = shared.clone();
    let id = peer.id;
    let _ = std::thread::Builder::new().name("ui-link-write".into()).spawn(move || {
        let _ = out.set_write_timeout(Some(Duration::from_secs(5)));
        while let Ok(line) = rx.recv() {
            if out.write_all(line.as_bytes()).is_err() {
                break;
            }
            queued.fetch_sub(1, Ordering::AcqRel);
            if let Some(resync) = take_resync(&writer_shared, id)
                && out.write_all(resync.as_bytes()).is_err()
            {
                break;
            }
        }
        let _ = out.shutdown(std::net::Shutdown::Both);
    });
    let reader_shared = shared.clone();
    let _ = std::thread::Builder::new().name("ui-link-read".into()).spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match (&mut reader).take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if buf.len() > MAX_LINE {
                reply(&reader_shared, peer.id, json!({"id": null, "err": {"code": "too_large", "msg": "line over 1 MiB"}}));
                break;
            }
            let line = String::from_utf8_lossy(&buf);
            if line.trim().is_empty() {
                continue;
            }
            request(&reader_shared, &peer, line.trim(), &handler);
        }
        let (gone, counts) = {
            let mut st = reader_shared.state.lock().unwrap();
            let gone = st.clients.remove(&peer.id);
            let counts: Vec<(String, usize)> =
                gone.as_ref().map(|c| c.subs.iter().map(|t| (t.clone(), watchers(&st, t))).collect()).unwrap_or_default();
            (gone, counts)
        };
        if let Some(f) = &reader_shared.on_watch {
            for (t, n) in &counts {
                f(t, *n);
            }
        }
        if gone.is_some()
            && let Some(f) = &reader_shared.on_disconnect
        {
            f(&peer);
        }
    });
}

/// The `Resync` line for a client whose queue just emptied after dropping events.
fn take_resync(shared: &Shared, id: u64) -> Option<String> {
    let mut st = shared.state.lock().unwrap();
    let c = st.clients.get_mut(&id)?;
    if !c.resync || c.queued.load(Ordering::Acquire) != 0 {
        return None;
    }
    c.resync = false;
    c.seq += 1;
    let topics: Vec<&String> = c.subs.iter().collect();
    Some(line(&json!({"ev": "Resync", "seq": c.seq, "body": {"topics": topics}})))
}

fn watchers(st: &State, topic: &str) -> usize {
    st.clients.values().filter(|c| c.subs.contains(topic)).count()
}

fn line(v: &Value) -> String {
    let mut s = v.to_string();
    s.push('\n');
    s
}

fn enqueue(c: &Client, text: String) {
    c.queued.fetch_add(1, Ordering::AcqRel);
    if c.tx.send(text).is_err() {
        c.queued.fetch_sub(1, Ordering::AcqRel);
    }
}

fn reply(shared: &Shared, client: u64, v: Value) {
    let st = shared.state.lock().unwrap();
    if let Some(c) = st.clients.get(&client) {
        enqueue(c, line(&v));
    }
}

/// An event to one client, or `Resync` later if it is too far behind.
fn event(c: &mut Client, max: usize, name: &str, body: &Value) {
    if c.resync || c.queued.load(Ordering::Acquire) >= max {
        c.resync = true;
        return;
    }
    c.seq += 1;
    enqueue(c, line(&json!({"ev": name, "seq": c.seq, "body": body})));
}

fn request(shared: &Arc<Shared>, peer: &Peer, text: &str, handler: &Arc<dyn Fn(Request) + Send + Sync>) {
    let bad = |id: Value, code: &str, msg: &str| reply(shared, peer.id, json!({"id": id, "err": {"code": code, "msg": msg}}));
    let Ok(Value::Object(mut m)) = serde_json::from_str::<Value>(text) else {
        return bad(Value::Null, "bad_request", "not a JSON object");
    };
    let id = m.remove("id").unwrap_or(Value::Null);
    let Some(op) = m.get("op").and_then(Value::as_str).map(str::to_string) else {
        return bad(id, "bad_request", "no op");
    };
    if m.get("v").and_then(Value::as_u64) != Some(PROTOCOL_VERSION) {
        return bad(id, "unsupported_version", "this backend speaks version 1");
    }
    let body = m.remove("body").unwrap_or(Value::Null);
    let topic = body.get("topic").and_then(Value::as_str).map(str::to_string);
    match op.as_str() {
        "Hello" => reply(shared, peer.id, json!({"id": id, "ok": {"protocol": PROTOCOL_VERSION, "server": shared.name}})),
        "Subscribe" | "Get" | "Unsubscribe" => {
            let Some(topic) = topic else {
                return bad(id, "bad_request", "no topic");
            };
            let changed = {
                let mut st = shared.state.lock().unwrap();
                let (rev, state) = st.topics.get(&topic).map(|t| (t.rev, t.value.clone())).unwrap_or((0, Value::Null));
                let Some(c) = st.clients.get_mut(&peer.id) else { return };
                let (ok, changed) = match op.as_str() {
                    "Subscribe" => {
                        let new = c.subs.insert(topic.clone());
                        (json!({"seq": c.seq, "rev": rev, "state": state}), new)
                    }
                    "Get" => (json!({"rev": rev, "state": state}), false),
                    _ => {
                        let had = c.subs.remove(&topic);
                        (json!({}), had)
                    }
                };
                enqueue(c, line(&json!({"id": id, "ok": ok})));
                changed.then(|| watchers(&st, &topic))
            };
            if let (Some(n), Some(f)) = (changed, &shared.on_watch) {
                f(&topic, n);
            }
        }
        _ => handler(Request { op, body, peer: peer.clone(), id, shared: shared.clone(), answered: false }),
    }
}

impl Link {
    pub fn path(&self) -> &Path {
        &self.shared.path
    }

    /// Set a topic's state. Subscribers get the difference; nothing is sent when it
    /// didn't change.
    pub fn publish(&self, topic: &str, value: Value) {
        let mut st = self.shared.state.lock().unwrap();
        let t = st.topics.entry(topic.to_string()).or_insert(Topic { rev: 0, value: Value::Null });
        let Some(p) = patch::diff(&t.value, &value) else { return };
        let base = t.rev;
        t.rev += 1;
        let rev = t.rev;
        let body = if p.to_string().len() < value.to_string().len() {
            ("Patch", json!({"topic": topic, "base": base, "rev": rev, "patch": p}))
        } else {
            ("State", json!({"topic": topic, "rev": rev, "state": value}))
        };
        t.value = value;
        let max = self.shared.max_queue;
        for c in st.clients.values_mut().filter(|c| c.subs.contains(topic)) {
            event(c, max, body.0, &body.1);
        }
    }

    /// The state a topic holds now.
    pub fn state(&self, topic: &str) -> Value {
        self.shared.state.lock().unwrap().topics.get(topic).map(|t| t.value.clone()).unwrap_or(Value::Null)
    }

    /// An event to every connected client (`Toggle`, `Show`, …).
    pub fn emit(&self, name: &str, body: Value) {
        let mut st = self.shared.state.lock().unwrap();
        let max = self.shared.max_queue;
        for c in st.clients.values_mut() {
            event(c, max, name, &body);
        }
    }

    /// An event to one client.
    pub fn emit_to(&self, peer: u64, name: &str, body: Value) {
        let mut st = self.shared.state.lock().unwrap();
        let max = self.shared.max_queue;
        if let Some(c) = st.clients.get_mut(&peer) {
            event(c, max, name, &body);
        }
    }

    /// Connected clients.
    pub fn peers(&self) -> Vec<Peer> {
        self.shared.state.lock().unwrap().clients.values().map(|c| c.peer.clone()).collect()
    }

    /// Stop accepting and remove the socket (clients already connected stay until they
    /// go).
    pub fn shutdown(&self) {
        self.shared.stopped.store(true, Ordering::Relaxed);
        let _ = std::fs::remove_file(&self.shared.path);
        let _ = UnixStream::connect(&self.shared.path);
    }
}

/// Why a [`call`] failed.
#[derive(Debug)]
pub enum CallError {
    /// No backend answers at the socket.
    Unavailable(std::io::Error),
    /// The backend answered with an error.
    Refused {
        code: String,
        msg: String,
    },
    Io(std::io::Error),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Unavailable(e) => write!(f, "the backend isn't running ({e})"),
            CallError::Refused { code, msg } => write!(f, "{code}: {msg}"),
            CallError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for CallError {}

/// One request to a backend from a command line (`nyx-control-center --toggle`).
pub fn call(path: &Path, op: &str, body: Value) -> Result<Value, CallError> {
    let mut s = UnixStream::connect(path).map_err(CallError::Unavailable)?;
    s.set_read_timeout(Some(Duration::from_secs(5))).map_err(CallError::Io)?;
    s.write_all(line(&json!({"v": PROTOCOL_VERSION, "id": 1, "op": op, "body": body})).as_bytes()).map_err(CallError::Io)?;
    let mut reader = BufReader::new(s);
    let mut text = String::new();
    loop {
        text.clear();
        if reader.read_line(&mut text).map_err(CallError::Io)? == 0 {
            return Err(CallError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the backend closed the connection",
            )));
        }
        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
        if v.get("id") != Some(&json!(1)) {
            continue;
        }
        if let Some(e) = v.get("err") {
            return Err(CallError::Refused {
                code: e.get("code").and_then(Value::as_str).unwrap_or("").to_string(),
                msg: e.get("msg").and_then(Value::as_str).unwrap_or("").to_string(),
            });
        }
        return Ok(v.get("ok").cloned().unwrap_or(Value::Null));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestClient {
        out: UnixStream,
        input: BufReader<UnixStream>,
        next: u64,
    }

    impl TestClient {
        fn new(path: &Path) -> TestClient {
            let out = UnixStream::connect(path).unwrap();
            out.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let input = BufReader::new(out.try_clone().unwrap());
            TestClient { out, input, next: 0 }
        }
        fn send(&mut self, op: &str, body: Value) -> u64 {
            self.next += 1;
            self.out.write_all(line(&json!({"v": 1, "id": self.next, "op": op, "body": body})).as_bytes()).unwrap();
            self.next
        }
        fn read(&mut self) -> Value {
            let mut s = String::new();
            self.input.read_line(&mut s).unwrap();
            serde_json::from_str(&s).unwrap()
        }
        fn call(&mut self, op: &str, body: Value) -> Value {
            let id = self.send(op, body);
            loop {
                let v = self.read();
                if v["id"] == json!(id) {
                    return v;
                }
            }
        }
    }

    fn start(dir: &Path) -> Link {
        let mut o = Options::new("test");
        o.path = dir.join("app/ui.sock");
        serve(o, |r: Request| match r.op.as_str() {
            "Echo" => {
                let b = r.body.clone();
                r.ok(b)
            }
            "Fail" => r.err("invalid", "no"),
            _ => {}
        })
        .unwrap()
    }

    #[test]
    fn snapshot_then_patches() {
        let dir = tempfile::tempdir().unwrap();
        let link = start(dir.path());
        link.publish("dock", json!({"items": [1], "hidden": false}));
        let mut c = TestClient::new(link.path());
        let r = c.call("Subscribe", json!({"topic": "dock"}));
        assert_eq!(r["ok"]["rev"], json!(1));
        assert_eq!(r["ok"]["state"]["items"], json!([1]));
        link.publish("dock", json!({"items": [1], "hidden": true}));
        let e = c.read();
        assert_eq!(e["ev"], json!("Patch"));
        assert_eq!(e["seq"], json!(1));
        assert_eq!(e["body"]["base"], json!(1));
        assert_eq!(e["body"]["patch"], json!({"hidden": true}));
        // Unchanged state sends nothing; the next event is the next change.
        link.publish("dock", json!({"items": [1], "hidden": true}));
        link.emit("Toggle", json!({}));
        let e = c.read();
        assert_eq!((e["ev"].clone(), e["seq"].clone()), (json!("Toggle"), json!(2)));
    }

    #[test]
    fn requests_go_to_the_handler() {
        let dir = tempfile::tempdir().unwrap();
        let link = start(dir.path());
        let mut c = TestClient::new(link.path());
        assert_eq!(c.call("Echo", json!({"a": 1}))["ok"], json!({"a": 1}));
        assert_eq!(c.call("Fail", json!({}))["err"]["code"], json!("invalid"));
        assert_eq!(c.call("Unknown", json!({}))["err"]["code"], json!("internal"));
        assert_eq!(c.call("Hello", json!({}))["ok"]["server"], json!("test"));
        assert_eq!(call(link.path(), "Echo", json!(5)).unwrap(), json!(5));
    }

    #[test]
    fn second_backend_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let _link = start(dir.path());
        let mut o = Options::new("test");
        o.path = dir.path().join("app/ui.sock");
        assert!(matches!(serve(o, |_| {}), Err(Error::AlreadyRunning(_))));
    }

    #[test]
    fn stale_socket_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("app")).unwrap();
        drop(UnixListener::bind(dir.path().join("app/ui.sock")).unwrap());
        let link = start(dir.path());
        assert!(call(link.path(), "Hello", json!({})).is_ok());
    }

    #[test]
    fn slow_client_gets_resync() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = Options::new("test");
        o.path = dir.path().join("app/ui.sock");
        o.max_queue = 4;
        let link = serve(o, |_| {}).unwrap();
        let mut c = TestClient::new(link.path());
        c.call("Subscribe", json!({"topic": "t"}));
        // A large state the client doesn't read fills the socket, then the queue.
        let big = "x".repeat(64 * 1024);
        for i in 0..200 {
            link.publish("t", json!({"n": i, "pad": format!("{big}{i}")}));
        }
        let mut seen = 0;
        let mut last = 0;
        loop {
            let e = c.read();
            let seq = e["seq"].as_u64().unwrap();
            assert_eq!(seq, last + 1, "no gaps");
            last = seq;
            if e["ev"] == json!("Resync") {
                assert_eq!(e["body"]["topics"], json!(["t"]));
                break;
            }
            seen += 1;
        }
        assert!(seen < 200);
        let r = c.call("Get", json!({"topic": "t"}));
        assert_eq!(r["ok"]["state"]["n"], json!(199));
    }

    #[test]
    fn watchers_are_counted() {
        let dir = tempfile::tempdir().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut o = Options::new("test");
        o.path = dir.path().join("app/ui.sock");
        let s2 = seen.clone();
        o.on_watch = Some(Arc::new(move |t: &str, n| s2.lock().unwrap().push((t.to_string(), n))));
        let link = serve(o, |_| {}).unwrap();
        let mut a = TestClient::new(link.path());
        let mut b = TestClient::new(link.path());
        a.call("Subscribe", json!({"topic": "audio"}));
        b.call("Subscribe", json!({"topic": "audio"}));
        a.call("Subscribe", json!({"topic": "audio"}));
        b.call("Unsubscribe", json!({"topic": "audio"}));
        drop(a);
        for _ in 0..50 {
            if seen.lock().unwrap().len() == 4 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let v = seen.lock().unwrap().clone();
        assert_eq!(v, vec![("audio".into(), 1), ("audio".into(), 2), ("audio".into(), 1), ("audio".into(), 0)]);
    }

    #[test]
    fn unsupported_version_and_bad_lines() {
        let dir = tempfile::tempdir().unwrap();
        let link = start(dir.path());
        let mut c = TestClient::new(link.path());
        c.out.write_all(b"{\"v\":2,\"id\":1,\"op\":\"Hello\"}\nnot json\n").unwrap();
        assert_eq!(c.read()["err"]["code"], json!("unsupported_version"));
        assert_eq!(c.read()["err"]["code"], json!("bad_request"));
    }
}
