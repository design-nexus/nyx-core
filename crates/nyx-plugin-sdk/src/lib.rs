//! Write Nyx plugins in Rust.
//!
//! ```no_run
//! use nyx_plugin_sdk::{Input, Plugin, view::*};
//!
//! let (plugin, inputs) = Plugin::<()>::start();
//! plugin.item(item("main").view(label("Hello").id("text")));
//! for input in inputs {
//!     match input {
//!         Input::Event(e) if e.kind == nyx_plugin_sdk::protocol::EventKind::Click => {
//!             plugin.patch("main", "text", [("text", "Clicked".into())]);
//!         }
//!         Input::Shutdown => break,
//!         _ => {}
//!     }
//! }
//! ```
//!
//! [`Plugin::start`] reads the host's `hello`, then delivers everything else, and
//! whatever your own threads send through [`Plugin::data`], as one stream of
//! [`Input`]s. The services (`network`, `bluetooth`, `audio`, `power`, `mpris`
//! features) push changes as they happen; none of them polls when it can listen.

pub mod frames;
#[cfg(feature = "gtk")]
pub mod popup;
pub mod services;
pub mod view;

pub use nyx_plugin_protocol as protocol;
use protocol::{Event, Hello, HostMsg, Item, MenuEntry, Node, NodePatch, Patch, PluginMsg, encode};
use serde_json::{Map, Value};
use std::io::{BufRead, Write};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

/// Everything a plugin reacts to.
#[derive(Debug)]
pub enum Input<T> {
    Event(Event),
    /// Whether anything the plugin shows can be seen.
    Visible(bool),
    /// Whether one item can be seen (desktop widgets under windows can't).
    ItemVisible { item: String, visible: bool },
    /// The desktop widgets placed from this plugin (see [`Plugin::widgets`]).
    Widgets(Vec<protocol::Widget>),
    Settings(Value),
    Monitors(Vec<protocol::MonitorInfo>),
    Desktop(protocol::DesktopState),
    Theme(protocol::ThemeInfo),
    /// From your own threads or a service.
    Data(T),
    Shutdown,
}

/// The plugin's side of the conversation. Cheap to clone; safe to use from threads.
pub struct Plugin<T: Send + 'static> {
    pub hello: Arc<Hello>,
    out: Arc<Mutex<std::io::Stdout>>,
    data: Sender<Input<T>>,
}

impl<T: Send + 'static> Clone for Plugin<T> {
    fn clone(&self) -> Self {
        Plugin { hello: self.hello.clone(), out: self.out.clone(), data: self.data.clone() }
    }
}

