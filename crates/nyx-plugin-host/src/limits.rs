//! What a plugin may use. Third-party plugins get strict defaults, raised only by the
//! user (`[plugins."<id>".limits]` in the host's config); trusted plugins get what
//! their manifest asks for, up to a system maximum.

use nyx_plugin::manifest::LimitRequest;

#[derive(Debug, Clone, PartialEq)]
pub struct Limits {
    pub memory_mb: u64,
    /// Percent of one core.
    pub cpu_percent: u32,
    pub tasks: u32,
    pub nice: i32,
    pub io_weight: u32,
    pub messages_per_sec: u32,
    pub message_bytes: usize,
    pub image_bytes: usize,
    pub vector_elements: usize,
    pub fps: u32,
    /// Largest frame, logical pixels.
    pub frame_max: (i32, i32),
    pub popover_max: (i32, i32),
    /// Items per plugin, per monitor.
    pub items: usize,
    pub interval_min_secs: u64,
    pub interval_timeout_secs: u64,
    pub infinite_animations: usize,
    /// Crashes or kills in `crash_window_secs` before the plugin is turned off.
    pub crashes: usize,
    pub crash_window_secs: u64,
}

impl Limits {
    pub fn third_party() -> Limits {
        Limits {
            memory_mb: 128,
            cpu_percent: 10,
            tasks: 32,
            nice: 10,
            io_weight: 50,
            messages_per_sec: 30,
            message_bytes: 256 * 1024,
            image_bytes: 2 * 1024 * 1024,
            vector_elements: 5000,
            fps: 30,
            frame_max: (400, 64),
            popover_max: (480, 640),
            items: 16,
            interval_min_secs: 5,
            interval_timeout_secs: 10,
            infinite_animations: 4,
            crashes: 5,
            crash_window_secs: 600,
        }
    }

    /// The most a trusted plugin's manifest can ask for.
    pub fn system_max() -> Limits {
        Limits {
            memory_mb: 1024,
            cpu_percent: 100,
            tasks: 256,
            nice: 0,
            io_weight: 100,
            messages_per_sec: 240,
            message_bytes: 1024 * 1024,
            image_bytes: 8 * 1024 * 1024,
            vector_elements: 50_000,
            fps: 60,
            frame_max: (1600, 1600),
            popover_max: (800, 900),
            items: 128,
            interval_min_secs: 1,
            interval_timeout_secs: 30,
            infinite_animations: 32,
            crashes: 5,
            crash_window_secs: 600,
        }
    }

    /// Trusted plugins start from a roomier default.
    pub fn trusted() -> Limits {
        Limits { memory_mb: 256, cpu_percent: 50, tasks: 128, nice: 0, messages_per_sec: 120, items: 64, ..Limits::system_max() }
    }

    /// The limits for one plugin: the default for its trust, its manifest's requests
    /// (capped for third-party plugins), then the user's overrides (never capped:
    /// the user decides).
    pub fn resolve(trusted: bool, request: &LimitRequest, user: Option<&toml::Table>) -> Limits {
        let (mut l, cap) = if trusted { (Limits::trusted(), Limits::system_max()) } else { (Limits::third_party(), Limits::third_party()) };
        if let Some(m) = request.memory.as_deref().and_then(parse_mb) {
            l.memory_mb = m.min(cap.memory_mb);
        }
        if let Some(c) = request.cpu {
            l.cpu_percent = c.min(cap.cpu_percent);
        }
        if let Some(t) = request.tasks {
            l.tasks = t.min(cap.tasks);
        }
        if let Some(m) = request.messages {
            l.messages_per_sec = m.min(cap.messages_per_sec);
        }
        if let Some(f) = request.fps {
            l.fps = f.min(cap.fps);
        }
        if let Some(u) = user {
            if let Some(m) = u.get("memory").and_then(|v| v.as_str()).and_then(parse_mb) {
                l.memory_mb = m;
            }
            let int = |k: &str| u.get(k).and_then(|v| v.as_integer()).filter(|v| *v > 0);
            if let Some(v) = int("cpu") {
                l.cpu_percent = v as u32;
            }
            if let Some(v) = int("tasks") {
                l.tasks = v as u32;
            }
            if let Some(v) = int("messages") {
                l.messages_per_sec = v as u32;
            }
            if let Some(v) = int("fps") {
                l.fps = (v as u32).min(120);
            }
            if let Some(v) = int("items") {
                l.items = v as usize;
            }
        }
        l
    }
}

/// `64M`, `1G`, `512` (megabytes).
pub fn parse_mb(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, mult) = match s.chars().last()? {
        'G' | 'g' => (&s[..s.len() - 1], 1024),
        'M' | 'm' => (&s[..s.len() - 1], 1),
        'K' | 'k' => return s[..s.len() - 1].trim().parse::<u64>().ok().map(|k| (k / 1024).max(1)),
        _ => (s, 1),
    };
    num.trim().parse::<u64>().ok().map(|n| n * mult).filter(|n| *n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn third_party_requests_are_capped_but_the_user_decides() {
        let req = LimitRequest { memory: Some("1G".into()), cpu: Some(80), ..Default::default() };
        let l = Limits::resolve(false, &req, None);
        assert_eq!((l.memory_mb, l.cpu_percent), (128, 10));
        let user: toml::Table = toml::from_str("memory = \"512M\"\ncpu = 25").unwrap();
        let l = Limits::resolve(false, &req, Some(&user));
        assert_eq!((l.memory_mb, l.cpu_percent), (512, 25));
        let l = Limits::resolve(true, &req, None);
        assert_eq!((l.memory_mb, l.cpu_percent), (1024, 80));
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_mb("64M"), Some(64));
        assert_eq!(parse_mb("2g"), Some(2048));
        assert_eq!(parse_mb("300"), Some(300));
        assert_eq!(parse_mb("x"), None);
    }
}
