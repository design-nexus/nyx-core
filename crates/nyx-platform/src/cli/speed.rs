//! `network-speedtest down|up`: the live speed, in Mbps, one sample a line every half
//! second for about seven seconds, against Cloudflare's speed test (speed.cloudflare.com).

use super::{Fail, Res};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The largest download it serves at once; the test runs them back to back.
const DOWN: &str = "https://speed.cloudflare.com/__down?bytes=25000000";
const UP: &str = "https://speed.cloudflare.com/__up";
const LENGTH: Duration = Duration::from_secs(7);
const SAMPLE: Duration = Duration::from_millis(500);

/// One transfer, counting its bytes into `counted` until it ends or `stop` is set.
fn transfer(up: bool, left: Duration, counted: &AtomicU64, stop: &AtomicBool) -> bool {
    let secs = left.as_secs().max(1).to_string();
    let mut cmd = Command::new("curl");
    cmd.args(["-s", "-f", "--max-time", &secs]);
    if up {
        // `-T -` streams standard input as it comes (`--data-binary @-` reads it all first).
        cmd.args(["-X", "POST", "-H", "Content-Type: application/octet-stream", "-T", "-", "-o", "/dev/null", UP])
            .stdin(Stdio::piped())
            .stdout(Stdio::null());
    } else {
        cmd.arg(DOWN).stdout(Stdio::piped());
    }
    let Ok(mut child) = cmd.stderr(Stdio::null()).spawn() else { return false };
    let mut moved = 0u64;
    if up {
        if let Some(mut stdin) = child.stdin.take() {
            let chunk = vec![0u8; 64 * 1024];
            while !stop.load(Ordering::Relaxed) && stdin.write_all(&chunk).is_ok() {
                counted.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                moved += chunk.len() as u64;
            }
        }
    } else if let Some(mut stdout) = child.stdout.take() {
        let mut buf = vec![0u8; 256 * 1024];
        while !stop.load(Ordering::Relaxed) {
            match stdout.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    counted.fetch_add(n as u64, Ordering::Relaxed);
                    moved += n as u64;
                }
            }
        }
    }
    let _ = child.kill();
    let ok = child.wait().is_ok_and(|s| s.success());
    ok || moved > 0
}

pub fn run(args: &[&str]) -> Res {
    let up = match args {
        ["down"] => false,
        ["up"] => true,
        _ => return Err(Fail::Usage("[down|up]")),
    };
    let bytes = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let start = Instant::now();
    let worker = {
        let (bytes, stop) = (bytes.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let Some(left) = LENGTH.checked_sub(start.elapsed()) else { break };
                if !transfer(up, left, &bytes, &stop) {
                    break;
                }
            }
        })
    };
    let (mut last, mut last_at) = (0u64, start);
    let mut out = std::io::stdout();
    while start.elapsed() < LENGTH && !worker.is_finished() {
        std::thread::sleep(SAMPLE);
        let now = bytes.load(Ordering::Relaxed);
        let secs = last_at.elapsed().as_secs_f64();
        if secs > 0.0 && writeln!(out, "{:.1}", (now - last) as f64 * 8.0 / 1e6 / secs).and_then(|_| out.flush()).is_err() {
            break;
        }
        (last, last_at) = (now, Instant::now());
    }
    stop.store(true, Ordering::Relaxed);
    let _ = worker.join();
    if bytes.load(Ordering::Relaxed) == 0 {
        return Err("the speed test server didn't answer.".into());
    }
    Ok(())
}
