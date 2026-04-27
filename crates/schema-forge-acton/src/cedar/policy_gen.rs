//! Cedar policy source generator.
//!
//! Pure functions that turn SchemaForge schema definitions into Cedar policy
//! source. The output is concatenated with any operator-supplied custom
//! policies, parsed into a `cedar_policy::PolicySet`, and validated in strict
//! mode against the schema produced by [`super::schema_gen`].
//!
//! Three categories of policy come out of this module:
//!
//! 1. **Global policies** — apply to every action on every resource. Includes
//!    the `platform_admin` always-permit rule and the User-management role-rank
//!    forbid rule. Emitted exactly once per policy set.
//! 2. **Per-schema policies** — drive the `@access` / `@owner` annotation
//!    contract. Emitted once per registered schema.
//! 3. **Per-field policies** — drive the `@field_access` annotation. Emitted
//!    only for fields that carry the annotation.
//!
//! All identifiers reference the `Forge::` namespace declared by the
//! schema generator, so generated policies always parse and validate.

use schema_forge_core::types::{Annotation, FieldAnnotation, FieldDefinition, SchemaDefinition};

/// A generated Cedar policy template carrying a description for audit logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CedarPolicy {
    /// Human-readable description of what this policy allows or denies.
    pub description: String,
    /// Cedar policy source text.
    pub cedar_text: String,
}

/// Generates the full policy set (global + per-schema + per-field) for a
/// slice of schema definitions. Used by `PolicyStore::compile` at boot and
/// on every schema-apply.
pub fn generate_full_policy_set(schemas: &[SchemaDefinition]) -> Vec<CedarPolicy> {
    let mut out = generate_global_policies();
    for schema in schemas {
        out.extend(generate_cedar_policies(schema));
    }
    out
}

/// Generates the cross-schema "global" policies installed exactly once.
pub fn generate_global_policies() -> Vec<CedarPolicy> {
    vec![
        platform_admin_permit_policy(),
        user_management_role_rank_forbid_policy(),
    ]
}

/// Generates the per-schema policy set.
///
/// When the schema has an `@access` annotation, role-based policies derive
/// from its role lists. Otherwise the default secure set applies:
///
/// 1. Read access for any authenticated user
/// 2. Owner-only create/update (if the schema has any field with `@owner`),
///    falling back to a `platform_admin`-only baseline otherwise
/// 3. Delete for `platform_admin` only
/// 4. Schema modification for `Forge::Group::"schema-admin"`
///
/// Per-field `@field_access` policies are appended at the end.
pub fn generate_cedar_policies(schema: &SchemaDefinition) -> Vec<CedarPolicy> {
    let name = schema.name.as_str();

    let mut policies = if let Some(Annotation::Access {
        read,
        write,
        delete,
        ..
    }) = schema.access_annotation()
    {
        generate_annotation_policies(name, read, write, delete)
    } else {
        vec![
            default_read_policy(name),
            default_write_policy(schema),
            default_delete_policy(name),
        ]
    };
    policies.push(schema_admin_policy(name));
    policies.extend(generate_field_access_policies(schema));
    policies
}

// ---------------------------------------------------------------------------
// Global policies
// ---------------------------------------------------------------------------

fn platform_admin_permit_policy() -> CedarPolicy {
    CedarPolicy {
        description: "Members of Forge::Group::\"platform_admin\" are permitted any action on any resource".to_string(),
        cedar_text: r#"@id("forge.global.platform_admin_permit")
permit (
    principal,
    action,
    resource
) when {
    principal in Forge::Group::"platform_admin"
};"#
            .to_string(),
    }
}

fn user_management_role_rank_forbid_policy() -> CedarPolicy {
    CedarPolicy {
        description:
            "Forbid User-management actions when the caller's role_rank is below the target's"
                .to_string(),
        cedar_text: r#"@id("forge.global.user_role_rank_guard")
forbid (
    principal,
    action in [
        Action::"ReadUser",
        Action::"ListUser",
        Action::"CreateUser",
        Action::"UpdateUser",
        Action::"DeleteUser"
    ],
    resource is User
) unless {
    principal.role_rank >= resource.role_rank
};"#
            .to_string(),
    }
}

// ---------------------------------------------------------------------------
// Default per-schema policies (no @access annotation)
// ---------------------------------------------------------------------------

fn default_read_policy(name: &str) -> CedarPolicy {
    CedarPolicy {
        description: format!("Allow any authenticated user to read or list {name} entities"),
        cedar_text: format!(
            r#"@id("forge.{lname}.read_default")
permit (
    principal is Forge::Principal,
    action in [Action::"Read{name}", Action::"List{name}"],
    resource is {name}
);"#,
            lname = name.to_ascii_lowercase()
        ),
    }
}

