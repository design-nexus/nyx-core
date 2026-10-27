//! Moving an app's files from the names it had before: Nexus, then Nixe, now
//! Nyx. Each old directory is renamed to the new one and a symlink is left in
//! its place, so absolute paths saved inside libraries and databases still work.

use crate::{app, paths};
use std::path::{Path, PathBuf};

/// Move `~/.config/<old>`, `~/.local/share/<old>`, `~/.local/state/<old>` and
/// `~/.cache/<old>` to the running app's own names. Safe to call on every start.
pub fn from(old: &[&str]) {
    let new = app::slug();
    for base in [paths::config_home(), paths::data_home(), paths::state_home(), paths::cache_home()] {
        for name in old {
            if *name != new {
                move_dir(&base.join(name), &base.join(new));
            }
        }
    }
}

/// Move the running app's `nixe-<name>` directories, and the suite's own
/// `~/.config/nixe`, to their Nyx names. [`crate::app::init`] calls it.
pub fn from_nixe() {
    if let Some(name) = app::slug().strip_prefix("nyx-") {
        let old = format!("nixe-{name}");
        from(&[old.as_str()]);
    }
    move_dir(&paths::config_home().join("nixe"), &paths::suite_dir());
}

/// Move one directory and leave a symlink behind. Nothing happens when the old
/// one is missing or already a link, or when the new one already exists.
pub fn move_dir(old: &Path, new: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(old) else { return false };
    if !meta.is_dir() || new.exists() {
        return false;
    }
    if let Some(parent) = new.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::rename(old, new) {
        eprintln!("{}: couldn't move {} to {}: {e}", app::slug(), old.display(), new.display());
        return false;
    }
    let target: PathBuf = new.file_name().map(PathBuf::from).filter(|_| old.parent() == new.parent()).unwrap_or(new.to_path_buf());
    let _ = std::os::unix::fs::symlink(&target, old);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_and_links() {
        let root = std::env::temp_dir().join(format!("nyx-migrate-test-{}", std::process::id()));
        let old = root.join("nexus-x");
        std::fs::create_dir_all(old.join("sub")).unwrap();
        std::fs::write(old.join("sub/a.txt"), "hi").unwrap();
        let new = root.join("nyx-x");
        assert!(move_dir(&old, &new));
        assert_eq!(std::fs::read_to_string(new.join("sub/a.txt")).unwrap(), "hi");
        // The old path still reaches the files, through a relative link.
        assert_eq!(std::fs::read_link(&old).unwrap(), PathBuf::from("nyx-x"));
        assert_eq!(std::fs::read_to_string(old.join("sub/a.txt")).unwrap(), "hi");
        // A second run changes nothing.
        assert!(!move_dir(&old, &new));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn leaves_an_existing_new_dir_alone() {
        let root = std::env::temp_dir().join(format!("nyx-migrate-both-{}", std::process::id()));
        std::fs::create_dir_all(root.join("old")).unwrap();
        std::fs::create_dir_all(root.join("new")).unwrap();
        assert!(!move_dir(&root.join("old"), &root.join("new")));
        assert!(root.join("old").is_dir() && !root.join("old").is_symlink());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
