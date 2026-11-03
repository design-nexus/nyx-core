//! Which Nyx program a local caller runs, for services that decide what a caller may
//! do by its executable (Clipboard, Accounts).
//!
//! A program is trusted when its executable sits in the service's own folder (all of
//! the suite in `~/.local/bin` on Omarchy) or in a folder the system lists in
//! [`TRUSTED_DIRS_FILE`]. NixOS installs each app in its own store folder and writes
//! that list. Wrappers built by Nix run the real binary as `.NAME-wrapped`; such a
//! binary counts as `NAME`.
//!
//! Callers of the same user who run one of these programs themselves are beyond what a
//! same-user check can stop; this only keeps other programs out.

use std::path::{Path, PathBuf};

/// Folders of trusted programs, one absolute path per line. Read only when root owns it
/// and nobody else can write it.
pub const TRUSTED_DIRS_FILE: &str = "/etc/nyx/trusted-bin";

/// The program name `exe` runs as, if it's one of the suite's: in `own_dir` or a
/// folder listed in [`TRUSTED_DIRS_FILE`].
pub fn program(exe: &Path, own_dir: &Path) -> Option<String> {
    program_in(exe, own_dir, &trusted_dirs(Path::new(TRUSTED_DIRS_FILE)))
}

/// [`program`] with the system's folders given.
pub fn program_in(exe: &Path, own_dir: &Path, trusted: &[PathBuf]) -> Option<String> {
    let dir = exe.parent()?;
    if dir != own_dir && !trusted.iter().any(|t| t == dir) {
        return None;
    }
    unwrapped(exe.file_name()?.to_str()?)
}

/// `NAME` for Nix's `.NAME-wrapped` (wrapped again: `..NAME-wrapped-wrapped`). A binary
/// replaced while running reads as "… (deleted)" and is refused.
fn unwrapped(mut name: &str) -> Option<String> {
    if name.is_empty() || name.ends_with(" (deleted)") {
        return None;
    }
    while let Some(inner) = name.strip_prefix('.').and_then(|n| n.strip_suffix("-wrapped")) {
        name = inner;
    }
    (!name.is_empty() && !name.starts_with('.')).then(|| name.to_string())
}

/// The folders listed in `file`, if root owns it and only root can write it.
pub fn trusted_dirs(file: &Path) -> Vec<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::metadata(file) else {
        return Vec::new();
    };
    if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        return Vec::new();
    }
    std::fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('/'))
        .map(PathBuf::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_folder_and_listed_folders() {
        let own = Path::new("/home/u/.local/bin");
        let store = PathBuf::from("/nix/store/abc-nyx-glimpse/bin");
        assert_eq!(program_in(&own.join("nyx-clipboard"), own, &[]).as_deref(), Some("nyx-clipboard"));
        assert_eq!(program_in(&store.join("nyx-glimpse"), own, &[]), None);
        assert_eq!(program_in(&store.join("nyx-glimpse"), own, std::slice::from_ref(&store)).as_deref(), Some("nyx-glimpse"));
        assert_eq!(program_in(Path::new("/tmp/nyx-clipboard"), own, std::slice::from_ref(&store)), None);
    }

    #[test]
    fn nix_wrappers_count_as_the_program() {
        let own = Path::new("/nix/store/abc-nyx-clipboard/bin");
        assert_eq!(program_in(&own.join(".nyx-clipboard-wrapped"), own, &[]).as_deref(), Some("nyx-clipboard"));
        assert_eq!(program_in(&own.join("..nyx-clipboard-wrapped-wrapped"), own, &[]).as_deref(), Some("nyx-clipboard"));
        assert_eq!(program_in(&own.join(".nyx-clipboard"), own, &[]), None);
        assert_eq!(program_in(&own.join(".-wrapped"), own, &[]), None);
        assert_eq!(program_in(&own.join("nyx-clipboard (deleted)"), own, &[]), None);
    }

    #[test]
    fn list_must_be_root_owned() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("trusted-bin");
        std::fs::write(&file, "/nix/store/abc-nyx-glimpse/bin\nrelative/bin\n").unwrap();
        // The test runs as an ordinary user, so its file is never trusted.
        assert!(trusted_dirs(&file).is_empty());
        assert!(trusted_dirs(&dir.path().join("missing")).is_empty());
    }
}
