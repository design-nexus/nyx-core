//! `nyx-platform JOB [ARGS…]`: the jobs Omarchy's `omarchy-JOB` scripts do, for NyxOS,
//! with the same arguments and output. Where the script is installed, it runs instead.

mod desktop;
mod dns;
mod input;
mod net;
mod speed;
mod tailscale;

use std::process::{Command, Stdio};

pub fn main(args: Vec<String>) -> i32 {
    let Some((job, rest)) = args.split_first() else {
        usage();
        return 2;
    };
    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
    match job.as_str() {
        "jobs" => {
            for j in crate::JOBS {
                println!("{j}");
            }
            return 0;
        }
        "has" => return if rest.first().is_some_and(|j| crate::available(j)) { 0 } else { 1 },
        "-h" | "--help" | "help" => {
            usage();
            return 0;
        }
        _ => {}
    }
    // Omarchy's own script, where there is one.
    if crate::JOBS.contains(&job.as_str())
        && let Some(script) = crate::find(&format!("omarchy-{job}"))
    {
        use std::os::unix::process::CommandExt;
        let err = Command::new(script).args(&rest).exec();
        eprintln!("nyx-platform: {err}");
        return 1;
    }
    let r = match job.as_str() {
        "network-band" => net::band(&rest),
        "network-status" => net::status(&rest),
        "network-speedtest" => speed::run(&rest),
        "network-captive-portal" => net::captive_portal(&rest),
        "restart-wifi" => net::restart_wifi(),
        "restart-bluetooth" => net::restart_bluetooth(),
        "dns" => dns::run_dns(&rest),
        "tailscale-send" => tailscale::send(&rest),
        "tailscale-receive" => tailscale::receive(&rest),
        "hw-touchpad" => input::hw("touchpad"),
        "hw-touchscreen" => input::hw("touchscreen"),
        "toggle-touchpad" => input::toggle("touchpad", &rest),
        "toggle-touchscreen" => input::toggle("touchscreen", &rest),
        "battery-present" => input::battery_present(),
        "hyprland-monitor-focused" => input::monitor_focused(),
        "restart-hyprsunset" => input::restart_hyprsunset(),
        _ => {
            eprintln!("nyx-platform: no job called {job} (nyx-platform jobs lists them)");
            return 127;
        }
    };
    match r {
        Ok(()) => 0,
        Err(Fail::Usage(u)) => {
            eprintln!("Usage: nyx-platform {job} {u}");
            2
        }
        Err(Fail::Error(e)) => {
            if !e.is_empty() {
                eprintln!("Error: {e}");
            }
            1
        }
    }
}

fn usage() {
    eprintln!("Usage: nyx-platform JOB [ARGS…]   (nyx-platform jobs lists the jobs)");
    eprintln!("       nyx-platform has JOB       (whether this computer does JOB)");
}

pub enum Fail {
    Usage(&'static str),
    Error(String),
}

impl From<String> for Fail {
    fn from(s: String) -> Self {
        Fail::Error(s)
    }
}

impl From<&str> for Fail {
    fn from(s: &str) -> Self {
        Fail::Error(s.to_string())
    }
}

pub type Res = Result<(), Fail>;

/// A program's standard output, when it succeeds. `LC_ALL=C`, so the words nmcli
/// prints (`connected`, `full`) don't change with the session's language.
pub fn output(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).env("LC_ALL", "C").stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Runs a program; its error output (the last line of it) when it fails.
pub fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{program} didn't run: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    Err(err.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("failed").trim().trim_start_matches("Error:").trim().to_string())
}

/// `nmcli -e no -g FIELDS …`: values unescaped, `:` between fields.
pub fn nm_get(args: &[&str]) -> String {
    let mut a = vec!["-e", "no", "-g"];
    a.extend_from_slice(args);
    output("nmcli", &a).unwrap_or_default()
}
