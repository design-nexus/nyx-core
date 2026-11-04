//! Running Nyx plugins for a host. Each plugin runs on its own supervisor thread:
//!
//! - in a transient systemd scope with cgroup limits (memory, CPU, tasks), when the
//!   user's systemd can make one, and niced;
//! - third-party plugins inside bubblewrap ([`sandbox`]) with a filtering D-Bus proxy
//!   ([`proxy`]) when they may use D-Bus;
//! - `stream` plugins kept running and restarted with backoff (1 s doubling to 60 s);
//!   `interval` plugins run on a wall-clock-aligned timer and killed when late;
//! - their messages paced and checked ([`pacer`]), frames passed over shared memory
//!   ([`frames`]);
//! - turned off after repeated crashes ([`Report::Suspended`]).
//!
//! Hosts get [`Report`]s on an async channel (for the GTK main loop) and send
//! [`HostMsg`]s through the [`Handle`].

pub mod frames;
pub mod limits;
pub mod metrics;
pub mod pacer;
pub mod proxy;
pub mod sandbox;

use limits::Limits;
use nyx_plugin::manifest::Mode;
use nyx_plugin::{Kind, Plugin};
use nyx_plugin_protocol::{Hello, HostMsg, PluginMsg, encode, parse_plugin_line};
use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{BufReader, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// What a host needs to run one plugin for one kind.
#[derive(Debug, Clone)]
pub struct Spec {
    pub plugin: Plugin,
    pub kind: Kind,
    /// The host's short name, for unit and socket names (`bar`).
    pub host: String,
    /// Sent first; the supervisor fills in permissions and the frame socket.
    pub hello: Hello,
    pub limits: Limits,
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Report {
    Started { pid: u32, scoped: bool, sandboxed: bool },
    Message(PluginMsg),
    Frames(frames::Buffers),
    /// The process ended without being asked to.
    Exited { status: String, stderr: String },
    /// Turned off: crashed too often (`crashed`), or it can't run here.
    Suspended { reason: String, crashed: bool },
    Warning(String),
}

#[allow(clippy::large_enum_variant)]
enum Ev {
    Line(u64, String),
    TooLong(u64, usize),
    Eof(u64),
    Send(HostMsg),
    Stop,
    Restart,
}

/// The host's side of a running plugin.
pub struct Handle {
    pub id: String,
    pub kind: Kind,
    tx: mpsc::Sender<Ev>,
    pid: Arc<AtomicU32>,
    restarts: Arc<AtomicU32>,
    pub scoped: bool,
    pub sandboxed: bool,
    pub limits: Limits,
    meter: Mutex<metrics::CpuMeter>,
}

impl Handle {
    pub fn send(&self, msg: HostMsg) {
        let _ = self.tx.send(Ev::Send(msg));
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Ev::Stop);
    }

    pub fn restart(&self) {
        let _ = self.tx.send(Ev::Restart);
    }

    /// The running process, if any.
    pub fn pid(&self) -> Option<u32> {
        Some(self.pid.load(Ordering::SeqCst)).filter(|p| *p != 0)
    }

    pub fn restarts(&self) -> u32 {
        self.restarts.load(Ordering::SeqCst)
    }

    /// Memory (bytes) and CPU (percent of one core since the last call).
    pub fn usage(&self) -> Option<(u64, Option<f64>)> {
        let u = metrics::usage(self.pid()?, self.scoped)?;
        Some((u.memory_bytes, self.meter.lock().unwrap().percent(u.cpu_usec)))
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = self.tx.send(Ev::Stop);
    }
}

/// A process's cgroup, when it's one of our plugin scopes (not the host's own).
fn scope_cgroup(pid: u32) -> Option<std::path::PathBuf> {
    // The scope exists once systemd-run has moved the process into it.
    for _ in 0..50 {
        if let Some(dir) = metrics::cgroup_dir(pid)
            && dir.file_name().is_some_and(|n| n.to_string_lossy().starts_with("nyx-plugin-"))
        {
            return Some(dir);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// Empty the plugin scopes a host of this name left behind when it died (another
/// process id than ours). Call once at start.
pub fn reap_stale(host: &str) {
    let uid = unsafe { libc::getuid() };
    let slice = std::path::PathBuf::from(format!("/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/app.slice"));
    let Ok(entries) = std::fs::read_dir(&slice) else { return };
    let prefix = format!("nyx-plugin-{host}-");
    let me = std::process::id().to_string();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix(&prefix).and_then(|r| r.strip_suffix(".scope")) else { continue };
        // nyx-plugin-<host>-<id>-<host pid>-<run>
        let mut parts = rest.rsplitn(3, '-');
        let (_run, pid) = (parts.next(), parts.next());
        let Some(pid) = pid else { continue };
        if pid != me && !std::path::Path::new(&format!("/proc/{pid}")).exists() {
            let _ = std::fs::write(e.path().join("cgroup.kill"), "1");
        }
    }
}

/// Whether the host may run a command a plugin's item asks for.
pub fn allowed_command(plugin: &Plugin, argv: &[String]) -> bool {
    !argv.is_empty() && (plugin.trust.is_trusted() || plugin.manifest.permissions.host_commands.iter().any(|c| c == argv))
}

pub fn spawn(spec: Spec, out: async_channel::Sender<Report>) -> Handle {
    let (tx, rx) = mpsc::channel();
    let pid = Arc::new(AtomicU32::new(0));
    let restarts = Arc::new(AtomicU32::new(0));
    let sandboxed = !spec.plugin.trust.is_trusted();
    let scoped = metrics::scopes_available();
    let handle = Handle {
        id: spec.plugin.manifest.id.clone(),
        kind: spec.kind,
        tx: tx.clone(),
        pid: pid.clone(),
        restarts: restarts.clone(),
        scoped,
        sandboxed,
        limits: spec.limits.clone(),
        meter: Mutex::new(metrics::CpuMeter::new()),
    };
    let name = format!("nyx-plugin-{}", spec.plugin.manifest.id);
    std::thread::Builder::new()
        .name(name)
        .spawn(move || {
            let mut sup = Supervisor { spec, rx, tx, out, pid, restarts, generation: 0, sandboxed, scoped, crashes: VecDeque::new() };
            sup.run();
        })
        .expect("spawn a plugin supervisor");
    handle
}

struct Supervisor {
    spec: Spec,
    rx: mpsc::Receiver<Ev>,
    tx: mpsc::Sender<Ev>,
    out: async_channel::Sender<Report>,
    pid: Arc<AtomicU32>,
    restarts: Arc<AtomicU32>,
    generation: u64,
    sandboxed: bool,
    scoped: bool,
    crashes: VecDeque<Instant>,
}

enum Outcome {
    Stopped,
    Restart,
    Crashed,
}

/// Lines a plugin sends beyond a budget aren't parsed at all (that alone costs the
/// host CPU): they're counted, and only the newest is kept, for the end of the second.
#[derive(Default)]
struct Gate {
    window: Option<Instant>,
    count: u32,
    last: Option<String>,
    skipped: u64,
    /// Skipped in the current second.
    window_skipped: u32,
}

impl Gate {
    /// Whether to parse this line now; otherwise it's kept as the newest.
    fn admit(&mut self, line: String, budget: u32) -> Option<String> {
        let now = Instant::now();
        if self.window.is_none_or(|w| now.duration_since(w) >= Duration::from_secs(1)) {
            self.window = Some(now);
            self.count = 0;
            self.window_skipped = 0;
        }
        if self.count < budget {
            self.count += 1;
            Some(line)
        } else {
            self.last = Some(line);
            self.skipped += 1;
            self.window_skipped += 1;
            None
        }
    }

    /// How long to stop reading: once a whole budget has been skipped this second, the
    /// rest of the second. The plugin's pipe fills and its writes wait, so a flood costs
    /// neither the host nor, much, the plugin.
    fn pause(&self, budget: u32) -> Option<Duration> {
        let w = self.window?;
        (self.window_skipped >= budget).then(|| Duration::from_secs(1).saturating_sub(w.elapsed()))
    }

    /// The newest skipped line, once its second is over (or a new second has room).
    fn overflow(&mut self, budget: u32) -> Option<(String, u64)> {
        if self.last.is_some() && (self.count < budget || self.window.is_some_and(|w| w.elapsed() >= Duration::from_secs(1))) {
            let skipped = std::mem::take(&mut self.skipped);
            return self.last.take().map(|l| (l, skipped));
        }
        None
    }
}

/// Resources made for one run, dropped (and cleaned up) with it.
struct Run {
    child: Child,
    /// The plugin's own scope, emptied when the run ends.
    cgroup: Option<std::path::PathBuf>,
    stdin: Option<ChildStdin>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    gate: Arc<Mutex<Gate>>,
    frames: Option<frames::Server>,
    _proxy: Option<proxy::Proxy>,
}

impl Supervisor {
    fn report(&self, r: Report) {
        let _ = self.out.send_blocking(r);
    }

    fn run(&mut self) {
        if self.sandboxed && !sandbox::available() {
            self.report(Report::Suspended { reason: "the sandbox (bubblewrap) isn't available, so third-party plugins can't run".into(), crashed: false });
            return;
        }
        let p = &self.spec.plugin.manifest.permissions;
        if self.sandboxed && !(p.session_talk.is_empty() && p.system_talk.is_empty()) && !proxy::available() {
            self.report(Report::Suspended { reason: "it needs D-Bus, and xdg-dbus-proxy isn't installed".into(), crashed: false });
            return;
        }
        let missing = self.spec.plugin.missing_needs();
        if !missing.is_empty() {
            self.report(Report::Suspended { reason: format!("it needs {}, which isn't installed", missing.join(", ")), crashed: false });
            return;
        }
        let section = self.spec.plugin.manifest.section(self.spec.kind);
        match section.mode {
            Mode::Stream => self.run_stream(),
            Mode::Interval => self.run_interval(section.interval.unwrap_or(60).max(self.spec.limits.interval_min_secs)),
        }
        self.pid.store(0, Ordering::SeqCst);
    }

    /// Count a crash; true when it's one too many.
    fn crashed(&mut self) -> bool {
        let now = Instant::now();
        self.crashes.push_back(now);
        let window = Duration::from_secs(self.spec.limits.crash_window_secs);
        while self.crashes.front().is_some_and(|t| now.duration_since(*t) > window) {
            self.crashes.pop_front();
        }
        if self.crashes.len() >= self.spec.limits.crashes {
            let mins = self.spec.limits.crash_window_secs / 60;
            self.report(Report::Suspended { reason: format!("it stopped {} times in {mins} minutes", self.crashes.len()), crashed: true });
            return true;
        }
        false
    }

    fn run_stream(&mut self) {
        let mut backoff = Duration::from_secs(1);
        loop {
            let started = Instant::now();
            match self.stream_once() {
                Outcome::Stopped => return,
                Outcome::Restart => {
                    self.restarts.fetch_add(1, Ordering::SeqCst);
                    backoff = Duration::from_secs(1);
                    continue;
                }
                Outcome::Crashed => {
                    if self.crashed() {
                        return;
                    }
                    self.restarts.fetch_add(1, Ordering::SeqCst);
                    if started.elapsed() > Duration::from_secs(60) {
                        backoff = Duration::from_secs(1);
                    }
                }
            }
            // Wait, but stop at once if asked.
            let until = Instant::now() + backoff;
            loop {
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                match self.rx.recv_timeout(left) {
                    Ok(Ev::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    Ok(Ev::Restart) => break,
                    _ => {}
                }
            }
            backoff = (backoff * 2).min(Duration::from_secs(60));
        }
    }

    fn command(&self, sockets: &sandbox::Sockets) -> Command {
        let plugin = &self.spec.plugin;
        let m = &plugin.manifest;
        let exec = m.exec_for(self.spec.kind);
        let limits = &self.spec.limits;
        let mut argv: Vec<OsString> = vec![];
        if self.scoped {
            let unit = format!("nyx-plugin-{}-{}-{}-{}", self.spec.host, m.id, std::process::id(), self.generation);
            argv.extend(["systemd-run", "--user", "--scope", "--quiet", "--collect"].map(OsString::from));
            argv.push(format!("--unit={unit}").into());
            argv.push(format!("--description=Nyx plugin {} ({})", m.name, self.spec.host).into());
            for p in [
                format!("MemoryMax={}M", limits.memory_mb),
                format!("MemoryHigh={}M", limits.memory_mb * 4 / 5),
                format!("MemorySwapMax={}M", limits.memory_mb / 2),
                format!("CPUQuota={}%", limits.cpu_percent),
                format!("TasksMax={}", limits.tasks),
                format!("IOWeight={}", limits.io_weight),
            ] {
                argv.push("-p".into());
                argv.push(p.into());
            }
            argv.push("--".into());
        }
        if limits.nice > 0 {
            argv.extend(["nice".into(), "-n".into(), limits.nice.to_string().into()]);
        }
        let storage = nyx_plugin::paths::storage_dir(&m.id);
        let _ = std::fs::create_dir_all(&storage);
        if self.sandboxed {
            let env = sandbox::env(&m.permissions, sockets, &m.id, self.spec.kind.name());
            argv.push("bwrap".into());
            argv.extend(sandbox::args(&plugin.dir, &storage, &m.permissions, sockets, &env));
            argv.extend(sandbox::inner_exec(&plugin.dir, &exec).into_iter().map(OsString::from));
        } else {
            argv.extend(sandbox::outer_exec(&plugin.dir, &exec));
        }
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..]);
        if !self.sandboxed {
            cmd.current_dir(&plugin.dir)
                .env("NYX_PLUGIN_STORAGE", &storage)
                .env("NYX_PLUGIN_DIR", &plugin.dir)
                .env("NYX_PLUGIN_ID", &m.id)
                .env("NYX_PLUGIN_KIND", self.spec.kind.name());
        }
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
        // The plugin dies with the host, even when the host is killed outright.
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
        cmd
    }

    fn hello(&self, frames: Option<&frames::Server>) -> HostMsg {
        let mut hello = self.spec.hello.clone();
        hello.permissions = self.spec.plugin.manifest.permissions.names();
        hello.frame_socket = frames.map(|f| {
            if self.sandboxed { sandbox::inner_frames_socket().to_string_lossy().into_owned() } else { f.path.to_string_lossy().into_owned() }
        });
        HostMsg::Hello(hello)
    }

    /// Start one run: sockets, the process, its reader threads.
    fn start(&mut self) -> Result<Run, String> {
        self.generation += 1;
        let run_gen = self.generation;
        let m = &self.spec.plugin.manifest;
        let rt = nyx_plugin::paths::plugin_runtime_dir(&self.spec.host, &m.id);
        let _ = std::fs::create_dir_all(&rt);
        let mut sockets = sandbox::Sockets::default();
        let proxy = if self.sandboxed {
            match proxy::start(&rt, &m.permissions.session_talk, &m.permissions.system_talk) {
                Ok(p) => p,
                Err(e) => return Err(format!("couldn't start its D-Bus proxy: {e}")),
            }
        } else {
            None
        };
        if let Some(p) = &proxy {
            sockets.session_bus = p.session.clone();
            sockets.system_bus = p.system.clone();
        }
        let frames = if m.permissions.frames || !self.sandboxed {
            frames::Server::listen(&rt.join("frames.sock")).ok()
        } else {
            None
        };
        sockets.frames = frames.as_ref().map(|f| f.path.clone());
        let mut cmd = self.command(&sockets);
        let mut child = cmd.spawn().map_err(|e| format!("couldn't start {:?}: {e}", m.exec_for(self.spec.kind)))?;
        self.pid.store(child.id(), Ordering::SeqCst);
        self.report(Report::Started { pid: child.id(), scoped: self.scoped, sandboxed: self.sandboxed });
        let stdout = child.stdout.take().expect("stdout");
        let stderr = child.stderr.take().expect("stderr");
        let tx = self.tx.clone();
        let max = self.spec.limits.image_bytes;
        let gate: Arc<Mutex<Gate>> = Arc::default();
        let g = gate.clone();
        let budget = self.spec.limits.messages_per_sec.saturating_mul(8).max(16);
        std::thread::spawn(move || {
            let mut r = BufReader::new(stdout);
            loop {
                match pacer::read_line_limited(&mut r, max) {
                    Ok(pacer::Line::Text(l)) => {
                        let admitted = g.lock().unwrap().admit(l, budget);
                        let Some(l) = admitted else {
                            let pause = g.lock().unwrap().pause(budget);
                            if let Some(d) = pause {
                                std::thread::sleep(d);
                            }
                            continue;
                        };
                        if tx.send(Ev::Line(run_gen, l)).is_err() {
                            return;
                        }
                    }
                    Ok(pacer::Line::TooLong(n)) => {
                        let _ = tx.send(Ev::TooLong(run_gen, n));
                    }
                    Ok(pacer::Line::Eof) | Err(_) => {
                        let _ = tx.send(Ev::Eof(run_gen));
                        return;
                    }
                }
            }
        });
        let tail: Arc<Mutex<VecDeque<String>>> = Arc::default();
        let t = tail.clone();
        let prefix = format!("[{}]", m.id);
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("{prefix} {line}");
                let mut t = t.lock().unwrap();
                t.push_back(line);
                if t.len() > 20 {
                    t.pop_front();
                }
            }
        });
        let cgroup = if self.scoped { scope_cgroup(child.id()) } else { None };
        Ok(Run { stdin: child.stdin.take(), child, cgroup, stderr: tail, gate, frames, _proxy: proxy })
    }

    fn write(run: &mut Run, msg: &HostMsg) {
        if let Some(s) = run.stdin.as_mut()
            && (s.write_all(encode(msg).as_bytes()).is_err() || s.flush().is_err())
        {
            run.stdin = None;
        }
    }

    fn finish(&self, run: &mut Run, polite: bool) -> String {
        let status = self.finish_process(run, polite);
        // Whatever the plugin started goes too.
        if let Some(cg) = &run.cgroup {
            let _ = std::fs::write(cg.join("cgroup.kill"), "1");
        }
        status
    }

    fn finish_process(&self, run: &mut Run, polite: bool) -> String {
        if polite {
            Self::write(run, &HostMsg::Shutdown);
        }
        run.stdin = None;
        let deadline = Instant::now() + Duration::from_secs(if polite { 1 } else { 0 });
        loop {
            if let Ok(Some(status)) = run.child.try_wait() {
                self.pid.store(0, Ordering::SeqCst);
                return status.to_string();
            }
            if Instant::now() >= deadline {
                let pid = run.child.id() as i32;
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
                let _ = run.child.kill();
                let status = run.child.wait().map(|s| s.to_string()).unwrap_or_default();
                self.pid.store(0, Ordering::SeqCst);
                return status;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn handle_line(&mut self, run: &Run, pacer: &mut pacer::Pacer, line: &str, plain: Option<&str>) {
        if line.trim().is_empty() {
            return;
        }
        let msg = match parse_plugin_line(line, plain) {
            Ok(m) => m,
            Err(e) => {
                self.report(Report::Warning(format!("a line it sent isn't a message ({e})")));
                return;
            }
        };
        if line.len() > self.spec.limits.message_bytes && !line.contains("\"image\"") {
            self.report(Report::Warning(format!("a {} KB message (the limit is {} KB)", line.len() / 1024, self.spec.limits.message_bytes / 1024)));
            return;
        }
        if let PluginMsg::FrameRequest { item, node, width, height } = &msg {
            let (mw, mh) = self.spec.limits.frame_max;
            if *width <= 0 || *height <= 0 || *width > mw * 2 || *height > mh * 2 {
                self.report(Report::Warning(format!("a {width}×{height} frame (the limit is {mw}×{mh}, at twice for HiDPI)")));
                return;
            }
            match run.frames.as_ref().map(|f| f.allocate(item, node, *width, *height)) {
                Some(Ok(b)) => self.report(Report::Frames(b)),
                Some(Err(e)) => self.report(Report::Warning(format!("frames: {e}"))),
                None => self.report(Report::Warning("it asked for frames without the `frames` permission".into())),
            }
            return;
        }
        for m in pacer.offer(msg, Instant::now()) {
            self.report(Report::Message(m));
        }
        for w in std::mem::take(&mut pacer.warnings) {
            self.report(Report::Warning(w));
        }
    }

    fn stream_once(&mut self) -> Outcome {
        let mut run = match self.start() {
            Ok(r) => r,
            Err(e) => {
                self.report(Report::Exited { status: e, stderr: String::new() });
                return Outcome::Crashed;
            }
        };
        let run_gen = self.generation;
        let hello = self.hello(run.frames.as_ref());
        Self::write(&mut run, &hello);
        let mut pacer = pacer::Pacer::new(self.spec.limits.clone());
        let cgroup_cpu = self.scoped && metrics::cpu_controller();
        let mut cpu = metrics::CpuMeter::new();
        let mut over = 0u32;
        let mut last_check = Instant::now();
        let mut flood_warned = false;
        loop {
            let wait = pacer.next_wake().unwrap_or(Duration::from_secs(1));
            match self.rx.recv_timeout(wait) {
                Ok(Ev::Line(g, l)) if g == run_gen => self.handle_line(&run, &mut pacer, &l, None),
                Ok(Ev::TooLong(g, n)) if g == run_gen => {
                    self.report(Report::Warning(format!("a {} KB line (the limit is {} KB)", n / 1024, self.spec.limits.image_bytes / 1024)))
                }
                Ok(Ev::Eof(g)) if g == run_gen => {
                    let status = self.finish(&mut run, false);
                    let stderr = run.stderr.lock().unwrap().iter().cloned().collect::<Vec<_>>().join("\n");
                    self.report(Report::Exited { status, stderr });
                    return Outcome::Crashed;
                }
                Ok(Ev::Send(m)) => Self::write(&mut run, &m),
                Ok(Ev::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.finish(&mut run, true);
                    return Outcome::Stopped;
                }
                Ok(Ev::Restart) => {
                    self.finish(&mut run, true);
                    return Outcome::Restart;
                }
                _ => {}
            }
            let overflow = run.gate.lock().unwrap().overflow(self.spec.limits.messages_per_sec.saturating_mul(8).max(16));
            if let Some((line, skipped)) = overflow {
                if !flood_warned {
                    flood_warned = true;
                    self.report(Report::Warning(format!("it sends far more than {} messages a second; {skipped} skipped, the newest kept", self.spec.limits.messages_per_sec)));
                }
                self.handle_line(&run, &mut pacer, &line, None);
            }
            for m in pacer.tick(Instant::now()) {
                self.report(Report::Message(m));
            }
            // Without the cgroup CPU controller, watch CPU time ourselves.
            if !cgroup_cpu && last_check.elapsed() > Duration::from_secs(5) {
                last_check = Instant::now();
                if let Some(pct) = metrics::process_usage(run.child.id()).and_then(|u| cpu.percent(u.cpu_usec)) {
                    over = if pct > self.spec.limits.cpu_percent as f64 * 1.5 { over + 1 } else { 0 };
                    if over >= 6 {
                        self.report(Report::Warning(format!("it used {pct:.0}% CPU for 30 seconds; stopping it")));
                        let status = self.finish(&mut run, false);
                        self.report(Report::Exited { status, stderr: String::new() });
                        return Outcome::Crashed;
                    }
                }
            }
        }
    }

    fn run_interval(&mut self, every: u64) {
        let mut visible = true;
        let mut first = true;
        let plain = self.spec.hello.items.first().cloned();
        loop {
            // Line up with the wall clock: a 60 s clock runs on the minute.
            let wait = if first {
                Duration::ZERO
            } else {
                let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
                let next = (now.as_secs() / every + 1) * every;
                Duration::from_secs(next) - now + Duration::from_millis(50)
            };
            first = false;
            let until = Instant::now() + wait;
            let mut now_please = false;
            loop {
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() || now_please {
                    break;
                }
                match self.rx.recv_timeout(left) {
                    Ok(Ev::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    Ok(Ev::Restart) => now_please = true,
                    Ok(Ev::Send(HostMsg::Visible { visible: v, item: None })) => {
                        let was = visible;
                        visible = v;
                        // Coming back into view: catch up now.
                        now_please = v && !was;
                    }
                    Ok(Ev::Send(HostMsg::Settings { settings })) => {
                        self.spec.hello.settings = settings;
                        now_please = true;
                    }
                    // Desktop widgets moved, resized, added: draw them again for it.
                    Ok(Ev::Send(HostMsg::Widgets { widgets })) => {
                        self.spec.hello.widgets = widgets;
                        now_please = true;
                    }
                    Ok(Ev::Send(HostMsg::Monitors { monitors })) => self.spec.hello.monitors = monitors,
                    _ => {}
                }
            }
            if !visible {
                continue;
            }
            let failed = match self.interval_once(plain.as_deref()) {
                Ok(()) => false,
                Err(e) => {
                    self.report(Report::Exited { status: e, stderr: String::new() });
                    true
                }
            };
            if failed && self.crashed() {
                return;
            }
        }
    }

    fn interval_once(&mut self, plain: Option<&str>) -> Result<(), String> {
        let mut run = self.start()?;
        let run_gen = self.generation;
        let hello = self.hello(run.frames.as_ref());
        Self::write(&mut run, &hello);
        run.stdin = None;
        let mut pacer = pacer::Pacer::new(Limits { messages_per_sec: 1000, ..self.spec.limits.clone() });
        let deadline = Instant::now() + Duration::from_secs(self.spec.limits.interval_timeout_secs);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                self.finish(&mut run, false);
                return Err(format!("it took longer than {} seconds", self.spec.limits.interval_timeout_secs));
            }
            match self.rx.recv_timeout(left) {
                Ok(Ev::Line(g, l)) if g == run_gen => self.handle_line(&run, &mut pacer, &l, plain),
                Ok(Ev::Eof(g)) if g == run_gen => break,
                Ok(Ev::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.finish(&mut run, false);
                    // Put the stop back for the outer loop.
                    let _ = self.tx.send(Ev::Stop);
                    return Ok(());
                }
                _ => {}
            }
        }
        let status = loop {
            if let Ok(Some(s)) = run.child.try_wait() {
                break s;
            }
            if Instant::now() >= deadline {
                self.finish(&mut run, false);
                return Err(format!("it took longer than {} seconds", self.spec.limits.interval_timeout_secs));
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        self.pid.store(0, Ordering::SeqCst);
        if status.success() {
            Ok(())
        } else {
            let stderr = run.stderr.lock().unwrap().iter().cloned().collect::<Vec<_>>().join("\n");
            Err(format!("{status}{}", if stderr.is_empty() { String::new() } else { format!(": {stderr}") }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyx_plugin::discovery::Trust;
    use nyx_plugin_protocol::API_VERSION;

    fn plugin(dir: &std::path::Path, manifest: &str, script: &str, trust: Trust) -> Plugin {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("plugin.toml"), manifest).unwrap();
        std::fs::write(dir.join("run.sh"), script).unwrap();
        Plugin { manifest: nyx_plugin::Manifest::load(dir).unwrap(), dir: dir.to_path_buf(), trust, origin: None, linked: false }
    }

    fn spec(p: Plugin) -> Spec {
        Spec {
            hello: Hello {
                api_version: API_VERSION,
                kind: "bar".into(),
                plugin: p.manifest.id.clone(),
                items: vec!["main".into()],
                settings: serde_json::json!({}),
                monitors: vec![],
                theme: Default::default(),
                permissions: vec![],
                frame_socket: None,
                bar_height: Some(32),
                widgets: vec![],
            },
            limits: Limits::resolve(p.trust.is_trusted(), &p.manifest.limits, None),
            plugin: p,
            kind: Kind::Bar,
            host: "test".into(),
        }
    }

    fn next_message(rx: &async_channel::Receiver<Report>) -> PluginMsg {
        loop {
            match rx.recv_blocking().unwrap() {
                Report::Message(m) => return m,
                Report::Suspended { reason, .. } => panic!("suspended: {reason}"),
                _ => {}
            }
        }
    }

    const STREAM: &str = "id = \"t.echo\"\nname = \"Echo\"\napi_version = 1\nkinds = [\"bar\"]\nexec = [\"sh\", \"run.sh\"]\n";
    // Reads hello, sends an item, then echoes every event's item back as text.
    const ECHO: &str = r#"read hello
echo '{"type":"item","id":"main","text":"hi"}'
while read line; do
  case "$line" in
    *shutdown*) exit 0 ;;
    *event*) echo '{"type":"item","id":"main","text":"clicked"}' ;;
  esac
done
"#;

    #[test]
    fn a_trusted_stream_plugin_talks_both_ways() {
        let t = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_DATA_HOME", t.path().join("data")) };
        let p = plugin(&t.path().join("echo"), STREAM, ECHO, Trust::FirstParty);
        let (tx, rx) = async_channel::unbounded();
        let h = spawn(spec(p), tx);
        let PluginMsg::Item(i) = next_message(&rx) else { panic!() };
        assert_eq!(i.text.as_deref(), Some("hi"));
        h.send(HostMsg::Event(nyx_plugin_protocol::Event {
            item: "main".into(),
            kind: nyx_plugin_protocol::EventKind::Click,
            monitor: None,
            button: Some(1),
            direction: None,
            control: None,
            value: None,
            anchor: None,
        }));
        let PluginMsg::Item(i) = next_message(&rx) else { panic!() };
        assert_eq!(i.text.as_deref(), Some("clicked"));
        assert!(h.pid().is_some());
        h.stop();
    }

    #[test]
    fn a_sandboxed_plugin_cannot_read_home() {
        if !sandbox::available() {
            eprintln!("skipping: no bubblewrap");
            return;
        }
        let t = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_DATA_HOME", t.path().join("data")) };
        let home = std::env::var("HOME").unwrap();
        let script = format!(
            "read hello\nif ls {home} >/dev/null 2>&1; then r=seen; else r=hidden; fi\n\
             if [ -w /storage ]; then s=writable; else s=no; fi\n\
             echo \"{{\\\"type\\\":\\\"item\\\",\\\"id\\\":\\\"main\\\",\\\"text\\\":\\\"$r $s $HOME\\\"}}\"\nread x\n"
        );
        let p = plugin(&t.path().join("spy"), &STREAM.replace("t.echo", "t.spy"), &script, Trust::ThirdParty);
        let (tx, rx) = async_channel::unbounded();
        let h = spawn(spec(p), tx);
        let PluginMsg::Item(i) = next_message(&rx) else { panic!() };
        assert_eq!(i.text.as_deref(), Some("hidden writable /storage"));
        h.stop();
    }

    #[test]
    fn interval_plugins_print_plain_text() {
        let t = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_DATA_HOME", t.path().join("data")) };
        let manifest = "id = \"t.clock\"\nname = \"Clock\"\napi_version = 1\nkinds = [\"bar\"]\nexec = [\"sh\", \"run.sh\"]\n[bar]\nmode = \"interval\"\ninterval = 60\n";
        let p = plugin(&t.path().join("clock"), manifest, "echo 12:00\n", Trust::FirstParty);
        let (tx, rx) = async_channel::unbounded();
        let h = spawn(spec(p), tx);
        let PluginMsg::Item(i) = next_message(&rx) else { panic!() };
        assert_eq!((i.id.as_str(), i.text.as_deref()), ("main", Some("12:00")));
        h.stop();
    }

    #[test]
    fn crashing_plugins_are_suspended() {
        let t = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_DATA_HOME", t.path().join("data")) };
        let p = plugin(&t.path().join("crash"), &STREAM.replace("t.echo", "t.crash"), "exit 3\n", Trust::FirstParty);
        let mut s = spec(p);
        s.limits.crashes = 2;
        let (tx, rx) = async_channel::unbounded();
        let _h = spawn(s, tx);
        let mut exits = 0;
        loop {
            match rx.recv_blocking().unwrap() {
                Report::Exited { .. } => exits += 1,
                Report::Suspended { reason, crashed } => {
                    assert!(crashed);
                    assert!(reason.contains("stopped 2 times"), "{reason}");
                    break;
                }
                _ => {}
            }
        }
        assert_eq!(exits, 2);
    }

    #[test]
    fn the_gate_keeps_the_newest_line() {
        let mut g = Gate::default();
        let admitted = (0..100).filter(|i| g.admit(i.to_string(), 10).is_some()).count();
        assert_eq!(admitted, 10);
        assert!(g.overflow(10).is_none());
        // A new second: the newest skipped line goes first.
        assert!(g.admit("100".into(), 10).is_none());
        g.window = Some(Instant::now() - Duration::from_secs(2));
        assert!(g.admit("101".into(), 10).is_some());
        assert_eq!(g.overflow(10), Some(("100".to_string(), 91)));
        assert!(g.overflow(10).is_none());
    }

    #[test]
    fn a_flood_pauses_reading() {
        let mut g = Gate::default();
        for i in 0..15 {
            g.admit(i.to_string(), 10);
        }
        assert!(g.pause(10).is_none(), "a few skipped lines don't pause");
        for i in 15..25 {
            g.admit(i.to_string(), 10);
        }
        let d = g.pause(10).expect("a whole budget skipped pauses");
        assert!(d <= Duration::from_secs(1));
        // The next second reads again.
        g.window = Some(Instant::now() - Duration::from_secs(2));
        assert!(g.admit("x".into(), 10).is_some());
        assert!(g.pause(10).is_none());
    }

    #[test]
    fn host_commands_for_third_party_plugins() {
        let t = tempfile::tempdir().unwrap();
        let manifest = format!("{STREAM}[permissions]\nhost_commands = [[\"nyx-weather\", \"--toggle\"]]\n");
        let p = plugin(&t.path().join("w"), &manifest, "", Trust::ThirdParty);
        assert!(allowed_command(&p, &["nyx-weather".into(), "--toggle".into()]));
        assert!(!allowed_command(&p, &["sh".into(), "-c".into(), "rm -rf ~".into()]));
        let trusted = Plugin { trust: Trust::FirstParty, ..p };
        assert!(allowed_command(&trusted, &["anything".into()]));
    }
}
