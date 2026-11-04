//! Adding, updating, removing, linking and cloning plugins. Installing never runs the
//! plugin's code: no install scripts, no builds. Script plugins run as they are;
//! compiled plugins ship prebuilt archives (`[artifacts]`) whose hash is checked.
//!
//! A user plugin's folder is a plain copy (no `.git`, no symlinks) plus
//! `.nyx-plugin.toml`, which records where it came from for `update`.

use crate::discovery::{self, ORIGIN_FILE, Origin, Plugin, Trust};
use crate::index::Entry;
use crate::manifest::{Artifact, Manifest};
use crate::{paths, state};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Refuse anything but plain remote git URLs, before git sees them: options
/// (`-oProxyCommand=…`), transport helpers (`ext::sh -c …`) and local paths.
pub fn check_url(url: &str) -> Result<()> {
    let url = url.trim();
    // Debug builds only, for the CLI tests: a local repo by file:// URL.
    #[cfg(debug_assertions)]
    if std::env::var_os("NYX_PLUGIN_TEST_ALLOW_FILE").is_some() && url.starts_with("file:///") && !url.contains("::") {
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
        bail!("{url:?} is a local path; use `nyx plugin link PATH` for a folder on this computer");
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

fn git(dir: Option<&Path>, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("git");
    if let Some(d) = dir {
        cmd.arg("-C").arg(d);
    }
    let file = if cfg!(debug_assertions) && std::env::var_os("NYX_PLUGIN_TEST_ALLOW_FILE").is_some() { "always" } else { "never" };
    cmd.args(["-c", &format!("protocol.file.allow={file}"), "-c", "protocol.ext.allow=never", "-c", "advice.detachedHead=false"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", std::env::var("GIT_SSH_COMMAND").unwrap_or_else(|_| "ssh -oBatchMode=yes".into()));
    let out = cmd.output().context("running git")?;
    if !out.status.success() {
        bail!("git {}: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A plugin unpacked in a staging folder, checked and ready to move into place.
#[derive(Debug)]
pub struct Staged {
    pub manifest: Manifest,
    /// The package (what becomes the plugin's folder).
    pub dir: PathBuf,
    /// Everything to delete afterwards.
    work: PathBuf,
    pub origin: Origin,
    /// `needs` that aren't installed.
    pub missing: Vec<String>,
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.work);
    }
}

pub struct Source<'a> {
    pub url: &'a str,
    pub git_ref: Option<&'a str>,
    /// From the index: the commit must be exactly this one.
    pub pinned_commit: Option<&'a str>,
    pub subdir: Option<&'a str>,
    pub index_entry: Option<&'a Entry>,
}

impl<'a> Source<'a> {
    pub fn url(url: &'a str, git_ref: Option<&'a str>) -> Source<'a> {
        Source { url, git_ref, pinned_commit: None, subdir: None, index_entry: None }
    }

    pub fn index(entry: &'a Entry) -> Source<'a> {
        Source {
            url: &entry.repo,
            git_ref: entry.tag.as_deref(),
            pinned_commit: Some(&entry.commit),
            subdir: entry.subdir.as_deref(),
            index_entry: Some(entry),
        }
    }
}

/// Clone, check out, copy (refusing symlinks), read the manifest, fetch and check
/// artifacts. Nothing from the plugin runs.
pub fn stage(src: &Source) -> Result<Staged> {
    check_url(src.url)?;
    let root = paths::user_dir();
    std::fs::create_dir_all(&root)?;
    let work = root.join(format!(".add.tmp.{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    let mut staged = Staged {
        manifest: Manifest::default(),
        dir: work.join("pkg"),
        work: work.clone(),
        origin: Origin::default(),
        missing: vec![],
    };
    let checkout = work.join("src");
    let checkout_s = checkout.to_string_lossy().to_string();
    git(None, &["clone", "--quiet", "--", src.url, &checkout_s])?;
    if let Some(r) = src.git_ref {
        if r.starts_with('-') {
            bail!("{r:?} isn't a branch, tag or commit");
        }
        git(Some(&checkout), &["checkout", "--quiet", r, "--"])?;
    }
    if let Some(c) = src.pinned_commit {
        git(Some(&checkout), &["checkout", "--quiet", c, "--"])
            .with_context(|| format!("the index pins commit {c}, which the repo doesn't have"))?;
    }
    let commit = git(Some(&checkout), &["rev-parse", "HEAD"])?;
    if let Some(c) = src.pinned_commit
        && !commit.eq_ignore_ascii_case(c)
    {
        bail!("the repo is at {commit}, not the commit the index pins ({c})");
    }
    let pkg_src = match src.subdir {
        Some(s) if s.contains("..") || s.starts_with('/') => bail!("subdir {s:?}"),
        Some(s) => checkout.join(s),
        None => checkout.clone(),
    };
    copy_tree(&pkg_src, &staged.dir, false)?;
    staged.manifest = Manifest::load(&staged.dir)?;
    if let Some(e) = src.index_entry
        && e.id != staged.manifest.id
    {
        bail!("the index entry is {}, but the plugin says it's {}", e.id, staged.manifest.id);
    }
    let pinned = src.index_entry.and_then(|e| e.artifacts.get(&crate::manifest::platform()));
    fetch_artifact(&staged.manifest, &staged.dir, pinned)?;
    check_package(&staged.dir, &staged.manifest)?;
    staged.missing = staged.manifest.needs.iter().filter(|n| !discovery::on_path(n)).cloned().collect();
    staged.origin = Origin {
        source: src.url.to_string(),
        git_ref: src.git_ref.map(String::from),
        commit,
        installed: state::now_utc(),
        index: src.index_entry.map(|e| e.id.clone()),
    };
    Ok(staged)
}

/// Copy a folder, leaving out `.git`. Symlinks are refused (`follow = false`), or
/// copied as what they point to (`follow = true`, for cloning packaged plugins whose
/// files may be links into the Nix store).
pub fn copy_tree(from: &Path, to: &Path, follow: bool) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" || name == ORIGIN_FILE {
            continue;
        }
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)?;
        let target = to.join(&name);
        if meta.file_type().is_symlink() {
            if !follow {
                bail!("{} is a symbolic link; plugins can't contain links", path.display());
            }
            let real = std::fs::metadata(&path)?;
            if real.is_dir() {
                copy_tree(&path, &target, true)?;
            } else {
                std::fs::copy(&path, &target)?;
            }
        } else if meta.is_dir() {
            copy_tree(&path, &target, follow)?;
        } else if meta.is_file() {
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

/// Download this machine's artifact, check its hash (and the index's pin), refuse
/// archives with links or paths outside the folder, and unpack it into `dir`.
pub fn fetch_artifact(manifest: &Manifest, dir: &Path, pinned: Option<&Artifact>) -> Result<()> {
    let Some(a) = manifest.artifact() else { return Ok(()) };
    if let Some(p) = pinned
        && !p.sha256.eq_ignore_ascii_case(&a.sha256)
    {
        bail!("the artifact's hash ({}) isn't the one the index pins ({})", a.sha256, p.sha256);
    }
    let file = dir.join(".artifact.download");
    let out = Command::new("curl")
        .args(["-fsSL", "--proto", "=https", "--max-time", "300", "-o"])
        .arg(&file)
        .arg("--")
        .arg(&a.url)
        .output()
        .context("running curl")?;
    if !out.status.success() {
        bail!("downloading {}: {}", a.url, String::from_utf8_lossy(&out.stderr).trim());
    }
    unpack_checked(&file, &a.sha256, dir)
}

/// Check an archive's hash and contents, then unpack it.
pub fn unpack_checked(file: &Path, sha256: &str, dir: &Path) -> Result<()> {
    let got = sha256_file(file)?;
    if !got.eq_ignore_ascii_case(sha256) {
        let _ = std::fs::remove_file(file);
        bail!("the artifact's hash is {got}, not {sha256}");
    }
    let list = Command::new("tar").arg("-tvf").arg(file).output().context("running tar")?;
    if !list.status.success() {
        bail!("the artifact isn't an archive tar can read");
    }
    for line in String::from_utf8_lossy(&list.stdout).lines() {
        let kind = line.chars().next().unwrap_or('-');
        let name = line.split_whitespace().last().unwrap_or("");
        if !matches!(kind, '-' | 'd') {
            bail!("the artifact contains a link or device ({line})");
        }
        if name.starts_with('/') || name.split('/').any(|p| p == "..") {
            bail!("the artifact has a path outside its folder ({name})");
        }
    }
    let out = Command::new("tar")
        .arg("-xf")
        .arg(file)
        .args(["--no-same-owner", "--no-same-permissions", "-C"])
        .arg(dir)
        .output()
        .context("running tar")?;
    let _ = std::fs::remove_file(file);
    if !out.status.success() {
        bail!("unpacking the artifact: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// The program each kind runs exists: a file in the folder (executable, or run by an
/// interpreter), or a command on PATH.
pub fn check_package(dir: &Path, manifest: &Manifest) -> Result<()> {
    for kind in manifest.known_kinds() {
        let exec = manifest.exec_for(kind);
        let first = &exec[0];
        let local = dir.join(first);
        if first.contains('/') && !first.starts_with("./") && !local.is_file() {
            bail!("{}: {} isn't in the plugin's folder", kind.name(), first);
        }
        if local.is_file() {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::metadata(&local)?.permissions().mode() & 0o111 == 0 {
                bail!("{}: {} isn't executable (or run it with an interpreter: exec = [\"sh\", \"{first}\"])", kind.name(), first);
            }
        } else if !discovery::on_path(first) && !manifest.needs.contains(first) {
            bail!("{}: {} is neither in the plugin's folder nor installed", kind.name(), first);
        }
        for arg in &exec[1..] {
            if arg.starts_with('/') || arg.contains("..") {
                bail!("{}: exec argument {arg:?} points outside the plugin", kind.name());
            }
        }
    }
    Ok(())
}

fn user_target(id: &str) -> PathBuf {
    paths::user_dir().join(id)
}

/// Move a staged plugin into place. Refuses an id that's already in use.
pub fn install(staged: Staged) -> Result<PathBuf> {
    let id = staged.manifest.id.clone();
    let found = discovery::discover();
    if let Some(p) = found.get(&id) {
        bail!("{id} is already installed ({}, {}); update it with `nyx plugin update {id}`", p.source_label(), p.dir.display());
    }
    let target = user_target(&id);
    if std::fs::symlink_metadata(&target).is_ok() {
        bail!("{} already exists", target.display());
    }
    std::fs::rename(&staged.dir, &target)?;
    staged.origin.save(&target)?;
    std::fs::create_dir_all(paths::storage_dir(&id))?;
    Ok(target)
}

fn rollback_dir(id: &str) -> PathBuf {
    paths::user_dir().join(format!(".rollback-{id}"))
}

/// Replace an installed plugin with a staged copy, keeping the old one for
/// [`rollback`].
pub fn replace(plugin: &Plugin, staged: Staged) -> Result<()> {
    if plugin.trust.is_trusted() || plugin.linked {
        bail!("{} isn't a git plugin; there's nothing to update from", plugin.id());
    }
    if staged.manifest.id != plugin.manifest.id {
        bail!("the update says it's {}, not {}", staged.manifest.id, plugin.manifest.id);
    }
    let old = rollback_dir(plugin.id());
    let _ = std::fs::remove_dir_all(&old);
    std::fs::rename(&plugin.dir, &old)?;
    if let Err(e) = std::fs::rename(&staged.dir, &plugin.dir) {
        let _ = std::fs::rename(&old, &plugin.dir);
        return Err(e.into());
    }
    staged.origin.save(&plugin.dir)?;
    Ok(())
}

/// Put back the version before the last update.
pub fn rollback(id: &str) -> Result<()> {
    let old = rollback_dir(id);
    if !old.is_dir() {
        bail!("there's no earlier version of {id} to go back to");
    }
    let current = user_target(id);
    let swap = paths::user_dir().join(format!(".swap-{id}"));
    let _ = std::fs::remove_dir_all(&swap);
    if current.exists() {
        std::fs::rename(&current, &swap)?;
    }
    std::fs::rename(&old, &current)?;
    if swap.exists() {
        std::fs::rename(&swap, &old)?;
    }
    Ok(())
}

/// A unified diff between two folders (empty when they're the same).
pub fn diff(old: &Path, new: &Path) -> String {
    let out = Command::new("git")
        .args(["diff", "--no-index", "--no-color", "--"])
        .arg(old)
        .arg(new)
        .output();
    out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

pub enum Removed {
    Unlinked,
    Deleted,
    BackedUp(PathBuf),
}

/// Remove a user plugin: unlink a linked one, delete a git one (and its data), and
/// move a local folder aside rather than delete someone's work.
pub fn remove(plugin: &Plugin) -> Result<Removed> {
    if plugin.trust.is_trusted() {
        bail!("{} is {}; it can be turned off (`nyx plugin disable {}`) but not removed", plugin.id(), plugin.trust.label(), plugin.id());
    }
    if plugin.linked {
        std::fs::remove_file(&plugin.dir)?;
        return Ok(Removed::Unlinked);
    }
    if plugin.origin.is_some() {
        std::fs::remove_dir_all(&plugin.dir)?;
        let _ = std::fs::remove_dir_all(paths::storage_dir(plugin.id()));
        let _ = std::fs::remove_dir_all(rollback_dir(plugin.id()));
        return Ok(Removed::Deleted);
    }
    let stamp = state::now_utc().replace([':', '-'], "");
    let backup = paths::user_dir().join(format!(".{}.bak.{stamp}", plugin.id()));
    std::fs::rename(&plugin.dir, &backup)?;
    Ok(Removed::BackedUp(backup))
}

/// Link a working folder in, for developing a plugin. It's third-party like any other.
pub fn link(path: &Path) -> Result<Manifest> {
    let path = path.canonicalize().with_context(|| format!("{}", path.display()))?;
    let manifest = Manifest::load(&path)?;
    let target = user_target(&manifest.id);
    if std::fs::symlink_metadata(&target).is_ok() {
        bail!("{} is already installed at {}", manifest.id, target.display());
    }
    std::fs::create_dir_all(paths::user_dir())?;
    std::os::unix::fs::symlink(&path, &target)?;
    std::fs::create_dir_all(paths::storage_dir(&manifest.id))?;
    Ok(manifest)
}

/// Copy a packaged or first-party plugin to the user's folder to change it. The same
/// id overrides the original; a new id keeps both.
pub fn clone_plugin(plugin: &Plugin, new_id: Option<&str>) -> Result<PathBuf> {
    if plugin.trust == Trust::ThirdParty {
        bail!("{} is already yours to change, in {}", plugin.id(), plugin.dir.display());
    }
    let id = new_id.unwrap_or(plugin.id());
    if !crate::manifest::valid_id(id) {
        bail!("{id:?} isn't a valid plugin id");
    }
    let target = user_target(id);
    if std::fs::symlink_metadata(&target).is_ok() {
        bail!("{} already exists", target.display());
    }
    copy_tree(&plugin.dir, &target, true)?;
    if new_id.is_some() {
        let file = target.join(crate::manifest::FILE);
        let mut doc: toml_edit::DocumentMut = std::fs::read_to_string(&file)?.parse()?;
        doc["id"] = toml_edit::value(id);
        std::fs::write(&file, doc.to_string())?;
    }
    // Copied from the store, the files are read-only.
    make_writable(&target)?;
    std::fs::create_dir_all(paths::storage_dir(id))?;
    Ok(target)
}

fn make_writable(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let meta = std::fs::metadata(&path)?;
        let mut perm = meta.permissions();
        perm.set_mode(perm.mode() | 0o200);
        std::fs::set_permissions(&path, perm)?;
        if meta.is_dir() {
            make_writable(&path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        for ok in [
            "https://github.com/acme/nyx-ping.git",
            "ssh://git@github.com/acme/nyx-ping.git",
            "git@github.com:acme/nyx-ping.git",
        ] {
            check_url(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for bad in [
            "-oProxyCommand=touch /tmp/pwned",
            "ext::sh -c touch% /tmp/pwned",
            "file:///home/ken/plugin",
            "/home/ken/plugin",
            "./plugin",
            "https://",
            "http://example.com/p.git",
            "https://-oops/x",
            "git@host:/abs",
            "fd::3",
        ] {
            assert!(check_url(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn copying_refuses_links() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        std::fs::create_dir_all(src.join(".git")).unwrap();
        std::fs::write(src.join("a"), "1").unwrap();
        copy_tree(&src, &t.path().join("out"), false).unwrap();
        assert!(t.path().join("out/a").is_file());
        assert!(!t.path().join("out/.git").exists());
        std::os::unix::fs::symlink("/etc/passwd", src.join("link")).unwrap();
        assert!(copy_tree(&src, &t.path().join("out2"), false).is_err());
        copy_tree(&src, &t.path().join("out3"), true).unwrap();
        assert!(t.path().join("out3/link").is_file());
    }

    #[test]
    fn archives_are_checked() {
        let t = tempfile::tempdir().unwrap();
        let payload = t.path().join("payload");
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::write(payload.join("bin"), "x").unwrap();
        let tarball = t.path().join("a.tar");
        assert!(Command::new("tar").arg("-cf").arg(&tarball).arg("-C").arg(&payload).arg("bin").status().unwrap().success());
        let hash = sha256_file(&tarball).unwrap();
        let out = t.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        assert!(unpack_checked(&tarball, &"0".repeat(64), &out).is_err());
        assert!(Command::new("tar").arg("-cf").arg(&tarball).arg("-C").arg(&payload).arg("bin").status().unwrap().success());
        unpack_checked(&tarball, &hash, &out).unwrap();
        assert!(out.join("bin").is_file());

        std::os::unix::fs::symlink("/etc/passwd", payload.join("evil")).unwrap();
        let bad = t.path().join("b.tar");
        assert!(Command::new("tar").arg("-cf").arg(&bad).arg("-C").arg(&payload).arg("evil").status().unwrap().success());
        let h = sha256_file(&bad).unwrap();
        assert!(unpack_checked(&bad, &h, &out).is_err());
    }
}