fn default_write_policy(schema: &SchemaDefinition) -> CedarPolicy {
    let name = schema.name.as_str();
    let owner_field = find_owner_field(schema);

    if let Some(field) = owner_field {
        CedarPolicy {
            description: format!(
                "Allow the owner ({field}) to create or update {name} entities"
            ),
            cedar_text: format!(
                r#"@id("forge.{lname}.owner_write")
permit (
    principal is Forge::Principal,
    action in [Action::"Create{name}", Action::"Update{name}"],
    resource is {name}
) when {{
    resource has "{field}" && resource["{field}"] == principal.id
}};"#,
                lname = name.to_ascii_lowercase()
            ),
        }
    } else {
        CedarPolicy {
            description: format!(
                "Default-deny baseline for create/update on {name} (no @owner declared)"
            ),
            cedar_text: format!(
                r#"@id("forge.{lname}.default_write_baseline")
permit (
    principal in Forge::Group::"platform_admin",
    action in [Action::"Create{name}", Action::"Update{name}"],
    resource is {name}
);"#,
                lname = name.to_ascii_lowercase()
            ),
        }
    }
}

fn default_delete_policy(name: &str) -> CedarPolicy {
    CedarPolicy {
        description: format!("Allow platform_admin to delete {name} entities"),
        cedar_text: format!(
            r#"@id("forge.{lname}.delete_default")
permit (
    principal in Forge::Group::"platform_admin",
    action == Action::"Delete{name}",
    resource is {name}
);"#,
            lname = name.to_ascii_lowercase()
        ),
    }
}

fn schema_admin_policy(name: &str) -> CedarPolicy {
    CedarPolicy {
        description: format!("Allow Forge::Group::\"schema-admin\" to modify the {name} schema"),
        cedar_text: format!(
            r#"@id("forge.{lname}.schema_admin")
permit (
    principal in Forge::Group::"schema-admin",
    action in [Action::"UpdateSchema", Action::"DeleteSchema"],
    resource == Forge::Schema::"{name}"
);"#,
            lname = name.to_ascii_lowercase()
        ),
    }
}

// ---------------------------------------------------------------------------
// Annotation-driven per-schema policies (@access on schema)
// ---------------------------------------------------------------------------

fn generate_annotation_policies(
    name: &str,
    read_roles: &[String],
    write_roles: &[String],
    delete_roles: &[String],
) -> Vec<CedarPolicy> {
    let mut policies = Vec::new();
    push_role_policies(
        &mut policies,
        name,
        "read",
        &[
            format!("Action::\"Read{name}\""),
            format!("Action::\"List{name}\""),
        ],
        read_roles,
    );
    push_role_policies(
        &mut policies,
        name,
        "write",
        &[
            format!("Action::\"Create{name}\""),
            format!("Action::\"Update{name}\""),
        ],
        write_roles,
    );
    push_role_policies(
        &mut policies,
        name,
        "delete",
        &[format!("Action::\"Delete{name}\"")],
        delete_roles,
    );
    policies
}

fn push_role_policies(
    out: &mut Vec<CedarPolicy>,
    schema_name: &str,
    label: &str,
    actions: &[String],
    roles: &[String],
) {
    let lname = schema_name.to_ascii_lowercase();
    let actions_clause = format_actions_clause(actions);

    if roles.is_empty() {
        out.push(CedarPolicy {
            description: format!(
                "Allow any authenticated user to {label} {schema_name} entities"
            ),
            cedar_text: format!(
                r#"@id("forge.{lname}.{label}_authenticated")
permit (
    principal is Forge::Principal,
    {actions_clause},
    resource is {schema_name}
);"#
            ),
        });
        return;
    }

    for role in roles {
        let policy_id_suffix = sanitize_id(role);
        if role == "public" {
            out.push(CedarPolicy {
                description: format!(
                    "Allow public (unauthenticated) {label} on {schema_name} entities"
                ),
                cedar_text: format!(
                    r#"@id("forge.{lname}.{label}_public")
permit (
    principal,
    {actions_clause},
    resource is {schema_name}
);"#
                ),
            });
        } else {
            out.push(CedarPolicy {
                description: format!(
                    "Allow Forge::Group::\"{role}\" to {label} {schema_name} entities"
                ),
                cedar_text: format!(
                    r#"@id("forge.{lname}.{label}_{policy_id_suffix}")
permit (
    principal in Forge::Group::"{role}",
    {actions_clause},
    resource is {schema_name}
);"#
                ),
            });
        }
    }
}

