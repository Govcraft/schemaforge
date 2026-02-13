use serde::{Deserialize, Serialize};

/// Annotations that can be applied to individual fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "annotation")]
#[non_exhaustive]
pub enum FieldAnnotation {
    /// `@field_access(...)` -- role-based access control on a specific field.
    FieldAccess {
        read: Vec<String>,
        write: Vec<String>,
    },
    /// `@owner` -- marks this field as the ownership field for the record.
    Owner,
    /// `@widget("widget_type")` -- UI widget hint for rendering this field.
    Widget { widget_type: String },
    /// `@kanban_column` -- marks this field as the grouping column for kanban views.
    KanbanColumn,
}

impl FieldAnnotation {
    /// Returns the annotation kind as a string, for dedup checking.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::FieldAccess { .. } => "field_access",
            Self::Owner => "owner",
            Self::Widget { .. } => "widget",
            Self::KanbanColumn => "kanban_column",
        }
    }
}

impl std::fmt::Display for FieldAnnotation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FieldAccess { read, write } => {
                write!(
                    f,
                    "@field_access(read=[{}], write=[{}])",
                    format_role_list(read),
                    format_role_list(write),
                )
            }
            Self::Owner => write!(f, "@owner"),
            Self::Widget { widget_type } => write!(f, "@widget(\"{widget_type}\")"),
            Self::KanbanColumn => write!(f, "@kanban_column"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_field_access() {
        let a = FieldAnnotation::FieldAccess {
            read: vec!["admin".into(), "viewer".into()],
            write: vec!["admin".into()],
        };
        assert_eq!(
            a.to_string(),
            "@field_access(read=[\"admin\", \"viewer\"], write=[\"admin\"])"
        );
    }

    #[test]
    fn display_owner() {
        let a = FieldAnnotation::Owner;
        assert_eq!(a.to_string(), "@owner");
    }

    #[test]
    fn kind_returns_correct_strings() {
        assert_eq!(
            FieldAnnotation::FieldAccess {
                read: vec![],
                write: vec![],
            }
            .kind(),
            "field_access"
        );
        assert_eq!(FieldAnnotation::Owner.kind(), "owner");
    }

    #[test]
    fn serde_roundtrip_field_access() {
        let a = FieldAnnotation::FieldAccess {
            read: vec!["admin".into(), "viewer".into()],
            write: vec!["admin".into()],
        };
        let json = serde_json::to_string(&a).unwrap();
        let back: FieldAnnotation = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn serde_roundtrip_owner() {
        let a = FieldAnnotation::Owner;
        let json = serde_json::to_string(&a).unwrap();
        let back: FieldAnnotation = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn display_widget() {
        let a = FieldAnnotation::Widget {
            widget_type: "status_badge".into(),
        };
        assert_eq!(a.to_string(), "@widget(\"status_badge\")");
    }

    #[test]
    fn display_kanban_column() {
        let a = FieldAnnotation::KanbanColumn;
        assert_eq!(a.to_string(), "@kanban_column");
    }

    #[test]
    fn kind_widget() {
        assert_eq!(
            FieldAnnotation::Widget {
                widget_type: "progress".into(),
            }
            .kind(),
            "widget"
        );
    }

    #[test]
    fn kind_kanban_column() {
        assert_eq!(FieldAnnotation::KanbanColumn.kind(), "kanban_column");
    }

    #[test]
    fn serde_roundtrip_widget() {
        let a = FieldAnnotation::Widget {
            widget_type: "currency".into(),
        };
        let json = serde_json::to_string(&a).unwrap();
        let back: FieldAnnotation = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn serde_roundtrip_kanban_column() {
        let a = FieldAnnotation::KanbanColumn;
        let json = serde_json::to_string(&a).unwrap();
        let back: FieldAnnotation = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn serde_field_access_empty_vecs() {
        let a = FieldAnnotation::FieldAccess {
            read: vec![],
            write: vec![],
        };
        let json = serde_json::to_string(&a).unwrap();
        let back: FieldAnnotation = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }
}
