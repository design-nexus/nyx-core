//! Fetching declarative packages (plugins, themes) from git without running anything
//! from them: a URL allow-list checked before git sees the URL, git with every local
//! and helper transport off, a copy that leaves out `.git` and refuses links, and
//! atomic swaps into place with the previous copy kept for rollback.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Debug builds only, for tests: allow `file:///` repositories.
fn test_allows_file() -> bool {
    cfg!(debug_assertions)
        && (std::env::var_os("NYX_FETCH_TEST_ALLOW_FILE").is_some() || std::env::var_os("NYX_PLUGIN_TEST_ALLOW_FILE").is_some())
}

/// Refuse anything but plain remote git URLs, before git sees them: options
/// (`-oProxyCommand=…`), transport helpers (`ext::sh -c …`) and local paths.
pub fn check_url(url: &str) -> Result<()> {
    let url = url.trim();
    if test_allows_file() && url.starts_with("file:///") && !url.contains("::") {
        return Ok(());
    }
    if url.is_empty() {
        bail!("no URL");
    }
    if url.starts_with('-') {
        bail!("{url:?} looks like an option, not a URL");
    }
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        bail!("{url:?} has spaces or control characters");
    }
    if url.contains("::") {
        bail!("{url:?} uses a git transport helper; only https://, ssh:// and git@host:path are allowed");
    }
    if url.starts_with("file:") || url.starts_with('/') || url.starts_with('.') || url.starts_with('~') {
        bail!("{url:?} is a local path, not a repository URL");
    }
    let scp_like = || {
        let Some((user_host, path)) = url.split_once(':') else { return false };
        user_host.contains('@') && !user_host.contains('/') && !path.starts_with('/') && !path.is_empty()
    };
    let ok = url.strip_prefix("https://").or_else(|| url.strip_prefix("ssh://")).is_some_and(|rest| {
        let host = rest.split('/').next().unwrap_or("");
        !host.is_empty() && !host.starts_with('-') && rest.len() > host.len() + 1
    }) || scp_like();
    if !ok {
        bail!("{url:?}: use an https://, ssh:// or git@host:path URL");
    }
    Ok(())
}

/// How long a clone may take, and how big the checkout may be: anyone can ask for a
/// package from any repository.
const CLONE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
const MAX_CHECKOUT_BYTES: u64 = 200 * 1024 * 1024;

/// Run git with local and helper transports off and no prompts.
pub fn git(dir: Option<&Path>, args: &[&str]) -> Result<String> {
    git_within(dir, args, None)
}