impl<T: Send + 'static> Plugin<T> {
    /// Read `hello` (exits quietly if there's none) and start reading events.
    pub fn start() -> (Plugin<T>, Receiver<Input<T>>) {
        let stdin = std::io::stdin();
        let mut first = String::new();
        if stdin.lock().read_line(&mut first).unwrap_or(0) == 0 {
            std::process::exit(0);
        }
        let hello = match protocol::parse_host_line(&first) {
            Ok(HostMsg::Hello(h)) => h,
            _ => {
                eprintln!("expected hello from the host, got {first:?}");
                std::process::exit(1);
            }
        };
        let (tx, rx) = mpsc::channel();
        let events = tx.clone();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines().map_while(Result::ok) {
                let input = match protocol::parse_host_line(&line) {
                    Ok(HostMsg::Event(e)) => Input::Event(e),
                    Ok(HostMsg::Visible { visible, item: None }) => Input::Visible(visible),
                    Ok(HostMsg::Visible { visible, item: Some(item) }) => Input::ItemVisible { item, visible },
                    Ok(HostMsg::Widgets { widgets }) => Input::Widgets(widgets),
                    Ok(HostMsg::Settings { settings }) => Input::Settings(settings),
                    Ok(HostMsg::Monitors { monitors }) => Input::Monitors(monitors),
                    Ok(HostMsg::Desktop(d)) => Input::Desktop(d),
                    Ok(HostMsg::Theme { theme }) => Input::Theme(theme),
                    Ok(HostMsg::Shutdown) => Input::Shutdown,
                    _ => continue,
                };
                let stop = matches!(input, Input::Shutdown);
                if events.send(input).is_err() || stop {
                    return;
                }
            }
            let _ = events.send(Input::Shutdown);
        });
        (Plugin { hello: Arc::new(hello), out: Arc::new(Mutex::new(std::io::stdout())), data: tx }, rx)
    }

    /// For your threads: what they send arrives as [`Input::Data`].
    pub fn data(&self) -> impl Fn(T) + Send + Clone + 'static {
        let tx = self.data.clone();
        move |t| {
            let _ = tx.send(Input::Data(t));
        }
    }

    pub fn settings(&self) -> &Value {
        &self.hello.settings
    }

    /// A setting as a string, or `default`.
    pub fn setting_str(&self, key: &str, default: &str) -> String {
        self.hello.settings.get(key).and_then(Value::as_str).unwrap_or(default).to_string()
    }

    pub fn setting_bool(&self, key: &str, default: bool) -> bool {
        self.hello.settings.get(key).and_then(Value::as_bool).unwrap_or(default)
    }

    pub fn setting_i64(&self, key: &str, default: i64) -> i64 {
        self.hello.settings.get(key).and_then(Value::as_i64).unwrap_or(default)
    }

    /// The desktop widgets placed when the plugin started; [`Input::Widgets`] brings
    /// later changes.
    pub fn widgets(&self) -> &[protocol::Widget] {
        &self.hello.widgets
    }

    /// Whether the user turned on this part (or the whole plugin).
    pub fn enabled(&self, part: &str) -> bool {
        self.hello.items.iter().any(|p| p == part || p == "main")
    }

    pub fn send(&self, msg: &PluginMsg) {
        let mut out = self.out.lock().unwrap();
        if out.write_all(encode(msg).as_bytes()).and_then(|_| out.flush()).is_err() {
            // The host is gone.
            std::process::exit(0);
        }
    }

    pub fn item(&self, item: Item) {
        self.send(&PluginMsg::Item(item));
    }

    /// Set properties on node `node` of `item`.
    pub fn patch<I: IntoIterator<Item = (&'static str, Value)>>(&self, item: &str, node: &str, set: I) {
        self.patch_on(item, None, node, set);
    }

    /// [`Plugin::patch`] for one monitor's copy of an item.
    pub fn patch_on<I: IntoIterator<Item = (&'static str, Value)>>(&self, item: &str, monitor: Option<&str>, node: &str, set: I) {
        let set: Map<String, Value> = set.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let monitor = monitor.map(String::from);
        self.send(&PluginMsg::Patch(Patch { item: item.into(), monitor, nodes: vec![NodePatch { id: node.into(), set }] }));
    }

    pub fn popover(&self, item: &str, view: Option<Node>) {
        self.send(&PluginMsg::Popover(protocol::Popover { item: item.into(), monitor: None, view }));
    }

    pub fn menu(&self, item: &str, entries: Vec<MenuEntry>) {
        self.send(&PluginMsg::Menu(protocol::Menu { item: item.into(), monitor: None, entries }));
    }

    pub fn remove(&self, item: &str) {
        self.send(&PluginMsg::Remove { item: item.into(), monitor: None });
    }

    pub fn log(&self, message: impl Into<String>) {
        self.send(&PluginMsg::Log { level: protocol::LogLevel::Info, message: message.into() });
    }
}

/// Run a command, detached (for clicks in trusted plugins).
pub fn spawn(argv: &[&str]) {
    use std::os::unix::process::CommandExt;
    if argv.is_empty() {
        return;
    }
    let mut c = std::process::Command::new(argv[0]);
    c.args(&argv[1..]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).process_group(0);
    if let Ok(mut child) = c.spawn() {
        std::thread::spawn(move || child.wait());
    }
}

/// Whether a command is on PATH.
pub fn have(cmd: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}
