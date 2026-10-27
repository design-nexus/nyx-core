//! Credentials from GOA, in-process (feature `credentials`).
//!
//! For a mail engine or other binary that has to sign in itself. A [`Secret`] is
//! used for one connection and dropped: never stored, logged, sent over a Nyx
//! bus name or put on a command line (docs/online-accounts/SECURITY.md).

use super::mail::{OAUTH2_IFACE, PASSWORD_IFACE};
use super::{Account, Client, Error};
use glib::prelude::ToVariant;
use std::fmt;

/// A token or password. Debug and Display are redacted; the memory is
/// overwritten when it is dropped.
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // SAFETY: zero bytes are valid UTF-8; the string is not used again.
        for byte in unsafe { self.0.as_bytes_mut() } {
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
    }
}

/// An OAuth2 access token and how long GOA says it stays valid.
#[derive(Debug)]
pub struct AccessToken {
    pub token: Secret,
    pub expires_in: i32,
}

impl Client {
    /// A fresh OAuth2 access token for an account (GOA refreshes it as needed).
    /// Fails with [`Error::NeedsSignIn`] when the user has to sign in again.
    pub fn access_token(&self, account: &Account) -> Result<AccessToken, Error> {
        let reply = self.call(&account.path, OAUTH2_IFACE, "GetAccessToken", None, Some("(si)")).map_err(sign_in)?;
        let token = reply.try_child_value(0).and_then(|v| v.get::<String>()).unwrap_or_default();
        let expires_in = reply.try_child_value(1).and_then(|v| v.get::<i32>()).unwrap_or(0);
        if token.is_empty() {
            return Err(Error::NeedsSignIn);
        }
        Ok(AccessToken { token: Secret(token), expires_in })
    }

    /// A password GOA keeps for an account (`imap-password`, `smtp-password`, ...).
    pub fn password(&self, account: &Account, id: &str) -> Result<Secret, Error> {
        let reply = self
            .call(&account.path, PASSWORD_IFACE, "GetPassword", Some(&(id,).to_variant()), Some("(s)"))
            .map_err(sign_in)?;
        let password = reply.try_child_value(0).and_then(|v| v.get::<String>()).unwrap_or_default();
        if password.is_empty() {
            return Err(Error::NeedsSignIn);
        }
        Ok(Secret(password))
    }
}

/// GOA reports a refused or missing credential as NotAuthorized.
fn sign_in(err: Error) -> Error {
    match err {
        Error::Other(message) if message.contains("NotAuthorized") || message.to_lowercase().contains("credentials") => {
            Error::NeedsSignIn
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_is_redacted() {
        let secret = Secret("ya29.token".into());
        assert_eq!(format!("{secret:?}"), "Secret([redacted])");
        assert_eq!(secret.expose(), "ya29.token");
        let token = AccessToken { token: Secret("abc".into()), expires_in: 3600 };
        assert!(!format!("{token:?}").contains("abc"));
    }

    #[test]
    fn not_authorized_means_sign_in() {
        let err = Error::Other("GDBus.Error:org.gnome.OnlineAccounts.Error.NotAuthorized: no credentials".into());
        assert_eq!(sign_in(err), Error::NeedsSignIn);
        assert_eq!(sign_in(Error::NotResponding), Error::NotResponding);
    }
}
