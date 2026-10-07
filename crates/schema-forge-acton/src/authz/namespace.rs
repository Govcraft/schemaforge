//! String constants for the Cedar entity-type and action namespaces SchemaForge owns.
//!
//! All built-in Cedar types live under the `Forge::` namespace so user-defined
//! schemas with names like `User`, `Group`, or `Schema` cannot collide with
//! SchemaForge's own types. Application schemas appear as bare entity types
//! at the top level (e.g., `Contact`, `Order`).

/// Cedar namespace owning every SchemaForge-built-in entity type and action.
pub const FORGE_NAMESPACE: &str = "Forge";

/// Cedar entity type for the authenticated user (the principal).
///
/// Renders as `Forge::Principal` in Cedar source.
pub const PRINCIPAL_TYPE: &str = "Forge::Principal";

/// Cedar entity type for a role-membership group.
///
/// Renders as `Forge::Group` in Cedar source.
pub const GROUP_TYPE: &str = "Forge::Group";

/// Cedar entity type representing a multi-tenancy scope.
///
/// Renders as `Forge::Tenant` in Cedar source.
pub const TENANT_TYPE: &str = "Forge::Tenant";

/// Cedar entity type representing a SchemaForge schema definition.
///
/// Used by `Forge::Action::"UpdateSchema"` and friends. Renders as
/// `Forge::Schema` in Cedar source.
pub const SCHEMA_TYPE: &str = "Forge::Schema";

/// Cedar action UID prefix.
///
/// Actions live at the top level rather than inside the `Forge::` namespace —
/// this matches Cedar's conventional idioms and avoids cross-namespace
/// reference quirks for the per-action `appliesTo` declarations.
pub const ACTION_PREFIX: &str = "Action";

/// Action verbs recognised by the policy generator for entity CRUD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionVerb {
    /// Read a single entity by id.
    Read,
    /// List or query multiple entities of a schema.
    List,
    /// Create a new entity.
    Create,
    /// Invite a user without granting direct account creation.
    Invite,
    /// List pending invitations independently of User reads.
    ListInvites,
    /// Revoke a pending invitation independently of User deletion.
    RevokeInvite,
    /// Update an existing entity.
    Update,
    /// Delete an existing entity.
    Delete,
    /// Bulk-export many entities of a schema to a file.
    ///
    /// Deliberately distinct from [`ActionVerb::Read`]: a policy can grant
    /// reading one record at a time while forbidding draining the whole
    /// table to a downloadable file. See ADR-0003.
    Export,
}

impl ActionVerb {
    /// Returns the verb portion of the action name (before the schema name).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "Read",
            Self::List => "List",
            Self::Create => "Create",
            Self::Invite => "Invite",
            Self::ListInvites => "ListInvites",
            Self::RevokeInvite => "RevokeInvite",
            Self::Update => "Update",
            Self::Delete => "Delete",
            Self::Export => "Export",
        }
    }
}

/// Builds the fully-qualified action UID string for `verb` on `schema_name`.
///
/// e.g., `action_uid(ActionVerb::Read, "Contact")` = `Forge::Action::"ReadContact"`.
pub fn action_uid(verb: ActionVerb, schema_name: &str) -> String {
    format!("{ACTION_PREFIX}::\"{}\"", action_name(verb, schema_name))
}

/// Unquoted action name shared by policy UIDs and schema declarations.
/// A colon distinguishes application Invites listing from private management.
pub(crate) fn action_name(verb: ActionVerb, schema_name: &str) -> String {
    match verb {
        ActionVerb::ListInvites | ActionVerb::RevokeInvite => verb.as_str().into(),
        ActionVerb::List if schema_name == "Invites" => "List:Invites".into(),
        _ => format!("{}{}", verb.as_str(), schema_name),
    }
}

/// Builds the fully-qualified action UID for the per-field read action.
pub fn field_read_action_uid(schema_name: &str, field_name: &str) -> String {
    format!(
        "{ACTION_PREFIX}::\"ReadField{}_{}\"",
        schema_name, field_name
    )
}

/// Builds the fully-qualified action UID for the per-field write action.
pub fn field_write_action_uid(schema_name: &str, field_name: &str) -> String {
    format!(
        "{ACTION_PREFIX}::\"WriteField{}_{}\"",
        schema_name, field_name
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_uid_renders_correctly() {
        assert_eq!(
            action_uid(ActionVerb::Read, "Contact"),
            "Action::\"ReadContact\""
        );
        assert_eq!(
            action_uid(ActionVerb::Create, "Order"),
            "Action::\"CreateOrder\""
        );
    }

    #[test]
    fn invitation_management_uses_exact_independent_action_names() {
        assert_eq!(
            action_uid(ActionVerb::List, "Invites"),
            "Action::\"List:Invites\""
        );
        assert_eq!(
            action_uid(ActionVerb::ListInvites, "User"),
            "Action::\"ListInvites\""
        );
        assert_eq!(
            action_uid(ActionVerb::RevokeInvite, "User"),
            "Action::\"RevokeInvite\""
        );
        assert_ne!(
            action_uid(ActionVerb::ListInvites, "User"),
            action_uid(ActionVerb::List, "User")
        );
        assert_ne!(
            action_uid(ActionVerb::RevokeInvite, "User"),
            action_uid(ActionVerb::Delete, "User")
        );
    }

    #[test]
    fn export_action_uid_is_distinct_from_read() {
        // The read-vs-export split (ADR-0003) hinges on these rendering to
        // different Cedar action UIDs so a policy can permit one and forbid
        // the other on the same schema.
        let read = action_uid(ActionVerb::Read, "Subject");
        let export = action_uid(ActionVerb::Export, "Subject");
        assert_eq!(export, "Action::\"ExportSubject\"");
        assert_ne!(read, export);
    }

    #[test]
    fn field_read_action_uid_uses_underscore_separator() {
        // Underscore separator avoids any potential ambiguity with Cedar's
        // `::` namespace separator inside the quoted action id.
        assert_eq!(
            field_read_action_uid("Employee", "salary"),
            "Action::\"ReadFieldEmployee_salary\""
        );
    }
}
