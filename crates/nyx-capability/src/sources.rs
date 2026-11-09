//! The threads that turn each [`Source`]'s events into "probe these providers again".

use crate::Source;
use notify::{RecursiveMode, Watcher};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::time::Duration;

/// A command that ended (a daemon restarted, `pactl` lost PipeWire) is started again after
/// this long, doubling up to a minute while it keeps failing.
const RESTART: Duration = Duration::from_secs(2);

type Route<K> = (K, Vec<usize>);

pub fn spawn(sources: Vec<(Source, Vec<usize>)>, tx: Sender<usize>) {
    let mut folders: Vec<Route<PathBuf>> = Vec::new();
    let mut names: [Vec<Route<String>>; 2] = [Vec::new(), Vec::new()];
    let mut udev: Vec<Route<String>> = Vec::new();
    for (source, providers) in sources {
        match source {
            Source::Folder(dir) => folders.push((dir, providers)),
            Source::Units { system } => folders.push((units_dir(system), providers)),
            Source::BusName { system, name } => names[system as usize].push((name, providers)),
            Source::Udev(subsystem) => udev.push((subsystem, providers)),
            Source::Lines { cmd, matching } => {
                let t = tx.clone();
                thread("lines", move || {
                    let poke = |t: &Sender<usize>| {
                        providers.iter().for_each(|&i| {
                            let _ = t.send(i);
                        })
                    };
                    run_lines(
                        &cmd,
                        |line| {
                            if matching.is_empty() || matching.iter().any(|m| line.contains(m.as_str())) {
                                poke(&t);
                            }
                        },
                        || poke(&t),
                    );
                });
            }
        }
    }
    if !folders.is_empty() {
        watch_folders(folders, tx.clone());
    }
    for (system, names) in names.into_iter().enumerate() {
        if !names.is_empty() {
            bus_names(system == 1, names, tx.clone());
        }
    }
    if !udev.is_empty() {
        udev_events(udev, tx);
    }
}

fn thread(name: &str, f: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new().name(format!("nyx-capability-{name}")).spawn(f);
}

pub(crate) fn uid() -> u32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("Uid:")).and_then(|l| l.split_whitespace().nth(1)?.parse().ok()))
        .unwrap_or(0)
}

pub(crate) fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|| format!("/run/user/{}", uid()).into())
}

/// systemd keeps an `invocation:UNIT` link here for every running unit.
fn units_dir(system: bool) -> PathBuf {
    if system { "/run/systemd/units".into() } else { runtime_dir().join("systemd/units") }
}

/// One inotify watcher for every folder. A folder that doesn't exist yet is watched
/// through its parent, so its creation counts as a change.
fn watch_folders(folders: Vec<Route<PathBuf>>, tx: Sender<usize>) {
    let targets: Vec<(PathBuf, PathBuf, Vec<usize>)> = folders
        .into_iter()
        .filter_map(|(dir, p)| {
            let watched = if dir.is_dir() { dir.clone() } else { dir.parent().filter(|d| d.is_dir())?.to_path_buf() };
            Some((dir, watched, p))
        })
        .collect();
    let routes = targets.clone();
    thread("folders", move || {
        let Ok(mut watcher) = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            if matches!(event.kind, notify::EventKind::Access(_)) {
                return;
            }
            for (dir, watched, providers) in &routes {
                let hit = event.paths.iter().any(|p| {
                    p.parent() == Some(dir.as_path()) || p == dir || (watched != dir && p.parent() == Some(watched.as_path()))
                });
                if hit {
                    providers.iter().for_each(|&i| {
                        let _ = tx.send(i);
                    });
                }
            }
        }) else {
            return;
        };
        for (_, watched, _) in &targets {
            let _ = watcher.watch(watched, RecursiveMode::NonRecursive);
        }
        loop {
            std::thread::park();
        }
    });
}

/// `dbus-monitor` for NameOwnerChanged on one bus; a signal naming a watched name pokes
/// its providers.
fn bus_names(system: bool, names: Vec<Route<String>>, tx: Sender<usize>) {
    let cmd = vec![
        "dbus-monitor".to_string(),
        if system { "--system" } else { "--session" }.to_string(),
        "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged'".to_string(),
    ];
    thread("bus", move || {
        run_lines(
            &cmd,
            |line| {
                if let Some(name) = line.trim().strip_prefix("string \"").and_then(|r| r.strip_suffix('"')) {
                    for (n, providers) in &names {
                        if n == name {
                            providers.iter().for_each(|&i| {
                                let _ = tx.send(i);
                            });
                        }
                    }
                }
            },
            || {},
        );
    });
}

/// One `udevadm monitor` for every subsystem; each event line ends with `(SUBSYSTEM)`.
fn udev_events(routes: Vec<Route<String>>, tx: Sender<usize>) {
    let mut cmd = vec!["udevadm".to_string(), "monitor".to_string(), "--udev".to_string()];
    for (subsystem, _) in &routes {
        cmd.push(format!("--subsystem-match={subsystem}"));
    }
    thread("udev", move || {
        run_lines(
            &cmd,
            |line| {
                if !line.starts_with("UDEV") {
                    return;
                }
                let subsystem = line.trim_end().rsplit_once('(').and_then(|(_, s)| s.strip_suffix(')'));
                for (s, providers) in &routes {
                    if subsystem == Some(s.as_str()) {
                        providers.iter().for_each(|&i| {
                            let _ = tx.send(i);
                        });
                    }
                }
            },
            || {},
        );
    });
}

/// Run `cmd` for ever, handing each line to `f` and calling `ended` when it stops; start
/// it again after a delay. A program that isn't installed is given up on.
fn run_lines(cmd: &[String], mut f: impl FnMut(&str), ended: impl Fn()) {
    let Some((program, args)) = cmd.split_first() else { return };
    let mut delay = RESTART;
    loop {
        let started = std::time::Instant::now();
        match Command::new(program).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() {
            Ok(mut child) => {
                if let Some(out) = child.stdout.take() {
                    for line in BufReader::new(out).lines().map_while(Result::ok) {
                        f(&line);
                    }
                }
                let _ = child.wait();
                ended();
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !Path::new(program).is_absolute() => return,
            Err(_) => {}
        }
        if started.elapsed() > Duration::from_secs(60) {
            delay = RESTART;
        }
        std::thread::sleep(delay);
        delay = (delay * 2).min(Duration::from_secs(60));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn a_new_file_in_a_watched_folder_pokes_its_providers() {
        let d = tempfile::tempdir().unwrap();
        let later = d.path().join("later");
        let (tx, rx) = mpsc::channel();
        watch_folders(vec![(d.path().to_path_buf(), vec![1]), (later.clone(), vec![2])], tx);
        std::thread::sleep(Duration::from_millis(100));
        std::fs::write(d.path().join("nyx-dock"), "").unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(1));
        std::fs::create_dir(&later).unwrap();
        let got: Vec<usize> = std::iter::from_fn(|| rx.recv_timeout(Duration::from_millis(500)).ok()).collect();
        assert!(got.contains(&2), "a folder appearing counts: {got:?}");
    }

    #[test]
    fn lines_matching_poke_and_so_does_the_end() {
        let (tx, rx) = mpsc::channel();
        let cmd = vec!["sh".into(), "-c".into(), "echo \"Event 'change' on sink #1\"; echo \"Event 'new' on card #2\"".into()];
        spawn(vec![(Source::Lines { cmd, matching: vec!["card".into()] }, vec![7])], tx);
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(7));
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(7), "the command ending");
    }
}
