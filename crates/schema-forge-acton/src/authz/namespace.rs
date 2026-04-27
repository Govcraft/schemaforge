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

/// Cedar action namespace prefix for SchemaForge built-in actions.
pub const ACTION_PREFIX: &str = "Forge::Action";

/// Action verbs recognised by the policy generator for entity CRUD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionVerb {
    /// Read a single entity by id.
    Read,
    /// List or query multiple entities of a schema.
    List,
    /// Create a new entity.
    Create,
    /// Update an existing entity.
    Update,
    /// Delete an existing entity.
    Delete,
}

impl ActionVerb {
    /// Returns the verb portion of the action name (before the schema name).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "Read",
            Self::List => "List",
            Self::Create => "Create",
            Self::Update => "Update",
            Self::Delete => "Delete",
        }
    }
}

/// Builds the fully-qualified action UID string for `verb` on `schema_name`.
///
/// e.g., `action_uid(ActionVerb::Read, "Contact")` = `Forge::Action::"ReadContact"`.
pub fn action_uid(verb: ActionVerb, schema_name: &str) -> String {
    format!("{ACTION_PREFIX}::\"{}{}\"", verb.as_str(), schema_name)
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
            "Forge::Action::\"ReadContact\""
        );
        assert_eq!(
            action_uid(ActionVerb::Create, "Order"),
            "Forge::Action::\"CreateOrder\""
        );
    }

    #[test]
    fn field_read_action_uid_uses_underscore_separator() {
        // Underscore separator avoids any potential ambiguity with Cedar's
        // `::` namespace separator inside the quoted action id.
        assert_eq!(
            field_read_action_uid("Employee", "salary"),
            "Forge::Action::\"ReadFieldEmployee_salary\""
        );
    }
}
