//! Desktop Secret Service via `secret-tool`. The secret never implements a
//! readable [`Debug`], and these helpers do not write it to a file.

use std::fmt;
use std::io::Write;
use std::process::{Command, Stdio};

/// A token or app password. Debug output is redacted.
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> anyhow::Result<Self> {
        let value = value.into();
        anyhow::ensure!(
            !value.is_empty() && !value.chars().any(char::is_control),
            "the secret is empty or has control characters"
        );
        Ok(Self(value))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

/// `secret-tool` arguments for one Nyx account. The secret itself is written
/// to the child's stdin by the caller, not placed in the argument list.
pub fn secret_command(verb: &str, application: &str, account: &str, kind: &str) -> Command {
    let mut command = Command::new("secret-tool");
    command.arg(verb);
    if verb == "store" {
        command.arg(format!("--label={application} {kind}"));
    }
    command.args(["application", application, "account", account, "kind", kind]);
    command
}

/// Save a secret in the desktop keyring. The value is written to stdin.
pub fn store_secret(application: &str, account: &str, kind: &str, secret: &Secret) -> anyhow::Result<()> {
    let mut child = secret_command("store", application, account, kind)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(secret.expose().as_bytes())?;
    let out = child.wait_with_output()?;
    anyhow::ensure!(out.status.success(), "the keyring did not store the password");
    Ok(())
}

/// Read a secret back. A missing item is an error, not an empty password.
/// An item saved before the suite was called Nyx (`nixe-<app>`) is copied to
/// the new name on first use; the old one is cleared once the copy reads back.
pub fn lookup_secret(application: &str, account: &str, kind: &str) -> anyhow::Result<Secret> {
    match lookup(application, account, kind) {
        Ok(secret) => Ok(secret),
        Err(e) => {
            let Some(old) = nixe_name(application) else { return Err(e) };
            let secret = lookup(&old, account, kind).map_err(|_| e)?;
            if store_secret(application, account, kind, &secret).is_ok()
                && lookup(application, account, kind).is_ok_and(|copy| copy.expose() == secret.expose())
            {
                let _ = secret_command("clear", &old, account, kind).output();
            }
            Ok(secret)
        }
    }
}

fn lookup(application: &str, account: &str, kind: &str) -> anyhow::Result<Secret> {
    let out = secret_command("lookup", application, account, kind).output()?;
    anyhow::ensure!(out.status.success(), "no app password is saved for this account");
    let text = String::from_utf8(out.stdout)?;
    Secret::new(text.trim())
}

/// The application attribute an item had before the rename, if it changed.
fn nixe_name(application: &str) -> Option<String> {
    application.strip_prefix("nyx-").map(|name| format!("nixe-{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_names() {
        assert_eq!(nixe_name("nyx-news").as_deref(), Some("nixe-news"));
        assert_eq!(nixe_name("other"), None);
    }

    #[test]
    fn debug_hides_the_secret() {
        let secret = Secret::new("app-password-value").unwrap();
        let shown = format!("{secret:?}");
        assert_eq!(shown, "Secret([redacted])");
        assert!(!shown.contains("app-password"));
    }

    #[test]
    fn empty_and_control_characters_are_rejected() {
        assert!(Secret::new("").is_err());
        assert!(Secret::new("line\nbreak").is_err());
    }

    #[test]
    fn the_command_names_the_account_and_not_the_secret() {
        let command = secret_command("lookup", "nyx-news", "account-1", "nextcloud");
        let args: Vec<String> = command.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(command.get_program(), "secret-tool");
        assert_eq!(args, ["lookup", "application", "nyx-news", "account", "account-1", "kind", "nextcloud"]);
    }
}
