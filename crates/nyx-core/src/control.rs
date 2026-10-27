//! A small control interface on the session bus, so other programs (Nyx Assistant) can
//! work an app directly instead of through its window. Each app registers it on its own
//! GApplication connection and object path:
//!
//! `io.github.design_nexus.nyx.Control` with `Describe() -> s` (the app's tools as JSON)
//! and `Call(s name, s arguments) -> s` (`{"ok": …}` or `{"error": "…"}`).
//!
//! Only the primary instance registers it, so calls reach the open window. Handlers run on
//! the main thread. What they offer is kept narrow: nothing closes unsaved work or deletes,
//! and files are written only inside the home folder.

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const INTERFACE: &str = "io.github.design_nexus.nyx.Control";

const XML: &str = r#"<node>
  <interface name="io.github.design_nexus.nyx.Control">
    <method name="Describe"><arg type="s" name="tools" direction="out"/></method>
    <method name="Call">
      <arg type="s" name="name" direction="in"/>
      <arg type="s" name="arguments" direction="in"/>
      <arg type="s" name="result" direction="out"/>
    </method>
  </interface>
</node>"#;

/// The answer to one `Call`: give it the result once (now or from a later callback).
pub type Reply = Box<dyn FnOnce(Result<Value, String>)>;

/// Register the interface for `app` (call from `connect_startup`). `tools` is what
/// `Describe` returns; `handler` answers `Call`.
pub fn serve(app: &gtk::Application, tools: Value, handler: impl Fn(&str, Value) -> Result<Value, String> + 'static) {
    serve_async(app, tools, move |name, args, reply| reply(handler(name, args)));
}

/// As `serve`, for handlers that answer later (after a database read, say).
pub fn serve_async(app: &gtk::Application, tools: Value, handler: impl Fn(&str, Value, Reply) + 'static) {
    let (Some(connection), Some(path)) = (app.dbus_connection(), app.dbus_object_path()) else {
        return;
    };
    let info = match gio::DBusNodeInfo::for_xml(XML).map(|n| n.lookup_interface(INTERFACE)) {
        Ok(Some(info)) => info,
        _ => return,
    };
    let tools = tools.to_string();
    let registered = connection
        .register_object(&path, &info)
        .method_call(move |_, _, _, _, method, params, invocation| match method {
            "Describe" => invocation.return_value(Some(&(tools.clone(),).to_variant())),
            "Call" => {
                let (name, arguments) = params.get::<(String, String)>().unwrap_or_default();
                let arguments = serde_json::from_str(&arguments).unwrap_or(json!({}));
                handler(
                    &name,
                    arguments,
                    Box::new(move |result| {
                        let reply = match result {
                            Ok(v) => json!({ "ok": v }),
                            Err(e) => json!({ "error": e }),
                        };
                        invocation.return_value(Some(&(reply.to_string(),).to_variant()));
                    }),
                );
            }
            _ => invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method),
        })
        .build();
    if let Err(e) = registered {
        eprintln!("control interface: {e}");
    }
}

/// A string argument, or an error naming it.
pub fn text(args: &Value, key: &str) -> Result<String, String> {
    args[key].as_str().map(str::to_string).ok_or_else(|| format!("{key} is required"))
}

/// A path inside the home folder: `~/x`, a path relative to home, or an absolute one under
/// it. Symbolic links are resolved first, so they can't lead out.
pub fn home_path(input: &str) -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("no home folder")?;
    inside(&home, input)
}

fn inside(home: &Path, input: &str) -> Result<PathBuf, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("path is empty".into());
    }
    let raw = match input.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if input == "~" => home.to_path_buf(),
        None => home.join(input),
    };
    // The deepest folder that exists, resolved; the rest must be plain names.
    let mut existing = raw.clone();
    let mut rest = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name().map(|n| n.to_os_string()) else { return Err("not a usable path".into()) };
        rest.push(name);
        existing = existing.parent().ok_or("not a usable path")?.to_path_buf();
    }
    let mut resolved = existing.canonicalize().map_err(|e| e.to_string())?;
    for name in rest.into_iter().rev() {
        if name == ".." || name == "." {
            return Err("path can't use . or ..".into());
        }
        resolved.push(name);
    }
    let home = home.canonicalize().map_err(|e| e.to_string())?;
    if !resolved.starts_with(&home) || resolved == home {
        return Err("only paths inside the home folder".into());
    }
    Ok(resolved)
}

/// Where to save: inside home, its folder existing, and an existing file only with
/// `overwrite: true`.
pub fn save_target(args: &Value) -> Result<PathBuf, String> {
    let path = home_path(&text(args, "path")?)?;
    if path.is_dir() {
        return Err("that's a folder".into());
    }
    if !path.parent().is_some_and(Path::is_dir) {
        return Err("its folder doesn't exist".into());
    }
    if path.exists() && args["overwrite"] != true {
        return Err("a file of that name exists; pass overwrite: true to replace it".into());
    }
    Ok(path)
}

/// Run `f` once the main loop is idle (after a tab or window has appeared).
pub fn later(f: impl FnOnce() + 'static) {
    glib::idle_add_local_once(f);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside_home() {
        let home = std::env::temp_dir().join(format!("nyx-control-{}", std::process::id()));
        std::fs::create_dir_all(home.join("docs")).unwrap();
        let canon = home.canonicalize().unwrap();
        assert_eq!(inside(&home, "~/docs/a.txt").unwrap(), canon.join("docs/a.txt"));
        assert_eq!(inside(&home, "docs/new/b.txt").unwrap(), canon.join("docs/new/b.txt"));
        assert!(inside(&home, "/etc/passwd").is_err());
        assert!(inside(&home, "~/../x").is_err());
        assert!(inside(&home, "~").is_err());
        assert!(inside(&home, "").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/tmp", home.join("out")).unwrap();
            assert!(inside(&home, "~/out/x.txt").is_err());
        }
        std::fs::remove_dir_all(&home).unwrap();
    }
}