/// `git`, killed after `timeout`. A download slower than 1 KB/s for 30 s stops too.
fn git_within(dir: Option<&Path>, args: &[&str], timeout: Option<std::time::Duration>) -> Result<String> {
    let mut cmd = Command::new("git");
    if let Some(d) = dir {
        cmd.arg("-C").arg(d);
    }
    let file = if test_allows_file() { "always" } else { "never" };
    cmd.args(["-c", &format!("protocol.file.allow={file}"), "-c", "protocol.ext.allow=never", "-c", "advice.detachedHead=false"])
        .args(["-c", "http.lowSpeedLimit=1000", "-c", "http.lowSpeedTime=30"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", std::env::var("GIT_SSH_COMMAND").unwrap_or_else(|_| "ssh -oBatchMode=yes".into()))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().context("running git")?;
    if let Some(limit) = timeout {
        let start = std::time::Instant::now();
        while child.try_wait()?.is_none() {
            if start.elapsed() > limit {
                let _ = child.kill();
                let _ = child.wait();
                bail!("git {} took longer than {} minutes", args.first().unwrap_or(&""), limit.as_secs() / 60);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    let out = child.wait_with_output().context("running git")?;
    if !out.status.success() {
        bail!("git {}: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn tree_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => tree_size(&e.path()),
            Ok(_) => e.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

/// Clone `url` into `dest` and check out `git_ref` (a branch, tag or commit), then
/// `pinned_commit` if given, which HEAD must then be. Returns the commit. Only the
/// branch or tag's newest commit is fetched when that's all that's needed.
pub fn clone(url: &str, git_ref: Option<&str>, pinned_commit: Option<&str>, dest: &Path) -> Result<String> {
    check_url(url)?;
    if git_ref.is_some_and(|r| r.starts_with('-')) {
        bail!("{:?} isn't a branch, tag or commit", git_ref.unwrap_or_default());
    }
    let dest_s = dest.to_string_lossy().to_string();
    let shallow = pinned_commit.is_none() && {
        let mut args = vec!["clone", "--quiet", "--depth", "1", "--single-branch"];
        if let Some(r) = git_ref {
            args.extend(["--branch", r]);
        }
        args.extend(["--", url, &dest_s]);
        git_within(None, &args, Some(CLONE_TIMEOUT)).is_ok()
    };
    if !shallow {
        // A commit for a ref, or a pinned commit: the whole history.
        let _ = std::fs::remove_dir_all(dest);
        git_within(None, &["clone", "--quiet", "--", url, &dest_s], Some(CLONE_TIMEOUT))?;
        if let Some(r) = git_ref {
            git(Some(dest), &["checkout", "--quiet", r, "--"])?;
        }
    }
    if tree_size(dest) > MAX_CHECKOUT_BYTES {
        bail!("the repository is bigger than {} MB", MAX_CHECKOUT_BYTES / 1024 / 1024);
    }
    if let Some(c) = pinned_commit {
        git(Some(dest), &["checkout", "--quiet", c, "--"]).with_context(|| format!("the repository has no commit {c}"))?;
    }
    let commit = git(Some(dest), &["rev-parse", "HEAD"])?;
    if let Some(c) = pinned_commit
        && !commit.eq_ignore_ascii_case(c)
    {
        bail!("the repository is at {commit}, not the pinned commit {c}");
    }
    Ok(commit)
}

/// `sub` inside `root`, refusing absolute paths and `..`.
pub fn subpath(root: &Path, sub: Option<&str>) -> Result<PathBuf> {
    if let Some(s) = sub
        && s.starts_with('/')
    {
        bail!("{s:?} isn't a folder inside the repository");
    }
    match sub.map(|s| s.trim_end_matches('/')).filter(|s| !s.is_empty()) {
        None => Ok(root.to_path_buf()),
        Some(s) if s.split('/').any(|p| p == ".." || p == "." || p.is_empty()) => bail!("{s:?} isn't a folder inside the repository"),
        Some(s) => Ok(root.join(s)),
    }
}

/// Copy a folder, leaving out `.git` and any name in `skip`. Symlinks are refused
/// (`follow = false`), or copied as what they point to (`follow = true`, for copies
/// of packaged files that may be links into the Nix store).
pub fn copy_tree(from: &Path, to: &Path, follow: bool, skip: &[&str]) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" || skip.iter().any(|s| name == *s) {
            continue;
        }
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)?;
        let target = to.join(&name);
        if meta.file_type().is_symlink() {
            if !follow {
                bail!("{} is a symbolic link; packages can't contain links", path.display());
            }
            let real = std::fs::metadata(&path)?;
            if real.is_dir() {
                copy_tree(&path, &target, true, skip)?;
            } else {
                std::fs::copy(&path, &target)?;
            }
        } else if meta.is_dir() {
            copy_tree(&path, &target, follow, skip)?;
        } else if meta.is_file() {
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

/// Move `staged` to `target` atomically. An existing `target` is moved to `keep`
/// first (replacing an older `keep`) and put back if the move fails.
pub fn swap_in(staged: &Path, target: &Path, keep: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let had_old = std::fs::symlink_metadata(target).is_ok();
    if had_old {
        let _ = std::fs::remove_dir_all(keep);
        std::fs::rename(target, keep).with_context(|| format!("moving {} aside", target.display()))?;
    }
    if let Err(e) = std::fs::rename(staged, target) {
        if had_old {
            let _ = std::fs::rename(keep, target);
        }
        return Err(e).with_context(|| format!("moving the new copy to {}", target.display()));
    }
    Ok(())
}

/// Swap `target` and `keep` (the copy before the last [`swap_in`]).
pub fn swap_back(target: &Path, keep: &Path) -> Result<()> {
    if !keep.is_dir() {
        bail!("there's no earlier version to go back to");
    }
    let swap = keep.with_extension("swap");
    let _ = std::fs::remove_dir_all(&swap);
    if target.exists() {
        std::fs::rename(target, &swap)?;
    }
    std::fs::rename(keep, target)?;
    if swap.exists() {
        std::fs::rename(&swap, keep)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        for ok in ["https://github.com/acme/theme.git", "ssh://git@github.com/acme/theme.git", "git@github.com:acme/theme.git"] {
            check_url(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for bad in [
            "-oProxyCommand=touch /tmp/pwned",
            "ext::sh -c touch% /tmp/pwned",
            "file:///home/ken/theme",
            "/home/ken/theme",
            "./theme",
            "https://",
            "http://example.com/t.git",
            "https://-oops/x",
            "git@host:/abs",
            "fd::3",
        ] {
            assert!(check_url(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn subpaths_stay_inside() {
        let root = Path::new("/r");
        assert_eq!(subpath(root, None).unwrap(), root);
        assert_eq!(subpath(root, Some("themes/a/")).unwrap(), root.join("themes/a"));
        for bad in ["../x", "a/../../x", "/etc", "./a"] {
            assert!(subpath(root, Some(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn copying_refuses_links() {
        let t = std::env::temp_dir().join(format!("nyx-fetch-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let src = t.join("src");
        std::fs::create_dir_all(src.join(".git")).unwrap();
        std::fs::write(src.join("a"), "1").unwrap();
        std::fs::write(src.join("skip-me"), "1").unwrap();
        copy_tree(&src, &t.join("out"), false, &["skip-me"]).unwrap();
        assert!(t.join("out/a").is_file());
        assert!(!t.join("out/.git").exists() && !t.join("out/skip-me").exists());
        std::os::unix::fs::symlink("/etc/passwd", src.join("link")).unwrap();
        assert!(copy_tree(&src, &t.join("out2"), false, &[]).is_err());
        copy_tree(&src, &t.join("out3"), true, &[]).unwrap();
        assert!(t.join("out3/link").is_file());
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn swaps_keep_the_old_copy() {
        let t = std::env::temp_dir().join(format!("nyx-fetch-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let (new, target, keep) = (t.join("new"), t.join("target"), t.join("keep"));
        std::fs::create_dir_all(&new).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(new.join("v"), "2").unwrap();
        std::fs::write(target.join("v"), "1").unwrap();
        swap_in(&new, &target, &keep).unwrap();
        assert_eq!(std::fs::read_to_string(target.join("v")).unwrap(), "2");
        assert_eq!(std::fs::read_to_string(keep.join("v")).unwrap(), "1");
        swap_back(&target, &keep).unwrap();
        assert_eq!(std::fs::read_to_string(target.join("v")).unwrap(), "1");
        assert_eq!(std::fs::read_to_string(keep.join("v")).unwrap(), "2");
        let _ = std::fs::remove_dir_all(&t);
    }
}
