//! Print the desktop as Hyprland reports it, then every change. Read-only.
//!
//! `cargo run -p nyx-desktop --example watch`

use nyx_desktop::{omarchy, watch};

fn main() {
    let _watcher = watch::watch(|u| {
        let d = &u.desktop;
        if !d.connected {
            println!("Hyprland went away; waiting for it");
            return;
        }
        if u.changes.monitors || u.changes.active {
            for m in &d.monitors {
                let ws = d.workspaces.iter().find(|w| w.id == m.active_workspace);
                let mode = omarchy::workspace_mode(m.active_workspace, ws.map_or("", |w| w.tiled_layout.as_str()));
                println!(
                    "monitor {} [{}]{}{} shows {} ({})",
                    m.name,
                    m.stable_id(),
                    if m.focused { " focused" } else { "" },
                    if m.is_virtual() { " virtual" } else { "" },
                    m.active_workspace,
                    mode.label()
                );
            }
        }
        if u.changes.workspaces {
            let ids: Vec<i64> = d.user_workspaces().map(|w| w.id).collect();
            println!("user workspaces {ids:?}");
        }
        if u.changes.windows {
            println!("{} windows", d.windows.len());
        }
        if u.changes.focus {
            let w = d.active_window.as_deref().and_then(|a| d.window(a));
            println!("focus: {}", w.map_or("nothing".into(), |w| format!("{} — {}", w.class, w.title)));
        }
    });
    loop {
        std::thread::park();
    }
}
