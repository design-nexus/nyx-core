//! Typed errors (the contract's section 12), shared by every frontend.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// The arguments don't match the schema, or the words don't resolve to arguments.
    InvalidArguments,
    /// No app owns the namespace.
    UnknownApp,
    /// The app has no such action.
    UnknownAction,
    /// Something the action needs isn't installed.
    NotInstalled,
    /// The app only answers while it runs, and it's closed.
    NotRunning,
    /// Listed, but unusable now.
    Unavailable,
    /// The caller may not run it, or the app refused on permission grounds.
    PermissionDenied,
    /// It asks first and wasn't confirmed.
    ConfirmationRequired,
    /// The app ran it and it failed.
    Failed,
    Timeout,
    Cancelled,
    Internal,
}

impl ErrorKind {
    /// The `nyx` command's exit status for this error.
    pub fn exit_code(self) -> i32 {
        match self {
            ErrorKind::Failed => 1,
            ErrorKind::InvalidArguments => 2,
            ErrorKind::UnknownApp | ErrorKind::UnknownAction => 3,
            ErrorKind::NotInstalled | ErrorKind::NotRunning | ErrorKind::Unavailable => 4,
            ErrorKind::PermissionDenied => 5,
            ErrorKind::ConfirmationRequired => 6,
            ErrorKind::Internal => 70,
            ErrorKind::Timeout => 124,
            ErrorKind::Cancelled => 130,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::InvalidArguments => "invalid_arguments",
            ErrorKind::UnknownApp => "unknown_app",
            ErrorKind::UnknownAction => "unknown_action",
            ErrorKind::NotInstalled => "not_installed",
            ErrorKind::NotRunning => "not_running",
            ErrorKind::Unavailable => "unavailable",
            ErrorKind::PermissionDenied => "permission_denied",
            ErrorKind::ConfirmationRequired => "confirmation_required",
            ErrorKind::Failed => "failed",
            ErrorKind::Timeout => "timeout",
            ErrorKind::Cancelled => "cancelled",
            ErrorKind::Internal => "internal",
        }
    }

    /// A kind by its wire name; anything unknown is `failed` (an app's own word).
    pub fn parse(name: &str) -> ErrorKind {
        serde_json::from_value(serde_json::Value::String(name.to_string())).unwrap_or(ErrorKind::Failed)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Error { kind, message: message.into() }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip_and_unknown_ones_fail() {
        assert_eq!(ErrorKind::parse("confirmation_required"), ErrorKind::ConfirmationRequired);
        assert_eq!(ErrorKind::parse(ErrorKind::NotRunning.as_str()), ErrorKind::NotRunning);
        assert_eq!(ErrorKind::parse("vault_locked"), ErrorKind::Failed);
        assert_eq!(ErrorKind::ConfirmationRequired.exit_code(), 6);
    }
}
