//! Which command does a desktop job on this computer.
//!
//! The Nyx apps grew up on Omarchy and call its `omarchy-JOB` scripts (`omarchy-network-band`,
//! `omarchy-dns`, …). NyxOS has none of them: its `nyx-platform JOB` does the same jobs
//! with the same arguments and output, so an app parses one format everywhere.
//!
//! [`command`] picks: Omarchy's script where it's installed (Omarchy keeps its own),
//! else `nyx-platform JOB` where `nyx-platform` does that job, else nothing (the app
//! hides the feature, as it did without the script).

use std::path::{Path, PathBuf};

/// The jobs `nyx-platform` does, named as Omarchy names its scripts (`omarchy-` + job).
pub const JOBS: &[&str] = &[
    "network-band",
    "network-status",
    "network-speedtest",
    "network-captive-portal",
    "restart-wifi",
    "restart-bluetooth",
    "dns",
    "tailscale-send",
    "tailscale-receive",
];

/// The program on `PATH` called `name`.
pub fn find(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The command for `job` (without its arguments): `["omarchy-JOB"]` or
/// `["nyx-platform", "JOB"]`, or `None` when nothing here does it.
pub fn command(job: &str) -> Option<Vec<String>> {
    let script = format!("omarchy-{job}");
    if find(&script).is_some() {
        return Some(vec![script]);
    }
    (JOBS.contains(&job) && find("nyx-platform").is_some()).then(|| vec!["nyx-platform".into(), job.into()])
}

/// `job`'s command followed by `args`.
pub fn argv(job: &str, args: &[&str]) -> Option<Vec<String>> {
    let mut v = command(job)?;
    v.extend(args.iter().map(|a| a.to_string()));
    Some(v)
}

/// Whether something here does `job`.
pub fn available(job: &str) -> bool {
    command(job).is_some()
}

/// The job an `omarchy-JOB` script name stands for, when `nyx-platform` does it.
pub fn job_for_script(script: &str) -> Option<&'static str> {
    let job = script.strip_prefix("omarchy-")?;
    JOBS.iter().copied().find(|j| *j == job)
}

/// `args` to run here: an `omarchy-JOB …` command becomes `nyx-platform JOB …` where the
/// script isn't installed and `nyx-platform` does that job. Anything else is unchanged,
/// so an app's command helper can pass every command through this.
pub fn resolve(args: &[&str]) -> Vec<String> {
    if let Some((program, rest)) = args.split_first()
        && let Some(job) = job_for_script(program)
        && find(program).is_none()
        && find("nyx-platform").is_some()
    {
        let mut v = vec!["nyx-platform".to_string(), job.to_string()];
        v.extend(rest.iter().map(|a| a.to_string()));
        return v;
    }
    args.iter().map(|a| a.to_string()).collect()
}

/// Whether `program` runs here: on `PATH` (or, given a path, it exists), or an
/// `omarchy-JOB` script `nyx-platform` stands in for.
pub fn present(program: &str) -> bool {
    if program.contains('/') {
        return Path::new(program).exists();
    }
    find(program).is_some() || (job_for_script(program).is_some() && find("nyx-platform").is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_map_to_jobs() {
        assert_eq!(job_for_script("omarchy-network-band"), Some("network-band"));
        assert_eq!(job_for_script("omarchy-dns"), Some("dns"));
        assert_eq!(job_for_script("omarchy-theme-set"), None);
        assert_eq!(job_for_script("network-band"), None);
    }

    #[test]
    fn other_commands_pass_through() {
        assert_eq!(resolve(&["nmcli", "radio"]), ["nmcli", "radio"]);
        assert_eq!(resolve(&["omarchy-theme-set", "x"]), ["omarchy-theme-set", "x"]);
        assert!(resolve(&[]).is_empty());
    }

    #[test]
    fn an_unknown_job_has_no_command() {
        assert_eq!(command("no-such-job-anywhere"), None);
        assert!(!available("no-such-job-anywhere"));
    }
}

#[cfg(feature = "cli")]
pub mod cli;
