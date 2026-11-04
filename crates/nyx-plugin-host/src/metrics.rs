//! What a running plugin uses, from its cgroup (the systemd scope it runs in), so it
//! counts everything the plugin started. Without a scope, from `/proc` for the process.

use std::path::PathBuf;
use std::time::Instant;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub memory_bytes: u64,
    /// CPU time used, microseconds.
    pub cpu_usec: u64,
}

pub fn cgroup_dir(pid: u32) -> Option<PathBuf> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let path = text.lines().find_map(|l| l.strip_prefix("0::"))?;
    Some(PathBuf::from("/sys/fs/cgroup").join(path.trim_start_matches('/')))
}

/// Usage of the plugin's scope, or of the process when it has none of its own.
pub fn usage(pid: u32, scoped: bool) -> Option<Usage> {
    if scoped && let Some(dir) = cgroup_dir(pid) {
        let memory = std::fs::read_to_string(dir.join("memory.current")).ok().and_then(|s| s.trim().parse().ok());
        let cpu = std::fs::read_to_string(dir.join("cpu.stat"))
            .ok()
            .and_then(|s| s.lines().find_map(|l| l.strip_prefix("usage_usec ").and_then(|v| v.trim().parse().ok())));
        if let (Some(memory_bytes), Some(cpu_usec)) = (memory, cpu) {
            return Some(Usage { memory_bytes, cpu_usec });
        }
    }
    process_usage(pid)
}

pub fn process_usage(pid: u32) -> Option<Usage> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = stat.rsplit_once(") ")?.1;
    let f: Vec<&str> = after.split_whitespace().collect();
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    let cpu = (f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?) * 1_000_000 / ticks;
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let rss_kb: u64 = status.lines().find_map(|l| l.strip_prefix("VmRSS:")).and_then(|v| v.split_whitespace().next()?.parse().ok())?;
    Some(Usage { memory_bytes: rss_kb * 1024, cpu_usec: cpu })
}

/// CPU percentage between two samples.
pub struct CpuMeter {
    last: Option<(Instant, u64)>,
}

impl Default for CpuMeter {
    fn default() -> Self {
        CpuMeter::new()
    }
}

impl CpuMeter {
    pub fn new() -> CpuMeter {
        CpuMeter { last: None }
    }

    pub fn percent(&mut self, cpu_usec: u64) -> Option<f64> {
        let now = Instant::now();
        let out = self.last.and_then(|(t, before)| {
            let wall = now.duration_since(t).as_micros() as f64;
            (wall > 0.0).then(|| (cpu_usec.saturating_sub(before)) as f64 * 100.0 / wall)
        });
        self.last = Some((now, cpu_usec));
        out
    }
}

/// Whether this user's systemd can run plugins in scopes with limits.
pub fn scopes_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        std::process::Command::new("systemd-run")
            .args(["--user", "--scope", "--quiet", "--collect", "-p", "MemoryMax=64M", "--", "true"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// Whether the cgroup CPU controller is delegated to the user's systemd (CPUQuota
/// works). Otherwise the supervisor watches CPU time itself.
pub fn cpu_controller() -> bool {
    let uid = unsafe { libc::getuid() };
    std::fs::read_to_string(format!("/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/cgroup.controllers"))
        .is_ok_and(|s| s.split_whitespace().any(|c| c == "cpu"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_this_process() {
        let u = process_usage(std::process::id()).unwrap();
        assert!(u.memory_bytes > 0);
        let mut m = CpuMeter::new();
        assert!(m.percent(u.cpu_usec).is_none());
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(m.percent(u.cpu_usec + 1000).is_some());
    }
}
