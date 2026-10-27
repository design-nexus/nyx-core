//! Running external commands without blocking the UI thread.

use anyhow::{Context, Result, bail};
#[cfg(feature = "gtk")]
use gtk::prelude::*;
#[cfg(feature = "gtk")]
use gtk::{gio, glib};
use std::path::Path;
use std::process::{Command, Stdio};

/// Run a command to completion and return its trimmed stdout.
pub fn run(args: &[&str]) -> Result<String> {
    let (program, rest) = args.split_first().context("empty command")?;
    let output =
        Command::new(program).args(rest).stdin(Stdio::null()).output().with_context(|| format!("could not start {program}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim_end().to_string())
}

/// Like [`run`] but returns `None` on any failure.
pub fn output(args: &[&str]) -> Option<String> {
    run(args).ok()
}

/// Is `program` on `PATH` (or, given a path, does it exist)?
pub fn present(program: &str) -> bool {
    if program.contains('/') {
        return Path::new(program).exists();
    }
    std::env::var_os("PATH").map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file())).unwrap_or(false)
}

/// Quote a word for `sh`, so it can sit inside a command line handed to a terminal.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Fire and forget. The child is reaped on a helper thread so no zombies remain.
pub fn spawn(args: &[&str]) {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    std::thread::spawn(move || {
        if let Some((program, rest)) = args.split_first() {
            let _ = Command::new(program).args(rest).stdin(Stdio::null()).status();
        }
    });
}

#[cfg(feature = "gtk")]
/// Run a command off the main thread and deliver the result back on it.
pub fn run_async<F>(args: &[&str], done: F)
where
    F: FnOnce(Result<String>) + 'static,
{
    let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    background(
        move || {
            let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
            run(&refs)
        },
        done,
    );
}

#[cfg(feature = "gtk")]
/// Run arbitrary blocking work off the main thread, then call `done` on it.
pub fn background<T, W, F>(work: W, done: F)
where
    T: Send + 'static,
    W: FnOnce() -> T + Send + 'static,
    F: FnOnce(T) + 'static,
{
    let handle = gio::spawn_blocking(work);
    glib::spawn_future_local(async move {
        if let Ok(value) = handle.await {
            done(value);
        }
    });
}

/// Open a file in the user's default editor (Omarchy's choice), falling back to xdg-open.
pub fn open_in_editor(path: &Path) {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, "");
    }
    let p = path.to_string_lossy().to_string();
    if present("omarchy-launch-editor") {
        spawn(&["omarchy-launch-editor", &p]);
    } else {
        spawn(&["xdg-open", &p]);
    }
}

#[cfg(feature = "gtk")]
/// Show `path` selected in the file manager, or open its folder if no file
/// manager answers on D-Bus.
pub fn show_in_folder(path: &Path) {
    let uri = gio::File::for_path(path).uri().to_string();
    let parent = path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    gio::bus_get(gio::BusType::Session, gio::Cancellable::NONE, move |bus| {
        let Ok(bus) = bus else {
            spawn(&["xdg-open", &parent]);
            return;
        };
        let args = (vec![uri], String::new()).to_variant();
        bus.call(
            Some("org.freedesktop.FileManager1"),
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1",
            "ShowItems",
            Some(&args),
            None,
            gio::DBusCallFlags::NONE,
            2000,
            gio::Cancellable::NONE,
            move |res| {
                if res.is_err() {
                    spawn(&["xdg-open", &parent]);
                }
            },
        );
    });
}

/// Write a file atomically: a temp file in the same directory, synced, then
/// renamed over the target. Follows symlinks and keeps the file's permissions.
pub fn atomic_write(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    use std::io::Write;
    let path = &std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = path.parent().context("path has no parent")?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.tmp-{}", path.file_name().unwrap_or_default().to_string_lossy(), std::process::id()));
    let written = (|| -> Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents.as_ref())?;
        if let Ok(meta) = std::fs::metadata(path) {
            f.set_permissions(meta.permissions())?;
        }
        f.sync_all()?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.with_context(|| format!("could not write {}", path.display()))?;
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn quotes() {
        assert_eq!(shell_quote("tokyo-night"), "'tokyo-night'");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn writes_atomically() {
        let dir = std::env::temp_dir().join(format!("nyx-core-test-{}", std::process::id()));
        let file = dir.join("a/b.txt");
        atomic_write(&file, "one").unwrap();
        atomic_write(&file, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "two");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn atomic_write_keeps_permissions_and_symlinks() {
        let dir = std::env::temp_dir().join(format!("nyx-core-perm-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.sh");
        std::fs::write(&file, "old").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o750)).unwrap();
        let link = dir.join("link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&file, &link).unwrap();

        atomic_write(&link, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o750);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
