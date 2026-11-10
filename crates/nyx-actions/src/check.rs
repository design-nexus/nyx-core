//! Whether a caller may run an action with these arguments (the contract's section 3).
//! The registry checks first so frontends can answer early; the app's control helper
//! checks again, and its answer is the one that counts.

use crate::error::{Error, ErrorKind};
use crate::spec::{Action, MAX_ARGUMENTS, Risk};
use serde_json::Value;

/// `trusted`: the caller runs a Nyx program. `confirmed`: a person agreed to this call
/// in the caller's own UI.
pub fn check_call(trusted: bool, action: &Action, arguments: &Value, confirmed: bool) -> Result<(), Error> {
    if !trusted && action.risk != Risk::ReadOnly {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            format!("{} {}; only Nyx programs may run it", action.id, action.risk.label()),
        ));
    }
    if action.asks() && !confirmed {
        return Err(Error::new(
            ErrorKind::ConfirmationRequired,
            format!("{} needs your go-ahead first ({})", action.id, action.risk.label()),
        ));
    }
    validate_arguments(action, arguments)
}

/// The arguments against the action's schema, naming the place that's wrong.
pub fn validate_arguments(action: &Action, arguments: &Value) -> Result<(), Error> {
    let invalid = |m: String| Error::new(ErrorKind::InvalidArguments, m);
    if !arguments.is_object() {
        return Err(invalid("the arguments must be a JSON object".into()));
    }
    if arguments.to_string().len() > MAX_ARGUMENTS {
        return Err(invalid("the arguments are too large".into()));
    }
    let validator = jsonschema::validator_for(&action.input_schema)
        .map_err(|e| Error::new(ErrorKind::Internal, format!("{}'s schema doesn't compile: {e}", action.id)))?;
    if let Err(e) = validator.validate(arguments) {
        let at = e.instance_path.to_string();
        return Err(invalid(if at.is_empty() { format!("arguments: {e}") } else { format!("arguments at {at}: {e}") }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{Confirmation, Description};
    use serde_json::json;

    fn action(risk: Risk, confirmation: Confirmation) -> Action {
        let tool = json!({"name": "notes.create", "description": "Create.", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"], "additionalProperties": false},
            "x-nyx": {"risk": risk, "confirmation": confirmation}});
        let d = Description::from_value(&json!({"tools": [tool], "nyx": {"api": 2, "app": "io.github.design_nexus.nyx.Notes", "name": "Notes", "namespaces": ["notes"], "bus_name": "io.github.design_nexus.nyx.Notes"}}), None).unwrap();
        d.actions[0].clone()
    }

    #[test]
    fn callers_risk_and_confirmation() {
        let ok = json!({"text": "milk"});
        let read = action(Risk::ReadOnly, Confirmation::Never);
        let write = action(Risk::Writes, Confirmation::Never);
        let asks = action(Risk::Writes, Confirmation::Ask);
        let delete = action(Risk::Destructive, Confirmation::Never);
        check_call(false, &read, &ok, false).unwrap();
        check_call(true, &write, &ok, false).unwrap();
        assert_eq!(check_call(false, &write, &ok, true).unwrap_err().kind, ErrorKind::PermissionDenied);
        assert_eq!(check_call(true, &asks, &ok, false).unwrap_err().kind, ErrorKind::ConfirmationRequired);
        check_call(true, &asks, &ok, true).unwrap();
        assert_eq!(check_call(true, &delete, &ok, false).unwrap_err().kind, ErrorKind::ConfirmationRequired);
        assert_eq!(check_call(false, &delete, &ok, true).unwrap_err().kind, ErrorKind::PermissionDenied);
    }

    #[test]
    fn arguments_are_checked_by_schema() {
        let read = action(Risk::ReadOnly, Confirmation::Never);
        let bad = check_call(true, &read, &json!({"text": 3}), false).unwrap_err();
        assert_eq!(bad.kind, ErrorKind::InvalidArguments);
        assert!(bad.message.contains("/text"), "{}", bad.message);
        assert!(check_call(true, &read, &json!({"text": "a", "extra": 1}), false).is_err());
        assert!(check_call(true, &read, &json!([]), false).is_err());
        assert!(check_call(true, &read, &json!({}), false).is_err());
    }
}
