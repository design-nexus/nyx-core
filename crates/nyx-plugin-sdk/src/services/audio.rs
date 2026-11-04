//! The default output's volume, through WirePlumber's `wpctl`, kept current by
//! `pactl subscribe` (PipeWire's PulseAudio server). Without `pactl` it checks every
//! two seconds.

use std::io::BufRead;
use std::process::{Command, Stdio};
use std::time::Duration;

const SINK: &str = "@DEFAULT_AUDIO_SINK@";
const SOURCE: &str = "@DEFAULT_AUDIO_SOURCE@";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Audio {
    pub available: bool,
    /// 0–1.5.
    pub volume: f64,
    pub muted: bool,
    pub mic_muted: Option<bool>,
}

impl Audio {
    pub fn percent(&self) -> u32 {
        (self.volume * 100.0).round() as u32
    }

    pub fn icon(&self) -> &'static str {
        match self.percent() {
            _ if self.muted || !self.available => "audio-volume-muted-symbolic",
            0..=33 => "audio-volume-low-symbolic",
            34..=66 => "audio-volume-medium-symbolic",
            _ => "audio-volume-high-symbolic",
        }
    }
}

fn parse(out: &str) -> Option<(f64, bool)> {
    let rest = out.trim().strip_prefix("Volume:")?.trim();
    let num = rest.split_whitespace().next()?.parse().ok()?;
    Some((num, rest.contains("[MUTED]")))
}

pub fn read() -> Audio {
    let get = |dev: &str| Command::new("wpctl").args(["get-volume", dev]).stderr(Stdio::null()).output().ok().and_then(|o| parse(&String::from_utf8_lossy(&o.stdout)));
    match get(SINK) {
        Some((volume, muted)) => Audio { available: true, volume, muted, mic_muted: get(SOURCE).map(|(_, m)| m) },
        None => Audio::default(),
    }
}

pub fn watch(f: impl Fn(Audio) + Send + 'static) {
    std::thread::spawn(move || {
        let mut last = None;
        let trigger = super::refresher(Duration::from_secs(if crate::have("pactl") { 60 } else { 2 }), move || {
            let a = read();
            if last.as_ref() != Some(&a) {
                last = Some(a.clone());
                f(a);
            }
        });
        if !crate::have("pactl") {
            return;
        }
        loop {
            let mut cmd = Command::new("pactl");
            cmd.arg("subscribe").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
            // It goes when the plugin does.
            unsafe {
                use std::os::unix::process::CommandExt;
                cmd.pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                    Ok(())
                });
            }
            let Ok(mut child) = cmd.spawn() else { return };
            if let Some(out) = child.stdout.take() {
                for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                    if (line.contains("sink") || line.contains("source") || line.contains("server")) && trigger.send(()).is_err() {
                        let _ = child.kill();
                        return;
                    }
                }
            }
            let _ = child.wait();
            std::thread::sleep(Duration::from_secs(2));
        }
    });
}

/// Change the volume by `percent` points (negative lowers it), up to 150%.
pub fn change(percent: i32) {
    let arg = format!("{}%{}", percent.abs(), if percent < 0 { "-" } else { "+" });
    let _ = Command::new("wpctl").args(["set-volume", "-l", "1.5", SINK, &arg]).status();
}

pub fn set(volume: f64) {
    let _ = Command::new("wpctl").args(["set-volume", "-l", "1.5", SINK, &format!("{:.2}", volume.clamp(0.0, 1.5))]).status();
}

pub fn toggle_mute() {
    let _ = Command::new("wpctl").args(["set-mute", SINK, "toggle"]).status();
}

pub fn toggle_mic() {
    let _ = Command::new("wpctl").args(["set-mute", SOURCE, "toggle"]).status();
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_wpctl() {
        assert_eq!(super::parse("Volume: 0.45\n"), Some((0.45, false)));
        assert_eq!(super::parse("Volume: 1.00 [MUTED]"), Some((1.0, true)));
        assert_eq!(super::parse("nope"), None);
    }
}
