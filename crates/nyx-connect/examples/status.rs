//! What Nyx Connect sees, without anything private (no clipboard text, notifications,
//! file names or keys): `cargo run -p nyx-connect --example status`. With `--watch`,
//! keep printing each change.

use nyx_connect::{Access, Client, Event, Prefs, Snapshot, Watch};

fn print(s: &Snapshot) {
    println!(
        "fluxd {} on port {}; autoClipboard {}, notifications {}, Browse PC {}",
        s.this.version, s.this.tcp_port, s.settings.auto_clipboard, s.settings.notifications, s.settings.share_home
    );
    for d in &s.devices {
        let access: Vec<String> = Access::ALL.iter().filter(|a| s.allowed(&d.id, **a)).map(|a| a.key().to_string()).collect();
        println!(
            "  {} [{}] {:?} battery {:?} app {} {} features {:?} allowed {:?}",
            d.name,
            d.kind,
            d.status(),
            d.battery.map(|b| b.charge),
            d.app,
            d.app_version,
            d.features(),
            access
        );
    }
    let incoming = s.clipboard.iter().filter(|c| c.is_incoming_copy()).count();
    let active = s.transfers.iter().filter(|t| !t.finished()).count();
    println!(
        "  clipboard entries {} ({incoming} from devices), transfers {} ({active} running)",
        s.clipboard.len(),
        s.transfers.len()
    );
}

fn main() {
    let prefs = Prefs::load();
    println!("Nyx Clipboard sync: {}", prefs.clipboard_sync);
    if std::env::args().any(|a| a == "--watch") {
        let _watch = Watch::start(|e| match e {
            Event::State(s) => print(&s),
            Event::Toast(_) => {}
            Event::Unavailable(e) => println!("unavailable: {e}"),
        });
        loop {
            std::thread::park();
        }
    }
    match Client::new().state() {
        Ok(s) => print(&s),
        Err(e) => println!("unavailable: {e}"),
    }
}
