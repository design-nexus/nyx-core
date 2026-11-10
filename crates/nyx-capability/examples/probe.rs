//! Print what this computer can do: `cargo run -p nyx-capability --example probe`.
//! With `--watch`, keep printing changes.

use nyx_capability::providers::*;
use nyx_capability::{Provider, Registry};

fn main() {
    let providers: Vec<Box<dyn Provider>> = vec![
        Box::new(Programs::new(["nyx-dock", "nyx-ai", "nyx-assistant", "omarchy-version", "tailscale", "nyx-authctl"])),
        Box::new(Services::new([(false, "nyx-notifyd"), (false, "nyx-dock"), (true, "bluetooth")])),
        Box::new(BusNames::new([
            (false, "io.github.design_nexus.nyx.Notifications"),
            (false, "io.github.design_nexus.nyx.Theme"),
            (true, "io.github.nyxos_project.System"),
        ])),
        Box::new(Hardware::default()),
        Box::new(ShellCore),
        Box::new(Connect),
        Box::new(Audio),
        Box::new(Fingerprint),
    ];
    let registry = Registry::new(providers);
    let watch = std::env::args().any(|a| a == "--watch");
    if watch {
        registry.start();
    } else {
        registry.refresh_all();
    }
    for c in registry.snapshot().values() {
        println!("{:32} {:24} {}", c.id, c.state.label(), c.reason.as_deref().unwrap_or(""));
    }
    if watch {
        registry.subscribe(|changes| {
            for c in changes {
                println!("{} {:?} -> {:?}", c.id, c.before, c.after.as_ref().map(|a| a.state));
            }
        });
        loop {
            std::thread::park();
        }
    }
}