fn format_actions_clause(actions: &[String]) -> String {
    if actions.len() == 1 {
        format!("action == {}", actions[0])
    } else {
        format!("action in [{}]", actions.join(", "))
    }
}

// ---------------------------------------------------------------------------
// Per-field policies (@field_access)
// ---------------------------------------------------------------------------

fn generate_field_access_policies(schema: &SchemaDefinition) -> Vec<CedarPolicy> {
    let name = schema.name.as_str();
    let lname = name.to_ascii_lowercase();
    let mut out = Vec::new();
    for field in &schema.fields {
        let Some(FieldAnnotation::FieldAccess { read, write }) = field.field_access() else {
            continue;
        };
        let fname = field.name.as_str();
        out.extend(field_action_policies(
            &lname,
            name,
            fname,
            "read",
            &format!("Action::\"ReadField{name}_{fname}\""),
            read,
        ));
        out.extend(field_action_policies(
            &lname,
            name,
            fname,
            "write",
            &format!("Action::\"WriteField{name}_{fname}\""),
            write,
        ));
    }
    out
}

fn field_action_policies(
    lname: &str,
    schema_name: &str,
    field_name: &str,
    label: &str,
    action_uid: &str,
    roles: &[String],
) -> Vec<CedarPolicy> {
    if roles.is_empty() {
        return vec![CedarPolicy {
            description: format!(
                "Allow any authenticated user to {label} field {field_name} on {schema_name}"
            ),
            cedar_text: format!(
                r#"@id("forge.{lname}.field_{field_name}_{label}_authenticated")
permit (
    principal is Forge::Principal,
    action == {action_uid},
    resource is {schema_name}
);"#
            ),
        }];
    }
    roles
        .iter()
        .map(|role| {
            let suffix = sanitize_id(role);
            CedarPolicy {
                description: format!(
                    "Allow Forge::Group::\"{role}\" to {label} field {field_name} on {schema_name}"
                ),
                cedar_text: format!(
                    r#"@id("forge.{lname}.field_{field_name}_{label}_{suffix}")
permit (
    principal in Forge::Group::"{role}",
    action == {action_uid},
    resource is {schema_name}
);"#
                ),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn sanitize_id(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}

fn find_owner_field(schema: &SchemaDefinition) -> Option<String> {
    schema.fields.iter().find_map(|f: &FieldDefinition| {
        if f.annotations
            .iter()
            .any(|a| matches!(a, FieldAnnotation::Owner))
        {
            Some(f.name.as_str().to_string())
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cedar_policy::PolicySet;
    use schema_forge_core::types::{
        Annotation, FieldAnnotation, FieldDefinition, FieldName, FieldType, IntegerConstraints,
        SchemaId, SchemaName, TextConstraints,
    };

    fn make_test_schema() -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Contact").unwrap(),
            vec![FieldDefinition::new(
                FieldName::new("name").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            )],
            vec![],
        )
        .unwrap()
    }

    fn make_owner_schema() -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Task").unwrap(),
            vec![
                FieldDefinition::new(
                    FieldName::new("title").unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                ),
                FieldDefinition::with_annotations(
                    FieldName::new("created_by").unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                    vec![],
                    vec![FieldAnnotation::Owner],
                ),
            ],
            vec![],
        )
        .unwrap()
    }

    fn make_access_schema(read: &[&str], write: &[&str], delete: &[&str]) -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Secured").unwrap(),
            vec![FieldDefinition::new(
                FieldName::new("name").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            )],
            vec![Annotation::Access {
                read: read.iter().map(|r| r.to_string()).collect(),
                write: write.iter().map(|r| r.to_string()).collect(),
                delete: delete.iter().map(|r| r.to_string()).collect(),
                cross_tenant_read: vec![],
            }],
        )
        .unwrap()
    }

    fn make_field_access_schema() -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Employee").unwrap(),
            vec![
                FieldDefinition::new(
                    FieldName::new("name").unwrap(),
                    FieldType::Text(TextConstraints::unconstrained()),
                ),
                FieldDefinition::with_annotations(
                    FieldName::new("salary").unwrap(),
                    FieldType::Integer(IntegerConstraints::default()),
                    vec![],
                    vec![FieldAnnotation::FieldAccess {
                        read: vec!["hr".into()],
                        write: vec!["hr".into()],
                    }],
                ),
            ],
            vec![],
        )
        .unwrap()
    }

    fn make_user_schema() -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("User").unwrap(),
            vec![FieldDefinition::new(
                FieldName::new("email").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            )],
            vec![],
        )
        .unwrap()
    }

    fn join(policies: &[CedarPolicy]) -> String {
        policies
            .iter()
            .map(|p| p.cedar_text.clone())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    #[test]
    fn global_policies_include_platform_admin_permit() {
        let g = generate_global_policies();
        assert!(g.iter().any(|p| p.cedar_text.contains("platform_admin")));
    }

    #[test]
    fn global_policies_include_user_role_rank_forbid() {
        let g = generate_global_policies();
        assert!(g.iter().any(|p| p.cedar_text.contains("forbid")));
        assert!(g.iter().any(|p| p.cedar_text.contains("role_rank")));
    }

    #[test]
    fn default_schema_emits_read_write_delete_admin() {
        let schema = make_test_schema();
        let policies = generate_cedar_policies(&schema);
        assert!(policies.iter().any(|p| p.cedar_text.contains("ReadContact")));
        assert!(policies.iter().any(|p| p.cedar_text.contains("CreateContact")));
        assert!(policies.iter().any(|p| p.cedar_text.contains("DeleteContact")));
        assert!(policies.iter().any(|p| p.cedar_text.contains("UpdateSchema")));
    }

    #[test]
    fn owner_schema_emits_owner_write_policy() {
        let schema = make_owner_schema();
        let policies = generate_cedar_policies(&schema);
        assert!(policies
            .iter()
            .any(|p| p.cedar_text.contains("created_by") && p.cedar_text.contains("principal.id")));
    }

    #[test]
    fn access_annotation_drives_per_role_policies() {
        let schema = make_access_schema(&["viewer"], &["editor"], &["admin"]);
        let policies = generate_cedar_policies(&schema);
        assert!(policies
            .iter()
            .any(|p| p.cedar_text.contains("Forge::Group::\"viewer\"")));
        assert!(policies
            .iter()
            .any(|p| p.cedar_text.contains("Forge::Group::\"editor\"")));
        assert!(policies
            .iter()
            .any(|p| p.cedar_text.contains("Forge::Group::\"admin\"")));
    }

    #[test]
    fn public_role_emits_unauthenticated_permit() {
        let schema = make_access_schema(&["public"], &["editor"], &["admin"]);
        let policies = generate_cedar_policies(&schema);
        let read = policies
            .iter()
            .find(|p| p.cedar_text.contains("read_public"))
            .expect("should emit a public read policy");
        assert!(read.cedar_text.contains("principal,"));
    }

    #[test]
    fn empty_role_list_emits_authenticated_permit() {
        let schema = make_access_schema(&[], &["editor"], &["admin"]);
        let policies = generate_cedar_policies(&schema);
        let read = policies
            .iter()
            .find(|p| p.cedar_text.contains("read_authenticated"))
            .expect("should emit an authenticated read policy");
        assert!(read.cedar_text.contains("Forge::Principal"));
    }

    #[test]
    fn field_access_emits_per_field_per_role_policies() {
        let schema = make_field_access_schema();
        let policies = generate_cedar_policies(&schema);
        assert!(policies
            .iter()
            .any(|p| p.cedar_text.contains("ReadFieldEmployee_salary")));
        assert!(policies
            .iter()
            .any(|p| p.cedar_text.contains("WriteFieldEmployee_salary")));
        assert!(policies
            .iter()
            .any(|p| p.cedar_text.contains("Forge::Group::\"hr\"")));
    }

    #[test]
    fn full_policy_set_validates_against_generated_schema() {
        // The contract: generated schema + generated policies must pass
        // strict-mode validation. This is the test that gates every commit
        // touching either generator.
        use cedar_policy::{ValidationMode, Validator};

        let schemas = vec![
            make_test_schema(),
            make_owner_schema(),
            make_access_schema(&["viewer"], &["editor"], &["admin"]),
            make_field_access_schema(),
            make_user_schema(),
        ];

        let schema_src = crate::cedar::generate_cedar_schema(&schemas).unwrap();
        let (cedar_schema, _warnings) =
            cedar_policy::Schema::from_cedarschema_str(&schema_src).expect("schema must parse");

        let policies = generate_full_policy_set(&schemas);
        let policy_src = join(&policies);
        let policy_set: PolicySet = policy_src.parse().expect("policies must parse");

        let validator = Validator::new(cedar_schema);
        let result = validator.validate(&policy_set, ValidationMode::Strict);
        assert!(
            result.validation_passed(),
            "validator must accept the bundle.\nErrors:\n{}\n\nSchema:\n{}\n\nPolicies:\n{}",
            result
                .validation_errors()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n"),
            schema_src,
            policy_src
        );
    }
}
