use serde::{Deserialize, Serialize};

use super::field_name::FieldName;
use super::schema_name::SchemaName;
use super::schema_version::SchemaVersion;

/// Multi-tenancy configuration for a schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TenantKind {
    /// This schema is a tenant root (top-level tenant boundary).
    Root,
    /// This schema is a child of another tenant-root schema.
    Child { parent: SchemaName },
}

/// Annotations that can be applied to a schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "annotation")]
#[non_exhaustive]
pub enum Annotation {
    /// `@version(N)` -- declares the schema version.
    Version { version: SchemaVersion },
    /// `@display("field_name")` -- which field to use as display name.
    Display { field: FieldName },
    /// `@system` -- marks a schema as system-internal (not user-editable).
    System,
    /// `@access(...)` -- role-based access control on the schema.
    Access {
        read: Vec<String>,
        write: Vec<String>,
        delete: Vec<String>,
        cross_tenant_read: Vec<String>,
    },
    /// `@tenant(...)` -- multi-tenancy configuration.
    Tenant(TenantKind),
}

impl std::fmt::Display for Annotation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Version { version } => write!(f, "@version({version})"),
            Self::Display { field } => write!(f, "@display(\"{field}\")"),
            Self::System => write!(f, "@system"),
            Self::Access {
                read,
                write,
                delete,
                cross_tenant_read,
            } => {
                write!(
                    f,
                    "@access(read=[{}], write=[{}], delete=[{}], cross_tenant_read=[{}])",
                    format_role_list(read),
                    format_role_list(write),
                    format_role_list(delete),
                    format_role_list(cross_tenant_read),
                )
            }
            Self::Tenant(TenantKind::Root) => write!(f, "@tenant(root)"),
            Self::Tenant(TenantKind::Child { parent }) => {
                write!(f, "@tenant(child(\"{parent}\"))")
            }
        }
    }
}

/// Formats a list of role strings as a comma-separated, quoted list.
fn format_role_list(roles: &[String]) -> String {
    roles
        .iter()
        .map(|r| format!("\"{r}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

impl Annotation {
    /// Returns the annotation kind as a string, for dedup checking.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Version { .. } => "version",
            Self::Display { .. } => "display",
            Self::System => "system",
            Self::Access { .. } => "access",
            Self::Tenant(_) => "tenant",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_version() {
        let a = Annotation::Version {
            version: SchemaVersion::new(3).unwrap(),
        };
        assert_eq!(a.to_string(), "@version(3)");
        assert_eq!(a.kind(), "version");
    }

    #[test]
    fn display_display() {
        let a = Annotation::Display {
            field: FieldName::new("name").unwrap(),
        };
        assert_eq!(a.to_string(), "@display(\"name\")");
        assert_eq!(a.kind(), "display");
    }

    #[test]
    fn serde_roundtrip() {
        let annotations = vec![
            Annotation::Version {
                version: SchemaVersion::new(2).unwrap(),
            },
            Annotation::Display {
                field: FieldName::new("title").unwrap(),
            },
        ];
        for a in annotations {
            let json = serde_json::to_string(&a).unwrap();
            let back: Annotation = serde_json::from_str(&json).unwrap();
            assert_eq!(a, back);
        }
    }

    #[test]
    fn display_system() {
        let a = Annotation::System;
        assert_eq!(a.to_string(), "@system");
        assert_eq!(a.kind(), "system");
    }

    #[test]
    fn display_access() {
        let a = Annotation::Access {
            read: vec!["admin".into(), "viewer".into()],
            write: vec!["admin".into()],
            delete: vec!["admin".into()],
            cross_tenant_read: vec!["superadmin".into()],
        };
        assert_eq!(
            a.to_string(),
            "@access(read=[\"admin\", \"viewer\"], write=[\"admin\"], \
             delete=[\"admin\"], cross_tenant_read=[\"superadmin\"])"
        );
        assert_eq!(a.kind(), "access");
    }

    #[test]
    fn display_tenant_root() {
        let a = Annotation::Tenant(TenantKind::Root);
        assert_eq!(a.to_string(), "@tenant(root)");
        assert_eq!(a.kind(), "tenant");
    }

    #[test]
    fn display_tenant_child() {
        let a = Annotation::Tenant(TenantKind::Child {
            parent: SchemaName::new("Organization").unwrap(),
        });
        assert_eq!(a.to_string(), "@tenant(child(\"Organization\"))");
        assert_eq!(a.kind(), "tenant");
    }

    #[test]
    fn serde_roundtrip_system() {
        let a = Annotation::System;
        let json = serde_json::to_string(&a).unwrap();
        let back: Annotation = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn serde_roundtrip_access() {
        let a = Annotation::Access {
            read: vec!["admin".into(), "viewer".into()],
            write: vec!["admin".into()],
            delete: vec!["admin".into()],
            cross_tenant_read: vec![],
        };
        let json = serde_json::to_string(&a).unwrap();
        let back: Annotation = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn serde_roundtrip_tenant_root() {
        let a = Annotation::Tenant(TenantKind::Root);
        let json = serde_json::to_string(&a).unwrap();
        let back: Annotation = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn serde_roundtrip_tenant_child() {
        let a = Annotation::Tenant(TenantKind::Child {
            parent: SchemaName::new("Organization").unwrap(),
        });
        let json = serde_json::to_string(&a).unwrap();
        let back: Annotation = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn serde_access_empty_vecs() {
        let a = Annotation::Access {
            read: vec![],
            write: vec![],
            delete: vec![],
            cross_tenant_read: vec![],
        };
        let json = serde_json::to_string(&a).unwrap();
        let back: Annotation = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }
}
